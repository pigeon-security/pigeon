//! The Lander's git work: a branch's base on main and what each side changed since, and the
//! branch rebased onto main by the Lander itself, so no agent force-pushes to make a landing
//! fit. The rebase is on whole paths ([`pigeon_core::rebase`]): a path both sides changed is
//! merged line by line only for the files in [`TEXT_MERGE`], and refused otherwise.

use std::collections::BTreeMap;

use pigeon_core::Account;
use pigeon_core::artifacts::{self, Access};
use pigeon_core::git::{self, Remote, Update, ZERO};
use pigeon_core::gitobj::Object;
use pigeon_core::lanes::{self, Map};
use pigeon_core::pack::{self, Objects};
use pigeon_core::rebase::{self, Change};
use pigeon_core::{gitobj, merge3};
use worker::{Error, Result, console_error};

/// How many commits of history are read to find a branch's base on main.
const HISTORY: u32 = 500;
/// How long a git token minted for one step lives, in seconds.
const TOKEN_TTL: u64 = 900;
/// The map's path in the repository.
pub const MAP_PATH: &str = "ci/workers.toml";
/// The files both sides may change, merged line by line ([`merge3`]).
pub const TEXT_MERGE: [&str; 5] = [
    lanes::SMOKE,
    "docs/plans/README.md",
    // Lists that parallel landings each add an entry to; a merged result is tested again.
    MAP_PATH,
    "Cargo.toml",
    ".pigeon/ci.toml",
];
/// The map when main has none, or one that does not parse: docs merge alone, and everything
/// else is core's, so every other change is tested and deployed as today.
const FALLBACK: &str = r#"
version = 1
[docs]
paths = ["docs/*"]
[shared]
paths = []
[[worker]]
name = "core"
script = "pigeon-platform"
dir = "src/platform"
paths = ["*"]
test_job = "test"
deploy_job = "deploy"
after = ["architecture", "portal_sync", "connectors_setup"]
"#;

/// A git session on the repository: a remote with a token minted for it, revoked by
/// [`Session::close`].
pub struct Session<'a> {
    pub remote: Remote,
    account: &'a Account,
    namespace: &'a str,
    repo: &'a str,
    token: artifacts::GitToken,
}

impl<'a> Session<'a> {
    /// Opens a session with `access` on `namespace/repo`.
    pub async fn open(
        account: &'a Account,
        namespace: &'a str,
        repo: &'a str,
        access: Access,
    ) -> Result<Session<'a>> {
        let (found, token) =
            artifacts::session(account, namespace, repo, access, TOKEN_TTL).await?;
        Ok(Session {
            remote: Remote {
                url: found.remote.trim_end_matches('/').to_string(),
                authorization: format!("Bearer {}", token.value),
            },
            account,
            namespace,
            repo,
            token,
        })
    }

    /// Revokes the session's token; a failure is logged, never lost silently.
    pub async fn close(self) {
        if let Some(id) = &self.token.id
            && let Err(e) = artifacts::revoke(self.account, self.namespace, self.repo, id).await
        {
            console_error!("land: revoking a git token: {e}");
        }
    }
}

/// A commit seen against main: main's tip, their base, and what each side changed since it,
/// with main's objects (blobs too) and the commit's and base's trees.
pub struct Seen {
    pub main: String,
    pub base: String,
    pub changed: BTreeMap<String, Change>,
    pub main_moved: BTreeMap<String, Change>,
    pub objects: Objects,
    /// The map, as main has it.
    pub map: Map,
    /// The commit changed `Cargo.lock` only in the workspace's own crates' entries
    /// ([`lanes::lock_members_only`]), so the lockfile picks no lane by itself.
    pub lock_members_only: bool,
}

impl Seen {
    /// The paths the commit changed.
    pub fn changed_paths(&self) -> Vec<&str> {
        self.changed.keys().map(String::as_str).collect()
    }

    /// The paths main changed since the base.
    pub fn main_paths(&self) -> Vec<&str> {
        self.main_moved.keys().map(String::as_str).collect()
    }

    /// Whether `commit` is main itself or a fast-forward of it.
    pub fn forward(&self) -> bool {
        self.base == self.main
    }
}

fn bad(why: String) -> Error {
    Error::RustError(why)
}

/// `commit` seen against `main_ref` in the session's repository: `Ok(Err(why))` when it cannot
/// be landed onto main at all (no main, no common history within [`HISTORY`] commits).
pub async fn see(
    session: &Session<'_>,
    main_ref: &str,
    commit: &str,
) -> Result<std::result::Result<Seen, String>> {
    let remote = &session.remote;
    let refs = git::refs(remote, "git-upload-pack").await?;
    let Some(main) = refs.refs.get(main_ref).cloned() else {
        return Ok(Err(format!("{main_ref} does not exist")));
    };
    let base = if main == commit {
        main.clone()
    } else {
        let history = git::fetch_with(
            remote,
            &refs.caps,
            &[commit.to_string(), main.clone()],
            &[],
            Some("tree:0"),
            Some(HISTORY),
        )
        .await?;
        let commits = pack::objects(&history.pack).map_err(bad)?;
        let parents = rebase::parents(&commits);
        match rebase::merge_base(&parents, commit, &main) {
            Some(b) => b,
            None => {
                return Ok(Err(format!(
                    "{commit} has no common history with main within {HISTORY} commits: \
                     rebase it onto main and land again"
                )));
            }
        }
    };
    // Main whole (its blobs are the map and the other side of a text merge); the commit and its
    // base as trees only.
    let at_main = git::fetch_with(
        remote,
        &refs.caps,
        std::slice::from_ref(&main),
        &[],
        None,
        Some(1),
    )
    .await?;
    let mut objects = pack::objects(&at_main.pack).map_err(bad)?;
    let mut wants = vec![commit.to_string()];
    if base != main && base != commit {
        wants.push(base.clone());
    }
    if wants.iter().any(|w| !objects.by_id.contains_key(w)) {
        let trees =
            git::fetch_with(remote, &refs.caps, &wants, &[], Some("blob:none"), Some(1)).await?;
        let more = pack::objects(&trees.pack).map_err(bad)?;
        objects.by_id.extend(more.by_id);
    }
    let changed = rebase::changes(&objects, &base, commit).map_err(bad)?;
    let main_moved = rebase::changes(&objects, &base, &main).map_err(bad)?;
    let map = map_at(&objects, &main);
    let lock_members_only = if changed.contains_key(lanes::LOCK) {
        let text = |b: Option<Vec<u8>>| b.and_then(|b| String::from_utf8(b).ok());
        let before = text(git::read_file(remote, &base, lanes::LOCK).await?);
        let after = text(git::read_file(remote, commit, lanes::LOCK).await?);
        matches!((before, after), (Some(b), Some(a)) if lanes::lock_members_only(&b, &a))
    } else {
        false
    };
    Ok(Ok(Seen {
        main,
        base,
        changed,
        main_moved,
        objects,
        map,
        lock_members_only,
    }))
}

/// The map main holds, or [`FALLBACK`] when it holds none or one that does not parse.
fn map_at(objects: &Objects, main: &str) -> Map {
    let text = match pack::read_path(objects, main, MAP_PATH) {
        Ok(Some(bytes)) => String::from_utf8(bytes).ok(),
        Ok(None) => None,
        Err(e) => {
            console_error!("land: reading {MAP_PATH} at {main}: {e}");
            None
        }
    };
    let parsed = text.map(|t| lanes::parse(&t));
    match parsed {
        Some(Ok(map)) => map,
        Some(Err(e)) => {
            console_error!("land: {MAP_PATH} at {main} does not parse, so all is core's: {e}");
            fallback()
        }
        None => fallback(),
    }
}

/// The map used when main has none.
pub fn fallback() -> Map {
    // FALLBACK is a constant this crate's smoke scenario parses; an error here cannot happen,
    // and would still leave a usable (empty) map rather than a panic.
    lanes::parse(FALLBACK).unwrap_or(Map {
        docs: Vec::new(),
        shared: Vec::new(),
        checks: Vec::new(),
        core_first: None,
        workers: Vec::new(),
    })
}

/// A commit rebased onto main, not pushed yet.
pub struct Rebased {
    pub commit: String,
    /// Every object the push carries: the new trees, any merged blobs, the commit.
    pub objects: Vec<Object>,
    /// Whether its tests run again before it lands: main moved in what they cover.
    pub retest: bool,
    /// The paths merged line by line.
    pub merged: Vec<String>,
    /// Whether main already holds every change: nothing to commit, `commit` is main.
    pub already: bool,
}

/// `seen`'s commit rebased onto main, or `Ok(Err(why))` when main and it changed the same path
/// in ways that do not merge. `committer` and `unix_secs` sign the new commit.
pub async fn rebase_onto_main(
    session: &Session<'_>,
    seen: &Seen,
    commit: &str,
    committer: &str,
    unix_secs: i64,
) -> Result<std::result::Result<Rebased, String>> {
    let mut changes = seen.changed.clone();
    let mut new_blobs: Vec<Object> = Vec::new();
    let mut merged = Vec::new();
    let mut refused = Vec::new();
    for (path, ours) in &seen.changed {
        let Some(theirs) = seen.main_moved.get(path) else {
            continue;
        };
        if ours == theirs {
            // The same change on both sides: main has it already.
            changes.remove(path);
            continue;
        }
        let same_mode = matches!((ours, theirs), (Some((a, _)), Some((b, _))) if a == b);
        if !TEXT_MERGE.contains(&path.as_str()) || !same_mode {
            refused.push(path.clone());
            continue;
        }
        let text = |b: Option<Vec<u8>>| b.and_then(|b| String::from_utf8(b).ok());
        let base = text(git::read_file(&session.remote, &seen.base, path).await?);
        let ours_text = text(git::read_file(&session.remote, commit, path).await?);
        let theirs_text = text(pack::read_path(&seen.objects, &seen.main, path).map_err(bad)?);
        let joined = match (base, ours_text, theirs_text) {
            (Some(b), Some(o), Some(t)) => merge3::merge(&b, &o, &t),
            // Added on both sides: merge against an empty base, so only identical text joins.
            (None, Some(o), Some(t)) => merge3::merge("", &o, &t),
            _ => None,
        };
        match (joined, ours) {
            (Some(text), Some((mode, _))) => {
                let blob = Object::blob(text.as_bytes());
                changes.insert(path.clone(), Some((mode.clone(), blob.id())));
                new_blobs.push(blob);
                merged.push(path.clone());
            }
            _ => refused.push(path.clone()),
        }
    }
    if !refused.is_empty() {
        return Ok(Err(format!(
            "main changed {} since this branch's base too, and they do not merge: rebase the \
             branch onto main and land again",
            refused.join(", ")
        )));
    }
    let (tree, trees) = rebase::apply(&seen.objects, &seen.main, &changes).map_err(bad)?;
    // Main already holds every change, such as after a lost answer to the push that moved it:
    // nothing to commit, so no empty commit lands.
    let main_tree = match seen.objects.by_id.get(&seen.main) {
        Some((1, body)) => pack::commit_tree(body).map_err(bad)?,
        _ => return Err(bad(format!("main's commit {} was not fetched", seen.main))),
    };
    if tree == main_tree {
        return Ok(Ok(Rebased {
            commit: seen.main.clone(),
            objects: Vec::new(),
            retest: false,
            merged,
            already: true,
        }));
    }
    let original = match seen.objects.by_id.get(commit) {
        Some((1, body)) => body,
        _ => return Err(bad(format!("commit {commit} is not in what was fetched"))),
    };
    let rebased = rebase::commit(original, &tree, &seen.main, committer, unix_secs).map_err(bad)?;
    let id = rebased.id();
    let retest = seen
        .map
        .overlap(&seen.changed_paths(), &seen.main_paths())
        .retest
        || !merged.is_empty();
    let mut objects = new_blobs;
    objects.extend(trees);
    objects.push(rebased);
    Ok(Ok(Rebased {
        commit: id,
        objects,
        retest,
        merged,
        already: false,
    }))
}

/// Pushes `objects` and moves `name` from `old` (or creates it, from [`ZERO`]) to `new`:
/// `Ok(true)` when the remote took it, `Ok(false)` when it refused (such as `old` being stale).
pub async fn push(
    session: &Session<'_>,
    name: &str,
    old: Option<&str>,
    new: &str,
    objects: &[Object],
) -> Result<bool> {
    let caps = git::refs(&session.remote, "git-receive-pack").await?.caps;
    let update = Update {
        name: name.to_string(),
        old: old.unwrap_or(ZERO).to_string(),
        new: new.to_string(),
    };
    let status = git::push(&session.remote, &caps, &[update], &gitobj::pack(objects)).await?;
    Ok(status.iter().any(|s| s == &format!("ok {name}")))
}

/// The current tip of `name`, if it exists.
pub async fn tip(session: &Session<'_>, name: &str) -> Result<Option<String>> {
    Ok(git::refs(&session.remote, "git-upload-pack")
        .await?
        .refs
        .get(name)
        .cloned())
}

/// The ref a rebased commit is tested at before it lands, so CI can fetch it as a branch tip:
/// under the session prefix, which the GitHub mirror never copies.
pub fn test_ref(landing: &str) -> String {
    format!("refs/heads/claude/land/{landing}")
}
