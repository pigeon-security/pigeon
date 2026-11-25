//! Instances: named units of what the platform runs, here or in a Cloudflare account a person
//! linked. Code declares only their shape ([`record`]); every value an instance holds (its
//! account, zones, sites, buckets, Workers, routes, subdomains and repository) is data, kept in
//! the Directory. A record changes only with the signed-in person's passkey, bound to the hash of
//! the exact record they approve and to the version they edited, so pigeon touches only what a
//! person named. The console's Instances tab shows every instance and edits one; over MCP they
//! can only be listed.

pub mod plan;
mod publishing;
pub mod record;
pub mod tools;

use pigeon_core::mcp::Tool;
use pigeon_core::oauth::hash;
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::console::sections::{Section, Tab};
use crate::passkeys::Passkeys;
use crate::users::{User, directory};
use crate::{Call, Capability, Context, Serve, arg, to_value, unknown};
use record::{Instance, Rules};

/// The Worker that serves instances' platform subdomains, by host.
pub const SITES_WORKER: &str = "pigeon-sites";

/// The Directory key holding the list of instance ids.
pub const INDEX: &str = "instances:index";
/// How long a record is kept: for good, in practice (a century).
pub const KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;
/// The largest record accepted, in bytes.
const MAX_RECORD: usize = 32 * 1024;

/// The instances capability.
pub struct Instances {
    /// The platform's own zone, under which subdomain targets are served.
    pub zone: String,
    pub passkeys: Passkeys,
}

/// The Directory key of instance `id`'s record.
pub fn key(id: &str) -> String {
    format!("instance:{id}")
}

/// The challenge purpose binding an approval to record `id` with text hash `digest`.
pub fn write_purpose(id: &str, digest: &str) -> String {
    format!("instance:{id}:{digest}")
}

/// A record to start from on the console, with every kind of target.
pub fn template() -> Value {
    json!({
        "id": "example",
        "title": "example",
        "account": { "kind": "linked", "account_id": null },
        "zones": ["example.com"],
        "repo": { "kind": "github", "full_name": "owner/repo", "installation": null },
        "worker": "example",
        "sites": [{
            "name": "blog",
            "bucket": "example-site",
            "jurisdiction": "eu",
            "build": "workloads/example.com/blog",
            "targets": [
                { "kind": "preview" },
                { "kind": "zone", "hostname": "example.com", "route": "example.com/*", "enabled": false },
                { "kind": "subdomain", "label": "example" }
            ]
        }],
        "version": 0
    })
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// Every instance's record, in id order.
pub async fn all(env: &Env) -> Result<Vec<Instance>> {
    let mut out = Vec::new();
    for id in ids(env).await? {
        if let Some(i) = one(env, &id).await? {
            out.push(i);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Instance `id`'s record, if there is one.
pub async fn one(env: &Env, id: &str) -> Result<Option<Instance>> {
    match directory(env, "setting.get", json!({ "key": key(id) })).await? {
        Value::String(text) => Ok(Some(serde_json::from_str(&text)?)),
        _ => Ok(None),
    }
}

async fn ids(env: &Env) -> Result<Vec<String>> {
    match directory(env, "setting.get", json!({ "key": INDEX })).await? {
        Value::String(text) => Ok(serde_json::from_str(&text)?),
        _ => Ok(Vec::new()),
    }
}

async fn put(env: &Env, key: &str, value: &str) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": value, "ttl_ms": KEPT_MS }),
    )
    .await?;
    Ok(())
}

impl Instances {
    /// The labels a subdomain may not take: the platform's own hostnames under its zone.
    fn reserved(&self, cx: &Context<'_>) -> Vec<String> {
        let hosts: Vec<String> = cx
            .hosted
            .capabilities
            .iter()
            .flat_map(|c| c.footprint().hostnames.into_iter().map(|h| h.host))
            .collect();
        record::reserved_labels(&hosts, &self.zone)
    }

    /// The Worker names an instance may not take: the hosting Worker's and every Worker a
    /// capability declares.
    fn workers(&self, cx: &Context<'_>) -> Vec<String> {
        let mut names = vec![cx.hosted.worker.to_string()];
        for c in cx.hosted.capabilities {
            let f = c.footprint();
            names.extend(f.workers.into_iter().map(|w| w.name));
            names.extend(
                f.hostnames
                    .into_iter()
                    .map(|h| h.worker)
                    .filter(|w| !w.is_empty()),
            );
        }
        names.sort();
        names.dedup();
        names
    }

    /// Parses `text` and checks it against every other instance: the record, or the problems.
    async fn check(
        &self,
        cx: &Context<'_>,
        text: &str,
    ) -> Result<std::result::Result<(Instance, Option<Instance>), Vec<String>>> {
        if text.len() > MAX_RECORD {
            return Ok(Err(vec!["the record is larger than 32 KiB".into()]));
        }
        let record: Instance = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(e) => return Ok(Err(vec![format!("the record is not an instance: {e}")])),
        };
        let everyone = all(cx.env).await?;
        let current = everyone.iter().find(|i| i.id == record.id).cloned();
        let others: Vec<Instance> = everyone.into_iter().filter(|i| i.id != record.id).collect();
        let reserved = self.reserved(cx);
        let workers = self.workers(cx);
        let rules = Rules {
            platform_zone: &self.zone,
            reserved_labels: &reserved,
            reserved_workers: &workers,
        };
        let mut problems = record::validate(&record, &others, &rules);
        let carried: Vec<pigeon_core::versions::Component> = cx
            .hosted
            .capabilities
            .iter()
            .flat_map(|c| c.versions())
            .collect();
        problems.extend(pigeon_core::versions::pin_problems(&record.pins, &carried));
        let at = current.as_ref().map_or(0, |c| c.version);
        if record.version != at {
            problems.push(format!(
                "this record was edited from version {}, but the instance is at version {at}: reload and edit again",
                record.version
            ));
        }
        Ok(if problems.is_empty() {
            Ok((record, current))
        } else {
            Err(problems)
        })
    }

    /// Validates a record and starts its approval: the problems, or the passkey options with the
    /// changes it makes. `digest` is the base64url SHA-256 of `record` as sent.
    async fn put_begin(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let text = arg(body, "record")?;
        let digest = arg(body, "digest")?;
        if hash(text) != digest {
            return Err(bad("the digest is not the record's"));
        }
        let (record, current) = match self.check(cx, text).await? {
            Ok(pair) => pair,
            Err(errors) => return Ok(json!({ "errors": errors })),
        };
        let changes = record::diff(current.as_ref(), &record);
        if changes.is_empty() {
            return Ok(json!({ "errors": ["nothing changes"] }));
        }
        let mut begin = self
            .passkeys
            .sign_in_begin(cx.env, &write_purpose(&record.id, digest), None)
            .await?;
        begin["changes"] = to_value(changes);
        Ok(begin)
    }

    /// Checks the passkey approved this record, checks it again, and stores it a version on.
    async fn put_finish(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let text = arg(body, "record")?;
        let (mut record, _) = match self.check(cx, text).await? {
            Ok(pair) => pair,
            Err(errors) => return Err(bad(&errors.join("; "))),
        };
        let purpose = write_purpose(&record.id, &hash(text));
        let key_used = self
            .passkeys
            .sign_in_finish(cx.env, &purpose, body, None)
            .await?;
        let person: Option<User> = serde_json::from_value(
            directory(cx.env, "users.get", json!({ "id": key_used.user_id })).await?,
        )?;
        let person = person
            .filter(|u| u.kind == "standard")
            .ok_or_else(|| bad("only a person can change an instance"))?;
        record.version += 1;
        put(cx.env, &key(&record.id), &serde_json::to_string(&record)?).await?;
        let mut index = ids(cx.env).await?;
        if !index.contains(&record.id) {
            index.push(record.id.clone());
            index.sort();
            put(cx.env, INDEX, &serde_json::to_string(&index)?).await?;
        }
        Ok(json!({
            "message": format!("{} is at version {}, approved by {}.", record.id, record.version, person.name)
        }))
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        if req.method() != Method::Post {
            return Response::error("not found", 404);
        }
        // A console write: from the console's own page, by the person signed in to it.
        let host = req.url()?.host_str().unwrap_or_default().to_string();
        let origin = req.headers().get("Origin")?;
        if !crate::console::session::same_origin(origin.as_deref(), &host) {
            return Response::error("forbidden", 403);
        }
        if crate::console::signed_person(cx.env, &req).await?.is_none() {
            return Response::error("sign in to the console first", 401);
        }
        let path = req.path();
        let body: Value = req.json().await.unwrap_or(Value::Null);
        let answer = match path.as_str() {
            "/instance/put/begin" => self.put_begin(cx, &body).await,
            "/instance/put/finish" => self.put_finish(cx, &body).await,
            _ => return Response::error("not found", 404),
        };
        let headers = Headers::new();
        headers.set("Cache-Control", "no-store")?;
        match answer {
            Ok(v) => Ok(Response::from_json(&v)?.with_headers(headers)),
            Err(e) => Ok(Response::from_json(&json!({ "error": e.to_string() }))?
                .with_status(400)
                .with_headers(headers)),
        }
    }

    /// An agent's proposal: checked as an edit on the console is, then kept as an instance
    /// draft for the person the caller acts for to approve. Changes nothing itself.
    async fn propose(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("proposing an instance needs a known caller"))?;
        let person = caller
            .acting_for
            .as_ref()
            .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
        let text = serde_json::to_string(&args["record"])?;
        let (record, current) = match self.check(cx, &text).await? {
            Ok(pair) => pair,
            Err(problems) => return Ok(json!({ "problems": problems })),
        };
        let changes = record::diff(current.as_ref(), &record);
        if changes.is_empty() {
            return Ok(json!({ "problems": ["nothing changes"] }));
        }
        let base = current.as_ref().map_or(0, |c| c.version);
        let payload = json!({ "record": record, "base_version": base });
        let kind = pigeon_core::approval::Kind::InstanceDraft;
        let digest = pigeon_core::approval::payload_digest(&kind, &record.id, &payload);
        let made = directory(
            cx.env,
            "approval.create",
            json!({
                "kind": kind.name(),
                "subject": record.id,
                "digest": digest,
                "summary": { "instance": record.id, "version_from": base, "changes": changes },
                "payload": payload,
                "caller_id": caller.user.id,
                "approver_id": person.id,
            }),
        )
        .await?;
        Ok(json!({
            "approval": made["record"]["id"],
            "waits_until_ms": made["record"]["expires_ms"],
            "changes": changes,
            "note": "Nothing changes until the person approves this on the console's Approvals tab.",
        }))
    }

    /// Every instance as the console shows it: its record and its link's state.
    async fn listed(&self, env: &Env) -> Result<Value> {
        let mut instances: Vec<Value> = Vec::new();
        for i in all(env).await? {
            let link = match i.account {
                record::AccountRef::Home => json!({ "status": "this account" }),
                record::AccountRef::Linked { .. } => {
                    let l = crate::linked::link(env, &i.id).await?;
                    crate::linked::status(l.as_ref(), worker::js_sys::Date::now())
                }
            };
            let mut v = to_value(i);
            v["link"] = link;
            instances.push(v);
        }
        Ok(json!({ "zone": self.zone, "instances": instances, "template": template() }))
    }
}

/// The Instances section of the console's Instances tab.
pub fn instances_section() -> Section {
    Section {
        tab: Tab::Instances,
        id: "instances",
        title: "Instances",
        description: "What pigeon runs, here and in accounts you linked. Each is data: changing one asks for your passkey.",
        order: 1,
        cost: None,
    }
}

/// `instance_subdomains`: each Subdomain target whose plan was applied, with its upstream.
async fn subdomains(env: &Env, zone: &str) -> Result<Value> {
    let mut out = Vec::new();
    for i in all(env).await? {
        for (site, t) in i
            .sites
            .iter()
            .flat_map(|s| s.targets.iter().map(move |t| (s, t)))
        {
            let record::Target::Subdomain { label } = t else {
                continue;
            };
            let host = format!("{label}.{zone}");
            let key = plan::subdomain_key(&host);
            if let Value::String(upstream) =
                directory(env, "setting.get", json!({ "key": key })).await?
            {
                let mut entry = json!({ "host": host, "instance": i.id, "upstream": upstream });
                if let Some(auth) = &site.auth {
                    entry["auth"] = json!(auth);
                    entry["allow"] = json!(site.allow);
                }
                out.push(entry);
            }
        }
    }
    Ok(Value::Array(out))
}

/// What an instance's account holds now, for [`plan::plan`]. Reads only.
async fn observe(account: &pigeon_core::Account, instance: &Instance) -> Result<plan::Observed> {
    use pigeon_core::cloudflare::call;
    use pigeon_core::{r2, workers};
    let site = instance.sites.first();
    let bucket_exists = match site {
        Some(s) => {
            r2::bucket_exists(
                account,
                &r2::Bucket {
                    name: s.bucket.clone(),
                    jurisdiction: s.jurisdiction.clone(),
                },
            )
            .await?
        }
        None => false,
    };
    let token = account
        .bearer
        .as_deref()
        .ok_or_else(|| bad("the instance's account is not a linked one"))?;
    let (worker_exists, workers_dev) = match instance.worker.as_deref() {
        Some(w) if workers::exists(account, w).await? => {
            let path = format!("/accounts/{}/workers/scripts/{w}/subdomain", account.id);
            let sub = call(token, Method::Get, &path, None).await?;
            (true, Some(sub["enabled"].as_bool() == Some(true)))
        }
        _ => (false, None),
    };
    let path = format!("/accounts/{}/workers/subdomain", account.id);
    let account_subdomain = match call(token, Method::Get, &path, None).await {
        Ok(v) => v["subdomain"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Err(e) if workers::is_not_found(&e.to_string()) || e.to_string().contains(": 404:") => None,
        Err(e) => return Err(e),
    };
    let mut domains = Vec::new();
    for t in site.map(|s| s.targets.as_slice()).unwrap_or_default() {
        let record::Target::Domain { hostname } = t else {
            continue;
        };
        let zone = instance
            .zones
            .iter()
            .filter(|z| hostname == *z || hostname.ends_with(&format!(".{z}")))
            .max_by_key(|z| z.len());
        let zone_id = match zone {
            Some(z) => {
                let path = format!("/zones?account.id={}&name={z}", account.id);
                let found = call(token, Method::Get, &path, None).await?;
                found
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|z| z["id"].as_str())
                    .map(str::to_string)
            }
            None => None,
        };
        let attached = workers::custom_domains(account, hostname).await?;
        let service = attached
            .iter()
            .find(|d| d["hostname"].as_str() == Some(hostname))
            .and_then(|d| d["service"].as_str())
            .map(str::to_string);
        domains.push(plan::SeenDomain {
            hostname: hostname.clone(),
            zone_id,
            service,
        });
    }
    Ok(plan::Observed {
        account_id: account.id.clone(),
        bucket_exists,
        worker_exists,
        workers_dev,
        account_subdomain,
        domains,
        subdomains: Vec::new(),
    })
}

async fn ledger(env: &Env, id: &str) -> Result<plan::Ledger> {
    match directory(env, "setting.get", json!({ "key": plan::ledger_key(id) })).await? {
        Value::String(text) => Ok(serde_json::from_str(&text)?),
        _ => Ok(plan::Ledger::default()),
    }
}

async fn keep_ledger(env: &Env, id: &str, l: &plan::Ledger) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": plan::ledger_key(id), "value": serde_json::to_string(l)?, "ttl_ms": KEPT_MS }),
    )
    .await?;
    Ok(())
}

impl Instances {
    /// A linked instance, its account (refreshed), what is there and the plan for it.
    async fn planned(
        &self,
        cx: &Context<'_>,
        id: &str,
    ) -> Result<(Instance, pigeon_core::Account, Vec<plan::Step>, String)> {
        let env = cx.env;
        let instance = one(env, id)
            .await?
            .ok_or_else(|| bad(&format!("there is no instance {id}")))?;
        let wanted = match &instance.account {
            record::AccountRef::Linked {
                account_id: Some(a),
            } => a.clone(),
            record::AccountRef::Linked { account_id: None } => {
                return Err(bad(&format!(
                    "instance {id} names no account yet: link it, then set account.account_id"
                )));
            }
            record::AccountRef::Home => {
                return Err(bad(&format!(
                    "instance {id} runs in the platform's own account; plans are for linked ones"
                )));
            }
        };
        let account = crate::linked::account_for(env, &self.passkeys.host, id).await?;
        if account.id != wanted {
            return Err(bad(&format!(
                "instance {id} is linked to account {}, but its record names {wanted}",
                account.id
            )));
        }
        let mut seen = observe(&account, &instance).await?;
        seen.subdomains = self.observe_subdomains(cx, &instance).await?;
        let steps = plan::plan(&instance, &seen, &ledger(env, id).await?);
        let digest = plan::digest(&instance, &account.id, &steps);
        Ok((instance, account, steps, digest))
    }

    /// Each Subdomain target as the platform's own account has it. Reads only.
    async fn observe_subdomains(
        &self,
        cx: &Context<'_>,
        instance: &Instance,
    ) -> Result<Vec<plan::SeenSubdomain>> {
        let labels: Vec<&str> = instance
            .sites
            .iter()
            .flat_map(|s| s.targets.iter())
            .filter_map(|t| match t {
                record::Target::Subdomain { label } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let zone_id = pigeon_core::zones::active(cx.account, &self.zone)
            .await
            .ok()
            .map(|z| z.id);
        let mut out = Vec::new();
        for label in labels {
            let hostname = format!("{label}.{}", self.zone);
            let service = pigeon_core::workers::custom_domains(cx.account, &hostname)
                .await?
                .iter()
                .find(|d| d["hostname"].as_str() == Some(hostname.as_str()))
                .and_then(|d| d["service"].as_str())
                .map(str::to_string);
            let key = plan::subdomain_key(&hostname);
            let upstream = match directory(cx.env, "setting.get", json!({ "key": key })).await? {
                Value::String(u) => Some(u),
                _ => None,
            };
            out.push(plan::SeenSubdomain {
                hostname,
                zone_id: zone_id.clone(),
                worker: SITES_WORKER.to_string(),
                service,
                upstream,
            });
        }
        Ok(out)
    }

    /// `instance_plan`: the dry run, and an approval when there is something to apply.
    async fn plan(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("planning needs a known caller"))?;
        let person = caller
            .acting_for
            .as_ref()
            .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
        let id = arg(args, "instance")?;
        let (instance, account, steps, digest) = self.planned(cx, id).await?;
        let lines = plan::describe(&steps);
        if steps.iter().any(plan::Step::stops) {
            return Ok(json!({ "instance": id, "plan": lines, "applies": false,
                "note": "Something is in the way; nothing was asked or changed." }));
        }
        if !steps.iter().any(plan::Step::changes) {
            return Ok(json!({ "instance": id, "plan": lines, "applies": false,
                "note": "Everything is already in place." }));
        }
        let kind = pigeon_core::approval::Kind::LinkedPlan;
        let made = directory(
            cx.env,
            "approval.create",
            json!({
                "kind": kind.name(),
                "subject": id,
                "digest": digest,
                "summary": { "instance": id, "account_id": account.id,
                             "version": instance.version, "plan": lines },
                "payload": { "steps": steps },
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
            "note": "Nothing changes until the person approves this plan on the console's \
                     Approvals tab; then instance_apply {plan: <this approval>} applies it once.",
        }))
    }

    /// `instance_apply`: the approved plan, if the account still gives the same one.
    async fn apply(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        use pigeon_core::{r2, workers};
        let caller = cx
            .caller
            .ok_or_else(|| bad("applying needs a known caller"))?;
        // Not "approval": the passkey gate takes that argument for itself.
        let approval_id = arg(args, "plan")?;
        let got = directory(cx.env, "approval.get", json!({ "id": approval_id })).await?;
        let id = got["subject"]
            .as_str()
            .filter(|_| got["kind"] == "linked_plan")
            .ok_or_else(|| bad(&format!("there is no plan approval {approval_id}")))?
            .to_string();
        let (_, account, steps, digest) = self.planned(cx, &id).await?;
        let taken = directory(
            cx.env,
            "approval.take",
            json!({ "id": approval_id, "digest": digest, "caller_id": caller.user.id }),
        )
        .await?;
        if let Some(why) = taken["refused"].as_str() {
            return Err(bad(&format!(
                "the plan does not run: {why}. If the account changed since it was approved, \
                 plan again."
            )));
        }
        let mut l = ledger(cx.env, &id).await?;
        let mut done = Vec::new();
        for step in steps.iter().filter(|s| s.changes()) {
            let outcome = match step {
                plan::Step::CreateBucket { name, jurisdiction } => {
                    let b = r2::Bucket {
                        name: name.clone(),
                        jurisdiction: jurisdiction.clone(),
                    };
                    let e = r2::ensure_bucket(&account, &b).await?;
                    if !e.created {
                        return Err(bad(&format!(
                            "bucket {name} appeared before pigeon created it; it is not \
                             recorded as pigeon's, and the steps after it were not tried"
                        )));
                    }
                    format!("created bucket {name}")
                }
                plan::Step::UploadWorker {
                    name,
                    bucket,
                    jurisdiction,
                    ..
                } => {
                    let binding = workers::Binding::R2 {
                        name: plan::SITE_BINDING.into(),
                        bucket: bucket.clone(),
                        jurisdiction: jurisdiction.clone(),
                    };
                    workers::upload(
                        &account,
                        name,
                        plan::COMPATIBILITY_DATE,
                        &[binding],
                        plan::SITE_WORKER,
                    )
                    .await?;
                    format!("uploaded Worker {name}")
                }
                plan::Step::EnableWorkersDev { name } => {
                    let url = workers::enable_workers_dev(&account, name).await?;
                    format!("preview at {url}")
                }
                plan::Step::AttachDomain {
                    hostname,
                    zone_id,
                    worker,
                } => {
                    pigeon_core::custom_domain::attach(&account, zone_id, hostname, worker).await?;
                    format!("{hostname} is served by Worker {worker}")
                }
                plan::Step::AttachSubdomain {
                    hostname,
                    zone_id,
                    worker,
                    upstream,
                } => {
                    let key = plan::subdomain_key(hostname);
                    directory(
                        cx.env,
                        "setting.put",
                        json!({ "key": key, "value": upstream, "ttl_ms": KEPT_MS }),
                    )
                    .await?;
                    pigeon_core::custom_domain::attach(cx.account, zone_id, hostname, worker)
                        .await?;
                    format!("{hostname} is served by {worker}, from {upstream}")
                }
                _ => continue,
            };
            plan::record(&mut l, step);
            keep_ledger(cx.env, &id, &l).await?;
            done.push(outcome);
        }
        Ok(json!({ "instance": id, "done": done }))
    }
}

impl Capability for Instances {
    fn name(&self) -> &'static str {
        "instances"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        use pigeon_core::versions::{Component, Kind};
        vec![
            Component::began("instances", Kind::Capability),
            // The site Worker module (`plan::SITE_WORKER`) that instance_apply uploads.
            Component::began("instances.site_worker", Kind::Installed),
        ]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "instances_list",
                "List the platform's instances: each one's account, zones, sites with their \
                 targets and subdomains, Worker, repository and version. Read-only.",
            ),
            Tool::bare(
                "instances_tabs",
                "For the app: each instance's tabs, whether it has settings, and a few facts. Read-only.",
            ),
            Tool {
                name: "instance_plan",
                description: "Dry run of what a linked instance needs in its account: its R2 \
                              bucket, its site Worker and the Worker's workers.dev preview, \
                              read live and compared with what pigeon made. Changes nothing. \
                              When there is something to do and nothing in the way, the plan \
                              waits on the console's Approvals tab for the person's passkey; \
                              `instance_apply` then applies exactly that plan, once.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "instance": { "type": "string" } },
                    "required": ["instance"]
                }),
            },
            Tool {
                name: "instance_apply",
                description: "Apply a plan the person approved: reads the account again, \
                              refuses if it no longer gives the same plan, then creates or \
                              uploads each step in order and records it as pigeon's. Never \
                              deletes anything, and never touches what pigeon did not make.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "plan": { "type": "string", "description": "The plan's approval id, from instance_plan." } },
                    "required": ["plan"]
                }),
            },
            Tool::bare(
                "instance_subdomains",
                "The platform subdomains approved plans attached, each with the instance \
                 Worker's address it is served from: what pigeon-sites serves by host. Read-only.",
            ),
            Tool {
                name: "instance_propose",
                description: "Propose a new or changed instance record (the whole record, with \
                              the `version` it was edited from). It is checked as the console \
                              checks it and waits on the console's Approvals tab; it changes \
                              nothing until the person approves it with their passkey. Answers \
                              the approval's id and the changes, or the problems.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "record": { "type": "object", "additionalProperties": true } },
                    "required": ["record"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "instances_list" => Ok(to_value(all(cx.env).await?)),
                "instances_tabs" => Ok(tabs(&all(cx.env).await?)),
                "instance_propose" => self.propose(cx, args).await,
                "instance_plan" => self.plan(cx, args).await,
                "instance_subdomains" => subdomains(cx.env, &self.zone).await,
                "instance_apply" => self.apply(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }

    fn sections(&self) -> Vec<Section> {
        vec![instances_section()]
    }

    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(self.listed(cx.env))
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/instance/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

/// The first hostname a site is published at: its first zone or domain target.
fn first_hostname(i: &Instance) -> Option<&str> {
    i.sites
        .iter()
        .flat_map(|s| s.targets.iter())
        .find_map(|t| match t {
            record::Target::Zone { hostname, .. } | record::Target::Domain { hostname } => {
                Some(hostname.as_str())
            }
            _ => None,
        })
}

/// The `instances_tabs` answer: which instance the app is on, and for each instance which tabs
/// and settings it shows and a few facts to display. The home account's instance gets the
/// full set of tabs; a linked one gets fewer.
pub fn tabs(instances: &[Instance]) -> Value {
    let home = |i: &Instance| i.account == record::AccountRef::Home;
    let list: Vec<Value> = instances
        .iter()
        .map(|i| {
            let is_home = home(i);
            let host = first_hostname(i).or_else(|| i.zones.first().map(String::as_str));
            let site = first_hostname(i).or_else(|| i.sites.first().map(|s| s.name.as_str()));
            let repository = i.repo.as_ref().map(|r| match r {
                record::RepoRef::Artifacts { namespace, repo } => format!("{namespace}/{repo}"),
                record::RepoRef::Github { full_name, .. } => full_name.clone(),
            });
            json!({
                "id": i.id, "title": i.title,
                "account_kind": if is_home { "home" } else { "linked" },
                "zones": i.zones,
                "tabs": if is_home {
                    json!(["approvals", "status", "cost", "instances"])
                } else {
                    json!(["home", "instances"])
                },
                "settings": is_home,
                "facts": {
                    "website": host.map(|h| format!("https://{h}")),
                    "site": site,
                    "repository": repository,
                },
            })
        })
        .collect();
    json!({
        "current": instances.iter().find(|i| home(i)).map(|i| i.id.as_str()),
        "scoping": "app",
        "instances": list,
    })
}
