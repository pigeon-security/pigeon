//! Instances: named units of what the platform runs, here or in a Cloudflare account a person
//! linked. Code declares only their shape ([`record`]); every value an instance holds (its
//! account, zones, sites, buckets, Workers, routes, subdomains and repository) is data, kept in
//! the Directory. A record changes only with the signed-in person's passkey, bound to the hash of
//! the exact record they approve and to the version they edited, so pigeon touches only what a
//! person named. The console's Instances tab shows every instance and edits one; over MCP they
//! can only be listed.

pub mod record;

use pigeon_core::mcp::Tool;
use pigeon_core::oauth::hash;
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::console::sections::{Section, Tab};
use crate::passkeys::Passkeys;
use crate::users::{User, directory};
use crate::{Call, Capability, Context, Serve, arg, to_value, unknown};
use record::{Instance, Rules};

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
        let instances: Vec<Value> = all(env)
            .await?
            .into_iter()
            .map(|i| {
                let link = match i.account {
                    record::AccountRef::Home => json!({ "status": "this account" }),
                    record::AccountRef::Linked { .. } => json!({ "status": "not linked" }),
                };
                let mut v = to_value(i);
                v["link"] = link;
                v
            })
            .collect();
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

impl Capability for Instances {
    fn name(&self) -> &'static str {
        "instances"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "instances_list",
                "List the platform's instances: each one's account, zones, sites with their \
                 targets and subdomains, Worker, repository and version. Read-only.",
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
                    "properties": { "record": { "type": "object" } },
                    "required": ["record"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "instances_list" => Ok(to_value(all(cx.env).await?)),
                "instance_propose" => self.propose(cx, args).await,
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
