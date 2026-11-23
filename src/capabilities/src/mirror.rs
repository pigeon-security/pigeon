//! Keeps a read-only copy of an Artifacts repository on another git host, such as GitHub, for
//! disaster recovery. Artifacts is the source of truth: the copy only follows it, and a mirror
//! that fails never holds anything up.
//!
//! Rules: the named branches only move forward, tags are only added, and nothing is ever
//! deleted. When the copy has commits or a tag Artifacts lacks, that ref is left as it is and
//! reported as diverged. Refusing non-fast-forward updates on the copy's side (a ruleset that
//! blocks force pushes) backs the first rule up.

use std::collections::BTreeMap;

use pigeon_core::Account;
use pigeon_core::artifacts::{self, Access};
use pigeon_core::git::{self, Remote, Update, ZERO};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store;
use serde::Serialize;
use serde_json::Value;
use worker::{Error, Fetch, Headers, Request, RequestInit, Result};

use crate::expiry::{self, Expiry};
use crate::{Call, Capability, Context, Expiring, External, Footprint, Slot, to_value, unknown};

/// The mirror as a capability: on every push to the source's branches or tags, and on demand.
pub struct Mirror {
    pub source: Source,
    /// The copy's URL; its push token is read from Secrets Store binding `token_binding`.
    pub url: String,
    pub token_binding: String,
    /// The tool that runs it on demand, such as `source_mirror`.
    pub tool: &'static str,
}

impl Mirror {
    async fn run(&self, cx: &Context<'_>) -> Result<Report> {
        let token = secrets_store::read(&cx.env.secret_store(&self.token_binding)?)
            .await?
            .ok_or_else(|| {
                Error::RustError(format!(
                    "`{}` is not set in the Secrets Store",
                    self.token_binding
                ))
            })?;
        let copy = Copy {
            url: self.url.clone(),
            token,
        };
        mirror(cx.account, &self.source, &copy).await
    }
}

impl Capability for Mirror {
    fn name(&self) -> &'static str {
        "mirror"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "mirror",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool::bare(
            self.tool,
            "Bring the read-only copy up to the Artifacts repository now; it also runs on every \
             push.",
        )]
    }

    fn secrets(&self) -> Vec<Slot> {
        vec![Slot {
            name: self.token_binding.clone(),
            purpose: format!(
                "Pushes to the read-only copy at {}. A GitHub fine-grained token on that \
                 repository only, with Contents read and write.",
                self.url
            ),
            scopes: &[secrets_store::WORKERS],
        }]
    }

    /// The copy must offer `value` its push service, which GitHub does only for a token that
    /// can push.
    fn check_secret<'a>(
        &'a self,
        _cx: &'a Context<'a>,
        _name: &'a str,
        value: &'a str,
    ) -> Call<'a> {
        Box::pin(async move {
            let remote = Remote {
                url: self.url.clone(),
                authorization: format!(
                    "Basic {}",
                    git::base64(format!("x-access-token:{value}").as_bytes())
                ),
            };
            let refs = git::refs(&remote, "git-receive-pack")
                .await
                .map_err(|e| Error::RustError(format!("the copy refused this token: {e}")))?;
            Ok(Value::String(format!(
                "the copy accepts pushes with it ({} refs)",
                refs.refs.len()
            )))
        })
    }

    /// GitHub answers a token's expiry in the `github-authentication-token-expiration` header
    /// of any authenticated API call; `/rate_limit` costs nothing against the limit. No header
    /// means the token does not expire.
    fn secret_expiry<'a>(&'a self, cx: &'a Context<'a>, name: &'a str) -> Expiring<'a> {
        Box::pin(async move {
            if name != self.token_binding {
                return Ok(Expiry::Unknown {
                    why: "the mirror does not use this secret".into(),
                });
            }
            if !is_github(&self.url) {
                return Ok(Expiry::Unknown {
                    why: "the copy is not on GitHub, so there is no way to ask".into(),
                });
            }
            let token = secrets_store::read(&cx.env.secret_store(&self.token_binding)?)
                .await?
                .ok_or_else(|| Error::RustError("the token is not set".into()))?;
            let headers = Headers::new();
            headers.set("Authorization", &format!("Bearer {token}"))?;
            headers.set("Accept", "application/vnd.github+json")?;
            headers.set("User-Agent", "pigeon")?;
            let mut init = RequestInit::new();
            init.with_headers(headers);
            let req = Request::new_with_init("https://api.github.com/rate_limit", &init)?;
            let res = Fetch::Request(req).send().await?;
            let header = res
                .headers()
                .get("github-authentication-token-expiration")?;
            Ok(github_expiry(res.status_code(), header.as_deref()))
        })
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != self.tool {
                return Err(unknown(tool));
            }
            Ok(to_value(self.run(cx).await?))
        })
    }

    /// An Artifacts push to one of the source's branches or a tag of its repository.
    fn wants(&self, event: &Value) -> bool {
        let p = event.get("payload").unwrap_or(event);
        let r#ref = p["ref"].as_str().unwrap_or_default();
        let repo = &self.source.repo;
        let ours = ["repoName", "repo_name", "repo", "repository"]
            .iter()
            .find_map(|k| {
                event["source"][*k]
                    .as_str()
                    .or_else(|| p[*k].as_str())
                    .or_else(|| p[*k]["name"].as_str())
            })
            .is_none_or(|r| r == repo || r.ends_with(&format!("/{repo}")));
        let branch = self
            .source
            .branches
            .iter()
            .any(|b| r#ref == format!("refs/heads/{b}"));
        ours && (branch || r#ref.starts_with("refs/tags/"))
    }

    fn on_events<'a>(&'a self, cx: &'a Context<'a>) -> Call<'a> {
        Box::pin(async move { Ok(to_value(self.run(cx).await?)) })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            mirrors: vec![self.url.clone()],
            externals: vec![External::new("GitHub", "mirror", self.url.clone())],
            ..Footprint::default()
        }
    }
}

/// The copy: a git remote over HTTPS and the token that pushes to it, sent as
/// `x-access-token:<token>` basic credentials, as GitHub takes them.
pub struct Copy {
    /// Such as `https://github.com/pigeon-security/pigeon.git`.
    pub url: String,
    pub token: String,
}

/// What to mirror: an Artifacts repository's named branches and all its tags.
pub struct Source {
    pub namespace: String,
    pub repo: String,
    /// Such as `["main"]`.
    pub branches: Vec<String>,
}

/// What [`mirror`] did to each ref.
#[derive(Serialize)]
pub struct Report {
    pub refs: Vec<RefReport>,
    /// Whether anything was pushed.
    pub pushed: bool,
}

#[derive(Serialize)]
pub struct RefReport {
    pub name: String,
    pub source: String,
    /// The copy's object before the mirror, if it had the ref.
    pub copy: Option<String>,
    /// `up-to-date`, `created`, `updated`, `diverged` or `rejected: <why>`.
    pub outcome: String,
}

/// How long the Artifacts read token lives; it is revoked when the mirror ends.
const READ_TOKEN_TTL: u64 = 300;

/// Brings `copy` up to `source`; idempotent, and does nothing when the copy is current.
pub async fn mirror(account: &Account, source: &Source, copy: &Copy) -> Result<Report> {
    let repo = artifacts::repo(account, &source.namespace, &source.repo)
        .await?
        .ok_or_else(|| {
            Error::RustError(format!(
                "Artifacts has no repository `{}/{}`",
                source.namespace, source.repo
            ))
        })?;
    let read = artifacts::mint(
        account,
        &source.namespace,
        &source.repo,
        Access::Read,
        READ_TOKEN_TTL,
    )
    .await?;
    let from = Remote {
        url: repo.remote,
        authorization: format!("Bearer {}", read.value),
    };
    let to = Remote {
        url: copy.url.clone(),
        authorization: format!(
            "Basic {}",
            git::base64(format!("x-access-token:{}", copy.token).as_bytes())
        ),
    };
    let report = mirror_with(source, &from, &to).await;
    if let Some(id) = &read.id {
        let revoked = artifacts::revoke(account, &source.namespace, &source.repo, id).await;
        // The mirror's own error comes first; the token ends by its TTL either way.
        let report = report?;
        revoked?;
        return Ok(report);
    }
    report
}

/// What bringing `ours`, the copy's refs, up to `theirs`, the source's, takes: each mirrored
/// ref (the source's branches and every tag) with its outcome, and the updates to push. A tag
/// is never moved.
pub fn plan(
    source: &Source,
    theirs: &BTreeMap<String, String>,
    ours: &BTreeMap<String, String>,
) -> (Vec<RefReport>, Vec<Update>) {
    let mut refs = Vec::new();
    let mut updates = Vec::new();
    let mirrored = theirs.iter().filter(|(name, _)| {
        name.starts_with("refs/tags/")
            || source
                .branches
                .iter()
                .any(|b| name.as_str() == format!("refs/heads/{b}"))
    });
    for (name, sha) in mirrored {
        let copy = ours.get(name).cloned();
        let outcome = match &copy {
            Some(c) if c == sha => "up-to-date",
            // A tag is never moved.
            Some(_) if name.starts_with("refs/tags/") => "diverged",
            Some(_) => "updated",
            None => "created",
        };
        if outcome == "created" || outcome == "updated" {
            updates.push(Update {
                name: name.clone(),
                old: copy.clone().unwrap_or_else(|| ZERO.to_string()),
                new: sha.clone(),
            });
        }
        refs.push(RefReport {
            name: name.clone(),
            source: sha.clone(),
            copy,
            outcome: outcome.to_string(),
        });
    }
    (refs, updates)
}

/// Marks each ref the copy refused in `status`, its report-status lines, `rejected: <why>`.
pub fn rejected(refs: &mut [RefReport], status: &[String]) {
    for line in status {
        if let Some(rest) = line.strip_prefix("ng ") {
            let (name, why) = rest.split_once(' ').unwrap_or((rest, "refused"));
            if let Some(r) = refs.iter_mut().find(|r| r.name == name) {
                r.outcome = format!("rejected: {why}");
            }
        }
    }
}

async fn mirror_with(source: &Source, from: &Remote, to: &Remote) -> Result<Report> {
    let theirs = git::refs(from, "git-upload-pack").await?;
    let ours = git::refs(to, "git-receive-pack").await?;
    let (mut refs, mut updates) = plan(source, &theirs.refs, &ours.refs);
    if updates.is_empty() {
        return Ok(Report {
            refs,
            pushed: false,
        });
    }
    // What the copy already has, so the pack carries only what it lacks. A branch whose copy
    // Artifacts does not acknowledge has commits Artifacts lacks: it is left alone.
    let haves: Vec<String> = updates
        .iter()
        .filter(|u| u.old != ZERO)
        .map(|u| u.old.clone())
        .collect();
    let wants: Vec<String> = updates.iter().map(|u| u.new.clone()).collect();
    let fetched = git::fetch(from, &theirs.caps, &wants, &haves).await?;
    let known = |sha: &str| fetched.acked.iter().any(|a| a == sha);
    // Without multi-ack, Artifacts acknowledges only the first object it has; one branch is the
    // common case, and a second is checked by the copy's refusal of non-fast-forwards.
    let first_have = haves.first().cloned();
    updates.retain(|u| {
        let unknown = u.old != ZERO && Some(&u.old) == first_have.as_ref() && !known(&u.old);
        if unknown && let Some(r) = refs.iter_mut().find(|r| r.name == u.name) {
            r.outcome = "diverged".into();
        }
        !unknown
    });
    if updates.is_empty() {
        return Ok(Report {
            refs,
            pushed: false,
        });
    }
    let status = git::push(to, &ours.caps, &updates, &fetched.pack).await?;
    rejected(&mut refs, &status);
    Ok(Report { refs, pushed: true })
}

/// Whether the copy at `url` is on GitHub, whose API answers a token's expiry.
pub fn is_github(url: &str) -> bool {
    url.strip_prefix("https://")
        .and_then(|r| r.split('/').next())
        .is_some_and(|h| h.eq_ignore_ascii_case("github.com"))
}

/// What GitHub's answer with `status` and expiry header `header` says of a token's expiry.
pub fn github_expiry(status: u16, header: Option<&str>) -> Expiry {
    match status {
        200..=299 => expiry::from_field(header),
        401 => Expiry::Refused {
            why: "GitHub refuses the token (401): it has expired or been revoked".into(),
        },
        other => Expiry::Unknown {
            why: format!("GitHub answered {other}"),
        },
    }
}
