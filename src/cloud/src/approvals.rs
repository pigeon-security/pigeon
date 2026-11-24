//! Approvals for a plan. Core keeps them (its Directory), and a capability Worker reaches them
//! only through core's `approval` op, which fixes the kind and the approver.

use pigeon_contract::{Answer, FailureKind};
use serde_json::{Value, json};
use worker::Error;

use crate::link::Link;
use crate::transport::Fut;

/// The one approval kind the cloud gateway asks for.
pub const KIND: &str = "cloud_change";

/// A new approval, as core answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    /// The approval's ID: what `cloud_apply` takes.
    pub id: String,
    /// When it lapses, in milliseconds since the epoch.
    pub expires_ms: i64,
    /// Whether an equal pending approval already existed and is handed back.
    pub existing: bool,
}

/// An approval as core keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Its ID.
    pub id: String,
    /// Its kind; always [`KIND`] from core's table.
    pub kind: String,
    /// What it was created for.
    pub subject: String,
    /// `pending`, `approved`, `denied`, `expired` or `used`.
    pub state: String,
    /// The plan it holds.
    pub payload: Value,
    /// When it lapses, in milliseconds since the epoch.
    pub expires_ms: i64,
}

/// Creates, reads and spends approvals. Core backs it in the Worker; the smoke suite stubs it.
pub trait Approvals {
    /// Creates an approval for `payload`, shown to the person as `lines`.
    fn create<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Value,
        lines: &'a [String],
    ) -> Fut<'a, Created>;

    /// Reads approval `id`.
    fn get<'a>(&'a self, id: &'a str) -> Fut<'a, Record>;

    /// Spends approval `id` for exactly this subject and payload; an error says why not.
    fn take<'a>(&'a self, id: &'a str, subject: &'a str, payload: &'a Value) -> Fut<'a, ()>;
}

/// The approvals in core, asked over `PLATFORM`.
pub struct CoreApprovals<'a>(pub Link<'a>);

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

impl CoreApprovals<'_> {
    /// Asks core's `approval` op and answers its value.
    async fn ask(&self, body: Value) -> worker::Result<Value> {
        match self.0.ask("approval", &body).await? {
            Answer::Ok(value) => Ok(value),
            Answer::Error(f) if f.kind == FailureKind::UnknownOp => {
                Err(bad("core does not take capability approvals yet"))
            }
            Answer::Error(f) => Err(bad(format!("core refused the approval: {}", f.message))),
        }
    }
}

impl Approvals for CoreApprovals<'_> {
    fn create<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Value,
        lines: &'a [String],
    ) -> Fut<'a, Created> {
        Box::pin(async move {
            let v = self
                .ask(json!({
                    "action": "create",
                    "kind": KIND,
                    "subject": subject,
                    "payload": payload,
                    "lines": lines,
                }))
                .await?;
            Ok(Created {
                id: v["id"]
                    .as_str()
                    .ok_or_else(|| bad("core's approval has no id"))?
                    .to_string(),
                expires_ms: v["expires_ms"].as_i64().unwrap_or(0),
                existing: v["existing"].as_bool().unwrap_or(false),
            })
        })
    }

    fn get<'a>(&'a self, id: &'a str) -> Fut<'a, Record> {
        Box::pin(async move {
            let v = self.ask(json!({ "action": "get", "id": id })).await?;
            let text = |key: &str| v[key].as_str().unwrap_or_default().to_string();
            Ok(Record {
                id: text("id"),
                kind: text("kind"),
                subject: text("subject"),
                state: text("state"),
                payload: v["payload"].clone(),
                expires_ms: v["expires_ms"].as_i64().unwrap_or(0),
            })
        })
    }

    fn take<'a>(&'a self, id: &'a str, subject: &'a str, payload: &'a Value) -> Fut<'a, ()> {
        Box::pin(async move {
            let v = self
                .ask(json!({
                    "action": "take",
                    "id": id,
                    "kind": KIND,
                    "subject": subject,
                    "payload": payload,
                }))
                .await?;
            if v["taken"].as_bool() == Some(true) {
                return Ok(());
            }
            let why = v["refused"].as_str().unwrap_or("no reason given");
            Err(bad(format!("the approval was not taken: {why}")))
        })
    }
}
