//! Passkeys for a site pigeon-sites serves (docs/plans/sites.md, part A). Core holds the
//! Directory, so it does the WebAuthn checks; pigeon-sites asks through the one tool here and
//! decides for itself who may come in. The relying party is the site's own host, so a passkey
//! made for one host cannot sign in on another.

pub mod console;

use pigeon_core::{mcp::Tool, site_auth};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::instances::record;
use crate::passkeys::Passkeys;
use crate::users::{User, directory};
use crate::{Call, Capability, Context, arg, unknown};

/// The only caller that may use `site_passkey`.
const CALLER: &str = "worker:pigeon-sites";

/// The site passkey capability.
pub struct SitePasskeys {
    /// The platform's own zone: a site's host is one label under it.
    pub zone: String,
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// The approved and applied binding, as seen by core at ceremony time.
pub(crate) async fn approved(
    env: &worker::Env,
    zone: &str,
    host: &str,
) -> Result<(String, site_auth::Config)> {
    if !host
        .strip_suffix(&format!(".{zone}"))
        .is_some_and(record::label)
    {
        return Err(bad("`host` must be a subdomain of the platform zone"));
    }
    let instances = crate::instances::all(env).await?;
    let (instance, site) = record::site_for_host(&instances, host, zone)
        .filter(|(_, site)| site.auth.as_deref() == Some("passkey"))
        .ok_or_else(|| bad("passkey access is not enabled for this site"))?;
    if !applied(env, host).await? {
        return Err(bad("this site's hostname has not been applied"));
    }
    Ok((
        site_auth::binding(&instance.id, &site.name, host),
        site_auth::Config {
            allow: site.allow.clone(),
        },
    ))
}

pub(crate) async fn applied(env: &worker::Env, host: &str) -> Result<bool> {
    let key = crate::instances::plan::subdomain_key(host);
    Ok(directory(env, "setting.get", json!({ "key": key }))
        .await?
        .is_string())
}

impl SitePasskeys {
    /// The user an enrolment code is for, when they may register a passkey on `host`: a
    /// standard user, or a site user of that same host. Another host's site user and service
    /// users are refused.
    async fn enrolling(
        &self,
        cx: &Context<'_>,
        host: &str,
        scope: &str,
        code: &str,
    ) -> Result<User> {
        let user: User = serde_json::from_value(
            directory(
                cx.env,
                "enrol.peek",
                json!({ "code": code, "site_scope": scope }),
            )
            .await?,
        )?;
        if user.kind == "standard" || user.kind == format!("site:{host}") {
            Ok(user)
        } else {
            Err(bad("this link is not for this site"))
        }
    }

    async fn run(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        if cx.caller.is_none_or(|c| c.user.id != CALLER) {
            return Err(bad("only pigeon-sites may use site_passkey"));
        }
        let host = arg(args, "host")?;
        let (scope, config) = approved(cx.env, &self.zone, host).await?;
        if args
            .get("site_scope")
            .is_some_and(|provided| provided != &scope)
        {
            return Err(bad("the site's approved binding changed; reload the page"));
        }
        let passkeys = Passkeys {
            host: host.to_string(),
            site_name: host.to_string(),
        };
        let purpose = format!("site:{scope}");
        Ok(match arg(args, "step")? {
            "sign_in_begin" => passkeys.sign_in_begin(cx.env, &purpose, None).await?,
            "sign_in_finish" => {
                let key = passkeys
                    .sign_in_finish(cx.env, &purpose, args, None)
                    .await?;
                let user: User = serde_json::from_value(
                    directory(cx.env, "users.get", json!({ "id": key.user_id })).await?,
                )
                .map_err(|_| bad("this passkey has no user"))?;
                if !site_auth::allowed(&config, host, &user.id, &user.kind)
                    || (user.kind != "standard"
                        && directory(cx.env, "users.site_scope", json!({ "id": user.id })).await?
                            != scope)
                {
                    return Err(bad("this passkey is not allowed here"));
                }
                json!({ "user": { "id": user.id, "kind": user.kind, "name": user.name }, "site_scope": scope })
            }
            "enrol_peek" => {
                let user = self.enrolling(cx, host, &scope, arg(args, "code")?).await?;
                json!({ "name": user.name })
            }
            "enrol_begin" => {
                self.enrolling(cx, host, &scope, arg(args, "code")?).await?;
                passkeys.enrol_begin_for(cx, args, Some(&scope)).await?
            }
            "enrol_finish" => {
                self.enrolling(cx, host, &scope, arg(args, "code")?).await?;
                passkeys.enrol_finish_for(cx, args, Some(&scope)).await?
            }
            other => return Err(bad(&format!("no step `{other}`"))),
        })
    }
}

impl Capability for SitePasskeys {
    fn name(&self) -> &'static str {
        "site_passkeys"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "site_passkeys",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "site_passkey",
            description: "Passkey ceremonies for a site pigeon-sites serves: sign_in_begin, \
                          sign_in_finish (answers the user's id, kind and name), enrol_peek, \
                          enrol_begin and enrol_finish. For pigeon-sites only.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "step": { "type": "string" },
                    "host": { "type": "string", "description": "The site's host, such as notes.pigeon.dev." },
                    "site_scope": { "type": "string", "description": "The site binding from the serving Worker's approved host map." },
                    "code": { "type": "string" },
                    "challenge_id": { "type": "string" },
                    "assertion": { "type": "object", "additionalProperties": true },
                    "credential": { "type": "object", "additionalProperties": true }
                },
                "required": ["step", "host"]
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "site_passkey" => self.run(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }
}
