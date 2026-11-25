//! The console's one write path: `/console/write/{action}/begin` and `/finish`, each with a body
//! of `{target, change}`. `begin` checks both, answers the exact change in words and a passkey
//! challenge whose purpose is bound to the caller, the action, the target and the change's
//! digest; `finish` takes the same body with the passkey's answer, works the purpose out again
//! from what it is sent now (so a changed body finds no challenge), and makes the write once.
//! The Directory keeps each challenge for [`CHALLENGE_TTL_MS`] and gives it out once
//! ([`redeem`]).
//!
//! [`ACTIONS`] is the one table of writes: each action's handler (the capability that makes it,
//! or the tool `dispatch` runs for it under the same policy gate as MCP), the policy its passkey
//! answers, whether it is destructive, and how its target and change are read and summarised.
//! The pure parts are here; [`begin`] and [`finish`] do the rest. The routes that came before
//! it ([`LEGACY`]) are thin adapters onto the same actions until they are removed.
//!
//! [`CHALLENGE_TTL_MS`]: pigeon_core::webauthn::CHALLENGE_TTL_MS

use pigeon_core::approval::canonical;
use pigeon_core::oauth::hash;
use serde::Serialize;
use serde_json::{Map, Value, json};
use worker::{Error, Result};

use crate::Context;
use crate::passkeys::Passkeys;
use crate::users::{Caller, User, directory};

/// What makes an approved write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handler {
    /// The hosted capability of this name, through its `write_check` and `write_make`.
    Capability(&'static str),
    /// This tool, run by `dispatch` for the person, through the same policy gate as MCP.
    Tool(&'static str),
}

/// What the person's passkey answers for a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The person's own passkey, bound to the exact change.
    Passkey,
    /// As [`Policy::Passkey`], and to the digest of the approval being decided, so the passkey
    /// covers the exact call or plan the approval holds.
    Approval,
    /// The person's `call_tool` policy for the tool first: a call it refuses is refused before
    /// the passkey, and one it gates on the passkey is approved with this one, once.
    ToolGate,
}

/// What a write's target is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// An id: 1 to 200 base64url characters.
    Id,
    /// A person's name, as an invite takes it.
    Name,
    /// `on` or `off`.
    Switch,
    /// A secret's name: 1 to 128 of A-Z, a-z, 0-9, `_` and `-`.
    Secret,
    /// A proxy credential's label.
    Label,
    /// A proxy credential's id.
    Token,
    /// A site's selection: `{instance, site, host}`.
    Site,
    /// This one fixed word, or nothing.
    Fixed(&'static str),
}

/// What a write's change is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Nothing: null or `{}`.
    Nothing,
    /// `{value}`, or at `begin` only `{digest}`: the value's base64url SHA-256, so the value is
    /// sent once. Bound by its digest.
    Secret,
    /// `{record}`: an instance record as JSON text, bound by its digest.
    Record,
    /// `{days}`: a proxy credential's lifetime.
    Days,
    /// `{name}`: a site visitor's name.
    Visitor,
    /// `{user}`: a site visitor's user id.
    VisitorUser,
    /// `{release}`: an immutable site release.
    Release,
    /// `{rev}`: a commit or branch, `main` when not given.
    Rev,
    /// `{set}`: model defaults by slot.
    Models,
    /// Concurrency caps for AI agents.
    Caps,
    /// `{text}`: a note to a run.
    Note,
    /// The amounts a run is extended by.
    Extend,
}

/// One write the console makes.
#[derive(Debug, PartialEq, Eq)]
pub struct Action {
    /// The id in its path, such as `secret-set`.
    pub id: &'static str,
    /// What it does, in a few words, for the confirm dialog.
    pub title: &'static str,
    pub handler: Handler,
    pub policy: Policy,
    /// Whether it removes or revokes something, or moves something live back: the dialog's
    /// button is a danger one.
    pub destructive: bool,
    /// Whether an app acting for the person (the iOS app, by bearer) may make it; the passkey
    /// is still the person's.
    pub by_bearer: bool,
    pub target: Target,
    pub change: Change,
}

const fn act(
    id: &'static str,
    title: &'static str,
    handler: Handler,
    destructive: bool,
    target: Target,
    change: Change,
) -> Action {
    let policy = match handler {
        Handler::Tool(_) => Policy::ToolGate,
        Handler::Capability(_) => Policy::Passkey,
    };
    Action {
        id,
        title,
        handler,
        policy,
        destructive,
        by_bearer: false,
        target,
        change,
    }
}

use Handler::{Capability as Cap, Tool};

/// Every write the console makes.
pub const ACTIONS: &[Action] = &[
    act(
        "secret-set",
        "Set a secret",
        Cap("secrets"),
        false,
        Target::Secret,
        Change::Secret,
    ),
    act(
        "token-issue",
        "Issue a proxy token",
        Cap("proxy"),
        false,
        Target::Label,
        Change::Days,
    ),
    act(
        "token-revoke",
        "Revoke a proxy token",
        Cap("proxy"),
        true,
        Target::Token,
        Change::Nothing,
    ),
    act(
        "linked-unlink",
        "Unlink a Cloudflare account",
        Cap("linked"),
        true,
        Target::Id,
        Change::Nothing,
    ),
    act(
        "instance-put",
        "Save an instance record",
        Cap("instances"),
        false,
        Target::Id,
        Change::Record,
    ),
    act(
        "visitor-invite",
        "Invite a site visitor",
        Cap("console"),
        false,
        Target::Site,
        Change::Visitor,
    ),
    act(
        "visitor-revoke",
        "Remove a site visitor",
        Cap("console"),
        true,
        Target::Site,
        Change::VisitorUser,
    ),
    act(
        "passkey-remove",
        "Remove a passkey",
        Cap("console"),
        true,
        Target::Id,
        Change::Nothing,
    ),
    act(
        "app-disconnect",
        "Disconnect an app",
        Cap("console"),
        true,
        Target::Id,
        Change::Nothing,
    ),
    act(
        "invite",
        "Invite a person",
        Cap("console"),
        false,
        Target::Name,
        Change::Nothing,
    ),
    Action {
        by_bearer: true,
        ..act(
            "land-approve",
            "Approve a landing",
            Cap("console"),
            false,
            Target::Id,
            Change::Nothing,
        )
    },
    act(
        "land-approval",
        "Set whether landings wait for approval",
        Cap("console"),
        false,
        Target::Switch,
        Change::Nothing,
    ),
    Action {
        by_bearer: true,
        policy: Policy::Approval,
        ..act(
            "approval-approve",
            "Approve a request",
            Cap("console"),
            false,
            Target::Id,
            Change::Nothing,
        )
    },
    Action {
        by_bearer: true,
        ..act(
            "approval-deny",
            "Deny a request",
            Cap("console"),
            false,
            Target::Id,
            Change::Nothing,
        )
    },
    act(
        "model-defaults",
        "Set model defaults",
        Tool("ai_agents_models"),
        false,
        Target::Fixed("models"),
        Change::Models,
    ),
    act(
        "agents-config",
        "Set AI agent limits",
        Tool("ai_agents_config"),
        false,
        Target::Fixed("limits"),
        Change::Caps,
    ),
    act(
        "site-rollback",
        "Roll a site back to a release",
        Tool("site_rollback"),
        true,
        Target::Id,
        Change::Release,
    ),
    act(
        "site-deploy",
        "Deploy a site",
        Tool("site_deploy"),
        false,
        Target::Id,
        Change::Rev,
    ),
    act(
        "run-stop",
        "Stop a run",
        Tool("ai_agents_stop"),
        true,
        Target::Id,
        Change::Nothing,
    ),
    act(
        "run-note",
        "Send a run a note",
        Tool("ai_agents_note"),
        false,
        Target::Id,
        Change::Note,
    ),
    act(
        "run-extend",
        "Extend a run's budget",
        Tool("ai_agents_extend"),
        false,
        Target::Id,
        Change::Extend,
    ),
];

/// The action named `id`.
pub fn action(id: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// A write's two steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Begin,
    Finish,
}

/// The action and step a `/console/write/{action}/{begin|finish}` path names.
pub fn route(path: &str) -> Option<(&'static Action, Step)> {
    let (id, step) = path.strip_prefix("/console/write/")?.split_once('/')?;
    let step = match step {
        "begin" => Step::Begin,
        "finish" => Step::Finish,
        _ => return None,
    };
    Some((action(id)?, step))
}

/// The slots model defaults may be set in.
pub const MODEL_SLOTS: &[&str] = &[
    "planner",
    "executor",
    "mechanical",
    "summary",
    "planner_integrate",
    "planner_docs",
    "planner_fix",
    "planner_design",
];

/// The concurrency caps AI agents may be given.
pub const CAPS: &[&str] = &[
    "max_live_runs",
    "max_executors_per_planner",
    "executor_reserve",
];

/// The amounts a run may be extended by.
pub const EXTEND: &[&str] = &["cost_usd", "turns", "wall_min", "sandbox_min", "executors"];

fn id_like(s: &str) -> bool {
    (1..=200).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn digest_like(s: &str) -> bool {
    s.len() == 43 && id_like(s)
}

/// `v` as an object, null or `{}` as an empty one.
fn object(v: &Value) -> std::result::Result<Map<String, Value>, String> {
    match v {
        Value::Null => Ok(Map::new()),
        Value::Object(o) => Ok(o.clone()),
        _ => Err("the change must be an object".into()),
    }
}

/// An object with only `keys`, at least one of them when `some`.
fn only(o: &Map<String, Value>, keys: &[&str], some: bool) -> std::result::Result<(), String> {
    if let Some(k) = o.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("the change has no field `{k}`"));
    }
    if some && o.is_empty() {
        return Err(format!("the change sets none of {}", keys.join(", ")));
    }
    Ok(())
}

impl Action {
    /// `raw` as this action's target: a trimmed string, or for a site its selection.
    pub fn target(&self, raw: &Value) -> std::result::Result<Value, String> {
        let text = || raw.as_str().map(str::trim).unwrap_or_default();
        Ok(match self.target {
            Target::Id => {
                let t = text();
                if !id_like(t) {
                    return Err("the target is not an id".into());
                }
                json!(t)
            }
            Target::Name => json!(crate::users::invite_name(text())?),
            Target::Switch => match text() {
                t @ ("on" | "off") => json!(t),
                _ => return Err("the target is on or off".into()),
            },
            Target::Secret => {
                let t = text();
                if t.is_empty() || t.len() > 128 || !id_like(t) {
                    return Err("the target is not a secret's name".into());
                }
                json!(t)
            }
            Target::Label => json!(crate::proxy::label(text())?),
            Target::Token => json!(crate::proxy::id(text())?),
            Target::Site => {
                let o = raw
                    .as_object()
                    .ok_or("the target is {instance, site, host}")?;
                only(o, &["instance", "site", "host"], false)?;
                let field = |k: &str| {
                    o.get(k)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && s.len() <= 253)
                        .map(str::to_string)
                        .ok_or_else(|| format!("the target needs `{k}`"))
                };
                json!({ "instance": field("instance")?, "site": field("site")?, "host": field("host")? })
            }
            Target::Fixed(word) => match raw {
                Value::Null => json!(word),
                v if v.as_str().map(str::trim) == Some(word) => json!(word),
                _ => return Err(format!("the target is `{word}`")),
            },
        })
    }

    /// `raw` as this action's change, checked, at `step`. A secret's value is required at
    /// `finish`; at `begin` its digest stands for it.
    pub fn change(&self, raw: &Value, step: Step) -> std::result::Result<Value, String> {
        let o = object(raw)?;
        let s = |k: &str| o.get(k).and_then(Value::as_str);
        Ok(match self.change {
            Change::Nothing => {
                only(&o, &[], false)?;
                json!({})
            }
            Change::Secret => {
                only(&o, &["value", "digest"], true)?;
                match (s("value").map(str::trim), s("digest"), step) {
                    (Some(""), ..) => return Err("the value is empty".into()),
                    (Some(v), None, _) => json!({ "value": v }),
                    (None, Some(d), Step::Begin) if digest_like(d) => json!({ "digest": d }),
                    (None, Some(_), Step::Begin) => return Err("not a SHA-256 digest".into()),
                    _ => return Err("the change is {value}, or {digest} to begin".into()),
                }
            }
            Change::Record => {
                only(&o, &["record", "digest"], true)?;
                let record = s("record").ok_or("the change is {record}")?;
                if s("digest").is_some_and(|d| d != hash(record)) {
                    return Err("the digest is not the record's".into());
                }
                json!({ "record": record })
            }
            Change::Days => {
                only(&o, &["days"], false)?;
                let days = crate::proxy::days(o.get("days").unwrap_or(&Value::Null))?;
                json!({ "days": days })
            }
            Change::Visitor => {
                only(&o, &["name"], true)?;
                json!({ "name": crate::users::invite_name(s("name").unwrap_or_default())? })
            }
            Change::VisitorUser => {
                only(&o, &["user"], true)?;
                match s("user").map(str::trim) {
                    Some(u) if id_like(u) => json!({ "user": u }),
                    _ => return Err("the change is {user}, a visitor's id".into()),
                }
            }
            Change::Release => {
                only(&o, &["release"], true)?;
                match s("release").map(str::trim) {
                    Some(r) if pigeon_core::site_publish::is_release_id(r) => {
                        json!({ "release": r })
                    }
                    _ => return Err("the change is {release}, a site release id".into()),
                }
            }
            Change::Rev => {
                only(&o, &["rev"], false)?;
                let rev = s("rev").map(str::trim).unwrap_or("main");
                let ok = (1..=200).contains(&rev.len())
                    && rev
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
                if !ok {
                    return Err("the rev is a commit or branch".into());
                }
                json!({ "rev": rev })
            }
            Change::Models => {
                only(&o, &["set"], true)?;
                let set = o
                    .get("set")
                    .and_then(Value::as_object)
                    .ok_or("the change is {set}")?;
                only(set, MODEL_SLOTS, true)?;
                for (slot, model) in set {
                    let m = model.as_str().map(str::trim).unwrap_or_default();
                    if m.is_empty() || m.len() > 100 || m.chars().any(char::is_control) {
                        return Err(format!("`{slot}` is not a model"));
                    }
                }
                json!({ "set": set })
            }
            Change::Caps => {
                only(&o, CAPS, true)?;
                if o.values().any(|v| v.as_u64().is_none_or(|n| n > 64)) {
                    return Err("each limit is a whole number up to 64".into());
                }
                Value::Object(o)
            }
            Change::Note => {
                only(&o, &["text"], true)?;
                match s("text") {
                    Some(t) if !t.trim().is_empty() && t.len() <= 4000 => json!({ "text": t }),
                    _ => return Err("the note is 1 to 4000 characters".into()),
                }
            }
            Change::Extend => {
                only(&o, EXTEND, true)?;
                let ok = o.iter().all(|(k, v)| {
                    if k == "cost_usd" {
                        v.as_f64().is_some_and(|n| n > 0.0)
                    } else {
                        v.as_u64().is_some_and(|n| n > 0)
                    }
                });
                if !ok {
                    return Err("each amount is more than 0".into());
                }
                Value::Object(o)
            }
        })
    }

    /// What the passkey is bound to of a checked `change`: the change itself, but a secret's
    /// value and a record by their digest.
    pub fn bound(&self, change: &Value) -> Value {
        match self.change {
            Change::Secret => match change["value"].as_str() {
                Some(v) => json!({ "digest": hash(v) }),
                None => json!({ "digest": change["digest"] }),
            },
            Change::Record => json!({ "digest": hash(change["record"].as_str().unwrap_or("")) }),
            _ => change.clone(),
        }
    }

    /// The tool's arguments for a tool write of checked `target` and `change`.
    pub fn args(&self, target: &Value, change: &Value) -> Value {
        let mut args = match change {
            Value::Object(o) => o.clone(),
            _ => Map::new(),
        };
        match self.handler {
            Handler::Tool("site_rollback" | "site_deploy") => {
                args.insert("instance".into(), target.clone());
            }
            Handler::Tool("ai_agents_stop" | "ai_agents_note" | "ai_agents_extend") => {
                args.insert("run".into(), target.clone());
            }
            _ => {}
        }
        Value::Object(args)
    }

    /// The change in words, for the confirm dialog: never a secret's value.
    pub fn summary(&self, target: &Value, bound: &Value) -> Summary {
        let target_text = match target {
            Value::String(s) => s.clone(),
            v => format!(
                "{} / {} ({})",
                v["instance"].as_str().unwrap_or(""),
                v["site"].as_str().unwrap_or(""),
                v["host"].as_str().unwrap_or("")
            ),
        };
        let change = match self.change {
            Change::Secret => vec![format!(
                "new value, SHA-256 {}",
                bound["digest"].as_str().unwrap_or("")
            )],
            Change::Record => vec![format!(
                "new record, SHA-256 {}",
                bound["digest"].as_str().unwrap_or("")
            )],
            Change::Models => bound["set"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or("")))
                .collect(),
            _ => bound
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}: {s}"),
                    v => format!("{k}: {v}"),
                })
                .collect(),
        };
        Summary {
            action: self.id,
            title: self.title,
            target: target_text,
            change,
            destructive: self.destructive,
        }
    }
}

/// What the confirm dialog shows before the passkey.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub action: &'static str,
    pub title: &'static str,
    pub target: String,
    pub change: Vec<String>,
    pub destructive: bool,
}

/// The caller a write is bound to when any registered person's passkey may approve it: only
/// the old secret and proxy-token routes, which never needed a console session.
pub const ANY_PASSKEY: &str = "*";

/// The digest a write's passkey is bound to: the caller's id, the action, the target, the
/// bound change, and for [`Policy::Approval`] the approval's own purpose.
pub fn digest(
    action: &Action,
    caller: &str,
    target: &Value,
    bound: &Value,
    anchor: Option<&str>,
) -> String {
    hash(&canonical(&json!({
        "action": action.id,
        "caller": caller,
        "target": target,
        "change": bound,
        "anchor": anchor,
    })))
}

/// The challenge purpose for a write with `digest`.
pub fn purpose(action: &Action, digest: &str) -> String {
    format!("console:write:{}:{digest}", action.id)
}

/// A challenge as the Directory keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub purpose: String,
    pub challenge: String,
    pub expires_ms: i64,
}

/// Takes the stored challenge for `purpose` at `now_ms`: it is gone after, whether it was
/// answered or not, so it is used once. One past its time, or for another purpose, is refused.
pub fn redeem(
    stored: &mut Option<Stored>,
    purpose: &str,
    now_ms: i64,
) -> std::result::Result<String, &'static str> {
    const GONE: &str = "this challenge is used or has expired";
    match stored.take() {
        Some(s) if s.expires_ms > now_ms && s.purpose == purpose => Ok(s.challenge),
        _ => Err(GONE),
    }
}

/// One of the routes before this path, as an adapter onto an action.
#[derive(Debug, PartialEq)]
pub struct Legacy {
    pub action: &'static Action,
    pub step: Step,
    pub target: Value,
    pub change: Value,
    /// Whether any person's passkey approves it, as the route did.
    pub any_passkey: bool,
}

/// The old write routes, adapters onto [`ACTIONS`] until phase 5 removes them.
pub const LEGACY: &[(&str, &str)] = &[
    ("/secrets/put/begin", "secret-set"),
    ("/secrets/put/finish", "secret-set"),
    ("/tokens/issue/begin", "token-issue"),
    ("/tokens/issue/finish", "token-issue"),
    ("/tokens/revoke/begin", "token-revoke"),
    ("/tokens/revoke/finish", "token-revoke"),
    ("/linked/unlink/begin", "linked-unlink"),
    ("/linked/unlink/finish", "linked-unlink"),
    ("/instance/put/begin", "instance-put"),
    ("/instance/put/finish", "instance-put"),
    ("/console/site-access/invite/begin", "visitor-invite"),
    ("/console/site-access/invite/finish", "visitor-invite"),
];

/// The action, target and change an old route's `body` asks for at `path`.
pub fn legacy(path: &str, body: &Value) -> Option<Legacy> {
    let (_, id) = LEGACY.iter().find(|(p, _)| *p == path)?;
    let step = if path.ends_with("/begin") {
        Step::Begin
    } else {
        Step::Finish
    };
    let s = |k: &str| body[k].clone();
    let (target, change, any_passkey) = match *id {
        "secret-set" => {
            let change = match step {
                Step::Begin => json!({ "digest": s("digest") }),
                Step::Finish => json!({ "value": s("value") }),
            };
            (s("name"), change, true)
        }
        "token-issue" => (s("label"), json!({ "days": s("days") }), true),
        "token-revoke" => (s("id"), json!({}), true),
        "linked-unlink" => (s("instance"), json!({}), false),
        "instance-put" => {
            let record = body["record"].as_str().unwrap_or_default();
            let id = serde_json::from_str::<Value>(record).ok()?["id"].clone();
            let mut change = json!({ "record": record });
            if step == Step::Begin {
                change["digest"] = s("digest");
            }
            (id, change, false)
        }
        "visitor-invite" => (
            json!({ "instance": s("instance"), "site": s("site"), "host": s("host") }),
            json!({ "name": s("name") }),
            false,
        ),
        _ => return None,
    };
    let change = match change {
        Value::Object(mut o) => {
            o.retain(|_, v| !v.is_null());
            Value::Object(o)
        }
        v => v,
    };
    Some(Legacy {
        action: action(id)?,
        step,
        target,
        change,
        any_passkey,
    })
}

// ---- the steps ----

/// Who a write is bound to.
#[derive(Clone, Copy)]
pub enum Who<'a> {
    /// The signed-in person (or the person an app acts for): only their passkey answers.
    Person(&'a User),
    /// Any person's passkey, who is then the person: only the old secret and token routes.
    AnyPasskey,
}

/// A checked write, as its handler gets it.
pub struct Write<'a> {
    pub action: &'static Action,
    pub target: Value,
    pub change: Value,
    /// The person making it: always known at `finish`; at `begin` unknown for
    /// [`Who::AnyPasskey`].
    pub person: Option<&'a User>,
}

impl Write<'_> {
    /// The target as text, for an action whose target is one.
    pub fn text(&self) -> &str {
        self.target.as_str().unwrap_or_default()
    }
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The hosted capability that makes `action`'s writes.
fn owner<'a>(cx: &'a Context<'a>, name: &str) -> Result<&'a dyn crate::Capability> {
    cx.hosted
        .capabilities
        .iter()
        .find(|c| c.name() == name)
        .map(|c| c.as_ref())
        .ok_or_else(|| {
            bad(format!(
                "`{name}` is not hosted here, so it makes no writes"
            ))
        })
}

/// The checked target and change, and the purpose bound to them for `who`.
async fn bind(
    cx: &Context<'_>,
    action: &'static Action,
    who: Who<'_>,
    target: &Value,
    change: &Value,
    step: Step,
) -> Result<(Value, Value, Value, String)> {
    let target = action.target(target).map_err(bad)?;
    let change = action.change(change, step).map_err(bad)?;
    let bound = action.bound(&change);
    let anchor = match action.policy {
        Policy::Approval => {
            Some(crate::approvals::purpose(cx.env, target.as_str().unwrap_or("")).await?)
        }
        _ => None,
    };
    let caller = match who {
        Who::Person(p) => p.id.as_str(),
        Who::AnyPasskey => ANY_PASSKEY,
    };
    let d = digest(action, caller, &target, &bound, anchor.as_deref());
    Ok((target, change, bound, d))
}

/// Starts `action` for `who`: checks the target and change, asks its handler, and answers the
/// change's summary and a passkey challenge bound to it. A handler's `errors` are answered as
/// they are, with no challenge.
pub async fn begin(
    cx: &Context<'_>,
    passkeys: &Passkeys,
    action: &'static Action,
    who: Who<'_>,
    target: &Value,
    change: &Value,
) -> Result<Value> {
    let (target, change, bound, d) = bind(cx, action, who, target, change, Step::Begin).await?;
    let person = match who {
        Who::Person(p) => Some(p),
        Who::AnyPasskey => None,
    };
    let checked = match action.handler {
        Handler::Capability(name) => {
            let w = Write {
                action,
                target: target.clone(),
                change,
                person,
            };
            owner(cx, name)?.write_check(cx, &w).await?
        }
        Handler::Tool(tool) => {
            let person = person.ok_or_else(|| bad("this write needs a signed-in person"))?;
            if let Some(why) = crate::approvals::refused(&person.name, tool) {
                return Err(bad(why));
            }
            Value::Null
        }
    };
    if checked.get("errors").is_some() {
        return Ok(checked);
    }
    let mut out = passkeys
        .sign_in_begin(cx.env, &purpose(action, &d), person.map(|p| p.id.as_str()))
        .await?;
    let mut summary = crate::to_value(action.summary(&target, &bound));
    if let Value::Object(more) = checked {
        for (k, v) in more {
            summary[&k] = v.clone();
            out[&k] = v;
        }
    }
    out["summary"] = summary;
    out["digest"] = json!(d);
    Ok(out)
}

/// Finishes `action` for `who`: the passkey's answer in `body` must be to the challenge bound to
/// this exact target and change, and for [`Who::Person`] from their passkey; then the write is
/// made, once.
#[allow(clippy::too_many_arguments)]
pub async fn finish(
    cx: &Context<'_>,
    passkeys: &Passkeys,
    action: &'static Action,
    who: Who<'_>,
    target: &Value,
    change: &Value,
    body: &Value,
) -> Result<Value> {
    let (target, change, _, d) = bind(cx, action, who, target, change, Step::Finish).await?;
    let only = match who {
        Who::Person(p) => Some(p.id.as_str()),
        Who::AnyPasskey => None,
    };
    let key = passkeys
        .sign_in_finish(cx.env, &purpose(action, &d), body, only)
        .await?;
    let owned;
    let person = match who {
        Who::Person(p) => p,
        Who::AnyPasskey => {
            let user: Option<User> = serde_json::from_value(
                directory(cx.env, "users.get", json!({ "id": key.user_id })).await?,
            )?;
            owned = user
                .filter(|u| u.kind == "standard")
                .ok_or_else(|| bad("only a person can make this change"))?;
            &owned
        }
    };
    match action.handler {
        Handler::Capability(name) => {
            let w = Write {
                action,
                target,
                change,
                person: Some(person),
            };
            owner(cx, name)?.write_make(cx, &w).await
        }
        Handler::Tool(tool) => {
            let mut args = action.args(&target, &change);
            if let Some(id) = crate::approvals::approve_own(cx.env, person, tool, &args).await? {
                args[crate::approvals::ARG] = json!(id);
            }
            let caller = Caller {
                user: person.clone(),
                acting_for: None,
                run: None,
            };
            let person_cx = Context {
                account: cx.account,
                env: cx.env,
                hosted: cx.hosted,
                caller: Some(&caller),
                caller_note: None,
                instance: None,
                worker: cx.worker,
            };
            let data = crate::dispatch(&person_cx, tool, &args).await?;
            Ok(json!({ "message": format!("{}: done.", action.title), "data": data }))
        }
    }
}

/// An old route's write, made through [`begin`] or [`finish`]; `person` is the signed-in person
/// for a route that needed one.
pub async fn adapt(
    cx: &Context<'_>,
    passkeys: &Passkeys,
    path: &str,
    body: &Value,
    person: Option<&User>,
) -> Result<Value> {
    let l = legacy(path, body).ok_or_else(|| bad("not found"))?;
    let who = match (l.any_passkey, person) {
        (true, _) => Who::AnyPasskey,
        (false, Some(p)) => Who::Person(p),
        (false, None) => return Err(bad("sign in to the console first")),
    };
    match l.step {
        Step::Begin => begin(cx, passkeys, l.action, who, &l.target, &l.change).await,
        Step::Finish => finish(cx, passkeys, l.action, who, &l.target, &l.change, body).await,
    }
}
