//! The `email` capability (docs/plans/email.md): mail received by Email Routing on the
//! instance's zone, through one catch-all rule, into the instance's unified data store (the
//! `data` capability). Listed addresses are mailboxes of their own; everything else lands in
//! `catch-all`. Everything is kept; no tool deletes mail.
//!
//! The tools read through the data Worker's signed ops (`data_store::call`); the two writes
//! only propose a record change, which the person approves with their passkey, and the next
//! instance plan carries it to the Worker's configuration. Off by default: a record without
//! `data.email` has no mail, no tools' answers and no Email tab.

use pigeon_core::mcp::Tool;
use pigeon_ingest::datastore::{self, CATCH_ALL, DataCall, EmailConfig, ForwardScope, ListQuery};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::instances::record::{CapabilitySettings, Instance};
use crate::instances::tools::{Enabled, Registration, View};
use crate::{Call, Capability, Context, arg, unknown};

/// The capability's name in the catalogue (docs/plans/record-reshape.md: `email` before). Its
/// settings sit at the record's `data.email`.
pub const NAME: &str = "data-email";

/// The tools the person's policy must gate on their passkey, each call. A renamed tool is not
/// gated until the policy names its new name, which only the person's passkey changes
/// (docs/plans/record-reshape.md, "Fail closed until the policy gates it"); until then a call
/// is refused rather than run without the passkey.
pub const MUST_BE_GATED: &[&str] = &["data_email_forward_set"];

/// Why a call of `tool` is refused because the policy does not gate it on the passkey as
/// [`MUST_BE_GATED`] requires; `gated` answers whether the live policy does. Pure.
pub fn ungated(tool: &str, gated: impl Fn(&str) -> bool) -> Option<String> {
    (MUST_BE_GATED.contains(&tool) && !gated(tool)).then(|| {
        format!(
            "`{tool}` waits for the policy update that gates it on the person's passkey \
             (rule risky-tools-need-passkey in .pigeon/policy.toml); until the person approves \
             that update it does not run"
        )
    })
}

/// The listed addresses the person named first (docs/plans/email.md, "The person's decisions").
pub const FIRST_ADDRESSES: &[&str] = &["cloudflare", "buildkite", "apple"];

/// One listed address on the record.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Address {
    /// The local part, lowercase: `cloudflare` for `cloudflare@pigeon.dev`.
    pub local: String,
    /// Where its mail is forwarded, if anywhere. Changing it needs the person's passkey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward: Option<String>,
}

/// The record's forwarding: its scope (`listed` unless set) and default destination.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ForwardScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// The record's `email` settings, when it enables mail.
pub fn settings(record: &Instance) -> Option<&CapabilitySettings> {
    record.capabilities.get(NAME)
}

/// Whether the record enables mail (and so the data store it writes into).
pub fn enabled(record: &Instance) -> bool {
    settings(record).is_some() && crate::data_store::enabled(record)
}

/// The domain mail is received on: the record's `domain`, else its first zone.
pub fn domain(record: &Instance) -> Option<String> {
    let s = settings(record)?;
    s.domain
        .clone()
        .or_else(|| record.zones.first().cloned())
        .map(|d| d.to_ascii_lowercase())
}

/// The data Worker's configuration for `record`, as the plan's `WriteEmailConfig` writes it:
/// `None` when mail is off or there is no domain.
pub fn config(record: &Instance) -> Option<EmailConfig> {
    if !enabled(record) {
        return None;
    }
    let s = settings(record)?;
    let forward = s.forward.clone().unwrap_or_default();
    Some(EmailConfig {
        instance: record.id.clone(),
        domain: domain(record)?,
        addresses: s
            .addresses
            .iter()
            .map(|a| datastore::Listed {
                local: a.local.to_ascii_lowercase(),
                forward: a.forward.as_ref().map(|f| f.to_ascii_lowercase()),
            })
            .collect(),
        scope: forward.scope.unwrap_or_default(),
        forward_to: forward.to.map(|t| t.to_ascii_lowercase()),
    })
}

/// Whether `local` is a local part this capability lists: 1 to 64 of `a-z 0-9 . _ -`, not
/// starting or ending with a dot, never `catch-all`.
pub fn valid_local(local: &str) -> bool {
    (1..=64).contains(&local.len())
        && local
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && local != CATCH_ALL
}

/// Whether `address` looks like a mailbox Cloudflare can verify as a destination.
pub fn valid_destination(address: &str) -> bool {
    let Some((local, domain)) = address.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && crate::instances::record::dns_name(&domain.to_ascii_lowercase())
        && !address.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// What is wrong with capability `name`'s email fields on `record`.
pub fn field_problems(record: &Instance, name: &str, s: &CapabilitySettings) -> Vec<String> {
    let mut out = Vec::new();
    if name != NAME {
        if s.domain.is_some() || !s.addresses.is_empty() || s.forward.is_some() {
            out.push(format!(
                "The capability \"{name}\" does not accept a domain, addresses or forward."
            ));
        }
        return out;
    }
    match domain(record) {
        None => out.push("Email needs a domain: the record has no zone.".into()),
        Some(d) if !record.zones.iter().any(|z| z.eq_ignore_ascii_case(&d)) => out.push(format!(
            "Email's domain {d} is not one of the instance's zones."
        )),
        Some(_) => {}
    }
    let mut seen = std::collections::BTreeSet::new();
    for a in &s.addresses {
        if !valid_local(&a.local) {
            out.push(format!(
                "\"{}\" is not a listed address: use 1 to 64 of a-z, 0-9, '.', '_' and '-', not \
                 \"{CATCH_ALL}\".",
                a.local
            ));
        }
        if !seen.insert(a.local.clone()) {
            out.push(format!("The address \"{}\" is listed twice.", a.local));
        }
        if let Some(f) = &a.forward
            && !valid_destination(f)
        {
            out.push(format!("\"{f}\" is not a forward destination address."));
        }
    }
    if let Some(f) = &s.forward
        && let Some(to) = &f.to
        && !valid_destination(to)
    {
        out.push(format!("\"{to}\" is not a forward destination address."));
    }
    out
}

/// The record with listed address `local` added (or removed when `remove`), its forward kept.
pub fn with_address(
    record: &Instance,
    local: &str,
    remove: bool,
) -> std::result::Result<Instance, String> {
    let local = local.trim().to_ascii_lowercase();
    if !valid_local(&local) {
        return Err(format!("\"{local}\" is not a listed address"));
    }
    let mut next = record.clone();
    let s = next
        .capabilities
        .get_mut(NAME)
        .ok_or("email is not enabled on this instance")?;
    let at = s.addresses.iter().position(|a| a.local == local);
    match (at, remove) {
        (Some(_), false) => return Err(format!("{local} is already listed")),
        (None, true) => return Err(format!("{local} is not listed")),
        (Some(i), true) => {
            s.addresses.remove(i);
        }
        (None, false) => s.addresses.push(Address {
            local,
            forward: None,
        }),
    }
    Ok(next)
}

/// The record with a forward changed: `local`'s destination (or, with no `local`, the default
/// `to`) set to `to` (`None` clears it), and the scope when given.
pub fn with_forward(
    record: &Instance,
    local: Option<&str>,
    to: Option<&str>,
    scope: Option<ForwardScope>,
) -> std::result::Result<Instance, String> {
    let to = to
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty());
    if let Some(t) = &to
        && !valid_destination(t)
    {
        return Err(format!("\"{t}\" is not a forward destination address"));
    }
    let mut next = record.clone();
    let s = next
        .capabilities
        .get_mut(NAME)
        .ok_or("email is not enabled on this instance")?;
    match local.map(|l| l.trim().to_ascii_lowercase()) {
        Some(l) => {
            let a = s
                .addresses
                .iter_mut()
                .find(|a| a.local == l)
                .ok_or_else(|| format!("{l} is not listed: add it with data_email_address_set"))?;
            a.forward = to;
        }
        None => {
            let f = s.forward.get_or_insert_with(Forward::default);
            if to.is_some() || scope.is_none() {
                f.to = to;
            }
        }
    }
    if let Some(scope) = scope {
        s.forward.get_or_insert_with(Forward::default).scope = Some(scope);
    }
    if s.forward == Some(Forward::default()) {
        s.forward = None;
    }
    Ok(next)
}

/// The instance tools and the Email tab while the record enables mail.
const TOOLS: &[Registration] = &[
    Registration {
        name: "data_email_status",
        description: Some("This instance's mail: setup state, counts and forwarding failures."),
        arguments: &[],
    },
    Registration {
        name: "data_email_mailboxes",
        description: Some("This instance's mailboxes with their counts."),
        arguments: &[],
    },
    Registration {
        name: "data_email_list",
        description: Some("A page of this instance's mail, newest first."),
        arguments: &["mailbox", "since", "from", "cursor", "limit"],
    },
    Registration {
        name: "data_email_get",
        description: Some("One message of this instance's mail."),
        arguments: &["id"],
    },
    Registration {
        name: "data_email_address_set",
        description: Some("Propose adding or removing a listed address on this instance."),
        arguments: &["local", "remove"],
    },
    Registration {
        name: "data_email_forward_set",
        description: Some("Propose a change to this instance's mail forwarding."),
        arguments: &["local", "to", "scope"],
    },
];

const VIEWS: &[View] = &[View {
    area: "email",
    id: "mailboxes",
    title: "Mailboxes",
    order: 10,
    since: None,
}];

/// The tools and the Email tab, on an instance whose record enables mail.
pub fn instance_tools(record: &Instance) -> serde_json::Result<Enabled> {
    Ok(Enabled::when(enabled(record), TOOLS, VIEWS))
}

/// The capability, hosted in core.
pub struct Email;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> Tool {
    Tool {
        name,
        description,
        input_schema: json!({ "type": "object", "properties": properties, "required": required }),
    }
}

impl Email {
    async fn record(&self, cx: &Context<'_>, args: &Value) -> Result<Instance> {
        let id = arg(args, "instance")?;
        crate::instances::one(cx.env, id)
            .await?
            .ok_or_else(|| bad(format!("there is no instance {id}")))
    }

    /// The record, refusing when mail is off with what to do.
    async fn on(&self, cx: &Context<'_>, args: &Value) -> Result<Instance> {
        let record = self.record(cx, args).await?;
        if !enabled(&record) {
            return Err(bad(format!(
                "email is not set up for {}. {}",
                record.id,
                crate::data_store::SETUP
            )));
        }
        Ok(record)
    }

    async fn status(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let record = self.record(cx, args).await?;
        let Some(config) = config(&record) else {
            return Ok(json!({
                "instance": record.id,
                "enabled": false,
                "setup": crate::data_store::SETUP,
            }));
        };
        let live = crate::data_store::call(cx.env, &record, DataCall::Status).await;
        let (store, note) = match live {
            Ok(v) => {
                let current = v["config_digest"].as_str() == Some(config.digest().as_str());
                let note = (!current).then_some(
                    "the data Worker's configuration differs from the record: run instance_plan",
                );
                (v, note)
            }
            Err(e) => (Value::Null, Some(leak_free(&e))),
        };
        Ok(json!({
            "instance": record.id,
            "enabled": true,
            "domain": config.domain,
            "store": crate::data_store::store_name(&record),
            "mailboxes": mailbox_names(&config),
            "forward": {
                "scope": config.scope.as_str(),
                "to": config.forward_to,
                "listed": config.addresses.iter().map(|a| json!({ "local": a.local, "forward": a.forward })).collect::<Vec<_>>(),
            },
            "live": store,
            "note": note,
        }))
    }

    async fn mailboxes(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let record = self.on(cx, args).await?;
        let config = config(&record).ok_or_else(|| bad("email has no domain"))?;
        let live = crate::data_store::call(cx.env, &record, DataCall::Mailboxes).await?;
        Ok(
            json!({ "instance": record.id, "mailboxes": merge_mailboxes(&config, &live["mailboxes"]) }),
        )
    }

    async fn list(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let record = self.on(cx, args).await?;
        let text = |k: &str| {
            args[k]
                .as_str()
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        let limit = match &args["limit"] {
            Value::Null => None,
            v => Some(
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| bad("`limit` is a number"))?,
            ),
        };
        let q = ListQuery {
            mailbox: text("mailbox"),
            since: text("since"),
            from: text("from"),
            cursor: text("cursor"),
            limit,
        };
        let mut out = crate::data_store::call(cx.env, &record, DataCall::List(q)).await?;
        out["instance"] = json!(record.id);
        Ok(out)
    }

    async fn get(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let record = self.on(cx, args).await?;
        let id = arg(args, "id")?.to_ascii_lowercase();
        let out = crate::data_store::call(cx.env, &record, DataCall::Get { id }).await?;
        if out.is_null() {
            return Err(bad("there is no such message"));
        }
        Ok(out)
    }

    /// Proposes `next` as the instance's record (`instance_propose`), for the person to
    /// approve with their passkey. Run as the platform's own tool, outside the instance
    /// endpoint's scope, which does not offer `instance_propose`.
    async fn propose(&self, cx: &Context<'_>, next: Instance) -> Result<Value> {
        let global = Context {
            instance: None,
            ..*cx
        };
        crate::dispatch(&global, "instance_propose", &json!({ "record": next })).await
    }

    async fn address_set(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let record = self.on(cx, args).await?;
        let remove = args["remove"].as_bool().unwrap_or(false);
        let next = with_address(&record, arg(args, "local")?, remove).map_err(bad)?;
        self.propose(cx, next).await
    }

    async fn forward_set(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let record = self.on(cx, args).await?;
        let scope = match args["scope"].as_str() {
            None => None,
            Some("listed") => Some(ForwardScope::Listed),
            Some("all") => Some(ForwardScope::All),
            Some(other) => return Err(bad(format!("scope is listed or all, not {other}"))),
        };
        let to = args["to"].as_str();
        if to.is_none() && scope.is_none() && args.get("to").is_none() {
            return Err(bad("give `to` (null to clear) or `scope`"));
        }
        let next = with_forward(&record, args["local"].as_str(), to, scope).map_err(bad)?;
        self.propose(cx, next).await
    }
}

/// An error's text with nothing from the data Worker's body.
fn leak_free(e: &Error) -> &'static str {
    if e.to_string().contains("has not been deployed") {
        "the data Worker has not been deployed: run instance_plan and instance_apply"
    } else {
        "the data Worker did not answer"
    }
}

/// The mailboxes `config` defines: each listed address, then catch-all.
pub fn mailbox_names(config: &EmailConfig) -> Vec<String> {
    config
        .addresses
        .iter()
        .map(|a| a.local.clone())
        .chain(std::iter::once(CATCH_ALL.to_string()))
        .collect()
}

/// Every mailbox of `config` with its count and newest time from the store's `rows`
/// (`{mailbox, count, newest}`), zero when it has none; a mailbox the store has that the record
/// no longer lists is kept, marked `unlisted`.
pub fn merge_mailboxes(config: &EmailConfig, rows: &Value) -> Vec<Value> {
    let rows = rows.as_array().cloned().unwrap_or_default();
    let find = |name: &str| rows.iter().find(|r| r["mailbox"].as_str() == Some(name));
    let mut out: Vec<Value> = mailbox_names(config)
        .iter()
        .map(|name| {
            let r = find(name);
            json!({
                "mailbox": name,
                "address": (name != CATCH_ALL).then(|| format!("{name}@{}", config.domain)),
                "count": r.and_then(|r| r["count"].as_u64()).unwrap_or(0),
                "newest": r.map(|r| r["newest"].clone()).unwrap_or(Value::Null),
                "forward": config.forward_for(name),
            })
        })
        .collect();
    for r in &rows {
        let name = r["mailbox"].as_str().unwrap_or_default();
        if !name.is_empty() && !out.iter().any(|m| m["mailbox"] == name) {
            out.push(json!({
                "mailbox": name, "count": r["count"], "newest": r["newest"], "unlisted": true,
            }));
        }
    }
    out
}

impl Capability for Email {
    fn name(&self) -> &'static str {
        NAME
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![
            pigeon_core::versions::Component::began(NAME, pigeon_core::versions::Kind::Capability)
                .breaking(
                    "2.0.0",
                    "Renamed from email: the capability is data-email, its settings sit at \
                     the record's data.email, and its tools are data_email_*.",
                    "Call data_email_status, data_email_list, data_email_get, \
                     data_email_mailboxes, data_email_address_set and data_email_forward_set \
                     instead of email_*; stored records move to data.email on deploy.",
                ),
        ]
    }

    fn tools(&self) -> Vec<Tool> {
        let instance =
            json!({ "type": "string", "description": "The instance, such as `pigeon.dev`." });
        vec![
            tool(
                "data_email_status",
                "An instance's mail: whether it is set up (or the steps to set it up), its \
                 domain, mailboxes and forwarding, and the data Worker's counts.",
                json!({ "instance": instance }),
                &["instance"],
            ),
            tool(
                "data_email_mailboxes",
                "An instance's mailboxes: each listed address and catch-all, with its count and \
                 newest message.",
                json!({ "instance": instance }),
                &["instance"],
            ),
            tool(
                "data_email_list",
                "A page of an instance's mail, newest first: id, from, subject, date, sender \
                 check and attachment count. Never bodies.",
                json!({
                    "instance": instance,
                    "mailbox": { "type": "string", "description": "A listed address's local part, or `catch-all`." },
                    "since": { "type": "string", "description": "RFC 3339: messages received at or after it." },
                    "from": { "type": "string", "description": "A sender address or domain." },
                    "cursor": { "type": "string", "description": "`next` from the page before." },
                    "limit": { "type": "integer", "description": "1 to 200; default 50." },
                }),
                &["instance"],
            ),
            tool(
                "data_email_get",
                "One message: headers, text, the HTML marked for sandboxed rendering, and its \
                 attachments with their keys in the data store.",
                json!({
                    "instance": instance,
                    "id": { "type": "string", "description": "The message id (its SHA-256) from data_email_list." },
                }),
                &["instance", "id"],
            ),
            tool(
                "data_email_address_set",
                "Propose adding (or with `remove`, removing) a listed address, its own mailbox. \
                 A record change the person approves; never a DNS or routing change.",
                json!({
                    "instance": instance,
                    "local": { "type": "string", "description": "The local part, such as `cloudflare`." },
                    "remove": { "type": "boolean" },
                }),
                &["instance", "local"],
            ),
            tool(
                "data_email_forward_set",
                "Propose a forwarding change: a listed address's destination (`local` and `to`), \
                 the default destination (`to` alone; null clears it), or the scope (`listed`, the \
                 default, or `all` to include catch-all). Each call needs the person's passkey.",
                json!({
                    "instance": instance,
                    "local": { "type": "string" },
                    "to": { "type": ["string", "null"], "description": "A destination address; Cloudflare emails it a link to verify." },
                    "scope": { "type": "string", "enum": ["listed", "all"] },
                }),
                &["instance"],
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if let Some(why) = ungated(tool, crate::approvals::gated) {
                return Err(bad(why));
            }
            match tool {
                "data_email_status" => self.status(cx, args).await,
                "data_email_mailboxes" => self.mailboxes(cx, args).await,
                "data_email_list" => self.list(cx, args).await,
                "data_email_get" => self.get(cx, args).await,
                "data_email_address_set" => self.address_set(cx, args).await,
                "data_email_forward_set" => self.forward_set(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }
}
