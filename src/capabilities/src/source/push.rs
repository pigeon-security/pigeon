//! Git over MCP for coding agents (docs/plans/agent-git.md): `branch_push`, the default way an
//! agent writes its work to a branch, and `session_credential`, a short-lived git credential for
//! pushes too large to send through a tool call. Both are scoped by which app is calling.
//!
//! **Which app, which branches.** The caller is a grant (a service user, usually an OAuth
//! client) linked to a person; [`crate::delegates::app_of`] says which app it is, an admin's
//! override first. [`NAMESPACES`], in code, gives each app its branch namespace: Claude writes
//! only `claude/…`, OpenAI (ChatGPT and Codex) only `codex/…`. The Cloudflare MCP Portal is
//! mapped to `claude/` because Claude Code reaches pigeon through it and pigeon cannot tell the
//! portal's clients apart. Any other app gets none. `main` is never in a namespace, and no branch
//! outside the caller's is written. The person must have `operator` or above on the home
//! instance.
//!
//! **`branch_push`** takes a git bundle (`git bundle create <file> origin/main..HEAD`),
//! base64-encoded, and a branch. The bundle's pack is pushed to the repository as it is, so
//! every commit keeps its id, author, message and trailers. It is refused when its base (its
//! prerequisites) is not an ancestor of main or of the branch's tip, when it does not move the
//! branch forward unless `force` is set, and when the policy does not allow the push
//! (`session-branches`). Its answer names the new tip, which the agent passes to
//! `land {branch, commit}`.
//!
//! **`session_credential`** mints a proxy credential (`pgn_px_…`) that reaches `/git/` only:
//! fetches of the repository, and pushes only under the app's namespace, for an hour by default
//! and four at most. It is held by the app's grant, so it is counted under the app in Access ›
//! People › Apps and revoked by that app's Disconnect and by removing the person.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;

use pigeon_core::artifacts::Access;
use pigeon_core::bundle::{self, Bundle};
use pigeon_core::git::{self, Remote, Update, ZERO};
use pigeon_core::pack::{self, Walk};
use pigeon_core::policy::{Answer, Decision, Kind, Policy, Vars};
use serde_json::{Value, json};
use worker::{Error, Result, console_log};

use crate::Context;
use crate::delegates::{self, Grant, Match};
use crate::land::merge::Session;
use crate::membership::{self, Role};
use crate::users::{Caller, directory, person_of};

/// Each app's branch namespace, by its key in [`delegates::KNOWN`]. An app not listed gets
/// none. The portal is Claude's: Claude Code's connector reaches pigeon through it, and pigeon
/// sees only the portal's grant (docs/plans/mcp-portal.md). An admin who puts a grant under
/// another app (Access › People › Apps, an override) moves it to that app's namespace.
pub const NAMESPACES: &[(&str, &str)] = &[
    ("claude", "claude/"),
    ("openai", "codex/"),
    ("cloudflare-portal", "claude/"),
];

/// The namespace of app `key`, if it has one.
pub fn namespace_of(key: &str) -> Option<&'static str> {
    NAMESPACES.iter().find(|(k, _)| *k == key).map(|(_, n)| *n)
}

/// Whether `scope` is one of the namespaces in [`NAMESPACES`].
pub fn is_namespace(scope: &str) -> bool {
    NAMESPACES.iter().any(|(_, n)| *n == scope)
}

/// The app calling and what it may write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// The person the app acts for: id and name.
    pub person: String,
    pub person_name: String,
    /// The grant (service user) calling.
    pub grant: String,
    /// The app, as [`delegates::app_of`] tells it.
    pub app: Match,
    /// Its branch namespace, such as `codex/`.
    pub namespace: &'static str,
}

/// Which app `caller` is, from its grant's row (`grant`, read from the Directory), and its
/// namespace; why not when it may write no branch.
pub fn agent_of(caller: &Caller, grant: Option<&Grant>) -> std::result::Result<Agent, String> {
    if caller.user.kind != "service" {
        return Err(
            "branch_push and session_credential are for an app acting for a person (Claude, \
             Codex), not a person's own session"
                .into(),
        );
    }
    let person = person_of(caller).ok_or(
        "this app acts for no person: a person must allow it first (sign in to pigeon through \
         it and approve with a passkey)",
    )?;
    let grant = grant.ok_or("this app's grant is not in the Directory")?;
    if grant.person.as_deref() != Some(person.id.as_str()) {
        return Err("this app is not linked to the person it says it acts for".into());
    }
    let app = delegates::app_of(grant);
    let namespace = namespace_of(&app.key).ok_or_else(|| {
        format!(
            "{} has no branch namespace: only Claude (claude/) and OpenAI, which covers Codex \
             (codex/), write branches. An admin can put this app under one of them in Access › \
             People › Apps",
            app.label
        )
    })?;
    Ok(Agent {
        person: person.id.clone(),
        person_name: person.name.clone(),
        grant: grant.id.clone(),
        app,
        namespace,
    })
}

/// The calling app, read now: the person it acts for must be an operator or above on the home
/// instance.
pub async fn agent(cx: &Context<'_>) -> Result<Agent> {
    let caller = cx.caller.ok_or_else(|| bad("no caller"))?;
    if let Some(person) = person_of(caller) {
        membership::require(cx.env, person, None, Role::Operator).await?;
    }
    let rows = directory(cx.env, "apps.grants", json!({})).await?;
    let grant = rows
        .as_array()
        .into_iter()
        .flatten()
        .map(Grant::from_row)
        .find(|g| g.id == caller.user.id);
    agent_of(caller, grant.as_ref()).map_err(bad)
}

/// `given` (`codex/x` or `refs/heads/codex/x`) as the full ref of a branch under `namespace`;
/// why not otherwise. Never `main` (`main_ref`), never the namespace itself, never a name git
/// would refuse.
pub fn branch_in(
    namespace: &str,
    given: &str,
    main_ref: &str,
) -> std::result::Result<String, String> {
    let given = given.trim();
    let full = if given.starts_with("refs/") {
        given.to_string()
    } else {
        format!("refs/heads/{given}")
    };
    if full == main_ref {
        return Err(format!(
            "`{given}` is the protected branch: it moves only by a landing (land)"
        ));
    }
    let inside = full
        .strip_prefix("refs/heads/")
        .and_then(|b| b.strip_prefix(namespace))
        .is_some_and(|rest| !rest.is_empty());
    if !inside {
        return Err(format!(
            "this app writes only branches under {namespace}; not `{given}`"
        ));
    }
    if !pigeon_proxies::git_proxy::refname_ok(&full) {
        return Err(format!("`{given}` is not a valid branch name"));
    }
    Ok(full)
}

/// The commits outside the bundle that its history from `tip` reaches: what it builds on. Each
/// must be a prerequisite the bundle names.
pub fn boundary(tip: &str, known: &HashMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut out = BTreeSet::new();
    let mut queue = vec![tip.to_string()];
    while let Some(c) = queue.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        match known.get(&c) {
            Some(ps) => queue.extend(ps.iter().cloned()),
            None => {
                out.insert(c);
            }
        }
    }
    out
}

/// What a checked bundle is to its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// The branch does not exist yet: it is created at the tip.
    Create,
    /// The branch is already at the tip: nothing to push.
    Already,
    /// The tip descends from the branch's tip.
    Forward,
    /// It does not: a rewrite, allowed only with `force`.
    Rewrite,
}

impl Move {
    /// The policy's action for it.
    pub fn kind(self) -> Kind {
        match self {
            Move::Rewrite => Kind::RewriteBranch,
            _ => Kind::PushBranch,
        }
    }
}

/// The bundle, checked: its tip and its commits' parents.
#[derive(Debug)]
pub struct Checked<'a> {
    pub bundle: Bundle<'a>,
    pub tip: String,
    pub known: HashMap<String, Vec<String>>,
}

/// `data` as a bundle to push: parsed, its pack's commits read, and its history from the tip
/// reaching only the prerequisites it names, of which it must name at least one.
pub fn read(data: &[u8]) -> std::result::Result<Checked<'_>, String> {
    let bundle = bundle::parse(data)?;
    let tip = bundle.tip()?;
    if bundle.prerequisites.is_empty() {
        return Err(
            "the bundle has no base: make it against a commit the repository has, such as \
             `git bundle create <file> origin/main..HEAD`"
                .into(),
        );
    }
    let known = pack::commit_parents(bundle.pack)?.parents;
    let outside: Vec<String> = boundary(&tip, &known)
        .into_iter()
        .filter(|c| !bundle.prerequisites.contains(c))
        .collect();
    if !outside.is_empty() {
        return Err(format!(
            "the bundle's history reaches {} without carrying it or naming it as a \
             prerequisite: make the bundle with `git bundle create <file> <base>..HEAD`",
            outside.join(", ")
        ));
    }
    Ok(Checked { bundle, tip, known })
}

/// Commits' parents the repository answers for a fetch wanting `wants` and having `have`:
/// every commit `wants` reach that `have` does not.
pub type History = HashMap<String, Vec<String>>;

/// Checks the bundle against the repository: every prerequisite is `main` or an ancestor of it,
/// or of the branch's tip `old`; and the tip moves the branch forward unless `force` (a
/// rewrite). `history(wants, have)` asks the repository for the commits `wants` reach that
/// `have` does not (see [`History`]); it is asked once per prerequisite, and once more when the
/// branch exists and the bundle alone does not settle whether the tip descends from it.
pub async fn check<U, F>(
    c: &Checked<'_>,
    main: &str,
    old: Option<&str>,
    force: bool,
    mut history: U,
) -> std::result::Result<Move, String>
where
    U: FnMut(Vec<String>, String) -> F,
    F: Future<Output = std::result::Result<History, String>>,
{
    if old == Some(c.tip.as_str()) {
        return Ok(Move::Already);
    }
    for p in &c.bundle.prerequisites {
        let mut tips: Vec<&str> = vec![main];
        tips.extend(old);
        let mut on = tips.contains(&p.as_str());
        if !on {
            let known = history(tips.iter().map(|t| t.to_string()).collect(), p.clone()).await?;
            on = tips
                .iter()
                .any(|t| pack::walk(p, t, &known, true) == Walk::Forward);
        }
        if !on {
            return Err(format!(
                "the bundle's base {p} is not on main or on the branch: fetch main, rebase onto \
                 it and bundle again (`git bundle create <file> origin/main..HEAD`)"
            ));
        }
    }
    let Some(old) = old else {
        return Ok(Move::Create);
    };
    let forward = match pack::walk(old, &c.tip, &c.known, false) {
        Walk::Forward => true,
        Walk::NotForward => false,
        Walk::Need(edge) => {
            let mut known = c.known.clone();
            known.extend(history(edge, old.to_string()).await?);
            pack::walk(old, &c.tip, &known, true) == Walk::Forward
        }
    };
    match (forward, force) {
        (true, _) => Ok(Move::Forward),
        (false, true) => Ok(Move::Rewrite),
        (false, false) => Err(format!(
            "{} does not descend from the branch's tip {old}: the push would rewrite it. Fetch \
             the branch and build on it, or pass `force: true` to rewrite a branch of your own",
            c.tip
        )),
    }
}

/// The policy's answer to writing `branch` (a full ref) as `m`, by `actor`.
pub fn ask(policy: &Policy, vars: &Vars, actor: &str, m: Move, branch: &str) -> Answer {
    let target = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    policy.evaluate(
        vars,
        &pigeon_core::policy_file::request(actor, m.kind(), target),
    )
}

/// Why `answer` refuses, if it does.
pub fn refusal(answer: &Answer) -> Option<String> {
    (answer.decision != Decision::Allow).then(|| match &answer.rule {
        Some(rule) => format!("{} (policy rule {rule})", answer.reason),
        None => format!("{} (.pigeon/policy.toml)", answer.reason),
    })
}

/// The audit line of a push: the app, the person (by id), the branch and the tips. Never a
/// token, a bundle or a name.
pub fn audit_line(a: &Agent, branch: &str, old: Option<&str>, tip: &str, outcome: &str) -> String {
    json!({
        "audit": "branch_push", "app": a.app.key, "grant": a.grant, "person": a.person,
        "branch": branch.strip_prefix("refs/heads/").unwrap_or(branch),
        "old": old, "tip": tip, "outcome": outcome,
    })
    .to_string()
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The commits `wants` reach that `have` does not, read from `remote` (commits only, where it
/// can filter).
async fn upstream(
    remote: &Remote,
    caps: &[String],
    wants: Vec<String>,
    have: String,
) -> std::result::Result<History, String> {
    let got = git::fetch_filtered(remote, caps, &wants, &[have], Some("tree:0"))
        .await
        .map_err(|e| format!("the repository's history could not be read: {e}"))?;
    if got.pack.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(pack::commit_parents(&got.pack)?.parents)
}

/// `branch_push`: the bundle in `args` pushed to the branch, for the calling app.
pub async fn branch_push(
    cx: &Context<'_>,
    namespace: &str,
    repo: &str,
    main_branch: &str,
    args: &Value,
) -> Result<Value> {
    let who = agent(cx).await?;
    let main_ref = format!("refs/heads/{main_branch}");
    let branch = branch_in(who.namespace, crate::arg(args, "branch")?, &main_ref).map_err(bad)?;
    let force = args["force"].as_bool().unwrap_or(false);
    let data = bundle::decode(crate::arg(args, "bundle")?).map_err(bad)?;
    let checked = read(&data).map_err(bad)?;
    let session = Session::open(cx.account, namespace, repo, Access::Write).await?;
    let out = push_in(
        cx,
        &session.remote,
        &who,
        &branch,
        &main_ref,
        force,
        &checked,
    )
    .await;
    session.close().await;
    let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
    match &out {
        Ok(v) => console_log!(
            "{}",
            audit_line(
                &who,
                &branch,
                v["old"].as_str(),
                &checked.tip,
                v["outcome"].as_str().unwrap_or("pushed")
            )
        ),
        Err(_) => console_log!(
            "{}",
            audit_line(&who, &branch, None, &checked.tip, "refused")
        ),
    }
    out.map(|mut v| {
        v["branch"] = json!(short);
        v
    })
}

/// [`branch_push`]'s work in an open session.
async fn push_in(
    cx: &Context<'_>,
    remote: &Remote,
    who: &Agent,
    branch: &str,
    main_ref: &str,
    force: bool,
    c: &Checked<'_>,
) -> Result<Value> {
    let refs = git::refs(remote, "git-receive-pack").await?;
    let main = refs
        .refs
        .get(main_ref)
        .cloned()
        .ok_or_else(|| bad("the repository has no main"))?;
    let old = refs.refs.get(branch).cloned();
    let fetch_caps = git::refs(remote, "git-upload-pack").await?.caps;
    let m = check(c, &main, old.as_deref(), force, |wants, have| {
        upstream(remote, &fetch_caps, wants, have)
    })
    .await
    .map_err(bad)?;
    let actor = format!("{} for {}", who.app.label, who.person_name);
    let vars = pigeon_proxies::git_proxy::vars(main_ref, crate::land::state::PREFIX);
    let req = pigeon_core::policy_file::request(
        &actor,
        m.kind(),
        branch.strip_prefix("refs/heads/").unwrap_or(branch),
    );
    let answer = crate::policy::decide(&vars, &req);
    crate::policy::record(cx.env, "branch_push", &req, &answer, None).await?;
    if let Some(why) = refusal(&answer) {
        return Err(bad(format!("the policy does not allow this push: {why}")));
    }
    let base = &c.bundle.prerequisites;
    let next = json!({ "tool": "land", "args": { "branch": branch.strip_prefix("refs/heads/").unwrap_or(branch), "commit": c.tip } });
    if m == Move::Already {
        return Ok(json!({
            "commit": c.tip, "old": old, "outcome": "already", "base": base, "next": next,
        }));
    }
    let update = Update {
        name: branch.to_string(),
        old: old.clone().unwrap_or_else(|| ZERO.to_string()),
        new: c.tip.clone(),
    };
    let status = git::push(remote, &refs.caps, &[update], c.bundle.pack).await?;
    if !status.iter().any(|s| s == &format!("ok {branch}")) {
        return Err(bad(format!(
            "the repository refused the update: {}. If the branch moved meanwhile, fetch it and \
             push again",
            status.join("; ")
        )));
    }
    Ok(json!({
        "commit": c.tip,
        "old": old,
        "outcome": match m { Move::Create => "created", Move::Rewrite => "rewritten", _ => "pushed" },
        "commits": c.known.len(),
        "base": base,
        "app": who.app.label,
        "next": next,
    }))
}

/// `session_credential`: a git-only proxy credential for the calling app's namespace.
pub async fn session_credential(cx: &Context<'_>, remote: &str, args: &Value) -> Result<Value> {
    let who = agent(cx).await?;
    let ttl_ms = crate::proxy::app_git_ttl_ms(&args["ttl"]).map_err(bad)?;
    let label = format!("{} session credential", who.app.label);
    let (token, id, expires_at) = crate::proxy::issue_app_git(
        cx.env,
        &who.grant,
        &who.person,
        who.namespace,
        &label,
        ttl_ms,
    )
    .await?;
    console_log!(
        "{}",
        json!({
            "audit": "session_credential", "app": who.app.key, "grant": who.grant,
            "person": who.person, "namespace": who.namespace, "token": id,
            "ttl_secs": ttl_ms / 1000,
        })
    );
    Ok(json!({
        "token": token,
        "id": id,
        "remote": remote,
        "namespace": who.namespace,
        "expires_at": expires_at,
        "ttl_secs": ttl_ms / 1000,
        "header": "Authorization: Bearer <token>",
        "use": format!(
            "git -c http.extraHeader=\"Authorization: Bearer $(cat <file holding the token>)\" \
             push {remote} HEAD:refs/heads/{}<name>; then land {{branch, commit}}",
            who.namespace
        ),
        "note": "Fetches the repository; pushes only branches under the namespace, never main; \
                 reaches /git/ only. It is in this transcript: keep it short-lived. Revoked by \
                 the app's Disconnect in Access › People › Apps, or by removing the person.",
    }))
}
