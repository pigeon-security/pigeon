//! The wire types: what core and capability Workers say to each other.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The contract version this build speaks.
pub const CONTRACT: u32 = 1;

/// The ops a capability Worker answers; the op is the request path.
pub const OPS: [&str; 6] = ["manifest", "call", "section", "fetch", "events", "secret"];

/// The ops core answers for capability Workers. `verify`, `grant` and `deploy_grant` are the
/// proxy Worker's checks: each carries the credential it asks about, so it is signed with
/// `CALLER_KEY` but for no caller. `credential_revoke` takes a [`CredentialRequest`] and ends a
/// run's git grant; it is signed for the run's caller. `credential_release` takes a
/// [`CredentialRequest`] of kind `scoped` and revokes every shared token leased to the signed
/// caller, when its last live run ends. `approval` takes an [`ApprovalRequest`]: a capability
/// Worker plans a change and the person approves it; it is signed for the caller, and core fixes
/// the kinds and the approver.
pub const CORE_OPS: [&str; 10] = [
    "credential",
    "credential_revoke",
    "credential_release",
    "ci_scope",
    "dispatch",
    "verify",
    "grant",
    "deploy_grant",
    "service_user",
    "approval",
];

/// The core ops a request signed for no caller may ask: the proxy Worker's checks, which carry
/// the credential they ask about.
pub const CALLERLESS_OPS: [&str; 3] = ["verify", "grant", "deploy_grant"];

/// Whether a peer speaking `version` is understood: this version or the one before it.
pub fn accepts(version: u32) -> bool {
    (CONTRACT.saturating_sub(1)..=CONTRACT).contains(&version)
}

/// What a Worker offers: the contract it speaks, its build and its capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The contract version the Worker speaks.
    pub contract: u32,
    /// The Worker's script name, such as `pigeon-sites`.
    pub worker: String,
    /// The commit the Worker was built from.
    #[serde(default)]
    pub commit: String,
    /// The capabilities the Worker hosts.
    #[serde(default)]
    pub capabilities: Vec<CapabilityManifest>,
}

/// One capability: its tools, sections, routes, footprint, secrets and the events it wants.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityManifest {
    /// The capability's name, such as `instances`.
    pub name: String,
    /// The MCP tools it offers.
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    /// The console sections it fills.
    #[serde(default)]
    pub sections: Vec<SectionSpec>,
    /// The path prefixes core forwards to it, such as `/sites/`.
    #[serde(default)]
    pub routes: Vec<String>,
    /// Its footprint, as the architecture page reads it; null when none.
    #[serde(default)]
    pub footprint: Value,
    /// The secrets it needs set.
    #[serde(default)]
    pub secrets: Vec<SlotSpec>,
    /// Event patterns: an object whose every key equals the event's same key ([`wants`]).
    #[serde(default)]
    pub wants: Vec<Value>,
    /// Its versions and those of the parts it carries, each with its changelog
    /// (docs/plans/versioning.md). Empty from a Worker built before versions began.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<pigeon_core::versions::Component>,
}

/// An MCP tool a capability offers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The tool's name.
    pub name: String,
    /// What the tool does.
    #[serde(default)]
    pub description: String,
    /// The JSON Schema of the tool's arguments.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

/// A console section a capability fills.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SectionSpec {
    /// The tab it sits on, such as `instances`.
    pub tab: String,
    /// Its anchor.
    pub id: String,
    /// Its title.
    #[serde(default)]
    pub title: String,
    /// One line under the title.
    #[serde(default)]
    pub description: String,
    /// Its place within the tab, smallest first.
    #[serde(default)]
    pub order: u16,
    /// The name it adds to the overview's total cost under, when it reports a cost.
    #[serde(default)]
    pub cost: Option<String>,
}

/// A secret a capability needs set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotSpec {
    /// The secret's name in the Secrets Store.
    pub name: String,
    /// What it is and where to get one.
    #[serde(default)]
    pub purpose: String,
    /// The services that may read it, such as `workers`.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// A request to run a tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallRequest {
    /// The capability the tool belongs to, when core names it.
    #[serde(default)]
    pub capability: Option<String>,
    /// The tool to run.
    pub tool: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Value,
}

/// A request for a console section's data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SectionRequest {
    /// The capability the section belongs to.
    pub capability: String,
    /// The section's anchor.
    pub id: String,
    /// The section's parameters, such as `days`.
    #[serde(default)]
    pub query: Value,
}

/// A batch of queue messages for the capabilities that want them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventsRequest {
    /// The messages' bodies.
    #[serde(default)]
    pub messages: Vec<Value>,
}

/// What to do with a capability's secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretOp {
    /// Check a value would work before it is written.
    Check,
    /// Act on the secret having just been written.
    Set,
    /// When the secret stops working, as its issuer says.
    Expiry,
}

/// A request about one of a capability's secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretRequest {
    /// The capability that needs the secret.
    pub capability: String,
    /// What to do.
    pub op: SecretOp,
    /// The secret's name.
    pub name: String,
    /// The value to check, for [`SecretOp::Check`].
    #[serde(default)]
    pub value: Option<String>,
    /// The Secrets Store secret's id, for [`SecretOp::Set`].
    #[serde(default)]
    pub id: Option<String>,
}

/// A capability Worker's request to core for a short-lived credential.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CredentialRequest {
    /// The kind of credential, such as `linked`.
    pub kind: String,
    /// The instance it is for, when the kind needs one.
    #[serde(default)]
    pub instance: Option<String>,
    /// The scopes wanted.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// A capability Worker's request to core's `approval` op: plan a change for the person's
/// passkey, read a plan's approval, or spend it. Only kinds in core's fixed table are served.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// `create`, `get` or `take`.
    pub action: String,
    /// The approval's id, for `get` and `take`.
    #[serde(default)]
    pub id: Option<String>,
    /// The approval's kind, for `create` and `take`, such as `cloud_change`.
    #[serde(default)]
    pub kind: Option<String>,
    /// What the approval is for, in a line, for `create` and `take`.
    #[serde(default)]
    pub subject: Option<String>,
    /// The plan the approval holds, for `create` and `take`. Core takes its digest itself.
    #[serde(default)]
    pub payload: Value,
    /// The plain lines the person reads before approving, for `create`.
    #[serde(default)]
    pub lines: Vec<String>,
}

/// The proxy Worker's `verify` request: who holds a proxy credential. Answered with the holder
/// (`{id, label, person, script}`) or null.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerifyRequest {
    /// The credential as presented, `pgn_px_…`.
    pub credential: String,
}

/// The proxy Worker's `grant` and `deploy_grant` requests: a CI job token's grant, or the
/// caller it stands for as a landing's deploy job, for one repository. Answered with it or
/// null.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GrantRequest {
    /// The job token as presented, `pgn_ci_…`.
    pub token: String,
    /// The repository, as `namespace/name`.
    pub repo: String,
}

/// A credential core hands a capability Worker for the call in hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credential {
    /// The bearer token.
    pub token: String,
    /// The account the token acts on.
    pub account_id: String,
    /// When the token expires, in milliseconds since the epoch.
    pub expires_at: i64,
    /// The id of the token core minted, for a `scoped` credential: what `credential_revoke`
    /// takes to end it. Absent for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
}

/// An answer: `{"ok": value}` or `{"error": {"message": .., "kind": ..}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// The op worked; this is its result.
    Ok(Value),
    /// The op did not work.
    Error(Failure),
}

impl Answer {
    /// An error answer of `kind` with `message`.
    pub fn error(kind: FailureKind, message: impl Into<String>) -> Self {
        Answer::Error(Failure {
            message: message.into(),
            kind,
            reason: None,
        })
    }

    /// An `unauthorized` answer with `message` and a short machine-readable `reason`.
    pub fn refused(reason: &str, message: impl Into<String>) -> Self {
        Answer::Error(Failure {
            message: message.into(),
            kind: FailureKind::Unauthorized,
            reason: Some(reason.to_string()),
        })
    }
}

/// Why an op did not work.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    /// A short plain message.
    #[serde(default)]
    pub message: String,
    /// The kind of failure.
    #[serde(default)]
    pub kind: FailureKind,
    /// A short machine-readable reason, such as `missing_authorization`; never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The kinds of failure; a kind from a newer peer reads as `Failed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The peer does not serve this op: an older peer, as core reads it.
    UnknownOp,
    /// The peer has no such tool.
    UnknownTool,
    /// The caller was refused.
    Unauthorized,
    /// The request was malformed.
    BadRequest,
    /// The op ran and failed, or the kind is not known here.
    #[default]
    #[serde(other)]
    Failed,
}

/// Whether `event` matches `pattern`: every key of the pattern object equals the event's. A
/// pattern that is not an object matches nothing; an empty object matches everything.
pub fn wants(pattern: &Value, event: &Value) -> bool {
    match pattern.as_object() {
        Some(map) => map.iter().all(|(key, value)| event.get(key) == Some(value)),
        None => false,
    }
}
