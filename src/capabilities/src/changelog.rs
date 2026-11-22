//! The changelog of main (docs/plans/changelog.md): made after the fact from git history, on
//! each Artifacts push to main or to the landing notes, and kept as R2 objects in pigeon's own
//! account. It never commits, and it never holds up a landing, a deploy or CI: it runs from the
//! queue after every other capability, a run that takes longer than [`RUN_LIMIT_MS`] is left
//! for the next push, and a failure is only logged. It reads and writes the bucket through
//! core's [`BINDING`] when core has one (no token at all), and otherwise through the R2 API
//! with short-lived tokens minted from the parent token.

use std::collections::BTreeMap;

use pigeon_core::artifacts::{self, Access};
use pigeon_core::changelog::{self, State, VERSION};
use pigeon_core::git::{self, Remote};
use pigeon_core::mcp::Tool;
use pigeon_core::{pack, r2};
use serde_json::{Value, json};
use worker::{Error, Result, console_error};

use crate::{Call, Capability, Context, unknown};

/// The ref the Lander's landing notes live under.
const NOTES_REF: &str = "refs/notes/land";
/// The rendered changelog's key in the bucket.
const MARKDOWN_KEY: &str = "main/CHANGELOG.md";
/// The state's key in the bucket.
const STATE_KEY: &str = "main/state.json";
/// How long the Artifacts read token lives; it is revoked when the run ends.
const READ_TOKEN_TTL: u64 = 300;
/// The R2 binding of the changelog's bucket in core's wrangler.toml, when it has one.
pub const BINDING: &str = "CHANGELOG";
/// How long a run from the queue may take, wall clock; then it stops and the next push catches
/// up, so the batch is never held.
pub const RUN_LIMIT_MS: u64 = 60_000;

/// The changelog as a capability: on pushes to main or the landing notes, and on demand.
pub struct Changelog {
    /// The Artifacts namespace and repository of main.
    pub namespace: String,
    pub repo: String,
    /// The branch it follows, such as `main`.
    pub branch: String,
    /// The R2 bucket it is kept in, in the account's default jurisdiction.
    pub bucket: String,
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

impl Changelog {
    fn bucket(&self) -> r2::Bucket {
        r2::Bucket {
            name: self.bucket.clone(),
            jurisdiction: None,
        }
    }

    /// The object at `key`, if there is one.
    async fn get(&self, cx: &Context<'_>, key: &str) -> Result<Option<Vec<u8>>> {
        if let Ok(bound) = cx.env.bucket(BINDING) {
            return match bound.get(key).execute().await? {
                Some(object) => match object.body() {
                    Some(body) => Ok(Some(body.bytes().await?)),
                    None => Ok(Some(Vec::new())),
                },
                None => Ok(None),
            };
        }
        r2::get(cx.account, &self.bucket(), key).await
    }

    /// Writes each `(key, content type, body)`; through the API, the bucket is made first when
    /// it is missing.
    async fn put_all(&self, cx: &Context<'_>, items: Vec<(&str, &str, Vec<u8>)>) -> Result<()> {
        if let Ok(bound) = cx.env.bucket(BINDING) {
            for (key, content_type, body) in items {
                bound
                    .put(key, worker::Data::Bytes(body))
                    .http_metadata(worker::HttpMetadata {
                        content_type: Some(content_type.to_string()),
                        ..Default::default()
                    })
                    .execute()
                    .await?;
            }
            return Ok(());
        }
        let bucket = self.bucket();
        r2::ensure_bucket(cx.account, &bucket).await?;
        for (key, content_type, body) in items {
            r2::put(cx.account, &bucket, key, content_type, body).await?;
        }
        Ok(())
    }

    /// Brings the stored changelog up to main: from the last processed commit, or from nothing
    /// when `full`, when nothing is stored, or when the stored commit is not on main.
    async fn update(&self, cx: &Context<'_>, full: bool) -> Result<Value> {
        let stored = if full {
            None
        } else {
            self.get(cx, STATE_KEY)
                .await?
                .and_then(|b| serde_json::from_slice::<State>(&b).ok())
                .filter(|s| s.version == VERSION)
        };
        let found = artifacts::repo(cx.account, &self.namespace, &self.repo)
            .await?
            .ok_or_else(|| bad(format!("Artifacts has no repository `{}`", self.repo)))?;
        let read = artifacts::mint(
            cx.account,
            &self.namespace,
            &self.repo,
            Access::Read,
            READ_TOKEN_TTL,
        )
        .await?;
        let remote = Remote {
            url: found.remote,
            authorization: format!("Bearer {}", read.value),
        };
        let made = self.make(&remote, stored).await;
        if let Some(id) = &read.id
            && let Err(e) = artifacts::revoke(cx.account, &self.namespace, &self.repo, id).await
        {
            console_error!("changelog: revoking the read token: {e}");
        }
        let (state, markdown, added, rebuilt) = made?;
        let body = serde_json::to_vec(&state).map_err(|e| bad(e.to_string()))?;
        self.put_all(
            cx,
            vec![
                (
                    MARKDOWN_KEY,
                    "text/markdown; charset=utf-8",
                    markdown.into_bytes(),
                ),
                (STATE_KEY, "application/json", body),
            ],
        )
        .await?;
        Ok(json!({
            "main": state.main,
            "added": added,
            "total": state.entries.len(),
            "full": rebuilt,
        }))
    }

    /// The new state and its rendering from `remote`, with how many entries were added and
    /// whether it was rebuilt from nothing.
    async fn make(
        &self,
        remote: &Remote,
        stored: Option<State>,
    ) -> Result<(State, String, usize, bool)> {
        let refs = git::refs(remote, "git-upload-pack").await?;
        let main_ref = format!("refs/heads/{}", self.branch);
        let tip = refs
            .refs
            .get(&main_ref)
            .cloned()
            .ok_or_else(|| bad(format!("{main_ref} does not exist")))?;
        let commits = |haves: Vec<String>| {
            let wants = [tip.clone()];
            let caps = refs.caps.clone();
            async move {
                let fetched =
                    git::fetch_with(remote, &caps, &wants, &haves, Some("tree:0"), None).await?;
                pack::objects(&fetched.pack).map_err(bad)
            }
        };
        let (entries, added, rebuilt) = match stored {
            Some(s) if s.main == tip => (s.entries, 0, false),
            Some(s) => {
                let objects = commits(vec![s.main.clone()]).await?;
                match changelog::walk(&objects, &tip, Some(&s.main)) {
                    Ok(mut new) => {
                        let added = new.len();
                        new.extend(s.entries);
                        (new, added, false)
                    }
                    Err(why) => {
                        console_error!("changelog: rebuilding in full: {why}");
                        let all = commits(Vec::new()).await?;
                        let new = changelog::walk(&all, &tip, None).map_err(bad)?;
                        (new.clone(), new.len(), true)
                    }
                }
            }
            None => {
                let all = commits(Vec::new()).await?;
                let new = changelog::walk(&all, &tip, None).map_err(bad)?;
                (new.clone(), new.len(), true)
            }
        };
        let landings = match refs.refs.get(NOTES_REF) {
            Some(notes) => landings(remote, &refs.caps, notes).await?,
            None => BTreeMap::new(),
        };
        let markdown = changelog::render(&entries, &landings, None);
        let state = State {
            version: VERSION,
            main: tip,
            entries,
        };
        Ok((state, markdown, added, rebuilt))
    }

    /// The stored changelog, or its newest `days` date sections.
    async fn read(&self, cx: &Context<'_>, days: Option<usize>) -> Result<Value> {
        let Some(markdown) = self.get(cx, MARKDOWN_KEY).await? else {
            return Ok(json!({
                "markdown": null,
                "note": "no run has written the changelog yet; changelog_update makes it now",
            }));
        };
        let markdown = String::from_utf8_lossy(&markdown).into_owned();
        let main = self
            .get(cx, STATE_KEY)
            .await?
            .and_then(|b| serde_json::from_slice::<State>(&b).ok())
            .map(|s| s.main);
        Ok(json!({
            "main": main,
            "markdown": match days {
                Some(d) => changelog::newest_days(&markdown, d),
                None => markdown,
            },
        }))
    }
}

/// Commit id to landing id, from the notes commit `notes` at `refs/notes/land`: a flat tree
/// whose entries are named by commit id, each blob a land note.
async fn landings(
    remote: &Remote,
    caps: &[String],
    notes: &str,
) -> Result<BTreeMap<String, String>> {
    let fetched = git::fetch_with(remote, caps, &[notes.to_string()], &[], None, Some(1)).await?;
    let objects = pack::objects(&fetched.pack).map_err(bad)?;
    let body = match objects.by_id.get(notes) {
        Some((1, body)) => body,
        _ => return Err(bad(format!("notes commit {notes} was not fetched"))),
    };
    let tree = pack::commit_tree(body).map_err(bad)?;
    let entries = match objects.by_id.get(&tree) {
        Some((2, data)) => pack::tree_entries(data).map_err(bad)?,
        _ => return Err(bad(format!("notes tree {tree} was not fetched"))),
    };
    let mut map = BTreeMap::new();
    for e in entries {
        if let Some((3, blob)) = objects.by_id.get(&e.id)
            && let Some(id) = changelog::landing_of(&String::from_utf8_lossy(blob))
        {
            map.insert(e.name.to_ascii_lowercase(), id);
        }
    }
    Ok(map)
}

/// Whether `event`, an Artifacts push event, moved `refs/heads/<branch>` or the landing notes
/// of `repo`.
pub fn wants_event(repo: &str, branch: &str, event: &Value) -> bool {
    let p = event.get("payload").unwrap_or(event);
    let r#ref = p["ref"].as_str().unwrap_or_default();
    let ours = ["repoName", "repo_name", "repo", "repository"]
        .iter()
        .find_map(|k| {
            event["source"][*k]
                .as_str()
                .or_else(|| p[*k].as_str())
                .or_else(|| p[*k]["name"].as_str())
        })
        .is_none_or(|r| r == repo || r.ends_with(&format!("/{repo}")));
    ours && (r#ref == format!("refs/heads/{branch}") || r#ref == NOTES_REF)
}

impl Capability for Changelog {
    fn name(&self) -> &'static str {
        "changelog"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "changelog",
                description: "The changelog of main: one bullet per commit (subject, short SHA, \
                              landing id when known), grouped by UTC date, newest first. \
                              Read-only; `days` keeps the newest that many dates.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "days": { "type": "integer", "minimum": 1 } }
                }),
            },
            Tool {
                name: "changelog_update",
                description: "Bring the changelog up to main now (it also follows every push \
                              to main); `full: true` rebuilds it from all of main's history.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "full": { "type": "boolean" } }
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "changelog" => {
                    let days = args["days"].as_u64().and_then(|d| usize::try_from(d).ok());
                    self.read(cx, days).await
                }
                "changelog_update" => {
                    self.update(cx, args["full"].as_bool().unwrap_or(false))
                        .await
                }
                _ => Err(unknown(tool)),
            }
        })
    }

    fn wants(&self, event: &Value) -> bool {
        wants_event(&self.repo, &self.branch, event)
    }

    /// Never fails or holds the batch: the queue would retry every other capability's work
    /// with it. A run past [`RUN_LIMIT_MS`] is dropped; the next push brings the changelog up to
    /// date from where the last finished run left it.
    fn on_events<'a>(&'a self, cx: &'a Context<'a>) -> Call<'a> {
        Box::pin(async move {
            let run = self.update(cx, false);
            let limit = worker::Delay::from(std::time::Duration::from_millis(RUN_LIMIT_MS));
            futures_util::pin_mut!(run);
            futures_util::pin_mut!(limit);
            match futures_util::future::select(run, limit).await {
                futures_util::future::Either::Left((Ok(v), _)) => Ok(v),
                futures_util::future::Either::Left((Err(e), _)) => {
                    console_error!("changelog failed: {e}");
                    Ok(json!({ "error": e.to_string() }))
                }
                futures_util::future::Either::Right(_) => {
                    console_error!(
                        "changelog: stopped after {RUN_LIMIT_MS} ms; the next push catches up"
                    );
                    Ok(json!({ "error": "timed out", "limit_ms": RUN_LIMIT_MS }))
                }
            }
        })
    }
}
