//! Approvals of single actions, each bound to a digest of exactly that action: approved once with
//! the person's passkey, and used once. Gated tool calls and instance drafts share it.
//! This module is pure: the Directory stores records, and the console and dispatch call these
//! functions.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Pending lifetime of a tool call approval.
pub const TOOL_CALL_TTL_MS: i64 = 10 * 60 * 1000;
/// Pending lifetime of an instance draft approval.
pub const INSTANCE_DRAFT_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Pending lifetime of a linked plan approval.
pub const LINKED_PLAN_TTL_MS: i64 = 30 * 60 * 1000;
/// After approval, how long an approved tool call or plan may still be used.
pub const USE_TTL_MS: i64 = 10 * 60 * 1000;
/// Most pending approvals one caller may hold at once.
pub const MAX_PENDING_PER_CALLER: usize = 5;
/// Shown in place of a secret value.
pub const REDACTED: &str = "‹redacted›";

/// Key fragments whose values are redacted.
const SENSITIVE: [&str; 5] = ["token", "secret", "password", "key", "value"];

/// What is being approved. Unknown names round-trip as Other, so a newer kind never breaks an
/// older reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    ToolCall,
    InstanceDraft,
    LinkedPlan,
    Other(String),
}

impl Serialize for Kind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Kind::named(&s))
    }
}

impl Kind {
    /// Its name as stored.
    pub fn name(&self) -> &str {
        match self {
            Kind::ToolCall => "tool_call",
            Kind::InstanceDraft => "instance_draft",
            Kind::LinkedPlan => "linked_plan",
            Kind::Other(s) => s.as_str(),
        }
    }

    /// The kind named `s`.
    pub fn named(s: &str) -> Kind {
        match s {
            "tool_call" => Kind::ToolCall,
            "instance_draft" => Kind::InstanceDraft,
            "linked_plan" => Kind::LinkedPlan,
            other => Kind::Other(other.to_string()),
        }
    }

    /// Pending lifetime: 10 min, 7 days, 30 min; an unknown kind's is a tool call's.
    pub fn ttl_ms(&self) -> i64 {
        match self {
            Kind::ToolCall | Kind::Other(_) => TOOL_CALL_TTL_MS,
            Kind::InstanceDraft => INSTANCE_DRAFT_TTL_MS,
            Kind::LinkedPlan => LINKED_PLAN_TTL_MS,
        }
    }

    /// Whether approval applies it at once (a draft), rather than leaving it to be used.
    pub fn applies_on_approve(&self) -> bool {
        matches!(self, Kind::InstanceDraft)
    }
}

/// Where an approval stands.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pending,
    Approved,
    Denied,
    Expired,
    Used,
}

impl State {
    /// Its name as stored.
    pub fn name(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Approved => "approved",
            State::Denied => "denied",
            State::Expired => "expired",
            State::Used => "used",
        }
    }
}

/// One approval as the Directory keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    pub kind: Kind,
    /// The tool's name, or the instance's.
    pub subject: String,
    pub digest: String,
    /// What the person sees: redacted.
    pub summary: Value,
    /// What approval applies (drafts, plans); Null for a tool call.
    #[serde(default)]
    pub payload: Value,
    pub caller_id: String,
    /// Who may approve: the person the caller acts for; None means any person.
    pub approver_id: Option<String>,
    pub state: State,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub decided_by: Option<String>,
    pub decided_ms: Option<i64>,
    pub used_ms: Option<i64>,
    /// Why it ended as it did, when it ended other than by use, such as "the record changed
    /// since this draft".
    #[serde(default)]
    pub note: Option<String>,
    /// What the platform did with it once approved (docs/plans/approvals-auto-run.md): None
    /// until a runner claims it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// The approval whose run asked for this one, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Where the run of an approved operation stands.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Done,
    Failed,
}

impl RunStatus {
    /// Its name as stored.
    pub fn name(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Done => "done",
            RunStatus::Failed => "failed",
        }
    }
}

/// The run of an approved operation, by the platform, as the approval's caller.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Outcome {
    pub status: RunStatus,
    /// The tool that ran (or runs).
    pub tool: String,
    pub started_ms: i64,
    #[serde(default)]
    pub finished_ms: Option<i64>,
    /// One line for the person: what came of it.
    #[serde(default)]
    pub summary: Option<String>,
    /// The tool's answer, redacted and bounded, for a caller that asks again.
    #[serde(default)]
    pub result: Value,
    /// Why it failed.
    #[serde(default)]
    pub error: Option<String>,
    /// The approval the run asked for next, if it asked for one.
    #[serde(default)]
    pub next: Option<String>,
}

/// The most of a run's answer an outcome keeps, as canonical JSON.
pub const RESULT_MAX: usize = 8 * 1024;
/// The longest summary or error an outcome keeps, in characters.
pub const LINE_MAX: usize = 500;
/// How long a run may be `running` before it is taken to have died.
pub const RUN_STALE_MS: i64 = 20 * 60 * 1000;

/// `s` cut to [`LINE_MAX`] characters.
pub fn line(s: &str) -> String {
    let mut out: String = s.chars().take(LINE_MAX).collect();
    if s.chars().count() > LINE_MAX {
        out.push('…');
    }
    out
}

/// A run's answer as an outcome keeps it: redacted, and when too long, only a note saying so.
pub fn kept_result(v: &Value) -> Value {
    let shown = redact(v);
    if canonical(&shown).len() > RESULT_MAX {
        serde_json::json!({
            "truncated": true,
            "note": "the answer was too long to keep; read the state again",
        })
    } else {
        shown
    }
}

/// `v` with object keys sorted at every depth.
fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut entries: Vec<(&String, &Value)> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, inner)| (k.clone(), sorted(inner)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// JSON with object keys sorted at every depth, no whitespace: the form a digest is taken of.
pub fn canonical(v: &Value) -> String {
    serde_json::to_string(&sorted(v)).unwrap_or_default()
}

/// The digest of a tool call: of its tool, arguments and caller.
pub fn call_digest(tool: &str, args: &Value, caller_id: &str) -> String {
    let body = serde_json::json!({ "tool": tool, "args": args, "caller": caller_id });
    crate::oauth::hash(&canonical(&body))
}

/// The digest of anything else approved: of its kind, subject and payload.
pub fn payload_digest(kind: &Kind, subject: &str, payload: &Value) -> String {
    let body = serde_json::json!({ "kind": kind.name(), "subject": subject, "payload": payload });
    crate::oauth::hash(&canonical(&body))
}

/// Whether an object key names a secret.
fn sensitive(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE.iter().any(|t| lower.contains(t))
}

/// `args` with the value of every object key, at any depth, whose name contains token, secret,
/// password, key or value replaced by [`REDACTED`]. Arrays are walked.
pub fn redact(args: &Value) -> Value {
    match args {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, inner)| {
                    let out = if sensitive(k) {
                        Value::String(REDACTED.to_string())
                    } else {
                        redact(inner)
                    };
                    (k.clone(), out)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// The passkey challenge purpose binding an approval to one record.
pub fn purpose(id: &str, digest: &str) -> String {
    format!("console:approval:{id}:{digest}")
}

/// A new pending record, its summary redacted.
#[allow(clippy::too_many_arguments)]
pub fn new(
    id: String,
    kind: Kind,
    subject: String,
    digest: String,
    summary: Value,
    payload: Value,
    caller_id: String,
    approver_id: Option<String>,
    now_ms: i64,
) -> Record {
    let expires_ms = now_ms.saturating_add(kind.ttl_ms());
    Record {
        id,
        kind,
        subject,
        digest,
        summary: redact(&summary),
        payload,
        caller_id,
        approver_id,
        state: State::Pending,
        created_ms: now_ms,
        expires_ms,
        decided_by: None,
        decided_ms: None,
        used_ms: None,
        note: None,
        outcome: None,
        after: None,
    }
}

/// Whether it is past its expiry and not yet ended.
pub fn is_stale(r: &Record, now_ms: i64) -> bool {
    matches!(r.state, State::Pending | State::Approved) && now_ms >= r.expires_ms
}

/// Marks a stale record Expired; answers whether it changed it.
pub fn expire(r: &mut Record, now_ms: i64) -> bool {
    if is_stale(r, now_ms) {
        r.state = State::Expired;
        r.note = Some("expired".to_string());
        true
    } else {
        false
    }
}

/// Why a step is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotFound(String),
    Used(String),
    Expired(String),
    Denied(String),
    NotApproved(String),
    OtherCall(String),
    NotYours(String),
    OtherCaller(String),
    /// The platform has already taken it to run.
    Claimed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotFound(id) => write!(f, "no approval {id}"),
            Refusal::Used(id) => write!(f, "approval {id} is used"),
            Refusal::Expired(id) => write!(f, "approval {id} has expired"),
            Refusal::Denied(id) => write!(f, "approval {id} was denied"),
            Refusal::NotApproved(id) => {
                write!(f, "approval {id} waits for the person's passkey")
            }
            Refusal::OtherCall(id) => write!(f, "approval {id} is for another call"),
            Refusal::NotYours(id) => write!(f, "approval {id} is not for this person"),
            Refusal::OtherCaller(id) => write!(f, "approval {id} is for another caller"),
            Refusal::Claimed(id) => write!(f, "approval {id} already ran, or runs, on approval"),
        }
    }
}

/// Refuses a record that has already ended.
fn ended(r: &Record) -> Result<(), Refusal> {
    match r.state {
        State::Used => Err(Refusal::Used(r.id.clone())),
        State::Denied => Err(Refusal::Denied(r.id.clone())),
        State::Expired => Err(Refusal::Expired(r.id.clone())),
        State::Pending | State::Approved => Ok(()),
    }
}

/// Refuses a record that has ended or is past its time, expiring it in the second case.
fn open(r: &mut Record, now_ms: i64) -> Result<(), Refusal> {
    ended(r)?;
    if expire(r, now_ms) {
        return Err(Refusal::Expired(r.id.clone()));
    }
    Ok(())
}

/// The person `person_id` approves (`approve`) or denies `r`, having signed for `digest`. Only
/// a pending record is approved, and only for its own digest; a pending or approved one may be
/// denied. Either way, only by its approver when it names one.
pub fn decide(
    r: &mut Record,
    approve: bool,
    digest: &str,
    person_id: &str,
    now_ms: i64,
) -> Result<(), Refusal> {
    open(r, now_ms)?;
    if approve {
        if r.state != State::Pending {
            return Err(Refusal::Used(r.id.clone()));
        }
        if r.digest != digest {
            return Err(Refusal::OtherCall(r.id.clone()));
        }
    }
    if r.approver_id.as_deref().is_some_and(|a| a != person_id) {
        return Err(Refusal::NotYours(r.id.clone()));
    }
    r.decided_by = Some(person_id.to_string());
    r.decided_ms = Some(now_ms);
    if approve {
        r.state = State::Approved;
        if !r.kind.applies_on_approve() {
            r.expires_ms = now_ms.saturating_add(USE_TTL_MS);
        }
    } else {
        r.state = State::Denied;
    }
    Ok(())
}

/// The caller `caller_id` uses approved record `r` for the call whose digest is `digest`: once.
pub fn take(r: &mut Record, digest: &str, caller_id: &str, now_ms: i64) -> Result<(), Refusal> {
    open(r, now_ms)?;
    if r.state == State::Pending {
        return Err(Refusal::NotApproved(r.id.clone()));
    }
    if r.caller_id != caller_id {
        return Err(Refusal::OtherCaller(r.id.clone()));
    }
    if r.digest != digest {
        return Err(Refusal::OtherCall(r.id.clone()));
    }
    r.state = State::Used;
    r.used_ms = Some(now_ms);
    Ok(())
}

/// Marks an approved instance draft Expired when the record has moved on from the version the
/// draft was made from; answers whether it did.
pub fn stale_draft(r: &mut Record, current_version: u64, draft_version: u64) -> bool {
    if r.state != State::Approved
        || !matches!(r.kind, Kind::InstanceDraft)
        || current_version == draft_version
    {
        return false;
    }
    r.state = State::Expired;
    r.note = Some("the record changed since this draft".to_string());
    true
}

/// Whether `r` is ready for the platform to run what it stands for, at `now_ms`: approved and
/// not past its time, or a draft the Directory has applied; and not claimed before.
pub fn runnable(r: &Record, now_ms: i64) -> Result<(), Refusal> {
    if r.outcome.is_some() {
        return Err(Refusal::Claimed(r.id.clone()));
    }
    if r.kind.applies_on_approve() {
        // A draft is applied when approved; what follows it runs only after an apply that held.
        return match r.state {
            State::Used if r.note.is_none() => Ok(()),
            State::Pending => Err(Refusal::NotApproved(r.id.clone())),
            State::Denied => Err(Refusal::Denied(r.id.clone())),
            _ => Err(Refusal::Expired(r.id.clone())),
        };
    }
    match r.state {
        State::Approved if !is_stale(r, now_ms) => Ok(()),
        State::Approved | State::Expired => Err(Refusal::Expired(r.id.clone())),
        State::Pending => Err(Refusal::NotApproved(r.id.clone())),
        State::Denied => Err(Refusal::Denied(r.id.clone())),
        State::Used => Err(Refusal::Used(r.id.clone())),
    }
}

/// A runner claims `r` to run `tool`, once: it must be [`runnable`]. Marks it running.
pub fn claim(r: &mut Record, tool: &str, now_ms: i64) -> Result<(), Refusal> {
    let ready = runnable(r, now_ms);
    if ready.is_err() {
        // A refusal may still expire a stale record.
        expire(r, now_ms);
    }
    ready?;
    r.outcome = Some(Outcome {
        status: RunStatus::Running,
        tool: tool.to_string(),
        started_ms: now_ms,
        finished_ms: None,
        summary: None,
        result: Value::Null,
        error: None,
        next: None,
    });
    Ok(())
}

/// Records how the claimed run of `r` ended: `Ok(answer)` or `Err(why)`, with the approval it
/// asked for next. Only a running outcome ends, and once; a tool call's arguments are cleared
/// from its payload. Answers whether it changed `r`.
pub fn finish(
    r: &mut Record,
    ended: Result<&Value, &str>,
    summary: Option<String>,
    next: Option<String>,
    now_ms: i64,
) -> bool {
    let Some(o) = r
        .outcome
        .as_mut()
        .filter(|o| o.status == RunStatus::Running)
    else {
        return false;
    };
    o.finished_ms = Some(now_ms);
    o.summary = summary.map(|s| line(&s));
    match ended {
        Ok(answer) => {
            o.status = RunStatus::Done;
            o.result = kept_result(answer);
            o.next = next;
        }
        Err(why) => {
            o.status = RunStatus::Failed;
            o.error = Some(line(why));
        }
    }
    if r.kind == Kind::ToolCall
        && let Some(p) = r.payload.as_object_mut()
    {
        p.remove("args");
    }
    true
}

/// Whether a run of `r` has been running for [`RUN_STALE_MS`] or more at `now_ms`.
pub fn run_stale(r: &Record, now_ms: i64) -> bool {
    r.outcome.as_ref().is_some_and(|o| {
        o.status == RunStatus::Running && now_ms.saturating_sub(o.started_ms) >= RUN_STALE_MS
    })
}

/// What a caller calling again with approval `r`, which the platform has claimed, is answered:
/// the run's answer, or why there is none. None when it has not been claimed, or is another
/// caller's (who is refused as before, by the tool).
pub fn recall(r: &Record, caller_id: &str) -> Option<Result<Value, String>> {
    if r.caller_id != caller_id {
        return None;
    }
    let o = r.outcome.as_ref()?;
    Some(match o.status {
        RunStatus::Done => Ok(o.result.clone()),
        RunStatus::Running => Err(format!(
            "approval {} was approved and `{}` is running now, once; `approvals_status` says \
             when it is done. Do not call it again.",
            r.id, o.tool
        )),
        RunStatus::Failed => Err(format!(
            "approval {} was approved and `{}` ran then, and failed: {}",
            r.id,
            o.tool,
            o.error.as_deref().unwrap_or("no reason recorded")
        )),
    })
}
