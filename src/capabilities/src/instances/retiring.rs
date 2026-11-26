//! `instance_retire_plan` and `instance_retire_apply`: the reads and writes around the pure
//! [`super::retire`] plan. The plan waits for the person's passkey as a linked plan does, bound
//! to its digest; applying it runs each step in order and keeps a journal, so a run that stopped
//! resumes where it stopped.

use pigeon_core::{Account, custom_domain, r2, workers};
use serde_json::{Value, json};

use super::retire::{self, Journal, SeenBucket, SeenWorker, SiteUser, Step};
use super::{Instances, bad, keep_ledger, ledger, one, record};
use crate::users::{User, directory};
use crate::{Context, arg};

/// The approval kind a retire plan waits under: the linked plan's, marked as a retire.
const KIND: pigeon_core::approval::Kind = pigeon_core::approval::Kind::LinkedPlan;

/// How long a retire journal is kept: long enough to resume, then forgotten.
const JOURNAL_KEPT_MS: i64 = 30 * 24 * 3_600_000;

/// Whether approval `got` (as `approval.get` answers it) is a retire plan.
pub fn is_retire(got: &Value) -> bool {
    got["kind"] == KIND.name() && got["payload"]["retire"] == true
}

async fn setting(env: &worker::Env, key: &str) -> worker::Result<Option<String>> {
    Ok(
        match directory(env, "setting.get", json!({ "key": key })).await? {
            Value::String(s) if !s.is_empty() => Some(s),
            _ => None,
        },
    )
}

async fn forget(env: &worker::Env, key: &str) -> worker::Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": "", "ttl_ms": 1 }),
    )
    .await?;
    Ok(())
}

impl Instances {
    /// The linked account to read, if the instance has a link: refreshed, and checked against
    /// the account its record names.
    async fn retire_account(
        &self,
        cx: &Context<'_>,
        instance: &record::Instance,
    ) -> worker::Result<Option<Account>> {
        let record::AccountRef::Linked {
            account_id: Some(wanted),
        } = &instance.account
        else {
            return Ok(None);
        };
        if crate::linked::link(cx.env, &instance.id).await?.is_none() {
            return Ok(None);
        }
        let account = crate::linked::account_for(cx.env, &self.passkeys.host, &instance.id).await?;
        if &account.id != wanted {
            return Err(bad(&format!(
                "instance {} is linked to account {}, but its record names {wanted}",
                instance.id, account.id
            )));
        }
        Ok(Some(account))
    }

    /// What is there for `instance` now. Reads only.
    async fn retire_seen(
        &self,
        cx: &Context<'_>,
        instance: &record::Instance,
        ledger: &super::plan::Ledger,
        account: Option<&Account>,
    ) -> worker::Result<retire::Seen> {
        let env = cx.env;
        let mut seen = retire::Seen {
            account_id: account.map(|a| a.id.clone()),
            ..Default::default()
        };
        if let Some(account) = account {
            let mut names: Vec<String> = ledger.workers.iter().map(|(n, _)| n.clone()).collect();
            names.extend(instance.worker.clone());
            names.sort();
            names.dedup();
            let token = account.bearer.as_deref().unwrap_or_default();
            for name in names {
                let exists = workers::exists(account, &name).await?;
                let workers_dev = if exists {
                    let path = format!("/accounts/{}/workers/scripts/{name}/subdomain", account.id);
                    let sub =
                        pigeon_core::cloudflare::call(token, worker::Method::Get, &path, None)
                            .await?;
                    Some(sub["enabled"].as_bool() == Some(true))
                } else {
                    None
                };
                seen.workers.push(SeenWorker {
                    name,
                    exists,
                    workers_dev,
                });
            }
            let mut buckets: Vec<String> = ledger.buckets.clone();
            buckets.extend(instance.sites.iter().map(|s| s.bucket.clone()));
            buckets.sort();
            buckets.dedup();
            for name in buckets {
                let jurisdiction = instance
                    .sites
                    .iter()
                    .find(|s| s.bucket == name)
                    .or(instance.sites.first())
                    .and_then(|s| s.jurisdiction.clone());
                let b = r2::Bucket {
                    name: name.clone(),
                    jurisdiction: jurisdiction.clone(),
                };
                let exists = r2::bucket_exists(account, &b).await?;
                // Only pigeon's own bucket is listed.
                let objects = if exists && ledger.buckets.contains(&name) {
                    r2::list(account, &b, "").await?.len() as u64
                } else {
                    0
                };
                seen.buckets.push(SeenBucket {
                    name,
                    jurisdiction,
                    exists,
                    objects,
                });
            }
            // Each Domain target and each redirect host: both are custom domains of the Worker.
            let mut domain_hosts: Vec<String> = instance
                .sites
                .iter()
                .flat_map(|s| s.targets.iter())
                .filter_map(|t| match t {
                    record::Target::Domain { hostname } => Some(hostname.clone()),
                    _ => None,
                })
                .chain(
                    instance
                        .redirects
                        .iter()
                        .map(|r| r.host.to_ascii_lowercase()),
                )
                .collect();
            domain_hosts.dedup();
            for hostname in &domain_hosts {
                let service = workers::custom_domains(account, hostname)
                    .await?
                    .iter()
                    .find(|d| d["hostname"].as_str() == Some(hostname.as_str()))
                    .and_then(|d| d["service"].as_str())
                    .map(str::to_string);
                seen.domains.push(super::plan::SeenDomain {
                    hostname: hostname.clone(),
                    zone_id: None,
                    service,
                });
            }
        }
        seen.subdomains = self.observe_subdomains(cx, instance).await?;
        let hosts = retire::hosts(instance, &self.zone);
        let listed = directory(env, "users.list", json!({})).await?;
        let users: Vec<User> = serde_json::from_value(listed["users"].clone())?;
        for u in users {
            if let Some(host) = u
                .kind
                .strip_prefix("site:")
                .filter(|h| hosts.iter().any(|x| x == h))
            {
                seen.site_users.push(SiteUser {
                    id: u.id.clone(),
                    host: host.to_string(),
                });
            }
        }
        for key in retire::state_keys(&instance.id) {
            if setting(env, &key).await?.is_some() {
                seen.settings.push(key);
            }
        }
        seen.linked = crate::linked::link(env, &instance.id).await?.is_some();
        Ok(seen)
    }

    /// The retire plan for instance `id` as things are now, with its digest.
    async fn retire_planned(
        &self,
        cx: &Context<'_>,
        id: &str,
        keep_link: bool,
    ) -> worker::Result<(record::Instance, Option<String>, Vec<Step>, String)> {
        let instance = one(cx.env, id)
            .await?
            .ok_or_else(|| bad(&format!("there is no instance {id}")))?;
        if let Some(why) = retire::refusal(&instance) {
            return Err(bad(&why));
        }
        let l = ledger(cx.env, id).await?;
        let account = self.retire_account(cx, &instance).await?;
        let seen = self
            .retire_seen(cx, &instance, &l, account.as_ref())
            .await?;
        let others: Vec<record::Instance> = super::all(cx.env)
            .await?
            .into_iter()
            .filter(|o| o.id != id)
            .collect();
        let steps = retire::plan(&instance, &seen, &l, &others, keep_link);
        let digest = retire::digest(&instance, seen.account_id.as_deref(), &steps);
        Ok((instance, seen.account_id, steps, digest))
    }

    /// `instance_retire_plan`: the dry run, and an approval when nothing is in the way.
    pub(super) async fn retire_plan(
        &self,
        cx: &Context<'_>,
        args: &Value,
    ) -> worker::Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("planning needs a known caller"))?;
        let person = caller
            .acting_for
            .as_ref()
            .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
        let id = arg(args, "instance")?;
        let keep_link = args["keep_link"].as_bool().unwrap_or(false);
        let (instance, account_id, steps, digest) = self.retire_planned(cx, id, keep_link).await?;
        let lines = retire::describe(&steps);
        if steps.iter().any(Step::stops) {
            return Ok(json!({ "instance": id, "plan": lines, "applies": false,
                "note": "Something is in the way; nothing was asked or removed." }));
        }
        let made = directory(
            cx.env,
            "approval.create",
            json!({
                "kind": KIND.name(),
                "subject": id,
                "digest": digest,
                "summary": { "retire": true, "instance": id, "account_id": account_id,
                             "version": instance.version, "plan": lines },
                "payload": { "retire": true, "keep_link": keep_link, "steps": steps },
                "caller_id": caller.user.id,
                "approver_id": person.id,
            }),
        )
        .await?;
        Ok(json!({
            "instance": id,
            "plan": lines,
            "applies": true,
            "approval": made["record"]["id"],
            "waits_until_ms": made["record"]["expires_ms"],
            "note": "Nothing is removed until the person approves this plan on the console's \
                     Approvals tab; then instance_retire_apply {plan: <this approval>} runs it \
                     once, and resumes it if it stops part way.",
        }))
    }

    /// `instance_retire_apply`: the approved retire plan, if things still give the same plan;
    /// or the rest of a run that stopped.
    pub(super) async fn retire_apply(
        &self,
        cx: &Context<'_>,
        args: &Value,
    ) -> worker::Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("applying needs a known caller"))?;
        let approval_id = arg(args, "plan")?;
        let jkey = retire::journal_key(approval_id);
        let mut journal: Journal = match setting(cx.env, &jkey).await? {
            Some(text) => serde_json::from_str(&text)?,
            None => {
                let got = directory(cx.env, "approval.get", json!({ "id": approval_id })).await?;
                let id = got["subject"]
                    .as_str()
                    .filter(|_| is_retire(&got))
                    .ok_or_else(|| bad(&format!("there is no retire plan approval {approval_id}")))?
                    .to_string();
                let keep_link = got["payload"]["keep_link"].as_bool().unwrap_or(false);
                let (_, account_id, steps, digest) =
                    self.retire_planned(cx, &id, keep_link).await?;
                let taken = directory(
                    cx.env,
                    "approval.take",
                    json!({ "id": approval_id, "digest": digest, "caller_id": caller.user.id }),
                )
                .await?;
                if let Some(why) = taken["refused"].as_str() {
                    return Err(bad(&format!(
                        "the retire plan does not run: {why}. If anything changed since it was \
                         approved, plan again."
                    )));
                }
                let j = Journal::new(&id, account_id.as_deref(), &caller.user.id, &steps);
                keep_journal(cx.env, &jkey, &j).await?;
                j
            }
        };
        if journal.caller_id != caller.user.id {
            return Err(bad("only the caller the plan was approved for may run it"));
        }
        let account = if journal.needs_account() {
            let a =
                crate::linked::account_for(cx.env, &self.passkeys.host, &journal.instance).await?;
            if journal.account_id.as_deref() != Some(a.id.as_str()) {
                return Err(bad(&format!(
                    "instance {} is now linked to account {}, not the one the plan read",
                    journal.instance, a.id
                )));
            }
            Some(a)
        } else {
            None
        };
        let mut done = Vec::new();
        while let Some(i) = journal.next() {
            let step = journal.steps[i].clone();
            match self
                .retire_step(cx, &journal.instance, account.as_ref(), &step)
                .await
            {
                Ok(outcome) => {
                    journal.finish(i);
                    keep_journal(cx.env, &jkey, &journal).await?;
                    done.push(outcome);
                }
                Err(e) => {
                    journal.fail(i, &e.to_string());
                    keep_journal(cx.env, &jkey, &journal).await?;
                    return Ok(json!({
                        "instance": journal.instance, "done": done, "complete": false,
                        "failed": journal.failed,
                        "note": "Nothing after the failed step was tried. Call \
                                 instance_retire_apply again with the same plan to resume.",
                    }));
                }
            }
        }
        Ok(json!({
            "instance": journal.instance, "done": done, "complete": true,
            "note": if done.is_empty() { "This retire plan had already run." } else { "Retired." },
        }))
    }

    /// Runs one step; each is safe to run again.
    async fn retire_step(
        &self,
        cx: &Context<'_>,
        id: &str,
        account: Option<&Account>,
        step: &Step,
    ) -> worker::Result<String> {
        let env = cx.env;
        let linked = || account.ok_or_else(|| bad("the linked account is not at hand"));
        let outcome = match step {
            Step::Keep { .. } | Step::Blocked { .. } => return Ok(String::new()),
            Step::DetachSubdomain { hostname, worker } => {
                let detached = match worker {
                    Some(w) => custom_domain::detach(cx.account, hostname, w).await?,
                    None => false,
                };
                forget(env, &retire::upstream_key(hostname)).await?;
                if detached {
                    format!("{hostname} is no longer served")
                } else {
                    format!("forgot {hostname}'s upstream")
                }
            }
            Step::DetachDomain { hostname, worker } => {
                custom_domain::detach(linked()?, hostname, worker).await?;
                format!("{hostname} is no longer served by {worker}")
            }
            Step::DisableWorkersDev { name } => {
                workers::disable_workers_dev(linked()?, name).await?;
                format!("{name}'s workers.dev address is off")
            }
            Step::DeleteWorker { name } => {
                workers::delete(linked()?, name).await?;
                format!("deleted Worker {name}")
            }
            Step::EmptyBucket {
                name, jurisdiction, ..
            } => {
                let b = r2::Bucket {
                    name: name.clone(),
                    jurisdiction: jurisdiction.clone(),
                };
                let n = r2::empty_bucket(linked()?, &b).await?;
                format!("deleted {n} objects in bucket {name}")
            }
            Step::DeleteBucket { name, jurisdiction } => {
                let b = r2::Bucket {
                    name: name.clone(),
                    jurisdiction: jurisdiction.clone(),
                };
                r2::delete_bucket(linked()?, &b).await?;
                format!("deleted bucket {name}")
            }
            Step::RemoveSiteUser { id: user, host } => {
                let got: Option<User> = serde_json::from_value(
                    directory(env, "users.get", json!({ "id": user })).await?,
                )?;
                if got.is_some_and(|u| u.kind == format!("site:{host}")) {
                    directory(env, "users.remove", json!({ "id": user })).await?;
                }
                format!("removed site user {user} of {host}")
            }
            Step::Forget { key } => {
                forget(env, key).await?;
                format!("forgot {key}")
            }
            Step::Unlink => crate::linked::forget(env, &self.passkeys.host, id).await?,
            Step::ForgetLink { .. } => crate::linked::forget_link_only(env, id).await?,
            Step::RemoveRecord { id } => {
                let mut index = super::ids(env).await?;
                if index.contains(id) {
                    index.retain(|i| i != id);
                    super::put(env, super::INDEX, &serde_json::to_string(&index)?).await?;
                }
                forget(env, &super::key(id)).await?;
                format!("instance {id} is retired")
            }
        };
        // What a step removed is no longer pigeon's: keep the ledger true until it is forgotten.
        if matches!(
            step,
            Step::DeleteWorker { .. }
                | Step::DeleteBucket { .. }
                | Step::DetachDomain { .. }
                | Step::DetachSubdomain { .. }
        ) && setting(env, &super::plan::ledger_key(id)).await?.is_some()
        {
            let mut l = ledger(env, id).await?;
            retire::unrecord(&mut l, step);
            keep_ledger(env, id, &l).await?;
        }
        Ok(outcome)
    }
}

async fn keep_journal(env: &worker::Env, key: &str, j: &Journal) -> worker::Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": serde_json::to_string(j)?, "ttl_ms": JOURNAL_KEPT_MS }),
    )
    .await?;
    Ok(())
}
