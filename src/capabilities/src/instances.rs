//! Instances: named units of what the platform runs, here or in a Cloudflare account a person
//! linked. Code declares only their shape ([`record`]); every value an instance holds (its
//! account, zones, sites, buckets, Workers, routes, subdomains and repository) is data, kept in
//! the Directory. A record changes only with the signed-in person's passkey, bound to the hash of
//! the exact record they approve and to the version they edited, so pigeon touches only what a
//! person named. The console's Instances tab shows every instance and edits one; over MCP they
//! can only be listed.

pub mod data_apply;
pub mod data_ingest_runtime;
pub mod data_plan;
pub mod home;
pub mod home_site;
pub mod layout;
pub mod manifest;
pub mod plan;
pub(crate) mod publishing;
pub mod record;
pub mod redirects;
pub mod rename;
mod renaming;
pub mod retire;
mod retiring;
pub mod tools;

use pigeon_core::mcp::Tool;

use serde_json::{Value, json};
use worker::{Env, Error, Method, Result};

use crate::console::sections::Section;
use crate::passkeys::Passkeys;
use crate::users::{Caller, User, directory};
use crate::{Call, Capability, Context, arg, to_value, unknown};
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

/// Instance `id`'s record, if there is one. An id renamed by a migration that has run (R1)
/// answers the record it was renamed to ([`rename::record`]).
pub async fn one(env: &Env, id: &str) -> Result<Option<Instance>> {
    rename::record(&renaming::Live { env }, id).await
}

/// The id `id` was renamed to, once its rename has run ([`rename::resolve`]): only an
/// [`rename::ALIASES`] id whose tombstone exists, and any other id is answered without a read.
/// Before the migration, `None`.
pub async fn renamed(env: &Env, id: &str) -> Result<Option<String>> {
    rename::resolve(&renaming::Live { env }, id).await
}

/// `args` with its `instance` argument naming the id it was renamed to, when it names a renamed
/// id; `None` when nothing changes ([`rename::current_args`]). What every tool call and console
/// read does with an old id during the compat window.
pub async fn current_args(env: &Env, args: &Value) -> Result<Option<Value>> {
    rename::current_args(&renaming::Live { env }, args).await
}

/// `instances_list`: the instances the person the caller is or acts for may see (an admin: all;
/// docs/plans/membership.md), so the console's and the iOS app's switcher shows only theirs. A
/// caller acting for no one (the platform's own automation) sees all, as before.
async fn listed(cx: &Context<'_>) -> Result<Vec<Instance>> {
    let everyone = all(cx.env).await?;
    let Some(person) = cx.caller.and_then(crate::users::person_of) else {
        return Ok(everyone);
    };
    let access = crate::membership::access(cx.env, &person.id).await?;
    Ok(everyone
        .into_iter()
        .filter(|i| access.role(&i.id).is_some())
        .collect())
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
        if let Some(to) = renamed(cx.env, &record.id).await? {
            return Ok(Err(vec![format!(
                "instance {} was renamed to {to}: edit {to}, or wait until the old id is retired",
                record.id
            )]));
        }
        let everyone = all(cx.env).await?;
        let current = everyone.iter().find(|i| i.id == record.id).cloned();
        let others: Vec<Instance> = everyone.into_iter().filter(|i| i.id != record.id).collect();
        let reserved = self.reserved(cx);
        let workers = self.workers(cx);
        let rules = Rules {
            platform_zone: &self.zone,
            reserved_labels: &reserved,
            reserved_workers: &workers,
            platform_worker: cx.hosted.worker,
        };
        let mut problems = record::validate(&record, &others, &rules);
        problems.extend(record::account_change(current.as_ref(), &record));
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

    /// The `instance-put` write's check: the problems, or the changes the record makes.
    async fn put_check(
        &self,
        cx: &Context<'_>,
        id: &str,
        text: &str,
        audit: Option<&str>,
        person: &User,
    ) -> Result<Value> {
        let (record, current) = match self.check(cx, text).await? {
            Ok(pair) => pair,
            Err(errors) => return Ok(json!({ "errors": errors })),
        };
        if record.id != id {
            return Ok(json!({ "errors": ["the record's id is not the target"] }));
        }
        federation_owner(cx, current.as_ref(), &record, person).await?;
        let mut changes = to_value(record::diff(current.as_ref(), &record));
        if changes.as_array().is_some_and(Vec::is_empty) {
            return Ok(json!({ "errors": ["nothing changes"] }));
        }
        let setup = match audit_setup_change(current.as_ref(), &record, audit) {
            Ok(setup) => setup,
            Err(e) => return Ok(json!({"errors": [e.to_string()]})),
        };
        if let Some((prior, action)) = setup {
            let plan = crate::jobs_audit::setup_plan(cx.env, &self.passkeys.host, prior).await?;
            if plan["action"].as_str() != Some(action) {
                return Ok(
                    json!({"errors": ["The audit bucket’s availability changed. Return to setup and review again."]}),
                );
            }
            if let Some(lines) = changes.as_array_mut() {
                let verb = if action == "create" {
                    "Create"
                } else {
                    "Reuse"
                };
                lines.insert(
                    0,
                    json!(format!(
                        "{verb} private R2 bucket {} in this instance’s linked account",
                        plan["bucket"].as_str().unwrap_or_default()
                    )),
                );
            }
        }
        Ok(json!({ "changes": changes }))
    }

    /// The `instance-put` write, approved by `person`'s passkey for this record: checks it
    /// again and stores it a version on.
    async fn put_record(
        &self,
        cx: &Context<'_>,
        id: &str,
        text: &str,
        person: &User,
        audit: Option<&str>,
    ) -> Result<Value> {
        let (mut record, current) = match self.check(cx, text).await? {
            Ok(pair) => pair,
            Err(errors) => return Err(bad(&errors.join("; "))),
        };
        if record.id != id {
            return Err(bad("the record's id is not the target"));
        }
        federation_owner(cx, current.as_ref(), &record, person).await?;
        if let Some((prior, action)) = audit_setup_change(current.as_ref(), &record, audit)? {
            crate::jobs_audit::setup_apply(cx.env, &self.passkeys.host, prior, action).await?;
            record.version += 1;
            let saved = directory(
                cx.env,
                "instance.jobs_audit_save",
                json!({
                    "instance": id, "base": prior, "record": serde_json::to_string(&record)?
                }),
            )
            .await
            .ok()
            .and_then(|result| result.as_bool())
            .unwrap_or(false);
            if !saved {
                return Err(bad(
                    "The audit bucket was verified, but the Jobs record could not be saved. Reload the instance and review reusing the bucket.",
                ));
            }
            return Ok(
                json!({"message": format!("Jobs audit storage is configured on {} at version {}.", record.id, record.version)}),
            );
        }
        record.version += 1;
        let saved = directory(
            cx.env,
            "instance.record_save",
            json!({
                "instance": id, "base": current, "record": serde_json::to_string(&record)?, "person": person.id
            }),
        )
        .await?;
        if saved != true {
            return Err(bad(
                "the instance changed during the write: reload and review again",
            ));
        }
        let mut index = ids(cx.env).await?;
        if !index.contains(&record.id) {
            index.push(record.id.clone());
            index.sort();
            put(cx.env, INDEX, &serde_json::to_string(&index)?).await?;
            // The person who made a new instance is its first owner.
            crate::membership::grant(
                cx.env,
                &person.id,
                Some(&record.id),
                crate::membership::Role::Owner,
            )
            .await?;
        }
        Ok(json!({
            "message": format!("{} is at version {}, approved by {}.", record.id, record.version, person.name)
        }))
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
        if record::jobs_audit_change(current.as_ref(), &record) {
            return Ok(
                json!({ "problems": ["Set up the required Jobs audit bucket from the instance’s Capabilities console."] }),
            );
        }
        if crate::federation::changed(current.as_ref(), &record) {
            return Ok(
                json!({"problems": ["An instance owner must set or remove MCP Federation trust in the console."]}),
            );
        }
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
            "note": "Nothing changes until the person approves this on the console's Approvals \
                     tab. Once approved, the record is saved and the platform plans the \
                     instance as you: a plan with something to do waits there for the person's \
                     approval too, and is then applied. approvals_status {id: <this approval>} \
                     follows each step.",
        }))
    }
}

/// Federation changes are membership-sensitive even though other instance edits need only operator.
async fn federation_owner(
    cx: &Context<'_>,
    prior: Option<&Instance>,
    next: &Instance,
    person: &User,
) -> Result<()> {
    if !crate::federation::changed(prior, next) {
        return Ok(());
    }
    let Some(prior) = prior else {
        return Err(bad("create the instance before enabling MCP Federation"));
    };
    if record::jobs_audit_change(Some(prior), next) {
        return Err(bad(
            "review Jobs audit setup and MCP Federation trust as separate changes",
        ));
    }
    crate::membership::access(cx.env, &person.id)
        .await?
        .may_manage(&next.id)
        .map_err(|_| bad("only an instance owner can set or remove MCP Federation trust"))?;
    for grant in crate::federation::grants(next)
        .iter()
        .filter(|grant| !crate::federation::grants(prior).contains(grant))
    {
        let peer = one(cx.env, &grant.peer).await?;
        if !peer.is_some_and(|peer| {
            peer.id == grant.peer
                && crate::catalogue::is_home(&peer) != crate::catalogue::is_home(next)
        }) {
            return Err(bad(
                "select the current ID of the home instance or a linked peer",
            ));
        }
        if grant.all_apps {
            let selected: Option<Caller> = serde_json::from_value(
                directory(cx.env, "caller.of", json!({"user_id": grant.person})).await?,
            )?;
            if !selected.is_some_and(|caller| caller.user.kind == "standard") {
                return Err(bad("select a current person for MCP Federation"));
            }
            continue;
        }
        let linked: Option<Caller> = serde_json::from_value(
            directory(cx.env, "caller.of", json!({"user_id": grant.app})).await?,
        )?;
        if !linked.is_some_and(|caller| {
            caller.user.kind == "service"
                && caller
                    .acting_for
                    .is_some_and(|person| person.kind == "standard" && person.id == grant.person)
        }) {
            return Err(bad(
                "the selected app is not currently connected to the named person",
            ));
        }
    }
    Ok(())
}

/// Bind setup to the existing linked account, the deterministic/configured bucket, and the
/// reviewed action. Drafts and ordinary JSON edits cannot create or bypass audit setup.
fn audit_setup_change<'a>(
    current: Option<&'a Instance>,
    next: &Instance,
    action: Option<&'a str>,
) -> Result<Option<(&'a Instance, &'a str)>> {
    if !record::jobs_audit_change(current, next) {
        return if action.is_none() {
            Ok(None)
        } else {
            Err(bad("There is no Jobs audit setup change in this record."))
        };
    }
    let (Some(prior), Some(action)) = (current, action) else {
        return Err(bad(
            "Set up the required Jobs audit bucket from the instance’s Capabilities console.",
        ));
    };
    if next.account != prior.account {
        return Err(bad(
            "Link the target Cloudflare account first, then review Jobs audit setup.",
        ));
    }
    let suggested = crate::jobs_audit::default_bucket(&prior.id);
    let expected = crate::jobs_audit::configured_bucket(prior).unwrap_or(&suggested);
    if crate::jobs_audit::configured_bucket(next) != Some(expected)
        || next.sites.iter().any(|site| site.bucket == expected)
    {
        return Err(bad(
            "Jobs requires the reviewed private audit bucket, separate from any site bucket.",
        ));
    }
    Ok(Some((prior, action)))
}

/// `multi-instance` (docs/plans/capability-scoping.md, M2): what the platform does across its
/// instances, split out of [`Instances`]: Instances › All instances, Platform › Endpoints, and
/// creating instances (`instance_propose` of a record with a new id) and linking them
/// (`/linked/begin`). Home-only (`catalogue::MULTI_INSTANCE`): a record that is not home may
/// not enable it, and the manifest never shows its views elsewhere. Its tools stay served by
/// `instances` and `linked`, on the platform Worker, which only home's console reaches.
pub struct MultiInstance;

impl Capability for MultiInstance {
    fn name(&self) -> &'static str {
        crate::catalogue::MULTI_INSTANCE
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        use pigeon_core::versions::{Component, Kind};
        vec![Component::began(
            crate::catalogue::MULTI_INSTANCE,
            Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn sections(&self) -> Vec<Section> {
        multi_instance_sections()
    }
}

/// The views `multi-instance` adds on the home instance: Instances › All instances and
/// Platform › Endpoints.
pub fn multi_instance_sections() -> Vec<Section> {
    vec![
        Section {
            area: "instances",
            id: "list",
            title: "All instances",
            description: "Every instance, with its account, link and what waits for it.",
            order: 10,
        },
        Section {
            area: "platform",
            id: "endpoints",
            title: "Endpoints",
            description: "The platform's MCP endpoint and each instance's scoped one.",
            order: 30,
        },
    ]
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
                let mut entry = json!({ "host": host, "instance": i.id, "site": site.name, "upstream": upstream });
                if let Some(auth) = &site.auth {
                    entry["auth"] = json!(auth);
                    entry["bucket"] = json!(site.bucket);
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
    // Each Domain target, then each redirect host: both are custom domains of the Worker.
    let targets = site
        .map(|s| s.targets.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| match t {
            record::Target::Domain { hostname } => Some(hostname.clone()),
            _ => None,
        });
    let redirect_hosts = instance
        .redirects
        .iter()
        .map(|r| r.host.to_ascii_lowercase());
    for hostname in targets.chain(redirect_hosts) {
        let hostname = &hostname;
        if domains
            .iter()
            .any(|d: &plan::SeenDomain| &d.hostname == hostname)
        {
            continue;
        }
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

/// What a linked account holds for the catalogue capabilities `instance`'s record enables, for
/// [`plan::capability_steps`]; reads only, and only for the capabilities it enables. A link
/// without a capability's scope is refused for it, never read with.
async fn observe_capabilities(
    env: &Env,
    account: &pigeon_core::Account,
    instance: &Instance,
) -> Result<plan::SeenCapabilities> {
    let mut seen = plan::SeenCapabilities::default();
    for name in plan::own_workers(instance) {
        if pigeon_core::workers::exists(account, &name).await? {
            seen.workers.push(name);
        }
    }
    let granted = crate::linked::link(env, &instance.id)
        .await?
        .and_then(|l| l.scope);
    if crate::catalogue::hosted_enabled(instance, "secrets") {
        let id = instance.id.as_str();
        match crate::linked::capability_refusal(id, "secrets", granted.as_deref()) {
            Some(why) => seen.refused.push(("secrets".into(), why)),
            None => match crate::secrets::linked_store(account).await {
                Ok(store) => seen.secrets_store = Some(store.is_some()),
                Err(e) => seen.refused.push((
                    "secrets".into(),
                    crate::linked::explain(id, "secrets", &e.to_string()),
                )),
            },
        }
    }
    Ok(seen)
}

/// What the platform's own account holds for the home instance, read with the home token
/// (`account`'s bearer) alone: whether its landing bucket exists. Its Worker is the platform's
/// when it names it (deployed by the Lander, not read: the token holds no Workers-scripts
/// permission), and nothing else is read; [`plan::plan`] keeps or blocks the rest.
async fn observe_home(
    account: &pigeon_core::Account,
    instance: &Instance,
    zone: &str,
    platform_worker: bool,
) -> Result<plan::Observed> {
    use pigeon_core::r2;
    let bucket_exists = match instance.sites.first() {
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
    let subdomains = instance
        .sites
        .iter()
        .flat_map(|s| s.targets.iter())
        .filter_map(|t| match t {
            record::Target::Subdomain { label } => Some(plan::SeenSubdomain {
                hostname: record::subdomain_host(label, zone),
                zone_id: None,
                worker: SITES_WORKER.to_string(),
                service: None,
                upstream: None,
            }),
            _ => None,
        })
        .collect();
    Ok(plan::Observed {
        account_id: account.id.clone(),
        bucket_exists,
        worker_exists: platform_worker,
        workers_dev: None,
        account_subdomain: None,
        domains: Vec::new(),
        subdomains,
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
                    "instance {id} runs in the platform's own account: it is planned with the \
                     home token"
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
        let owned = self.owned(cx, &instance);
        let kept = ledger(env, id).await?;
        let caps = observe_capabilities(env, &account, &instance).await?;
        let data = self.linked_data(cx, &account, &instance, &kept).await?;
        // A capability's own Worker first (M5), so the instance Worker can bind it; the data
        // store's bucket, D1 and own Worker before the instance Worker, which may run it inline,
        // and its mail after (docs/plans/linked-data.md, decision 7).
        let mut steps = plan::capability_workers(&instance, &caps, &kept);
        steps.extend(data.store);
        steps.extend(plan::plan_with(
            &instance,
            &seen,
            &kept,
            owned.as_ref(),
            data.inline.as_ref(),
        ));
        steps.extend(data.rest);
        steps.extend(plan::capability_steps(&instance, &caps));
        let digest = plan::digest(&instance, &account.id, &steps);
        Ok((instance, account, steps, digest))
    }

    /// A linked instance's data store and email steps (docs/plans/linked-data.md), read with
    /// its link's token. A link that lacks a scope `data` or `email` needs leaves both alone
    /// with why (re-link to enable), and the site's steps still apply.
    async fn linked_data(
        &self,
        cx: &Context<'_>,
        account: &pigeon_core::Account,
        instance: &Instance,
        kept: &plan::Ledger,
    ) -> Result<data_plan::DataParts> {
        if !crate::data_store::enabled(instance) {
            return Ok(data_plan::DataParts::default());
        }
        let granted = crate::linked::link(cx.env, &instance.id)
            .await?
            .and_then(|l| l.scope);
        let mut wanted = vec![crate::data_store::NAME];
        let backup = crate::data_store::backup(instance);
        if backup.is_some() {
            wanted.extend([crate::data_store::BACKUP_SCOPES, "secrets"]);
        }
        if data_ingest_runtime::configured(instance).is_some() {
            wanted.push(data_ingest_runtime::SCOPE);
        }
        if crate::email::enabled(instance) {
            wanted.push(crate::email::NAME);
        }
        let refused: Vec<plan::Step> = wanted
            .iter()
            .filter_map(|c| {
                crate::linked::capability_refusal(&instance.id, c, granted.as_deref()).map(|why| {
                    plan::Step::Keep {
                        what: "capability".into(),
                        name: c.to_string(),
                        why,
                    }
                })
            })
            .collect();
        if !refused.is_empty() {
            return Ok(data_plan::DataParts {
                store: refused,
                ..Default::default()
            });
        }
        let token = account
            .bearer
            .as_deref()
            .ok_or_else(|| bad("the instance's account is not a linked one"))?;
        if backup.is_some() || crate::data_store::mail_import_audit_bucket(instance).is_some() {
            crate::data_store::verify_data_jobs_audit(account, instance).await?;
        }
        let secrets_store = if backup.is_some() {
            crate::secrets::linked_store(account)
                .await?
                .ok_or_else(|| bad("Set up the linked instance's Secrets Store for Data backup."))?
        } else {
            String::new()
        };
        let built = data_apply::artifact(cx.account).await?;
        let seen =
            data_apply::observe(token, &account.id, &secrets_store, instance, built.as_ref())
                .await
                .map_err(|e| {
                    bad(&crate::linked::explain(
                        &instance.id,
                        "data",
                        &e.to_string(),
                    ))
                })?;
        Ok(data_plan::data_parts(instance, &seen, kept, None))
    }

    /// What is platform-owned for `instance`: `None` for a linked one.
    fn owned(&self, cx: &Context<'_>, instance: &Instance) -> Option<plan::PlatformOwned> {
        let reserved = self.reserved(cx);
        let workers = self.workers(cx);
        let rules = Rules {
            platform_zone: &self.zone,
            reserved_labels: &reserved,
            reserved_workers: &workers,
            platform_worker: cx.hosted.worker,
        };
        plan::platform_owned(instance, &rules)
    }

    /// The home instance's plan, read with the home token's `account`: the steps, their digest
    /// (bound to the platform's own account) and what is platform-owned.
    async fn home_planned(
        &self,
        cx: &Context<'_>,
        instance: &Instance,
        account: &pigeon_core::Account,
    ) -> Result<(Vec<plan::Step>, String, Option<plan::PlatformOwned>)> {
        let owned = self.owned(cx, instance);
        let platform_worker = instance
            .worker
            .as_deref()
            .is_some_and(|w| owned.as_ref().is_some_and(|o| o.worker(w)));
        let seen = observe_home(account, instance, &self.zone, platform_worker).await?;
        let steps = plan::plan(
            instance,
            &seen,
            &ledger(cx.env, &instance.id).await?,
            owned.as_ref(),
        );
        let digest = plan::digest(instance, &account.id, &steps);
        Ok((steps, digest, owned))
    }

    /// The data store's and email's steps for `instance` (`data_plan::data_steps`), read with
    /// a `data_home` token (`data_apply`), and the newest data Worker build. None when the record
    /// does not enable the store; a linked instance's are `linked_data`'s.
    async fn data_planned(
        &self,
        cx: &Context<'_>,
        instance: &Instance,
    ) -> Result<(Vec<plan::Step>, Option<data_plan::Artifact>)> {
        if !crate::data_store::enabled(instance) {
            return Ok((Vec::new(), None));
        }
        if instance.account != record::AccountRef::Home {
            return Ok((
                vec![plan::Step::Blocked {
                    why: "a linked instance's data store is planned with its link \
                          (`linked_data`), not with the home token"
                        .into(),
                }],
                None,
            ));
        }
        let built = data_apply::artifact(cx.account).await?;
        let zone = crate::email::domain(instance).unwrap_or_else(|| self.zone.clone());
        let zone_id = pigeon_core::zones::active(cx.account, &zone)
            .await
            .map(|z| z.id)
            .unwrap_or_default();
        let owned = self.owned(cx, instance);
        let kept = ledger(cx.env, &instance.id).await?;
        let b = built.as_ref();
        let steps = data_apply::with_token(cx.account, &zone_id, |token| async move {
            let seen = data_apply::observe(
                &token,
                &cx.account.id,
                &cx.account.secrets_store,
                instance,
                b,
            )
            .await?;
            Ok(data_plan::data_steps(
                instance,
                &seen,
                &kept,
                owned.as_ref(),
            ))
        })
        .await?;
        Ok((steps, built))
    }

    /// Applies the approved data steps with a `data_home` token, keeping the ledger as each is
    /// done.
    async fn data_run(
        &self,
        cx: &Context<'_>,
        instance: &Instance,
        steps: &[plan::Step],
        built: Option<&data_plan::Artifact>,
    ) -> Result<Vec<String>> {
        let zone = crate::email::domain(instance).unwrap_or_else(|| self.zone.clone());
        let zone_id = pigeon_core::zones::active(cx.account, &zone)
            .await
            .map(|z| z.id)
            .unwrap_or_default();
        let mut l = ledger(cx.env, &instance.id).await?;
        let lines = data_apply::with_token(cx.account, &zone_id, |token| {
            let l = &mut l;
            async move {
                let apply = data_apply::Apply {
                    env: cx.env,
                    parent: cx.account,
                    token: &token,
                    account_id: &cx.account.id,
                    secrets_store: &cx.account.secrets_store,
                    built,
                };
                data_apply::run(&apply, instance, steps, l).await
            }
        })
        .await;
        // What was done is pigeon's, even when a later step failed.
        keep_ledger(cx.env, &instance.id, &l).await?;
        lines
    }

    /// Runs `work` with the home token: minted from the parent token for purpose
    /// `instance_home`, scoped to the platform zone, and revoked after `work`, whether it
    /// succeeded or not. Refuses before any work when the purpose or the parent token's
    /// permissions are missing.
    async fn with_home<F, Fut, T>(&self, cx: &Context<'_>, work: F) -> Result<T>
    where
        F: FnOnce(pigeon_core::Account) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let zone_id = pigeon_core::zones::active(cx.account, &self.zone)
            .await
            .map(|z| z.id)
            .unwrap_or_default();
        home::with_token(
            cx.account,
            crate::split::purpose_permissions(home::PURPOSE),
            &zone_id,
            &cx.account.id,
            &cx.account.secrets_store,
            work,
        )
        .await
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
        let (instance, account_id, steps, digest) = match one(cx.env, id).await? {
            Some(home) if home.account == record::AccountRef::Home => {
                let h = &home;
                let (mut steps, account_id) = self
                    .with_home(cx, |account| async move {
                        let (steps, _, _) = self.home_planned(cx, h, &account).await?;
                        Ok((steps, account.id))
                    })
                    .await?;
                // The data store's and email's steps (docs/plans/email.md), read with their
                // own token; the digest binds both lists.
                let (data, _) = self.data_planned(cx, h).await?;
                steps.extend(data);
                let digest = plan::digest(h, &account_id, &steps);
                (home, cx.account.id.clone(), steps, digest)
            }
            _ => {
                let (instance, account, steps, digest) = self.planned(cx, id).await?;
                (instance, account.id, steps, digest)
            }
        };
        let lines = plan::describe(&steps);
        // The record's redirects as the plan serves them, and that removing one detaches nothing.
        let shown = (!instance.redirects.is_empty())
            .then(|| json!({ "hosts": redirects::lines(&instance), "note": plan::REDIRECTS_NOTE }));
        let with_redirects = |mut out: Value| {
            if let Some(r) = &shown {
                out["redirects"] = r.clone();
            }
            out
        };
        if steps.iter().any(plan::Step::stops) {
            return Ok(with_redirects(
                json!({ "instance": id, "plan": lines, "applies": false,
                "note": "Something is in the way; nothing was asked or changed." }),
            ));
        }
        if !steps.iter().any(plan::Step::changes) {
            // A capability left alone for want of a re-link is not "in place".
            let note = if steps.iter().any(plan::Step::awaits_relink) {
                "Nothing can change yet: a capability waits on a re-link (see the plan)."
            } else {
                "Everything is already in place."
            };
            return Ok(with_redirects(
                json!({ "instance": id, "plan": lines, "applies": false, "note": note }),
            ));
        }
        let kind = pigeon_core::approval::Kind::LinkedPlan;
        let made = directory(
            cx.env,
            "approval.create",
            json!({
                "kind": kind.name(),
                "subject": id,
                "digest": digest,
                "summary": { "instance": id, "account_id": account_id,
                             "version": instance.version, "plan": lines },
                "payload": { "steps": steps },
                "caller_id": caller.user.id,
                "approver_id": person.id,
            }),
        )
        .await?;
        Ok(with_redirects(json!({
            "instance": id,
            "plan": lines,
            "applies": true,
            "approval": made["record"]["id"],
            "waits_until_ms": made["record"]["expires_ms"],
            "note": "Nothing changes until the person approves this plan on the console's \
                     Approvals tab; then the platform applies it once, as you (instance_apply), \
                     if the account still gives the same plan. approvals_status {id: <this \
                     approval>} says how it went.",
        })))
    }

    /// `instance_apply`: the approved plan, if the account still gives the same one. The home
    /// instance's is read and applied with the home token, revoked after, and refused outright
    /// when anything in it is platform-owned.
    async fn apply(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("applying needs a known caller"))?;
        // Not "approval": the passkey gate takes that argument for itself.
        let approval_id = arg(args, "plan")?;
        let got = directory(cx.env, "approval.get", json!({ "id": approval_id })).await?;
        let id = got["subject"]
            .as_str()
            .filter(|_| {
                got["kind"] == "linked_plan"
                    && !retiring::is_retire(&got)
                    && !renaming::is_rename(&got)
            })
            .ok_or_else(|| bad(&format!("there is no plan approval {approval_id}")))?
            .to_string();
        if let Some(home) = one(cx.env, &id)
            .await?
            .filter(|i| i.account == record::AccountRef::Home)
        {
            let h = &home;
            let id = id.as_str();
            return self
                .with_home(cx, |account| async move {
                    let (steps, _, owned) = self.home_planned(cx, h, &account).await?;
                    let (data, built) = self.data_planned(cx, h).await?;
                    let mut all = steps.clone();
                    all.extend(data.iter().cloned());
                    let digest = plan::digest(h, &account.id, &all);
                    if let Some(why) = plan::home_refusal(&steps, owned.as_ref()) {
                        return Err(bad(&format!(
                            "the home plan does not run: {why}. Nothing was changed."
                        )));
                    }
                    if let Some(why) = data_plan::data_refusal(h, &data, owned.as_ref()) {
                        return Err(bad(&format!(
                            "the home plan does not run: {why}. Nothing was changed."
                        )));
                    }
                    take(cx, approval_id, &digest, &caller.user.id).await?;
                    let mut out = self.run(cx, id, h, &account, &steps).await?;
                    if data.iter().any(plan::Step::changes) {
                        out["data"] = json!(self.data_run(cx, h, &data, built.as_ref()).await?);
                    }
                    Ok(out)
                })
                .await;
        }
        let (instance, account, steps, digest) = self.planned(cx, &id).await?;
        if let Some(why) = data_plan::data_refusal(&instance, &steps, None) {
            return Err(bad(&format!(
                "the plan does not run: {why}. Nothing was changed."
            )));
        }
        take(cx, approval_id, &digest, &caller.user.id).await?;
        self.run(cx, &id, &instance, &account, &steps).await
    }

    /// Applies each changing step of `steps` in `account`, in order, recording each in
    /// instance `id`'s ledger as it is done.
    async fn run(
        &self,
        cx: &Context<'_>,
        id: &str,
        instance: &Instance,
        account: &pigeon_core::Account,
        steps: &[plan::Step],
    ) -> Result<Value> {
        use pigeon_core::{r2, workers};
        let mut l = ledger(cx.env, id).await?;
        let mut done = Vec::new();
        // The data store's build, when a step uploads it (docs/plans/linked-data.md).
        let built = if steps
            .iter()
            .any(|s| matches!(s, plan::Step::UploadDataWorker { .. }))
        {
            data_apply::artifact(cx.account).await?
        } else {
            None
        };
        let secrets_store = if crate::data_store::backup(instance).is_some()
            && steps
                .iter()
                .any(|s| matches!(s, plan::Step::UploadDataWorker { .. }))
        {
            crate::secrets::linked_store(account)
                .await?
                .ok_or_else(|| bad("The linked instance's Secrets Store disappeared."))?
        } else {
            String::new()
        };
        for step in steps.iter().filter(|s| s.changes()) {
            if data_step(step) {
                // A linked instance's data and mail steps, with its link's token.
                let token = account
                    .bearer
                    .as_deref()
                    .ok_or_else(|| bad("the instance's account is not a linked one"))?;
                let apply = data_apply::Apply {
                    env: cx.env,
                    parent: cx.account,
                    token,
                    account_id: &account.id,
                    secrets_store: &secrets_store,
                    built: built.as_ref(),
                };
                let lines =
                    data_apply::run(&apply, instance, std::slice::from_ref(step), &mut l).await;
                keep_ledger(cx.env, id, &l).await?;
                done.extend(lines?);
                continue;
            }
            let outcome = match step {
                plan::Step::CreateBucket {
                    name, jurisdiction, ..
                } => {
                    let b = r2::Bucket {
                        name: name.clone(),
                        jurisdiction: jurisdiction.clone(),
                    };
                    let e = r2::ensure_bucket(account, &b).await?;
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
                    origin,
                    redirects,
                    data,
                    ..
                } => {
                    let mut bindings = Vec::new();
                    if let Some(bucket) = bucket {
                        bindings.push(workers::Binding::R2 {
                            name: plan::SITE_BINDING.into(),
                            bucket: bucket.clone(),
                            jurisdiction: jurisdiction.clone(),
                        });
                    }
                    let module = if let Some(upstream) = origin.as_deref() {
                        let bucket = bucket
                            .as_deref()
                            .ok_or_else(|| bad("a protected origin needs its site's bucket"))?;
                        let site = instance
                            .sites
                            .first()
                            .ok_or_else(|| bad("missing protected site"))?;
                        // SITE_KEY, as pigeon-sites derives it (docs/plans/caller-keys.md,
                        // step 4).
                        let key = pigeon_core::secrets_store::site_key(cx.env).await?;
                        bindings.push(workers::Binding::Secret {
                            name: plan::ORIGIN_KEY_BINDING.into(),
                            text: pigeon_core::site_auth::origin_key(
                                &key, id, &site.name, bucket, upstream,
                            ),
                        });
                        plan::protected_site_worker()
                    } else {
                        plan::SITE_WORKER.to_string()
                    };
                    // Each capability placed in its own Worker (M5), by Service Binding.
                    for (binding, service) in plan::services(instance) {
                        bindings.push(workers::Binding::Service {
                            name: binding,
                            service,
                        });
                    }
                    // The redirect hosts are answered first, by the script in front of the site.
                    let module = match redirects {
                        Some(map) => {
                            bindings.push(workers::Binding::Text {
                                name: redirects::BINDING.into(),
                                text: map.clone(),
                            });
                            redirects::wrap(&module)
                        }
                        None => module,
                    };
                    match data {
                        // The store inline: the site module beside the data Worker's build.
                        Some(data) => {
                            data_apply::upload_inline(&data_apply::Inline {
                                env: cx.env,
                                parent: cx.account,
                                account,
                                instance,
                                worker: name,
                                site: &module,
                                bindings: &bindings,
                                data,
                            })
                            .await?;
                            format!("uploaded Worker {name}, with the data store inline")
                        }
                        None => {
                            workers::upload(
                                account,
                                name,
                                plan::COMPATIBILITY_DATE,
                                &bindings,
                                &module,
                            )
                            .await?;
                            format!("uploaded Worker {name}")
                        }
                    }
                }
                plan::Step::EnableWorkersDev { name } => {
                    let url = workers::enable_workers_dev(account, name).await?;
                    format!("preview at {url}")
                }
                plan::Step::UploadCapabilityWorker {
                    capability, name, ..
                } => {
                    let build = plan::LINKED_WORKERS
                        .iter()
                        .find(|w| w.capability == capability)
                        .ok_or_else(|| {
                            bad(&format!(
                                "{capability} has no Worker build for a linked account"
                            ))
                        })?;
                    workers::upload(account, name, plan::COMPATIBILITY_DATE, &[], build.module)
                        .await?;
                    format!("uploaded {capability}'s Worker {name}")
                }
                plan::Step::CreateSecretsStore { name } => {
                    let token = account
                        .bearer
                        .as_deref()
                        .ok_or_else(|| bad("the instance's account is not a linked one"))?;
                    let path = format!("/accounts/{}/secrets_store/stores", account.id);
                    let body = json!({ "name": name });
                    pigeon_core::cloudflare::call(token, Method::Post, &path, Some(&body))
                        .await
                        .map_err(|e| bad(&crate::linked::explain(id, "secrets", &e.to_string())))?;
                    format!("created the Secrets Store {name}")
                }
                plan::Step::AttachDomain {
                    hostname,
                    zone_id,
                    worker,
                } => {
                    pigeon_core::custom_domain::attach(account, zone_id, hostname, worker).await?;
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
            keep_ledger(cx.env, id, &l).await?;
            done.push(outcome);
        }
        Ok(json!({ "instance": id, "done": done }))
    }
}

/// Whether `step` is one of the data store's or email's (`data_apply::run` applies it). Its
/// bucket is created as any bucket is.
fn data_step(step: &plan::Step) -> bool {
    use plan::Step as S;
    matches!(
        step,
        S::CreateDatabase { .. }
            | S::Migrate { .. }
            | S::UploadDataWorker { .. }
            | S::WriteEmailConfig { .. }
            | S::CreateQueue { .. }
            | S::ConfigureBackupConsumer { .. }
            | S::QueueBackupInventory { .. }
            | S::AddBucketNotification { .. }
            | S::RemoveStaleDns { .. }
            | S::EnableEmailRouting { .. }
            | S::SetCatchAll { .. }
            | S::AddDestination { .. }
    )
}

/// Takes plan approval `approval_id` for `caller`, once, if it was approved for `digest`.
async fn take(cx: &Context<'_>, approval_id: &str, digest: &str, caller: &str) -> Result<()> {
    let taken = directory(
        cx.env,
        "approval.take",
        json!({ "id": approval_id, "digest": digest, "caller_id": caller }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        return Err(bad(&format!(
            "the plan does not run: {why}. If the account changed since it was approved, \
             plan again."
        )));
    }
    Ok(())
}

impl Capability for Instances {
    fn name(&self) -> &'static str {
        "instances"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        use pigeon_core::versions::{Component, Kind};
        vec![
            Component::began("instances", Kind::Capability).entry(
                "1.1.0",
                "Records may hold redirects: hostnames in the instance's zones that answer with \
                 an HTTP redirect, attached to the instance's Worker by its plan.",
            ),
            // The site Worker module (`plan::SITE_WORKER`) that instance_apply uploads.
            Component::began("instances.site_worker", Kind::Installed).entry(
                "1.1.0",
                "With redirects, the module answers each redirect host first \
                 (`redirects::MODULE`); without, it is unchanged.",
            ),
        ]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "instances_list",
                "List the platform's instances: each one's account, zones, sites with their \
                 targets and subdomains, Worker, repository and version. Read-only.",
            ),
            Tool {
                name: manifest::TOOL,
                description: "The console's navigation for one instance, as data: its areas in \
                              order, each with its views, the capability that adds each view \
                              and any upgrade it needs, and how many requests wait in its \
                              Inbox. Computed from the approved record. Read-only.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "instance": { "type": "string", "description": "The instance's id; the home instance when absent." } }
                }),
            },
            Tool {
                name: "instance_plan",
                description: "Dry run of what an instance needs in its account: its R2 \
                              bucket, its site Worker and the Worker's workers.dev preview, \
                              its custom domains (a redirect host is one, answered by the \
                              Worker's redirect map), \
                              read live and compared with what pigeon made. Changes nothing. \
                              The home instance is read with a short-lived `instance_home` \
                              token (revoked after) and plans only its landing bucket: the \
                              platform's Workers and hosts are kept as they are, never \
                              changed. When there is something to do and nothing in the way, \
                              the plan waits on the console's Approvals tab for the person's \
                              passkey; `instance_apply` then applies exactly that plan, once.",
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
            Tool {
                name: "instance_retire_plan",
                description: "Dry run of retiring a linked instance: lists exactly what would be \
                              removed, only what pigeon's ledger says pigeon made for it (its \
                              platform subdomains and custom domains, its Worker and workers.dev \
                              address, its R2 bucket after emptying it) and pigeon's own state \
                              for it (site users of its hosts with their passkeys and sessions, \
                              publishing queue rows, ledger, link, and last the record), and \
                              what is kept because pigeon did not make it. Removes nothing. \
                              Refuses the home instance; stops when another instance depends on \
                              its Worker or buckets. Otherwise the plan waits on the console's \
                              Approvals tab for the person's passkey; `instance_retire_apply` \
                              then runs exactly that plan. `keep_link` keeps the account link, \
                              to propose and apply the instance again.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "instance": { "type": "string" },
                        "keep_link": { "type": "boolean", "description": "Keep the account link (default false: revoke and forget it)." }
                    },
                    "required": ["instance"]
                }),
            },
            Tool {
                name: "instance_retire_apply",
                description: "Run a retire plan the person approved: reads everything again and \
                              refuses if it no longer gives the same plan, then stops serving \
                              (subdomains, custom domains, workers.dev), deletes the Worker, \
                              empties and deletes the bucket, removes pigeon's state for the \
                              instance and last its record, recording each step. If a step \
                              fails, call it again with the same plan to resume from that step.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "plan": { "type": "string", "description": "The retire plan's approval id, from instance_retire_plan." } },
                    "required": ["plan"]
                }),
            },
            Tool {
                name: rename::TOOL,
                description: "R1: rename the home instance's id from `pigeon-security/platform` \
                              to `pigeon.dev`, its zone. Without `plan`, the dry run: lists \
                              exactly what it would do (write instance:pigeon.dev, move the \
                              ledger and any GitHub state, expire open approvals naming the old \
                              id, swap the index entry, re-attribute CI jobs, leave a tombstone \
                              at the old key answering the old id for 30 days) and, unless \
                              `dry_run`, waits on the console's Approvals tab for the person's \
                              passkey, bound to both ids, the home record's digest and the \
                              steps. With `plan` (that approval), reads everything again, \
                              refuses if the record or the plan changed, and runs it once. \
                              Refuses when pigeon.dev already exists as a different record. A \
                              second run finds nothing to do.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "dry_run": { "type": "boolean", "description": "Only list the steps; ask for no approval." },
                        "plan": { "type": "string", "description": "The rename plan's approval id, to run it." }
                    }
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
                "instances_list" => Ok(to_value(listed(cx).await?)),
                manifest::TOOL => manifest::console_manifest(cx, args).await,
                "instance_propose" => self.propose(cx, args).await,
                "instance_plan" => self.plan(cx, args).await,
                "instance_subdomains" => subdomains(cx.env, &self.zone).await,
                "instance_apply" => self.apply(cx, args).await,
                "instance_retire_plan" => self.retire_plan(cx, args).await,
                "instance_retire_apply" => self.retire_apply(cx, args).await,
                rename::TOOL => self.rename_home(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }

    fn write_check<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            let text = w.change["record"].as_str().unwrap_or_default();
            self.put_check(
                cx,
                w.text(),
                text,
                w.change["jobs_audit"].as_str(),
                w.person,
            )
            .await
        })
    }

    fn write_make<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            let person = w.person;
            let text = w.change["record"].as_str().unwrap_or_default();
            self.put_record(cx, w.text(), text, person, w.change["jobs_audit"].as_str())
                .await
        })
    }
}
