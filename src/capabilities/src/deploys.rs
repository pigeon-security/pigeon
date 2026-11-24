//! Deploys by hand or by an agent: short-lived Cloudflare API tokens for `wrangler deploy`,
//! minted from the parent token and revoked when the deploy is done, so no deploy credential is
//! ever stored.

use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{self, MintRequest, Policy, Scope};
use serde::Serialize;
use serde_json::{Value, json};
use worker::Result;

use crate::{Call, Capability, Context, arg, to_value, unknown};

/// What a deploy token holds unless asked otherwise: what `wrangler deploy` needs for the
/// account's Workers. Binding a Secrets Store secret needs Write.
pub const PERMISSIONS: [&str; 3] = [
    "Workers Scripts Write",
    "Account Settings Read",
    "Secrets Store Write",
];

/// The presets `deploy_token` takes as `{"preset": name}`. `deploy` holds every permission the
/// deploy proxy's allowlist names ([`pigeon_proxies::cf_proxy::ALLOWED`]), so a direct
/// `wrangler deploy` of any platform Worker, containers and queues included, needs nothing more;
/// plus Account Settings Read from [`PERMISSIONS`], which a direct deploy held when it worked.
pub const PRESETS: [&str; 1] = ["deploy"];

/// The longest lifetime a deploy token may have, in seconds.
const TTL_MAX: u64 = 3_600;

/// The permissions of preset `name`, in the allowlist's order without repeats, then Account
/// Settings Read; `None` for an unknown preset. Derived from the deploy proxy's table, so the
/// two cannot drift.
pub fn preset(name: &str) -> Option<Vec<&'static str>> {
    if name != PRESETS[0] {
        return None;
    }
    let mut out: Vec<&'static str> = Vec::new();
    let table = pigeon_proxies::cf_proxy::ALLOWED
        .iter()
        .flat_map(|entry| entry.permissions.iter());
    for p in table.chain(std::iter::once(&"Account Settings Read")) {
        if !out.contains(p) {
            out.push(p);
        }
    }
    Some(out)
}

/// What a `deploy_token` call would mint: its permissions and lifetime in seconds. Refuses a
/// call naming both a preset and permissions, or an unknown preset.
pub fn grants(args: &Value) -> std::result::Result<(Vec<String>, u64), String> {
    let ttl = args["ttl"].as_u64().unwrap_or(1_800).min(TTL_MAX);
    let permissions = match (args["preset"].as_str(), args["permissions"].as_array()) {
        (Some(_), Some(_)) => {
            return Err("deploy_token takes a preset or permissions, not both".into());
        }
        (Some(name), None) => preset(name)
            .ok_or_else(|| {
                format!(
                    "deploy_token has no preset `{name}`; presets: {}",
                    PRESETS.join(", ")
                )
            })?
            .into_iter()
            .map(String::from)
            .collect(),
        (None, Some(a)) => a
            .iter()
            .filter_map(|p| p.as_str().map(String::from))
            .collect(),
        (None, None) => PERMISSIONS.map(String::from).to_vec(),
    };
    Ok((permissions, ttl))
}

/// Deploy tokens.
pub struct Deploys;

/// A Cloudflare API token handed to the caller, which is behind Access.
#[derive(Serialize)]
struct ApiToken {
    id: String,
    token: String,
    expires_on: String,
    account_id: String,
}

async fn mint(cx: &Context<'_>, args: &Value) -> Result<ApiToken> {
    let (permissions, ttl_secs) = grants(args).map_err(worker::Error::RustError)?;
    let names: Vec<&str> = permissions.iter().map(String::as_str).collect();
    let request = MintRequest {
        purpose: "deploy".into(),
        policies: vec![Policy::new(Scope::Account, &names)],
        ttl_secs,
    };
    let minted = tokens::mint_for(cx.account, &request).await?;
    Ok(ApiToken {
        id: minted.id,
        token: minted.value,
        expires_on: minted.expires_on,
        account_id: cx.account.id.clone(),
    })
}

impl Capability for Deploys {
    fn name(&self) -> &'static str {
        "deploys"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "deploys",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "deploy_token",
                description: "Mint a short-lived Cloudflare API token for `wrangler deploy` of \
                              this account's Workers. For a break-glass deploy pass \
                              {\"preset\": \"deploy\"}: every permission the deploy proxy's \
                              allowlist names, containers and queues included. Without a preset \
                              or permissions: Workers Scripts Write, Account Settings Read and \
                              Secrets Store Write. Pass it as CLOUDFLARE_API_TOKEN, then revoke \
                              it with revoke_api_token.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ttl": { "type": "integer", "description": "Seconds; 1800 by default, 3600 at most." },
                        "preset": {
                            "type": "string",
                            "enum": PRESETS,
                            "description": "A named set of permissions, instead of `permissions`: `deploy` is what the deploy proxy's allowlist needs."
                        },
                        "permissions": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Account-scoped permission group names, instead of the default."
                        }
                    }
                }),
            },
            Tool {
                name: "revoke_api_token",
                description: "Revoke a Cloudflare API token minted by deploy_token, by id.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "deploy_token" => to_value(mint(cx, args).await?),
                "revoke_api_token" => {
                    let id = arg(args, "id")?;
                    tokens::revoke(&cx.account.parent, &cx.account.id, id).await?;
                    json!({ "revoked": id })
                }
                other => return Err(unknown(other)),
            })
        })
    }
}
