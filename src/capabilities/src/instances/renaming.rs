//! `instance_rename_home`: the reads and writes around the pure [`super::rename`] plan. Its dry
//! run lists exactly what the rename would do; when there is something to do and nothing in the
//! way, the plan waits for the person's passkey as a linked plan does, bound to its digest (the
//! old id, the new id, the home record's digest and the steps). Called again with that approval,
//! it reads everything again, refuses if the plan is no longer the same, and runs it.

use serde_json::{Value, json};
use worker::{Env, Result};

use super::rename::{self, Step};
use super::{Instances, bad};
use crate::Context;
use crate::users::directory;

/// The approval kind the rename waits under: the linked plan's, marked as a rename.
const KIND: pigeon_core::approval::Kind = pigeon_core::approval::Kind::LinkedPlan;

/// Whether approval `got` (as `approval.get` or `approval.list` answers it) is a rename plan.
pub fn is_rename(got: &Value) -> bool {
    got["kind"] == KIND.name() && got["payload"]["rename"] == true
}

/// The Directory and the CI scheduler, as the rename's [`rename::Store`].
pub(super) struct Live<'a> {
    pub(super) env: &'a Env,
}

impl rename::Store for Live<'_> {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(
            match directory(self.env, "setting.get", json!({ "key": key })).await? {
                Value::String(s) if !s.is_empty() => Some(s),
                _ => None,
            },
        )
    }

    async fn put(&self, key: &str, value: &str) -> Result<()> {
        super::put(self.env, key, value).await
    }

    async fn forget(&self, key: &str) -> Result<()> {
        directory(
            self.env,
            "setting.put",
            json!({ "key": key, "value": "", "ttl_ms": 1 }),
        )
        .await?;
        Ok(())
    }

    async fn open_approvals(&self, subject: &str) -> Result<Vec<String>> {
        let listed = directory(self.env, "approval.list", json!({})).await?;
        Ok(listed["open"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["subject"] == subject && !is_rename(r))
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect())
    }

    async fn expire_approvals(&self, ids: &[String], note: &str) -> Result<()> {
        directory(
            self.env,
            "approval.expire",
            json!({ "ids": ids, "note": note }),
        )
        .await?;
        Ok(())
    }

    async fn jobs(&self, from: &str, to: &str, apply: bool) -> Result<u64> {
        pigeon_ci::rename_instance(self.env, from, to, apply).await
    }
}

impl Instances {
    /// The rename plan as things are now: the steps, the payload the approval binds and its
    /// digest.
    async fn rename_planned(&self, cx: &Context<'_>) -> Result<(Vec<Step>, Value, String)> {
        let (from, to) = (rename::FROM, rename::TO);
        let seen = rename::observe(&Live { env: cx.env }, from, to).await?;
        let steps = rename::plan(&seen, from, to);
        let payload = rename::payload(from, to, &rename::record_digest(&seen), &steps);
        let digest = pigeon_core::approval::payload_digest(&KIND, from, &payload);
        Ok((steps, payload, digest))
    }

    /// `instance_rename_home`: with `plan`, runs the approved rename; else the dry run, and,
    /// unless `dry_run`, an approval when there is something to do and nothing in the way.
    pub(super) async fn rename_home(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        if let Some(approval) = args["plan"].as_str() {
            return self.rename_apply(cx, approval).await;
        }
        let (from, to) = (rename::FROM, rename::TO);
        let (steps, payload, digest) = self.rename_planned(cx).await?;
        let lines = rename::describe(&steps);
        let base = json!({ "from": from, "to": to, "plan": lines, "digest": digest });
        let with = |mut v: Value, extra: Value| {
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            v
        };
        if steps.iter().any(Step::stops) {
            return Ok(with(
                base,
                json!({ "applies": false,
                "note": "Something is in the way; nothing was asked or changed." }),
            ));
        }
        if steps.is_empty() {
            return Ok(with(
                base,
                json!({ "applies": false,
                "note": format!("The home instance is already {to}; nothing to do.") }),
            ));
        }
        if args["dry_run"].as_bool() == Some(true) {
            return Ok(with(
                base,
                json!({ "applies": true,
                "note": "Dry run: nothing was asked or changed. Call again without dry_run to \
                         ask the person's passkey for exactly this plan." }),
            ));
        }
        let caller = cx
            .caller
            .ok_or_else(|| bad("planning needs a known caller"))?;
        let person = caller
            .acting_for
            .as_ref()
            .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
        let made = directory(
            cx.env,
            "approval.create",
            json!({
                "kind": KIND.name(),
                "subject": from,
                "digest": digest,
                "summary": { "rename": true, "instance": from, "to": to,
                             "record_digest": payload["record_digest"], "plan": lines },
                "payload": payload,
                "caller_id": caller.user.id,
                "approver_id": person.id,
            }),
        )
        .await?;
        Ok(with(
            base,
            json!({
                "applies": true,
                "approval": made["record"]["id"],
                "waits_until_ms": made["record"]["expires_ms"],
                "note": format!(
                    "Nothing changes until the person approves this plan on the console's \
                     Approvals tab; then {} {{plan: <this approval>}} runs it once.",
                    rename::TOOL
                ),
            }),
        ))
    }

    /// The approved rename `approval_id`, if things still give the same plan.
    async fn rename_apply(&self, cx: &Context<'_>, approval_id: &str) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("applying needs a known caller"))?;
        let got = directory(cx.env, "approval.get", json!({ "id": approval_id })).await?;
        if !is_rename(&got) || got["subject"] != rename::FROM {
            return Err(bad(&format!(
                "there is no rename plan approval {approval_id}"
            )));
        }
        let (steps, payload, digest) = self.rename_planned(cx).await?;
        if payload["record_digest"] != got["payload"]["record_digest"] {
            return Err(bad(
                "the home record changed since this rename was planned; nothing was changed. \
                 Plan again.",
            ));
        }
        if steps.iter().any(Step::stops) {
            return Err(bad(&format!(
                "the rename does not run: {}. Nothing was changed.",
                rename::describe(&steps)
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let taken = directory(
            cx.env,
            "approval.take",
            json!({ "id": approval_id, "digest": digest, "caller_id": caller.user.id }),
        )
        .await?;
        if let Some(why) = taken["refused"].as_str() {
            return Err(bad(&format!(
                "the rename does not run: {why}. If anything changed since it was approved, \
                 plan again."
            )));
        }
        let now = worker::js_sys::Date::now() as i64;
        let done = match rename::run(&Live { env: cx.env }, &steps, approval_id, now).await {
            Ok(done) => done,
            Err(e) => {
                return Err(bad(&format!(
                    "the rename stopped: {e}. Each step is safe to run again: call {} again \
                     to plan what remains.",
                    rename::TOOL
                )));
            }
        };
        Ok(json!({
            "from": rename::FROM,
            "to": rename::TO,
            "done": done,
            "complete": true,
            "note": "Renamed. Old console URLs, CI reads and MCP calls naming the old id are \
                     answered for the new one for 30 days.",
        }))
    }
}
