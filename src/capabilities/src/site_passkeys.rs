//! Passkeys for a site pigeon-sites serves (docs/plans/sites.md, part A). Core holds the
//! Directory, so it does the WebAuthn checks; pigeon-sites asks through the one tool here and
//! decides for itself who may come in. The relying party is the site's own host, so a passkey
//! made for one host cannot sign in on another.

use pigeon_core::mcp::Tool;
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

impl SitePasskeys {
    /// `host` when it is one label under the platform zone.
    fn host<'a>(&self, host: &'a str) -> Result<&'a str> {
        let label = host
            .strip_suffix(&format!(".{}", self.zone))
            .filter(|l| record::label(l));
        label
            .map(|_| host)
            .ok_or_else(|| bad("`host` must be a subdomain of the platform zone"))
    }

    /// The user an enrolment code is for, when they may register a passkey on `host`: a
    /// standard user, or a site user of that same host. Another host's site user and service
    /// users are refused.
    async fn enrolling(&self, cx: &Context<'_>, host: &str, code: &str) -> Result<User> {
        let user: User = serde_json::from_value(
            directory(cx.env, "enrol.peek", json!({ "code": code })).await?,
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
        let host = self.host(arg(args, "host")?)?;
        let passkeys = Passkeys {
            host: host.to_string(),
            site_name: host.to_string(),
        };
        let purpose = format!("site:{host}");
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
                json!({ "user": { "id": user.id, "kind": user.kind, "name": user.name } })
            }
            "enrol_peek" => {
                let user = self.enrolling(cx, host, arg(args, "code")?).await?;
                json!({ "name": user.name })
            }
            "enrol_begin" => {
                self.enrolling(cx, host, arg(args, "code")?).await?;
                passkeys.enrol_begin(cx, args).await?
            }
            "enrol_finish" => {
                self.enrolling(cx, host, arg(args, "code")?).await?;
                passkeys.enrol_finish(cx, args).await?
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
                    "code": { "type": "string" },
                    "challenge_id": { "type": "string" },
                    "assertion": { "type": "object" },
                    "credential": { "type": "object" }
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
