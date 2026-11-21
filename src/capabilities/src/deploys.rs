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
    let permissions: Vec<String> = args["permissions"].as_array().map_or_else(
        || PERMISSIONS.map(String::from).to_vec(),
        |a| {
            a.iter()
                .filter_map(|p| p.as_str().map(String::from))
                .collect()
        },
    );
    let names: Vec<&str> = permissions.iter().map(String::as_str).collect();
    let request = MintRequest {
        purpose: "deploy".into(),
        policies: vec![Policy::new(Scope::Account, &names)],
        ttl_secs: args["ttl"].as_u64().unwrap_or(1_800).min(3_600),
    };
    let minted = tokens::mint(&cx.account.parent, &cx.account.id, &request).await?;
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

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "deploy_token",
                description: "Mint a short-lived Cloudflare API token for `wrangler deploy` of \
                              this account's Workers: by default Workers Scripts Write, Account \
                              Settings Read and Secrets Store Write. Pass it as \
                              CLOUDFLARE_API_TOKEN, then revoke it with revoke_api_token.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ttl": { "type": "integer", "description": "Seconds; 1800 by default, 3600 at most." },
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
