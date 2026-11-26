//! Core's side of the Workers split (docs/plans/workers-split.md): the capability Workers' tools
//! merged with this Worker's own, the ops a capability Worker asks core (at
//! `https://core/<op>` over its `PLATFORM` binding), and two tools: `workers_setup`, which makes
//! the keys that sign callers (each Worker's own) and `SITE_KEY`, and `manifest_refresh`.
//!
//! A request is checked with the calling Worker's own key alone (there is no shared key since
//! step 4 of docs/plans/caller-keys.md), and may ask only the ops [`WORKER_OPS`] names for that
//! Worker (step 5).
//!
//! The ops: `credential`, `dispatch` and `approval` for a signed caller; and the proxy Worker's checks,
//! `verify` (who holds a proxy credential), `grant` (a CI job token's grant) and `deploy_grant`
//! (the caller a landing's deploy job token stands for), signed with the proxy's caller key but
//! for no caller, since each carries the credential it asks about.
//!
//! Core signs its own calls to each capability Worker with that Worker's key
//! ([`signing_key_name`], docs/plans/caller-keys.md, step 3).
//!
//! Core stays the front door. A capability Worker's call to `dispatch` runs through the same
//! [`crate::dispatch`] as `/mcp`, gate included, as the caller core signed for it. A
//! `credential` is only for a signed caller, and only for the call in hand.

use pigeon_contract::{
    Answer, ApprovalRequest, CALLERLESS_OPS, CORE_OPS, Credential, CredentialRequest, FailureKind,
    GrantRequest, SitesScopeRequest, VerifyRequest,
};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::site_auth::SITE_KEY;
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result};

use crate::remote::{self, Binding};
use crate::users::Caller;
use crate::{Call, Capability, Context, Footprint, Slot, unknown};

/// The host a capability Worker's requests to core carry, over its `PLATFORM` binding. A
/// request from the internet never has it: core is reached only on its own hostnames.
pub const CORE_HOST: &str = "core";

/// How long a linked account's access token is handed out for, at most, in milliseconds: the
/// call in hand, not the token's own life.
const LINKED_USE_MS: i64 = 10 * 60_000;

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn now_ms() -> i64 {
    worker::Date::now().as_millis() as i64
}

/// The local capabilities with the remote ones behind `bindings` added after them. A remote
/// capability whose name, a tool or a route is already taken locally is left out, so a
/// capability Worker can never shadow core.
pub async fn all(
    env: &Env,
    local: Vec<Box<dyn Capability>>,
    bindings: &[Binding],
) -> Vec<Box<dyn Capability>> {
    if bindings.is_empty() {
        return local;
    }
    merge(local, remote::capabilities(env, bindings).await)
}

/// Whether core's op `op` (the path without its leading `/`) needs the bound Workers'
/// capabilities. Only `dispatch` does: it runs any tool, wherever the tool lives. The other ops
/// are core's own business.
pub fn op_needs_bound(op: &str) -> bool {
    op == "dispatch"
}

/// The capabilities core uses to serve op `op`: [`all`] when the op needs the bound Workers'
/// ([`op_needs_bound`]), else `local` alone. `op` is the path without its leading `/`.
pub async fn for_op(
    env: &Env,
    op: &str,
    local: Vec<Box<dyn Capability>>,
    bindings: &[Binding],
) -> Vec<Box<dyn Capability>> {
    if op_needs_bound(op) {
        all(env, local, bindings).await
    } else {
        local
    }
}

/// How a remote capability meets the ones already in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clash {
    /// One already in the list has its name, exactly its tools and exactly its routes, and no
    /// other overlaps it: the hosting Worker serves that capability itself on purpose (the
    /// Lander's CI, [`crate::ci::Local`], so landing never waits on pigeon-ci-gateway).
    StandIn,
    /// Its name, a tool or a route is taken in any other way: a capability Worker would shadow
    /// core.
    Shadow,
}

/// How `remote` clashes with `list`, if it does.
pub fn clash(list: &[Box<dyn Capability>], remote: &dyn Capability) -> Option<Clash> {
    let tools_of = |c: &dyn Capability| {
        let mut t: Vec<&'static str> = c.tools().iter().map(|t| t.name).collect();
        t.sort_unstable();
        t
    };
    let routes_of = |c: &dyn Capability| {
        let mut r = c.routes();
        r.sort_unstable();
        r
    };
    let (tools, routes) = (tools_of(remote), routes_of(remote));
    let overlapping: Vec<&dyn Capability> = list
        .iter()
        .map(|c| c.as_ref())
        .filter(|c| {
            c.name() == remote.name()
                || c.tools().iter().any(|t| tools.contains(&t.name))
                || c.routes().iter().any(|x| routes.contains(x))
        })
        .collect();
    match overlapping[..] {
        [] => None,
        [c] if c.name() == remote.name() && tools_of(c) == tools && routes_of(c) == routes => {
            Some(Clash::StandIn)
        }
        _ => Some(Clash::Shadow),
    }
}

/// `local` with `remote` added after it. A remote capability whose name, a tool or a route is
/// already taken by one in the list is left out. Only a [`Clash::Shadow`] is logged: a
/// [`Clash::StandIn`] is the hosting Worker serving that capability itself, as meant.
pub fn merge(
    local: Vec<Box<dyn Capability>>,
    remote: Vec<Box<dyn Capability>>,
) -> Vec<Box<dyn Capability>> {
    let mut out = local;
    for r in remote {
        match clash(&out, r.as_ref()) {
            None => out.push(r),
            Some(Clash::StandIn) => {}
            Some(Clash::Shadow) => {
                // The console exists only in a Worker; a native caller (the smoke suite) gets
                // no line.
                #[cfg(target_arch = "wasm32")]
                worker::console_error!(
                    "split: remote capability {} clashes with a local one; left out",
                    r.name()
                );
            }
        }
    }
    out
}

/// The header a capability Worker sends to say which Worker it is, so core checks its
/// signature with that Worker's own key. The claim proves nothing alone: only a signature made
/// with that Worker's key passes, and then [`WORKER_OPS`] bounds what it may ask. A request
/// without it, or naming a Worker whose key core does not hold, is refused: there is no shared
/// key to fall back to (docs/plans/caller-keys.md, step 4).
pub const WORKER_HEADER: &str = "X-Pigeon-Worker";

/// The capability Workers that may get a key of their own, by the name they claim in
/// [`WORKER_HEADER`], with the Secrets Store name of that key. The names are the `name`s in
/// ci/workers.toml.
pub const WORKER_KEYS: [(&str, &str); 6] = [
    ("ci-gateway", "CALLER_KEY_CI_GATEWAY"),
    ("model-gateway", "CALLER_KEY_MODEL_GATEWAY"),
    ("sites", "CALLER_KEY_SITES"),
    ("proxy", "CALLER_KEY_PROXY"),
    ("ai-agents", "CALLER_KEY_AI_AGENTS"),
    ("cloud-gateway", "CALLER_KEY_CLOUD_GATEWAY"),
];

/// The Secrets Store name of `worker`'s own caller key; `None` for a name that is not a known
/// capability Worker.
pub fn worker_key_name(worker: &str) -> Option<&'static str> {
    WORKER_KEYS
        .iter()
        .find(|(w, _)| *w == worker)
        .map(|(_, k)| *k)
}

/// The name in [`WORKER_KEYS`] of the capability Worker deployed as `script` (its `script` in
/// ci/workers.toml, `pigeon-` and the name); `None` for any other script. Pure.
pub fn worker_of_script(script: &str) -> Option<&'static str> {
    let name = script.strip_prefix("pigeon-")?;
    WORKER_KEYS
        .iter()
        .find(|(w, _)| *w == name)
        .map(|(w, _)| *w)
}

/// The Secrets Store binding core signs its calls to the Worker deployed as `script` with: that
/// Worker's own key (docs/plans/caller-keys.md, step 3), so no other Worker's key signs for
/// it. An unknown script is an error: core sends it nothing signed. Pure.
pub fn signing_key_name(script: &str) -> std::result::Result<&'static str, String> {
    worker_of_script(script)
        .and_then(worker_key_name)
        .ok_or_else(|| format!("{script} is not a capability Worker with a caller key of its own"))
}

/// `worker`'s own caller key from the Secrets Store, when core binds it and it is set; `None`
/// otherwise, and the request is refused.
async fn worker_key(env: &Env, worker: &str) -> Option<Vec<u8>> {
    let name = worker_key_name(worker)?;
    let binding = env.secret_store(name).ok()?;
    let key = secrets_store::read(&binding).await.ok()??;
    Some(key.into_bytes())
}

/// The caller in `header`, checked with the calling Worker's own key `own`, the only key core
/// accepts from a Worker. A signature made with another Worker's key, the retired shared key or
/// a wrong key is refused. The skew checks are [`pigeon_contract::verify`]'s, and the check
/// compares in constant time. Pure.
pub fn verify_caller(
    own: &[u8],
    header: Option<&str>,
    op: &str,
    body: &[u8],
    now_ms: i64,
) -> std::result::Result<pigeon_contract::Signed, pigeon_contract::Refused> {
    pigeon_contract::verify(own, header, op, body, now_ms)
}

/// [`verify_caller`] as core's signed endpoint runs it ([`serve`]): with the recipient check of
/// [`pigeon_contract::verify_for`] for `recipient`, core's own Worker name. Pure.
pub fn verify_request(
    own: &[u8],
    header: Option<&str>,
    op: &str,
    body: &[u8],
    now_ms: i64,
    recipient: &str,
) -> std::result::Result<pigeon_contract::Signed, pigeon_contract::Refused> {
    pigeon_contract::verify_for(own, header, op, body, now_ms, recipient)
}

/// One core op a capability Worker may ask, and for some ops which requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allowed {
    /// The op, from [`CORE_OPS`].
    pub op: &'static str,
    /// For `credential`, `credential_revoke` and `credential_release`: the kinds it may name;
    /// for `dispatch`: the tools it may run. `None`: any.
    pub only: Option<&'static [&'static str]>,
}

const fn any(op: &'static str) -> Allowed {
    Allowed { op, only: None }
}

const fn only(op: &'static str, which: &'static [&'static str]) -> Allowed {
    Allowed {
        op,
        only: Some(which),
    }
}

/// What each capability Worker may ask core, by its name in [`WORKER_KEYS`]: every call its
/// crates make over their `PLATFORM` binding (docs/plans/caller-keys.md, step 5). It applies
/// to every request core accepts from a Worker: each is signed with that Worker's own key.
///
/// `ai-agents` runs whatever tools a run's model chooses, so its `dispatch` is bounded by the
/// signed caller and core's gate, not a tool list. `sites` dispatches two tools only, for its
/// public routes; its deploys reach core only through `sites_scope`.
pub const WORKER_OPS: [(&str, &[Allowed]); 6] = [
    // src/sites/src/deploy.rs (sites_scope), public.rs (dispatch).
    (
        "sites",
        &[
            any("sites_scope"),
            only("dispatch", &["instance_subdomains", "site_passkey"]),
        ],
    ),
    // src/ci/src/lib.rs, src/ci/src/scope.rs.
    (
        "ci-gateway",
        &[only("credential", &["scoped"]), any("ci_scope")],
    ),
    // src/models/src/lib.rs (`credential_given`).
    ("model-gateway", &[only("credential", &["scoped"])]),
    // src/cloud/src/link.rs, src/cloud/src/approvals.rs.
    (
        "cloud-gateway",
        &[only("credential", &["scoped"]), any("approval")],
    ),
    // src/proxy/src/lib.rs.
    (
        "proxy",
        &[
            any("verify"),
            any("grant"),
            any("deploy_grant"),
            only("credential", &["scoped"]),
        ],
    ),
    // src/ai-agents/src/object.rs.
    (
        "ai-agents",
        &[
            any("dispatch"),
            only("credential", &["run_grant"]),
            only("credential_revoke", &["run_grant"]),
            only("credential_release", &["scoped"]),
            any("service_user"),
        ],
    ),
];

/// Whether Worker `worker`, verified with its own key, may ask core op `op` with `body`
/// ([`WORKER_OPS`]); else why not. Pure.
pub fn worker_may(worker: &str, op: &str, body: &[u8]) -> std::result::Result<(), String> {
    let Some((_, allowed)) = WORKER_OPS.iter().find(|(w, _)| *w == worker) else {
        return Err(format!("`{worker}` is not a capability Worker core knows"));
    };
    let Some(allowed) = allowed.iter().find(|a| a.op == op) else {
        return Err(format!("Worker `{worker}` may not ask core `{op}`"));
    };
    let Some(which) = allowed.only else {
        return Ok(());
    };
    let asked: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let field = if op == "dispatch" { "tool" } else { "kind" };
    let named = asked[field].as_str().unwrap_or_default();
    if which.contains(&named) {
        Ok(())
    } else {
        Err(format!(
            "Worker `{worker}` may not ask core `{op}` for {field} `{named}`"
        ))
    }
}

/// Contextual callbacks are read-only and name the authenticated instance.
pub fn verify_ci_scope(
    signed: &pigeon_contract::Signed,
    request: &crate::ci::ScopeRequest,
) -> std::result::Result<(), pigeon_contract::Refused> {
    if signed.context.is_none() {
        return Ok(());
    }
    match request {
        crate::ci::ScopeRequest::Read { instance } => signed.require_instance(instance),
        crate::ci::ScopeRequest::Attribute { .. } => {
            Err(pigeon_contract::Refused::InstanceMismatch)
        }
    }
}

/// Serves one op a capability Worker asks core: `credential` or `dispatch`, with the caller
/// core signed for it; or one of the proxy Worker's checks, signed for no caller. Every answer
/// is JSON, as [`pigeon_contract::serve`] answers.
pub async fn serve(mut req: Request, cx: &Context<'_>) -> Result<Response> {
    let op = req.path().trim_start_matches('/').to_string();
    let reply = |a: Answer, status: u16| -> Result<Response> {
        Ok(Response::from_json(&a)?.with_status(status))
    };
    if !CORE_OPS.contains(&op.as_str()) {
        return reply(
            Answer::error(FailureKind::UnknownOp, format!("unknown op {op}")),
            200,
        );
    }
    let header = req.headers().get(pigeon_contract::HEADER)?;
    let body = req.bytes().await?;
    let worker_name = req.headers().get(WORKER_HEADER)?;
    let worker = worker_name.as_deref().unwrap_or_default();
    // Only a known Worker's own key verifies (docs/plans/caller-keys.md, step 4).
    let Some(own) = worker_key(cx.env, worker).await else {
        return reply(
            Answer::error(
                FailureKind::Unauthorized,
                format!(
                    "no caller key for Worker `{worker}`: a request to core names a known \
                     capability Worker in {WORKER_HEADER} and is signed with its own key"
                ),
            ),
            401,
        );
    };
    let signed = match verify_request(
        &own,
        header.as_deref(),
        &op,
        &body,
        now_ms(),
        cx.hosted.worker,
    ) {
        Ok(s) => s,
        Err(why) => {
            return reply(
                Answer::error(FailureKind::Unauthorized, why.to_string()),
                401,
            );
        }
    };
    // The signature was made with this Worker's own key, so it says which Worker asked.
    if let Err(why) = worker_may(worker, &op, &body) {
        #[cfg(target_arch = "wasm32")]
        worker::console_error!("split: refused a Worker's core op: {why}");
        return reply(Answer::error(FailureKind::Unauthorized, why), 403);
    }
    if signed.instance().is_some()
        && !matches!(op.as_str(), "dispatch" | "ci_scope" | "sites_scope")
    {
        return reply(
            Answer::error(
                FailureKind::Unauthorized,
                "core operation does not accept instance context",
            ),
            401,
        );
    }
    if CALLERLESS_OPS.contains(&op.as_str()) {
        let answer = match proxy_check(cx.env, &op, &body).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                return reply(
                    Answer::error(FailureKind::BadRequest, "the request does not parse"),
                    400,
                );
            }
            Err(e) => return reply(Answer::error(FailureKind::Failed, e.to_string()), 200),
        };
        return reply(Answer::Ok(answer), 200);
    }
    let Ok(caller) = serde_json::from_value::<Caller>(signed.caller.clone()) else {
        return reply(
            Answer::error(
                FailureKind::Unauthorized,
                "core acts only for a signed caller, and this request has none",
            ),
            401,
        );
    };
    if op == "sites_scope" {
        let request = match serde_json::from_slice::<SitesScopeRequest>(&body) {
            Ok(request) => request,
            Err(e) => return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400),
        };
        if let Err(why) = signed.require_instance(request.instance()) {
            return reply(
                Answer::error(FailureKind::Unauthorized, why.to_string()),
                401,
            );
        }
        return match crate::sites_scope::handle(cx.env, &request).await {
            Ok(v) => reply(Answer::Ok(v), 200),
            Err(e) => reply(Answer::error(FailureKind::Failed, e.to_string()), 200),
        };
    }
    let scope = if let Some(instance) = signed.instance() {
        let Some(record) = crate::instances::one(cx.env, instance).await? else {
            return reply(
                Answer::error(FailureKind::Unauthorized, "unknown signed instance"),
                401,
            );
        };
        Some(crate::instances::tools::Scope::new(&record)?)
    } else {
        None
    };
    let cx = Context {
        caller: Some(&caller),
        instance: scope.as_ref(),
        caller_note: None,
        ..*cx
    };
    let answer = match op.as_str() {
        "service_user" => {
            let asked: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            service_user(&cx, &asked).await
        }
        "ci_scope" => match serde_json::from_slice::<crate::ci::ScopeRequest>(&body) {
            Ok(request) => {
                if let Err(why) = verify_ci_scope(&signed, &request) {
                    return reply(
                        Answer::error(FailureKind::Unauthorized, why.to_string()),
                        401,
                    );
                }
                crate::ci::instance_scope(cx.env, &request).await
            }
            Err(e) => return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400),
        },
        "dispatch" => {
            let asked: Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400);
                }
            };
            let tool = asked["tool"].as_str().unwrap_or_default();
            let args = asked.get("args").cloned().unwrap_or_else(|| json!({}));
            crate::dispatch_checked(&cx, tool, &args).await
        }
        "approval" => match serde_json::from_slice::<ApprovalRequest>(&body) {
            Ok(r) => remote_approval(&cx, &r).await,
            Err(e) => return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400),
        },
        _ => match serde_json::from_slice::<CredentialRequest>(&body) {
            Ok(r) if op == "credential_revoke" => credential_revoke(&cx, &r).await,
            Ok(r) if op == "credential_release" => credential_release(&cx, &r).await,
            Ok(r) => credential(&cx, &r).await,
            Err(e) => return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400),
        },
    };
    match answer {
        Ok(v) => reply(Answer::Ok(v), 200),
        Err(e) => reply(Answer::error(FailureKind::Failed, e.to_string()), 200),
    }
}

/// The approval kinds a capability Worker may create, read and spend through core's `approval`
/// op: a fixed table, so a Worker never makes an approval that looks like a tool call, a draft
/// or a plan of core's own.
pub const REMOTE_APPROVAL_KINDS: [&str; 1] = ["cloud_change"];

/// How long a capability Worker's plan waits for the person's passkey: 30 minutes.
const REMOTE_PENDING_MS: i64 = 30 * 60_000;
/// The longest subject, in characters.
const REMOTE_SUBJECT_MAX: usize = 120;
/// The most lines a plan shows the person, and the longest line, in characters.
const REMOTE_LINES_MAX: usize = 20;
const REMOTE_LINE_MAX: usize = 400;
/// The largest payload, as canonical JSON, in bytes.
const REMOTE_PAYLOAD_MAX: usize = 16 * 1024;

/// What a `create` request becomes: its kind, subject, digest and the summary the person sees.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteApproval {
    /// The kind, from [`REMOTE_APPROVAL_KINDS`].
    pub kind: pigeon_core::approval::Kind,
    /// What the approval is for.
    pub subject: String,
    /// Core's digest of the kind, subject and payload.
    pub digest: String,
    /// `{plan: lines}`, as the Approvals tab shows it.
    pub summary: Value,
}

/// The kind a request names, when it is in [`REMOTE_APPROVAL_KINDS`]. Pure.
pub fn remote_kind(
    r: &ApprovalRequest,
) -> std::result::Result<pigeon_core::approval::Kind, String> {
    let kind = r.kind.as_deref().unwrap_or_default();
    if REMOTE_APPROVAL_KINDS.contains(&kind) {
        Ok(pigeon_core::approval::Kind::named(kind))
    } else {
        Err(format!(
            "`{kind}` is not an approval kind a capability Worker may ask for"
        ))
    }
}

/// The subject a request names, checked. Pure.
fn remote_subject(r: &ApprovalRequest) -> std::result::Result<&str, String> {
    let subject = r.subject.as_deref().unwrap_or_default();
    if subject.is_empty()
        || subject.chars().count() > REMOTE_SUBJECT_MAX
        || subject.chars().any(char::is_control)
    {
        return Err(format!(
            "an approval's subject is 1 to {REMOTE_SUBJECT_MAX} characters on one line"
        ));
    }
    Ok(subject)
}

/// Checks a `create` request and works out its digest and summary: the kind must be in the
/// table, the subject a short line, the payload an object of bounded size and the lines few and
/// short. Core takes the digest itself, so a Worker cannot name one. Pure.
pub fn remote_plan(r: &ApprovalRequest) -> std::result::Result<RemoteApproval, String> {
    let kind = remote_kind(r)?;
    let subject = remote_subject(r)?.to_string();
    if !r.payload.is_object() {
        return Err("an approval's payload is a JSON object".into());
    }
    if pigeon_core::approval::canonical(&r.payload).len() > REMOTE_PAYLOAD_MAX {
        return Err(format!(
            "an approval's payload is at most {REMOTE_PAYLOAD_MAX} bytes"
        ));
    }
    if r.lines.is_empty() || r.lines.len() > REMOTE_LINES_MAX {
        return Err(format!(
            "an approval shows the person 1 to {REMOTE_LINES_MAX} lines"
        ));
    }
    if r.lines
        .iter()
        .any(|l| l.trim().is_empty() || l.chars().count() > REMOTE_LINE_MAX)
    {
        return Err(format!(
            "each line of an approval is 1 to {REMOTE_LINE_MAX} characters"
        ));
    }
    let digest = pigeon_core::approval::payload_digest(&kind, &subject, &r.payload);
    Ok(RemoteApproval {
        kind,
        subject,
        digest,
        summary: json!({ "plan": r.lines }),
    })
}

/// The digest a `take` request stands for, from its kind, subject and payload. Pure.
pub fn remote_digest(r: &ApprovalRequest) -> std::result::Result<String, String> {
    let kind = remote_kind(r)?;
    let subject = remote_subject(r)?;
    Ok(pigeon_core::approval::payload_digest(
        &kind, subject, &r.payload,
    ))
}

/// What `get` answers for `record` to the caller `caller_id`: the record of a table kind the
/// caller made, its state as of `now_ms` (a lapsed one reads `expired`); none for any other, so
/// an id says nothing to anyone else. Pure.
pub fn remote_view(
    record: &pigeon_core::approval::Record,
    caller_id: &str,
    now_ms: i64,
) -> Option<Value> {
    if !REMOTE_APPROVAL_KINDS.contains(&record.kind.name()) || record.caller_id != caller_id {
        return None;
    }
    let state = if pigeon_core::approval::is_stale(record, now_ms) {
        pigeon_core::approval::State::Expired
    } else {
        record.state
    };
    Some(json!({
        "id": record.id,
        "kind": record.kind.name(),
        "subject": record.subject,
        "state": state.name(),
        "payload": record.payload,
        "expires_ms": record.expires_ms,
    }))
}

/// Core's `approval` op: `create` makes a pending approval for the person the caller acts for,
/// `get` reads one the caller made and `take` spends an approved one once, for the caller, for
/// exactly this payload. The approver is never the Worker's choice.
async fn remote_approval(cx: &Context<'_>, r: &ApprovalRequest) -> Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| bad("an approval is for a signed caller"))?;
    let id = r.id.as_deref().unwrap_or_default();
    match r.action.as_str() {
        "create" => {
            let person = caller
                .acting_for
                .as_ref()
                .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
            let planned = remote_plan(r).map_err(|e| bad(&e))?;
            let made = crate::users::directory(
                cx.env,
                "approval.create",
                json!({
                    "kind": planned.kind.name(),
                    "subject": planned.subject,
                    "digest": planned.digest,
                    "summary": planned.summary,
                    "payload": r.payload,
                    "caller_id": caller.user.id,
                    "approver_id": person.id,
                    "pending_ms": REMOTE_PENDING_MS,
                }),
            )
            .await?;
            Ok(json!({
                "id": made["record"]["id"],
                "expires_ms": made["record"]["expires_ms"],
                "existing": made["existing"],
            }))
        }
        "get" => {
            let got = crate::users::directory(cx.env, "approval.get", json!({ "id": id })).await?;
            let record: Option<pigeon_core::approval::Record> = serde_json::from_value(got)?;
            record
                .and_then(|rec| remote_view(&rec, &caller.user.id, now_ms()))
                .ok_or_else(|| bad(&format!("there is no approval {id} for this caller")))
        }
        "take" => {
            let digest = remote_digest(r).map_err(|e| bad(&e))?;
            let got = crate::users::directory(cx.env, "approval.get", json!({ "id": id })).await?;
            let record: Option<pigeon_core::approval::Record> = serde_json::from_value(got)?;
            if record
                .and_then(|rec| remote_view(&rec, &caller.user.id, now_ms()))
                .is_none()
            {
                return Err(bad(&format!("there is no approval {id} for this caller")));
            }
            let taken = crate::users::directory(
                cx.env,
                "approval.take",
                json!({ "id": id, "digest": digest, "caller_id": caller.user.id }),
            )
            .await?;
            Ok(match taken["refused"].as_str() {
                Some(why) => json!({ "taken": false, "refused": why }),
                None => json!({ "taken": true }),
            })
        }
        other => Err(bad(&format!(
            "no approval action `{other}`: create, get or take"
        ))),
    }
}

/// The service users a capability Worker may ask core for, by key, with their names: the AI
/// agent runs' delegates (docs/plans/ai-agents.md). A fixed table, so no Worker makes users of
/// its own choosing.
pub const SERVICE_USERS: [(&str, &str); 2] = [
    ("ai-agent:planner", "Pigeon planner"),
    ("ai-agent:executor", "Pigeon executor"),
];

/// The name of the service user `key` names, from [`SERVICE_USERS`].
pub fn service_user_name(key: &str) -> Option<&'static str> {
    SERVICE_USERS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, n)| *n)
}

/// `service_user {key}`: the service user `key` names, made the first time it is asked for. It
/// acts for a person only once they link it with their passkey: until then the answer carries
/// a link request for the person the caller acts for, to approve at `/auth/link`.
async fn service_user(cx: &Context<'_>, asked: &Value) -> Result<Value> {
    let key = asked["key"].as_str().unwrap_or_default();
    let name = service_user_name(key).ok_or_else(|| {
        bad(&format!(
            "no service user `{key}`; these are: ai-agent:planner, ai-agent:executor"
        ))
    })?;
    let found =
        crate::users::directory(cx.env, "service.find", json!({ "access_client_id": key })).await?;
    let user = if found.is_null() {
        crate::users::directory(
            cx.env,
            "service.add",
            json!({ "access_client_id": key, "name": name }),
        )
        .await?
    } else {
        found
    };
    let person = cx
        .caller
        .and_then(|c| c.acting_for.as_ref())
        .map(|p| p.id.clone());
    let mut out = json!({ "caller": user });
    if user["acting_for"].is_null()
        && let Some(person) = person
    {
        let id = crate::users::directory(
            cx.env,
            "link.request",
            json!({ "service_user_id": user["user"]["id"], "standard_user_id": person }),
        )
        .await?;
        let host = cx.env.var("PLATFORM_HOST")?.to_string();
        out["approve"] = json!(format!(
            "https://{host}/auth/link?request={}",
            id.as_str().unwrap_or_default()
        ));
    }
    Ok(out)
}

/// One of the proxy Worker's checks, `op` with `body`, answered as core's own proxies answer
/// it: the holder of a proxy credential, a job token's grant for a repository, or the caller a
/// landing's deploy job token stands for; null for none. `None` when the body does not parse.
async fn proxy_check(env: &Env, op: &str, body: &[u8]) -> Result<Option<Value>> {
    Ok(Some(match op {
        "verify" => {
            let Ok(r) = serde_json::from_slice::<VerifyRequest>(body) else {
                return Ok(None);
            };
            serde_json::to_value(crate::proxy::verify(env, &r.credential).await?)?
        }
        "grant" => {
            let Ok(r) = serde_json::from_slice::<GrantRequest>(body) else {
                return Ok(None);
            };
            serde_json::to_value(crate::proxy::job_grant(env, &r.token, &r.repo).await?)?
        }
        "deploy_grant" => {
            let Ok(r) = serde_json::from_slice::<GrantRequest>(body) else {
                return Ok(None);
            };
            serde_json::to_value(crate::land::deploy_caller(env, &r.token, &r.repo).await?)?
        }
        _ => return Ok(None),
    }))
}

/// The linked-account grant for `sites_scope`'s contextual publishing: the account the approved
/// record selects is checked as part of the same link lookup.
pub(crate) async fn linked_credential(env: &Env, instance: &str, account: &str) -> Result<Value> {
    let host = env.var("PLATFORM_HOST")?.to_string();
    let account = crate::linked::account_for_bound(env, &host, instance, account).await?;
    let token = account
        .bearer
        .ok_or_else(|| bad("the linked account came back without its access token"))?;
    Ok(serde_json::to_value(Credential {
        token,
        account_id: account.id,
        expires_at: now_ms() + LINKED_USE_MS,
        token_id: None,
    })?)
}

/// A short-lived credential for the call in hand: `scoped` (a minted Cloudflare token) or
/// `run_grant` (an AI agent run's git grant). An instance's linked-account and GitHub tokens are
/// no kinds here: Sites, their only user, gets them through the instance-scoped `sites_scope`
/// callback, which a call signed for that instance alone may ask.
async fn credential(cx: &Context<'_>, r: &CredentialRequest) -> Result<Value> {
    match r.kind.as_str() {
        "scoped" => scoped(cx, &r.scopes).await,
        // An AI agent run's git grant (docs/plans/ai-agents.md): for a caller acting for a
        // person, pushing only the run's own branches.
        "run_grant" => {
            let run = r
                .instance
                .as_deref()
                .ok_or_else(|| bad("a run grant names its run"))?;
            let person = grant_person(cx.caller).map_err(|e| bad(&e))?;
            let (token, expires_at) = crate::proxy::issue_run_grant(cx.env, &person, run).await?;
            Ok(serde_json::to_value(Credential {
                token,
                account_id: String::new(),
                expires_at,
                token_id: None,
            })?)
        }
        other => Err(bad(&format!("no credential of kind `{other}`"))),
    }
}

/// The person a run grant is for: the person a caller acts for, or the caller itself when it is
/// a standard user. Minting and revoking both use it, so they agree on whose grant it is.
pub fn grant_person(caller: Option<&Caller>) -> std::result::Result<String, String> {
    const NEEDS: &str = "a run grant needs a caller acting for a person";
    let caller = caller.ok_or_else(|| NEEDS.to_string())?;
    match (&caller.acting_for, caller.user.kind.as_str()) {
        (Some(p), _) => Ok(p.id.clone()),
        (None, "standard") => Ok(caller.user.id.clone()),
        _ => Err(NEEDS.to_string()),
    }
}

/// Whether a token named `name` is one core minted for a capability Worker, and so one
/// `credential_revoke` may end. Workers may revoke only `pigeon:worker-` tokens
/// ([`pigeon_core::tokens::WORKER_NAME_PREFIX`]); any other token (the parent, a person's, a
/// job's, or another pigeon token, see [`pigeon_core::tokens::minted`]) is refused. Pure.
pub fn scoped_token_revocable(name: &str) -> bool {
    name.starts_with(pigeon_core::tokens::WORKER_NAME_PREFIX)
}

/// `credential_revoke`: ends AI agent run `instance`'s git grant for the caller's person, or
/// (kind `scoped`) the scoped token whose id is `instance`. A scoped token is read from
/// Cloudflare first and revoked only when [`scoped_token_revocable`] accepts its name (a
/// `pigeon:worker-` token; see [`pigeon_core::tokens::minted`] for pigeon's names). This check
/// was not the cause of the 2026-11-24 quota incident and is not widened: the cause was tokens
/// nobody revoked (docs/plans/token-minting.md). Answers `{"revoked": n}`; a grant or token already gone is `0`, not an error.
async fn credential_revoke(cx: &Context<'_>, r: &CredentialRequest) -> Result<Value> {
    if r.kind == "scoped" {
        let id = r
            .instance
            .as_deref()
            .filter(|i| !i.is_empty() && i.chars().all(|c| c.is_ascii_alphanumeric()))
            .ok_or_else(|| bad("a scoped revoke names the token's id"))?;
        // A leased token is shared: core owns its life, so a taker's revoke keeps it.
        if crate::ledger::holds(cx.env, id).await? {
            return Ok(json!({ "revoked": 0, "kept": "shared" }));
        }
        let path = format!("/accounts/{}/tokens/{id}", cx.account.id);
        let read =
            pigeon_core::cloudflare::call(&cx.account.parent, worker::Method::Get, &path, None)
                .await;
        let Ok(token) = read else {
            // Not readable: already revoked or never ours.
            return Ok(json!({ "revoked": 0 }));
        };
        if !scoped_token_revocable(token["name"].as_str().unwrap_or_default()) {
            return Err(bad("that token was not minted for a capability Worker"));
        }
        pigeon_core::tokens::revoke(&cx.account.parent, &cx.account.id, id).await?;
        return Ok(json!({ "revoked": 1 }));
    }
    if r.kind != "run_grant" {
        return Err(bad(&format!(
            "no credential of kind `{}` to revoke",
            r.kind
        )));
    }
    let run = r
        .instance
        .as_deref()
        .ok_or_else(|| bad("a run grant names its run"))?;
    let person = grant_person(cx.caller).map_err(|e| bad(&e))?;
    let n = crate::proxy::revoke_run_grant(cx.env, &person, run).await?;
    Ok(json!({ "revoked": n }))
}

/// How long a scoped token minted for a capability Worker lives, in seconds.
const SCOPED_TTL_SECS: u64 = 900;

/// The permissions each purpose a capability Worker may ask for brings: a fixed table, so a
/// capability Worker never names a permission itself (docs/plans/workers-split.md).
pub fn purpose_permissions(purpose: &str) -> Option<&'static [&'static str]> {
    Some(match purpose {
        // Model calls, usage and logs, and the gateway's setup.
        "ai_gateway" => &[
            "AI Gateway Read",
            "AI Gateway Run",
            "AI Gateway Write",
            "Secrets Store Write",
        ],
        // Git tokens for the source repository: reading a job's config, pushing notes; and the
        // proxy Worker's `/git/`, a git token per request, revoked after.
        "artifacts" => &["Artifacts Read", "Artifacts Write"],
        // A capability's own secrets, set on the secrets page.
        "secrets_store" => &["Secrets Store Write"],
        // The model gateway's Workers AI calls: one token per call, revoked after.
        "workers_ai" => &["Workers AI Read"],
        // The MCP server portal, synced after a setup.
        "mcp_portal" => &["MCP Portals Write"],
        // The proxy Worker's `/cf/`: every call `wrangler deploy` of a platform Worker makes
        // (reading and uploading the Worker, with its Secrets Store bindings), the queues it
        // looks up and its container image and applications; the union of what the deploy
        // proxy's allowlist names (`pigeon_proxies::cf_proxy::ALLOWED`).
        "cf_deploy" => &[
            "Workers Scripts Read",
            "Workers Scripts Write",
            "Secrets Store Write",
            "Queues Read",
            "Workers Containers Read",
            "Workers Containers Write",
            "Workers R2 Storage Read",
        ],
        // The home instance's plans and applies (`instances::home`): R2 write for its landing
        // bucket (on the account, as creating a bucket needs), and the platform zone's custom
        // domains (on that zone alone). Never Workers-scripts write: the platform's Workers are
        // the Lander's. Core mints it itself; a capability Worker may not ask for it.
        "instance_home" => &["Workers R2 Storage Write", "Zone Read"],
        _ => return None,
    })
}

/// Purposes in [`purpose_permissions`] that core mints for itself and a capability Worker may
/// not ask `credential` for.
pub const CORE_ONLY_PURPOSES: [&str; 1] = [crate::instances::home::PURPOSE];

/// A token minted from the parent token for `purposes`, living [`SCOPED_TTL_SECS`]: the
/// capability Worker uses it as an `Account`'s bearer, and it expires on its own.
async fn scoped(cx: &Context<'_>, purposes: &[String]) -> Result<Value> {
    if purposes.is_empty() {
        return Err(bad("a scoped credential names its purposes"));
    }
    let mut permissions: Vec<&str> = Vec::new();
    for p in purposes {
        if CORE_ONLY_PURPOSES.contains(&p.as_str()) {
            return Err(bad(&format!(
                "credential purpose `{p}` is minted by core alone"
            )));
        }
        let more =
            purpose_permissions(p).ok_or_else(|| bad(&format!("no credential purpose `{p}`")))?;
        for m in more {
            if !permissions.contains(m) {
                permissions.push(m);
            }
        }
    }
    let caller = cx
        .caller
        .ok_or_else(|| bad("a scoped credential is for a signed caller"))?;
    let lease = crate::ledger::lease(
        cx.env,
        &ledger_caller(caller),
        &format!("worker-{}", purposes.join("-")),
        &permissions,
        SCOPED_TTL_SECS,
    )
    .await?;
    Ok(serde_json::to_value(Credential {
        token: lease.value,
        account_id: cx.account.id.clone(),
        expires_at: lease.expires_at,
        token_id: Some(lease.id),
    })?)
}

/// The ledger's hash of `caller`: its user id and the person it acts for, if any.
fn ledger_caller(caller: &Caller) -> String {
    crate::ledger::caller_hash(
        &caller.user.id,
        caller.acting_for.as_ref().map(|p| p.id.as_str()),
    )
}

/// `credential_release`: revokes every shared token leased to the signed caller (all permission
/// sets), as the caller's last live run ends. Answers `{"released": n}`.
async fn credential_release(cx: &Context<'_>, r: &CredentialRequest) -> Result<Value> {
    if r.kind != "scoped" {
        return Err(bad(&format!(
            "no credential of kind `{}` to release",
            r.kind
        )));
    }
    let caller = cx
        .caller
        .ok_or_else(|| bad("a release is for a signed caller"))?;
    let n = crate::ledger::release(cx.env, &ledger_caller(caller)).await?;
    Ok(json!({ "released": n }))
}

/// `workers_setup` and `manifest_refresh`.
pub struct Split {
    /// The capability Workers bound to this one.
    pub bindings: &'static [Binding],
}

/// Every caller key `workers_setup` keeps in the Secrets Store: each capability Worker's own
/// ([`WORKER_KEYS`]). The shared `CALLER_KEY` is no longer made (docs/plans/caller-keys.md,
/// step 4); the person deletes it once every Worker has deployed without it.
pub fn caller_key_names() -> Vec<&'static str> {
    WORKER_KEYS.iter().map(|(_, k)| *k).collect()
}

/// Every key `workers_setup` keeps in the Secrets Store: the caller keys
/// ([`caller_key_names`]), then [`SITE_KEY`], which protected sites' origin and visitor
/// session keys are derived from (docs/plans/caller-keys.md, step 4).
pub fn setup_key_names() -> Vec<&'static str> {
    let mut names = caller_key_names();
    names.push(SITE_KEY);
    names
}

/// The keys `workers_setup` makes, given the secrets the store already holds: each of
/// [`setup_key_names`] not in `existing`, in that order. An existing key is never remade.
/// Pure.
pub fn keys_to_make(existing: &[&str]) -> Vec<&'static str> {
    setup_key_names()
        .into_iter()
        .filter(|k| !existing.contains(k))
        .collect()
}

impl Split {
    /// Makes each key ([`setup_key_names`]) that is not in the Secrets Store, readable
    /// by Workers. An existing key is kept: replacing it would refuse every call in flight. No
    /// key's value is ever answered or logged.
    async fn setup(&self, cx: &Context<'_>) -> Result<Value> {
        let writer = Writer::open(cx.account).await?;
        let made = async {
            let listed = writer.list().await?;
            let existing: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
            let missing = keys_to_make(&existing);
            for name in &missing {
                let key = pigeon_core::webauthn::random()?;
                let comment = if *name == SITE_KEY {
                    "SITE_KEY: derives protected sites' origin and visitor session keys; \
                     bound by core and pigeon-sites only, by workers_setup"
                        .to_string()
                } else {
                    format!("{name}: one capability Worker's own caller key, by workers_setup")
                };
                writer
                    .put_scoped(name, &key, &comment, &[secrets_store::WORKERS])
                    .await?;
            }
            Ok(missing)
        }
        .await;
        let made = writer.finish(made).await?;
        let keys: serde_json::Map<String, Value> = setup_key_names()
            .into_iter()
            .map(|k| {
                let state = if made.contains(&k) {
                    "made"
                } else {
                    "already there; kept"
                };
                (k.to_string(), json!(state))
            })
            .collect();
        Ok(json!({
            "keys": keys,
            "next": "core binds every CALLER_KEY_<WORKER> and each capability Worker its own as \
                     CALLER_KEY_OWN; core and pigeon-sites bind SITE_KEY \
                     (docs/plans/caller-keys.md). A binding to a missing secret fails the \
                     deploy, so a new one lands after this",
        }))
    }
}

impl Capability for Split {
    fn name(&self) -> &'static str {
        "split"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "split",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "workers_setup",
                "Make the keys that sign the caller core forwards to its capability Workers, \
                 in the Secrets Store: each capability Worker's own CALLER_KEY_<WORKER>, and \
                 SITE_KEY (protected sites' keys are derived from it), each when it is not there; an existing key is kept and no value is shown. Run \
                 before a Worker binds a key.",
            ),
            Tool::bare(
                "manifest_refresh",
                "Ask every capability Worker for its tools again now, rather than within a \
                 minute: after a capability Worker deployed. Answers each Worker's capabilities \
                 and tools, or why it did not answer.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "workers_setup" => self.setup(cx).await,
                "manifest_refresh" => {
                    remote::forget();
                    let found = remote::capabilities(cx.env, self.bindings).await;
                    let answered: Vec<Value> = found
                        .iter()
                        .map(|c| {
                            json!({
                                "capability": c.name(),
                                "tools": c.tools().iter().map(|t| t.name).collect::<Vec<_>>(),
                            })
                        })
                        .collect();
                    let unreachable: Vec<Value> = remote::unreachable()
                        .into_iter()
                        .map(|(worker, why)| json!({ "worker": worker, "why": why }))
                        .collect();
                    Ok(json!({ "capabilities": answered, "unreachable": unreachable }))
                }
                other => Err(unknown(other)),
            }
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        Vec::new()
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            workers: self
                .bindings
                .iter()
                .map(|b| crate::WorkerFootprint {
                    name: b.worker.to_string(),
                    routes: Vec::new(),
                })
                .collect(),
            ..Footprint::default()
        }
    }
}
