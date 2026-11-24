//! The provider trait: what a cloud provider offers, in terms the tools share, and the plan a
//! change becomes. A plan is data. It is what the person approves and what `cloud_apply` checks
//! the provider against again.

use pigeon_contract::SlotSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approvals::Approvals;
use crate::sink::Sink;
use crate::slots::Slots;
use crate::transport::{Fut, Transport};

/// Everything a tool needs from outside: the network, the secret slots, the approvals, the
/// secrets sink, the clock and the caller. The Worker fills it from its environment and the
/// smoke suite from stubs.
pub struct Ports<'a> {
    /// Sends HTTP requests to the provider.
    pub transport: &'a dyn Transport,
    /// Reads the provider's secret slots.
    pub slots: &'a dyn Slots,
    /// Creates, reads and spends approvals.
    pub approvals: &'a dyn Approvals,
    /// Writes a new key's secret.
    pub sink: &'a dyn Sink,
    /// The time now, in milliseconds since the epoch.
    pub now_ms: i64,
    /// The caller as core serializes it.
    pub caller: &'a Value,
}

/// What a change does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Makes something new.
    Create,
    /// Removes something.
    Delete,
}

impl Action {
    /// Its name as the plan states it.
    pub fn name(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Delete => "delete",
        }
    }
}

/// A change, planned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// The provider's name.
    pub provider: String,
    /// The kind of resource.
    pub kind: String,
    /// What the change does.
    pub action: Action,
    /// The normalised arguments.
    pub args: Value,
    /// The state read when planning; it must still hold when the plan is applied.
    pub before: Value,
    /// What the person reads before approving.
    #[serde(default)]
    pub lines: Vec<String>,
}

/// The part of a plan the approval holds: everything but the lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanPayload {
    /// The provider's name.
    pub provider: String,
    /// The kind of resource.
    pub kind: String,
    /// What the change does.
    pub action: Action,
    /// The normalised arguments.
    pub args: Value,
    /// The state read when planning.
    pub before: Value,
}

impl Plan {
    /// The approval's payload.
    pub fn payload(&self) -> PlanPayload {
        PlanPayload {
            provider: self.provider.clone(),
            kind: self.kind.clone(),
            action: self.action,
            args: self.args.clone(),
            before: self.before.clone(),
        }
    }
}

impl PlanPayload {
    /// The approval's subject: what the change is, in a line.
    pub fn subject(&self) -> String {
        format!(
            "cloud:{}:{}:{}",
            self.provider,
            self.kind,
            self.action.name()
        )
    }

    /// The payload as JSON, as the approval stores it.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// A cloud provider.
pub trait Provider {
    /// Its name, such as `scaleway`.
    fn name(&self) -> &'static str;

    /// The secret slots it needs.
    fn slots(&self) -> Vec<SlotSpec>;

    /// The kinds of resource it lists (and plans changes to, where it can).
    fn kinds(&self) -> &'static [&'static str];

    /// Whether it can be used now: an error says what to set or bind.
    fn ready<'a>(&'a self, ports: &'a Ports<'a>) -> Fut<'a, ()>;

    /// Its status: ready or why not, each slot bound and set (never a value), its hosts and,
    /// when ready, a live check.
    fn status<'a>(&'a self, ports: &'a Ports<'a>) -> Fut<'a, Value>;

    /// Lists resources of `kind`.
    fn list<'a>(&'a self, ports: &'a Ports<'a>, kind: &'a str, args: &'a Value) -> Fut<'a, Value>;

    /// Plans a change to `kind`: reads the provider, checks the arguments and changes nothing.
    fn plan<'a>(
        &'a self,
        ports: &'a Ports<'a>,
        kind: &'a str,
        action: Action,
        args: &'a Value,
    ) -> Fut<'a, Plan>;

    /// Makes the change `plan` describes, reads the provider again and answers what was done
    /// with any `differences` from what the plan expected.
    fn apply<'a>(&'a self, ports: &'a Ports<'a>, plan: &'a Plan) -> Fut<'a, Value>;
}

/// A value as a short line for a message.
fn shown(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() > 80 {
        format!("{}…", text.chars().take(80).collect::<String>())
    } else {
        text
    }
}

/// How `actual` differs from `expected`, one line for each path that does: where it is, what
/// was expected, what is there.
pub fn differences(expected: &Value, actual: &Value) -> Vec<String> {
    let mut out = Vec::new();
    diff("", expected, actual, &mut out);
    out
}

fn diff(path: &str, expected: &Value, actual: &Value, out: &mut Vec<String>) {
    let name = if path.is_empty() { "the plan" } else { path };
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            let mut keys: Vec<&String> = e.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let inner = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                diff(
                    &inner,
                    e.get(key).unwrap_or(&Value::Null),
                    a.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (e, a) if e == a => {}
        (e, a) => out.push(format!("{name}: expected {}, found {}", shown(e), shown(a))),
    }
}
