//! Read-only DigitalOcean connection verification against an approved instance's team.

use pigeon_cloud::compute::is_digitalocean_team_id;
use pigeon_core::{crypto, mcp::Tool};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::console::write::Write;
use crate::instances::{
    self,
    record::Instance,
    tools::{Enabled, Registration},
};
use crate::{Call, Capability, Context, catalogue, users::directory};

pub mod health;
pub mod spaces;

pub const NAME: &str = "digitalocean";
pub const STATUS: &str = "digitalocean_status";
pub const VERIFY: &str = "digitalocean_verify";
pub const SET_TOKEN: &str = "digitalocean-token-set";
pub(crate) const TOKEN_KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;

const TOOLS: [Registration; 2] = [
    Registration {
        name: STATUS,
        description: None,
        arguments: &[],
    },
    Registration {
        name: VERIFY,
        description: None,
        arguments: &[],
    },
];

const SPACES_TOOLS: &[Registration] = &[
    TOOLS[0],
    TOOLS[1],
    Registration {
        name: spaces::STATUS,
        description: None,
        arguments: &[],
    },
    Registration {
        name: spaces::VERIFY,
        description: None,
        arguments: &[],
    },
];

const HEALTH_TOOL: Registration = Registration {
    name: health::STATUS,
    description: None,
    arguments: &[],
};
const ACCOUNT_AND_HEALTH: &[Registration] = &[TOOLS[0], TOOLS[1], HEALTH_TOOL];
const SPACES_AND_HEALTH: &[Registration] = &[
    SPACES_TOOLS[0],
    SPACES_TOOLS[1],
    SPACES_TOOLS[2],
    SPACES_TOOLS[3],
    HEALTH_TOOL,
];

pub fn instance_tools(record: &Instance) -> serde_json::Result<Enabled> {
    let tools = match (spaces::binding(record).is_some(), health::enabled(record)) {
        (true, true) => SPACES_AND_HEALTH,
        (true, false) => SPACES_TOOLS,
        (false, true) => ACCOUNT_AND_HEALTH,
        (false, false) => &TOOLS,
    };
    Ok(Enabled::when(team(record).is_some(), tools, &[]))
}

/// The approved identity, available only with both capabilities enabled.
pub fn team(record: &Instance) -> Option<&str> {
    if !catalogue::hosted_enabled(record, NAME)
        || !catalogue::hosted_enabled(record, crate::cloud_scope::NAME)
    {
        return None;
    }
    record
        .capabilities
        .get(NAME)?
        .team_id
        .as_deref()
        .filter(|id| is_digitalocean_team_id(id))
}

/// One fixed Directory setting per instance. The team IDs and ciphertext are not tool inputs.
pub fn token_key(instance: &str) -> String {
    format!("digitalocean-token:{instance}")
}

/// Bind the ciphertext to this purpose, instance, and team using unambiguous boundaries.
pub fn token_context(instance: &str, team_id: &str) -> Vec<u8> {
    format!("pigeon:digitalocean:v1\0{instance}\0{team_id}").into_bytes()
}

#[derive(Serialize, Deserialize)]
struct Credential {
    team_id: String,
    sealed: String,
}

fn bad(why: &str) -> Error {
    Error::RustError(why.into())
}

async fn approved(env: &Env, id: &str) -> Result<Instance> {
    instances::one(env, id)
        .await?
        .filter(|record| record.id == id && team(record).is_some())
        .ok_or_else(|| bad("DigitalOcean is not enabled on the approved instance"))
}

fn expected(record: &Instance) -> Result<&str> {
    team(record).ok_or_else(|| bad("DigitalOcean has no approved team"))
}

async fn credential(env: &Env, instance: &str) -> Result<Option<Credential>> {
    let value = directory(env, "setting.get", json!({ "key": token_key(instance) })).await?;
    value
        .as_str()
        .map(|s| {
            serde_json::from_str(s)
                .map_err(|_| bad("the saved DigitalOcean connection could not be read"))
        })
        .transpose()
}

/// Project only the public fields of the approved connection onto the status response.
pub fn connection_status(record: &Instance, saved_team: Option<&str>) -> Result<Value> {
    let expected = expected(record)?;
    let state = match saved_team {
        None => "missing",
        Some(team) if team == expected => "connected",
        Some(_) => "unverified",
    };
    Ok(json!({
        "instance": record.id,
        "enabled": true,
        "state": state,
        "team_id": expected
    }))
}

async fn status(record: &Instance, env: &Env) -> Result<Value> {
    let saved = credential(env, &record.id).await?;
    connection_status(record, saved.as_ref().map(|value| value.team_id.as_str()))
}

async fn open_pat(record: &Instance, env: &Env) -> Result<String> {
    let expected = expected(record)?;
    let saved = credential(env, &record.id)
        .await?
        .filter(|saved| saved.team_id == expected)
        .ok_or_else(|| bad("set a DigitalOcean token for the approved team before verifying"))?;
    let key = crate::linked::sealing_key(env).await?;
    let plaintext = crypto::open(&key, &saved.sealed, &token_context(&record.id, expected))
        .await
        .map_err(|_| bad("the saved DigitalOcean token could not be opened"))?;
    String::from_utf8(plaintext).map_err(|_| bad("the saved DigitalOcean token is invalid"))
}

async fn verify(record: &Instance, env: &Env) -> Result<Value> {
    let token = open_pat(record, env).await?;
    crate::cloud_scope::verify_digitalocean(record, &token).await?;
    Ok(json!({"instance": record.id, "verified": true}))
}

/// Compare the exact passkey target against the approved record before provider I/O.
pub fn matches_target(record: &Instance, target: &Value) -> bool {
    target["instance"].as_str() == Some(record.id.as_str())
        && target["version"].as_u64() == Some(record.version)
        && team(record).is_some_and(|id| target["team_id"].as_str() == Some(id))
}

async fn for_write(env: &Env, target: &Value) -> Result<Instance> {
    let id = target["instance"].as_str().unwrap_or_default();
    let record = approved(env, id).await?;
    if !matches_target(&record, target) {
        return Err(bad(
            "the approved DigitalOcean configuration changed; review it again",
        ));
    }
    Ok(record)
}

async fn save(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = for_write(cx.env, &w.target).await?;
    let expected = expected(&record)?;
    let token = w.change["value"].as_str().unwrap_or_default();
    if token.len() > 512 {
        return Err(bad("the DigitalOcean token is too long"));
    }
    let key = crate::linked::sealing_key(cx.env).await?;
    crate::cloud_scope::verify_digitalocean(&record, token).await?;
    let sealed = crypto::seal(&key, token.as_bytes(), &token_context(&record.id, expected)).await?;
    let value = serde_json::to_string(&Credential {
        team_id: expected.into(),
        sealed,
    })?;
    directory(
        cx.env,
        "digitalocean.token.put",
        json!({
            "target": w.target,
            "value": value,
        }),
    )
    .await?;
    Ok(json!({"instance": record.id, "verified": true}))
}

pub struct DigitalOcean;

impl Capability for DigitalOcean {
    fn name(&self) -> &'static str {
        NAME
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            NAME,
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                STATUS,
                "Show the approved instance's DigitalOcean connection status.",
            ),
            Tool::bare(
                VERIFY,
                "Verify the saved DigitalOcean token with a read-only account check.",
            ),
            Tool::bare(
                spaces::STATUS,
                "Show status for the approved DigitalOcean Spaces binding.",
            ),
            Tool::bare(
                health::STATUS,
                "Show the approved temporary Droplet health plan and state.",
            ),
            Tool::bare(
                spaces::VERIFY,
                "Verify the approved DigitalOcean Spaces binding with one bounded read.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            let scope = cx
                .instance
                .ok_or_else(|| bad("use the approved instance's MCP endpoint"))?;
            let record = approved(cx.env, scope.instance()).await?;
            match tool {
                STATUS => status(&record, cx.env).await,
                VERIFY => verify(&record, cx.env).await,
                spaces::STATUS => spaces::status(&record, cx.env).await,
                spaces::VERIFY => spaces::verify(&record, cx.env).await,
                health::STATUS => health::status(cx.env, &record).await,
                _ => Err(crate::unknown(tool)),
            }
        })
    }

    fn write_check<'a>(&'a self, cx: &'a Context<'a>, w: &'a Write<'a>) -> Call<'a> {
        Box::pin(async move {
            match w.action.id {
                spaces::SET_KEY => return spaces::check_write(cx, w).await,
                health::START | health::SET_TOKEN => return health::check_write(cx, w).await,
                _ => {}
            }
            for_write(cx.env, &w.target).await?;
            Ok(Value::Null)
        })
    }

    fn write_make<'a>(&'a self, cx: &'a Context<'a>, w: &'a Write<'a>) -> Call<'a> {
        Box::pin(async move {
            match w.action.id {
                spaces::SET_KEY => spaces::save(cx, w).await,
                health::START => health::start(cx, w).await,
                health::SET_TOKEN => health::save_token(cx, w).await,
                _ => save(cx, w).await,
            }
        })
    }
}
