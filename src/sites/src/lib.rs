//! pigeon-sites: the sites Worker (docs/plans/workers-split.md). It renders consumer
//! repositories with the template their `site.toml` names ([`templates`]: [`pigeon_blog`] or
//! [`pigeon_landing`], the version an instance's pin selects) and deploys instance sites
//! ([`deploy`]).
//! A capability Worker: it has no public surface, and answers only core over a Service Binding,
//! through [`pigeon_contract::serve`], which checks the caller core signed. `site_deploy` and
//! `site_rollback` require signed instance context matching their `instance` ([`context`]); a
//! deploy asks core for what it must not hold itself (the approved record, a linked account's
//! token, a GitHub installation token) only through the instance-scoped `sites_scope` callback
//! over its `PLATFORM` binding. For the home instance, whose bucket only core may write, core
//! asks the private `site_build` ([`templates::site_build`]) to render a `landing@1` release and
//! stages it itself (docs/plans/platform-self-hosting.md, L3). Built
//! for `wasm32-unknown-unknown` only.

mod context;
mod deploy;
pub mod public;
mod queued;
mod templates;
mod versions;

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-sites builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use std::collections::BTreeSet;

use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, Service, Signed,
    ToolSpec,
};
use pigeon_core::versions::{Requirement, Version};
use serde_json::{Value, json};
use worker::{Context, Env, Request, Response, Result, event};

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-sites";

/// The keys this Worker uses (docs/plans/caller-keys.md, step 3). No `Debug`: they are secrets.
pub struct SiteKeys {
    /// Its own caller key (`CALLER_KEY_OWN`), which signs every ask of core.
    pub sign: Vec<u8>,
    /// `SITE_KEY` ([`pigeon_core::site_auth::SITE_KEY`]), only to derive the keys core derives
    /// from it too: a protected site's origin key (`site_auth::origin_key`, also derived by
    /// core's instance apply) and its sign-in session key. It signs nothing
    /// (docs/plans/caller-keys.md, step 4).
    pub site: Vec<u8>,
}

/// [`SiteKeys`]; an error naming what is missing. Never falls back to another key.
async fn site_keys(env: &Env) -> std::result::Result<SiteKeys, String> {
    let sign = pigeon_contract::own_key(env)
        .await
        .map_err(|e| e.to_string())?;
    let site = pigeon_core::secrets_store::site_key(env)
        .await
        .map_err(|e| e.to_string())?;
    Ok(SiteKeys { sign, site })
}

/// The sites capability.
struct Sites;

/// `pairs` (`[[name, text], ...]` or `[{name, text}, ...]`) as file names and texts.
fn files(pairs: &Value) -> std::result::Result<Vec<(String, String)>, String> {
    let Some(list) = pairs.as_array() else {
        return Ok(Vec::new());
    };
    list.iter()
        .map(|p| {
            let (name, text) = match p {
                Value::Array(a) => (a.first(), a.get(1)),
                Value::Object(_) => (p.get("name"), p.get("text")),
                _ => (None, None),
            };
            match (name.and_then(Value::as_str), text.and_then(Value::as_str)) {
                (Some(n), Some(t)) => Ok((n.to_string(), t.to_string())),
                _ => Err("each file is [name, text] or {name, text}".to_string()),
            }
        })
        .collect()
}

/// `site_render`: a consumer repository's content rendered with the template its `site.toml`
/// names, at the version `pin` selects, answering what the
/// site would hold (every path, its size and type, and the template's digest) or the problems
/// with the content. Nothing is written anywhere.
fn render(args: &Value) -> std::result::Result<Value, String> {
    let text = |k: &str| args[k].as_str().unwrap_or_default().to_string();
    let statics: BTreeSet<String> = args["statics"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let input = templates::Parts {
        site_toml: text("site_toml"),
        posts: files(&args["posts"])?,
        pages: files(&args["pages"])?,
        index: args["index"].as_str().map(str::to_string),
        statics,
        base_url: text("base_url"),
        commit: text("commit"),
        commit_year: args["commit_year"]
            .as_u64()
            .and_then(|y| u16::try_from(y).ok())
            .unwrap_or(1970),
    };
    let pin = match args["pin"].as_str() {
        Some(p) => Some(Requirement::parse(p).map_err(|e| format!("`pin` `{p}`: {e}"))?),
        None => None,
    };
    let allows = |_: templates::Template, v: &str| {
        pin.as_ref()
            .is_none_or(|r| Version::parse(v).is_ok_and(|v| r.matches(&v)))
    };
    Ok(match templates::render(&input, &allows) {
        Ok(out) => json!({
            "template": out.template,
            "version": out.version,
            "digest": out.digest,
            "files": out.files.iter().map(|(path, (bytes, kind))| {
                json!({ "path": path, "bytes": bytes.len(), "type": kind })
            }).collect::<Vec<_>>(),
            "statics": out.statics,
        }),
        Err(problems) => json!({
            "problems": problems.iter().map(|p| json!({ "file": p.file, "message": p.message }))
                .collect::<Vec<_>>(),
        }),
    })
}

impl Service for Sites {
    fn manifest(&self) -> Manifest {
        Manifest {
            contract: CONTRACT,
            worker: WORKER.into(),
            commit: option_env!("PIGEON_COMMIT").unwrap_or_default().into(),
            capabilities: vec![CapabilityManifest {
                name: "sites".into(),
                tools: vec![
                    ToolSpec {
                        name: "site_render".into(),
                        description:
                            "Render a consumer repository's content with the template its site.toml names \
                                  (blog or landing), at the version the pin selects, \
                                  as a dry run: every path the site would hold, with its size \
                                  and type, and the template's digest; or the problems with \
                                  the content. Nothing is written. Served by pigeon-sites."
                                .into(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "site_toml": { "type": "string", "description": "The text of .pigeon/site.toml." },
                                "posts": { "type": "array", "items": { "type": "array", "items": { "type": "string" } }, "description": "content/posts/*.md as [name, text] pairs." },
                                "pages": { "type": "array", "items": { "type": "array", "items": { "type": "string" } }, "description": "content/pages/*.md as [name, text] pairs." },
                                "index": { "type": "string", "description": "The text of content/index.toml, the landing template's home page." },
                                "statics": { "type": "array", "items": { "type": "string" }, "description": "Paths under static/." },
                                "base_url": { "type": "string", "description": "Such as https://example.com." },
                                "commit": { "type": "string", "description": "The short commit, for the footer." },
                                "commit_year": { "type": "integer" },
                                "pin": { "type": "string", "description": "A version requirement such as ^1 or =1.0.0; the newest carried version when omitted." }
                            },
                            "required": ["site_toml"]
                        }),
                    },
                    ToolSpec {
                        name: "site_deploy".into(),
                        description:
                            "Deploy an instance's site: its GitHub repository at `rev` (a \
                                  commit or branch; main when omitted), rendered with the template its site.toml names (blog or landing), at the version the instance's pin selects \
                                  and written to the bucket its record names, then made live. \
                                  Only adds objects and moves `live`, so the previous deploy \
                                  stays whole. Answers the commit and counts, or the content's \
                                  problems. Served by pigeon-sites."
                                .into(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "instance": { "type": "string" },
                                "rev": { "type": "string", "description": "A commit or branch; main when omitted." }
                            },
                            "required": ["instance"]
                        }),
                    },
                    ToolSpec {
                        name: "site_releases".into(),
                        description: "Read-only. An instance's immutable site releases and the \
                                      one live: each one's commit and template, live first. \
                                      Roll back to one with site_rollback."
                            .into(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "instance": { "type": "string" },
                                "limit": { "type": "integer", "description": "20 by default, at most 50." }
                            },
                            "required": ["instance"]
                        }),
                    },
                    ToolSpec {
                        name: "site_rollback".into(),
                        description: "Activate a previously published immutable release of an instance's site.".into(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {"instance": {"type": "string"}, "release": {"type": "string"}},
                            "required": ["instance", "release"]
                        }),
                    },
                ],
                versions: versions::declared(),
                ..CapabilityManifest::default()
            }],
        }
    }

    fn call<'a>(
        &'a self,
        env: &'a Env,
        caller: &'a Signed,
        req: CallRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move {
            match req.tool.as_str() {
                queued::TOOL => {
                    let receipt = match queued::Receipt::read(caller, &req.args) {
                        Ok(receipt) => receipt,
                        Err(why) => return Answer::error(FailureKind::Unauthorized, why),
                    };
                    let keys = match site_keys(env).await {
                        Ok(keys) => keys,
                        Err(why) => return Answer::error(FailureKind::Failed, why),
                    };
                    match deploy::deploy_queued(env, &keys, caller, &receipt).await {
                        Ok(value) => Answer::Ok(value),
                        Err(error) => Answer::error(FailureKind::Failed, error.to_string()),
                    }
                }
                "site_deploy" | "site_rollback" => {
                    if req.tool == "site_deploy"
                        && let Err(why) = queued::refuse_public_binding(&req.args)
                    {
                        return Answer::error(FailureKind::BadRequest, why);
                    }
                    if req.args["instance"].as_str().is_none() {
                        return Answer::error(FailureKind::BadRequest, "`instance` is required");
                    }
                    let instance = match context::require(caller, &req.args) {
                        Ok(instance) => instance,
                        Err(why) => return Answer::error(FailureKind::Unauthorized, why),
                    };
                    let rev = req.args["rev"].as_str().unwrap_or("main");
                    if rev.is_empty()
                        || !rev
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
                    {
                        return Answer::error(
                            FailureKind::BadRequest,
                            "`rev` must be a commit or branch",
                        );
                    }
                    let keys = match site_keys(env).await {
                        Ok(keys) => keys,
                        Err(why) => return Answer::error(FailureKind::Failed, why),
                    };
                    let result = if req.tool == "site_rollback" {
                        let Some(release) = req.args["release"].as_str() else {
                            return Answer::error(FailureKind::BadRequest, "`release` is required");
                        };
                        deploy::rollback(env, &keys, caller, instance, release).await
                    } else {
                        deploy::deploy(env, &keys, caller, instance, rev).await
                    };
                    match result {
                        Ok(v) => Answer::Ok(v),
                        Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
                    }
                }
                "site_releases" => {
                    if req.args["instance"].as_str().is_none() {
                        return Answer::error(FailureKind::BadRequest, "`instance` is required");
                    }
                    // Like deploy and rollback: only for the instance the call is signed for,
                    // reading through the typed sites_scope callbacks.
                    let instance = match context::require(caller, &req.args) {
                        Ok(instance) => instance,
                        Err(why) => return Answer::error(FailureKind::Unauthorized, why),
                    };
                    let limit = req.args["limit"].as_u64().unwrap_or(20) as usize;
                    let keys = match site_keys(env).await {
                        Ok(keys) => keys,
                        Err(why) => return Answer::error(FailureKind::Failed, why),
                    };
                    match deploy::releases(env, &keys, caller, instance, limit).await {
                        Ok(v) => Answer::Ok(v),
                        Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
                    }
                }
                templates::BUILD_TOOL => {
                    // Core's private call for the home instance's release: pure, and only for
                    // the instance the call is signed for.
                    if let Err(why) = context::require(caller, &req.args) {
                        return Answer::error(FailureKind::Unauthorized, why);
                    }
                    match templates::site_build(&req.args) {
                        Ok(v) => Answer::Ok(v),
                        Err(why) => Answer::error(FailureKind::BadRequest, why),
                    }
                }
                "site_render" => match render(&req.args) {
                    Ok(v) => Answer::Ok(v),
                    Err(why) => Answer::error(FailureKind::BadRequest, why),
                },
                other => Answer::error(FailureKind::UnknownTool, format!("no tool `{other}`")),
            }
        })
    }
}

/// Names this Worker (`sites` in ci/workers.toml) on every request it signs to core, before any
/// fetch or Durable Object code runs, so core checks it with CALLER_KEY_SITES first
/// (docs/plans/caller-keys.md, step 2). worker-build also exposes it as an entrypoint method of
/// this name, reachable only over a service binding; calling it again changes nothing.
#[event(start)]
fn identify_worker() {
    pigeon_contract::identify("sites");
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // Its own key signs every ask of core and checks core's calls first; the shared key checks
    // them second. SITE_KEY derives protected sites' keys. Without its own key or SITE_KEY it
    // serves nothing (docs/plans/caller-keys.md, steps 3 and 4).
    let keys = match site_keys(&env).await {
        Ok(keys) => keys,
        Err(why) => {
            let answer = Answer::error(FailureKind::Failed, format!("pigeon-sites: {why}"));
            return Ok(Response::from_json(&answer)?.with_status(503));
        }
    };
    // A platform subdomain, such as noisypigeon.pigeon.dev, from the internet; core's calls
    // over the binding come as https://capability/… and go to the contract.
    let host = req
        .url()?
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let zone = env.var("ZONE")?.to_string();
    if host == zone.to_ascii_lowercase() {
        // Since S1 the apex is core's (pigeon-platform); this Worker no longer serves it.
        return Response::error("Not Found", 404);
    }
    if host.ends_with(&format!(".{zone}")) {
        return public::serve(req, &env, &keys, WORKER, &host).await;
    }
    let contract = pigeon_contract::Keys {
        own: keys.sign,
        shared: pigeon_contract::keys::shared_key(&env).await?,
    };
    pigeon_contract::serve(req, &env, &Sites, &contract).await
}
