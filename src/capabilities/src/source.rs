//! The source repository in Cloudflare Artifacts: the repository itself (imported from a git
//! remote when created), the queue its push events go to with the hosting Worker as consumer,
//! and short-lived git tokens on demand. Coding agents write their branches here through MCP
//! with `branch_push`, or with a `session_credential` through the git proxy ([`push`]).

pub mod push;
pub mod read;

use std::collections::BTreeSet;

use pigeon_core::artifacts::{self, Access};
use pigeon_core::mcp::Tool;
use pigeon_core::pack::{self, Objects};
use pigeon_core::{git, patch, queues, rebase};
use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::land::merge::{self, Session};
use crate::{Call, Capability, Context, Footprint, arg, to_value, unknown};

/// `error` with the remote's URL taken out: the answer must not show where the repository is.
fn hide(error: Error, url: &str) -> Error {
    let text = error.to_string();
    if url.is_empty() {
        return Error::RustError(text);
    }
    Error::RustError(text.replace(url, "<remote>"))
}

/// One side of a changed file for the patch; its bytes stay empty for a file the patch will
/// skip by path or mode, whose blob was not fetched.
fn side<'a>(
    objects: &'a Objects,
    change: Option<&'a (String, String)>,
    skip: bool,
) -> Result<Option<patch::Side<'a>>> {
    let Some((mode, id)) = change else {
        return Ok(None);
    };
    let data: &[u8] = if skip {
        &[]
    } else {
        match objects.by_id.get(id) {
            Some((3, data)) => data,
            _ => {
                return Err(Error::RustError(format!(
                    "blob {id} was not in what the remote sent"
                )));
            }
        }
    };
    Ok(Some(patch::Side { mode, id, data }))
}

/// The source repository and where its push events go.
pub struct Source {
    pub namespace: String,
    pub repo: String,
    pub default_branch: String,
    pub description: String,
    /// The git remote imported when the repository is created.
    pub import_from: Option<String>,
    /// The queue its push events go to, consumed by the hosting Worker.
    pub queue: String,
    /// The git proxy's remote for this repository, which a `session_credential` reaches, such
    /// as `https://proxy.pigeon.dev/git/<namespace>/<repo>.git`; `None` mints none.
    pub proxy_remote: Option<String>,
}

/// How long a git token lives unless asked otherwise, and at most.
const TOKEN_TTL: u64 = 900;
const TOKEN_MAX_TTL: u64 = 86_400;

/// What `setup_source` did.
#[derive(Serialize)]
struct Setup {
    repo: artifacts::Ensured,
    events: queues::Ensured,
    subscription: queues::Subscribed,
}

/// A git token handed to the caller, which is behind Access.
#[derive(Serialize)]
struct Handed {
    id: Option<String>,
    token: String,
    access: Access,
    ttl_secs: u64,
    remote: Option<String>,
}

impl Source {
    async fn setup(&self, cx: &Context<'_>) -> Result<Setup> {
        let spec = artifacts::Spec {
            namespace: self.namespace.clone(),
            name: self.repo.clone(),
            default_branch: self.default_branch.clone(),
            description: Some(self.description.clone()),
            import_from: self.import_from.clone(),
        };
        let repo = artifacts::ensure(cx.account, &spec).await?;
        let events = queues::Spec {
            queue: self.queue.clone(),
            worker: cx.hosted.worker.to_string(),
            batch_size: 10,
            max_wait_ms: 5_000,
            max_retries: 5,
        };
        let events = queues::ensure(cx.account, &events).await?;
        let subscription = queues::Subscription {
            name: format!("{}-{}-pushed", self.namespace, self.repo),
            queue_id: events.queue_id.clone(),
            sources: artifacts::events_source(&self.namespace, &self.repo),
            events: vec!["pushed".into()],
            source_permissions: artifacts::EVENTS_PERMISSIONS.map(String::from).to_vec(),
        };
        let subscription = queues::subscribe(cx.account, &subscription).await?;
        Ok(Setup {
            repo,
            events,
            subscription,
        })
    }

    /// `branch` as a patch against its merge base with the default branch. The git session is
    /// always closed, and the answer holds neither the token nor the remote's URL.
    async fn branch_patch(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let branch = patch::branch_ref(arg(args, "branch")?).map_err(Error::RustError)?;
        let session = Session::open(cx.account, &self.namespace, &self.repo, Access::Read).await?;
        let out = self
            .patch_of(&session, &branch)
            .await
            .map_err(|e| hide(e, &session.remote.url));
        session.close().await;
        out
    }

    /// The work of [`Source::branch_patch`] inside an open session.
    async fn patch_of(&self, session: &Session<'_>, branch: &str) -> Result<Value> {
        let remote = &session.remote;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        let refs = git::refs(remote, "git-upload-pack").await?;
        let Some(tip) = refs.refs.get(branch).cloned() else {
            return Err(Error::RustError(format!("unknown branch `{short}`")));
        };
        let main_ref = format!("refs/heads/{}", self.default_branch);
        let seen = match merge::see(session, &main_ref, &tip).await? {
            Ok(seen) => seen,
            Err(why) => return Err(Error::RustError(why)),
        };
        let merge::Seen {
            main,
            base,
            changed,
            mut objects,
            ..
        } = seen;
        let base_side = rebase::changes(&objects, &tip, &base).map_err(Error::RustError)?;
        let old_of = |path: &str| base_side.get(path).and_then(|c| c.as_ref());
        let skips = |path: &str, new: Option<&(String, String)>| {
            let old = old_of(path);
            patch::precheck(
                path,
                old.map(|(m, _)| m.as_str()),
                new.map(|(m, _)| m.as_str()),
            )
            .is_some()
        };
        // The blobs the patch needs and `see` did not bring.
        let mut need = BTreeSet::new();
        for (path, change) in &changed {
            if skips(path, change.as_ref()) {
                continue;
            }
            for (_, id) in [old_of(path), change.as_ref()].into_iter().flatten() {
                if !objects.by_id.contains_key(id) {
                    need.insert(id.clone());
                }
            }
        }
        if !need.is_empty() {
            let by_id = refs
                .caps
                .iter()
                .any(|c| c == "allow-reachable-sha1-in-want" || c == "allow-any-sha1-in-want");
            // One request: the blobs by id when the remote allows it, else the tip and the base
            // one commit deep.
            let (wants, depth) = if by_id {
                (need.into_iter().collect::<Vec<_>>(), None)
            } else {
                let mut wants = vec![tip.clone()];
                if base != tip {
                    wants.push(base.clone());
                }
                (wants, Some(1))
            };
            let got = git::fetch_with(remote, &refs.caps, &wants, &[], None, depth).await?;
            let more = pack::objects(&got.pack).map_err(Error::RustError)?;
            objects.by_id.extend(more.by_id);
        }
        let mut files = Vec::new();
        for (path, change) in &changed {
            let skip = skips(path, change.as_ref());
            files.push(patch::File {
                path,
                old: side(&objects, old_of(path), skip)?,
                new: side(&objects, change.as_ref(), skip)?,
            });
        }
        let built = patch::assemble(&files).map_err(Error::RustError)?;
        Ok(json!({
            "branch": short,
            "tip": tip,
            "base": base,
            "main": main,
            "files": built.files,
            "bytes": built.patch.len(),
            "patch": built.patch,
            "skipped": built.skipped,
        }))
    }

    async fn token(&self, cx: &Context<'_>, args: &Value) -> Result<Handed> {
        let access = match args["access"].as_str().unwrap_or("read") {
            "read" => Access::Read,
            "write" => Access::Write,
            other => {
                return Err(Error::RustError(format!(
                    "access `{other}` is not read or write"
                )));
            }
        };
        let ttl = args["ttl"].as_u64().unwrap_or(TOKEN_TTL).min(TOKEN_MAX_TTL);
        let minted = artifacts::mint(cx.account, &self.namespace, &self.repo, access, ttl).await?;
        let remote = artifacts::repo(cx.account, &self.namespace, &self.repo)
            .await?
            .map(|r| r.remote);
        Ok(Handed {
            id: minted.id,
            token: minted.value,
            access: minted.access,
            ttl_secs: minted.ttl_secs,
            remote,
        })
    }
}

impl Capability for Source {
    fn name(&self) -> &'static str {
        "source"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "source",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: read::RESOLVE,
                description: "Resolve HEAD, a branch or a commit in an instance's approved repository.",
                input_schema: json!({
                    "type": "object",
                    "properties": {"instance": {"type": "string"}, "revision": {"type": "string", "default": "HEAD"}},
                    "required": ["instance"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: read::FILE,
                description: "Read a regular file up to 1 MiB from an instance's approved repository.",
                input_schema: json!({
                    "type": "object",
                    "properties": {"instance": {"type": "string"}, "revision": {"type": "string", "default": "HEAD"}, "path": {"type": "string"}},
                    "required": ["instance", "path"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "source_token",
                description: "Mint a git token for the source repository. Git sends it as \
                              `Authorization: Bearer <token>`. Answers the token, its id and the \
                              repository's remote.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "access": { "type": "string", "enum": ["read", "write"], "default": "read" },
                        "ttl": { "type": "integer", "description": "Seconds; 900 by default, 86400 at most." }
                    }
                }),
            },
            Tool::bare(
                "source_tokens",
                "List the source repository's git tokens, live and past, without their values.",
            ),
            Tool {
                name: "source_revoke",
                description: "Revoke one of the source repository's git tokens by id.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool {
                name: "branch_patch",
                description: "Read-only. Answer a branch of the source repository as a unified \
                              patch against its merge base with main, for break-glass sessions \
                              that cannot reach Artifacts. Write `patch` to a file and run `git \
                              apply --3way` on a checkout of main from the GitHub mirror; the \
                              checkout needs the commit `base`. Answers branch, tip, base, main, \
                              files, bytes, patch and `skipped`: binary files, submodules, \
                              symlinks, secret-shaped files and files too large are not in the \
                              patch and are listed there. A patch over 1 MiB is an error.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "branch": { "type": "string", "description": "The branch, with or without `refs/heads/`." }
                    },
                    "required": ["branch"]
                }),
            },
            Tool {
                name: "branch_push",
                description: "Push your commits to a branch of the source repository, with no \
                              token: the default way for a coding agent to push. Send a git \
                              bundle, base64-encoded: `git bundle create /tmp/b.bundle \
                              origin/main..HEAD && base64 -w0 /tmp/b.bundle`. Its commits are \
                              written as they are (ids, authors, messages and trailers kept). \
                              The branch must be under your app's namespace: `claude/` for \
                              Claude, `codex/` for OpenAI's Codex and ChatGPT; never main. \
                              Fast-forward only, unless `force` (your own namespace only). The \
                              bundle's base must be on main or on the branch; 8 MiB at most \
                              decoded (use session_credential for more). Answers the new tip \
                              `commit`: then call land {branch, commit}.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "branch": { "type": "string", "description": "Such as `codex/my-change`, with or without `refs/heads/`." },
                        "bundle": { "type": "string", "description": "The git bundle, standard base64." },
                        "force": { "type": "boolean", "description": "Rewrite the branch when the bundle does not descend from its tip. Default false." }
                    },
                    "required": ["branch", "bundle"],
                    "additionalProperties": false
                }),
            },
            Tool {
                name: "session_credential",
                description: "Mint a short-lived git credential for pushes too large for \
                              branch_push. It reaches the git proxy only: it fetches the \
                              repository and pushes only branches under your app's namespace \
                              (`claude/` or `codex/`), never main, and nothing else. An hour by \
                              default, 4 hours at most. Git sends it as `Authorization: Bearer \
                              <token>` (http.extraHeader). The token lands in your transcript: \
                              do not save it anywhere lasting. Revoked by the app's Disconnect \
                              and by removing the person.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ttl": { "type": "integer", "description": "Seconds; 3600 by default, 60 to 14400." }
                    },
                    "additionalProperties": false
                }),
            },
            Tool::bare(
                "setup_source",
                "Make sure the source repository, its push-event queue and subscription exist.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                read::RESOLVE | read::FILE => read::run(cx, tool, args).await?,
                "source_token" => to_value(self.token(cx, args).await?),
                "source_tokens" => {
                    to_value(artifacts::tokens(cx.account, &self.namespace, &self.repo).await?)
                }
                "source_revoke" => {
                    let id = arg(args, "id")?;
                    artifacts::revoke(cx.account, &self.namespace, &self.repo, id).await?;
                    json!({ "revoked": id })
                }
                "branch_patch" => self.branch_patch(cx, args).await?,
                "branch_push" => {
                    push::branch_push(cx, &self.namespace, &self.repo, &self.default_branch, args)
                        .await?
                }
                "session_credential" => {
                    let remote = self
                        .proxy_remote
                        .as_deref()
                        .ok_or_else(|| Error::RustError("this platform has no git proxy".into()))?;
                    push::session_credential(cx, remote, args).await?
                }
                "setup_source" => to_value(self.setup(cx).await?),
                other => return Err(unknown(other)),
            })
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            repos: vec![format!("{}/{}", self.namespace, self.repo)],
            queues: vec![self.queue.clone()],
            ..Footprint::default()
        }
    }
}
