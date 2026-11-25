//! pigeon-sites: the sites Worker (docs/plans/workers-split.md). It renders consumer
//! repositories with the blog template ([`pigeon_blog`], the version an instance's pin selects) and deploys instance sites
//! ([`deploy`]).
//! A capability Worker: it has no public surface, and answers only core over a Service Binding,
//! through [`pigeon_contract::serve`], which checks the caller core signed. It asks core for
//! what it must not hold itself (a linked account's token) over its `PLATFORM` binding. Built
//! for `wasm32-unknown-unknown` only.

mod apex;
mod deploy;
pub mod public;
mod queued;
mod versions;

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-sites builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use std::collections::BTreeSet;

use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, Service, Signed,
    ToolSpec,
};
use pigeon_core::secrets_store;
use pigeon_core::versions::{Requirement, Version};
use serde_json::{Value, json};
use worker::{Context, Env, Request, Response, Result, event};

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-sites";
/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";

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

/// `site_render`: a consumer repository's content rendered with the blog template version `pin` selects, answering what the
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
    let input = pigeon_blog::Input {
        site_toml: text("site_toml"),
        posts: files(&args["posts"])?,
        pages: files(&args["pages"])?,
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
    let allows = |v: &str| {
        pin.as_ref()
            .is_none_or(|r| Version::parse(v).is_ok_and(|v| r.matches(&v)))
    };
    Ok(match pigeon_blog::render_pinned(&input, &allows) {
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
                            "Render a consumer repository's content with the blog template version the pin selects, \
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
                                  commit or branch; main when omitted), rendered with the blog template version the instance's pin selects \
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
                    let key = call_key(env).await;
                    let Some(key) = key else {
                        return Answer::error(FailureKind::Failed, "CALLER_KEY is not set");
                    };
                    match deploy::deploy(
                        env,
                        key.as_bytes(),
                        caller,
                        receipt.instance,
                        receipt.revision,
                        Some(receipt.binding),
                    )
                    .await
                    {
                        Ok(value) => Answer::Ok(value),
                        Err(error) => Answer::error(FailureKind::Failed, error.to_string()),
                    }
                }
                "site_deploy" | "site_rollback" => {
                    let Some(instance) = req.args["instance"].as_str() else {
                        return Answer::error(FailureKind::BadRequest, "`instance` is required");
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
                    let key = call_key(env).await;
                    let Some(key) = key else {
                        return Answer::error(FailureKind::Failed, "CALLER_KEY is not set");
                    };
                    let result = if req.tool == "site_rollback" {
                        let Some(release) = req.args["release"].as_str() else {
                            return Answer::error(FailureKind::BadRequest, "`release` is required");
                        };
                        deploy::rollback(env, key.as_bytes(), caller, instance, release).await
                    } else {
                        let publishing_binding = req.args["publishing_binding"].as_str();
                        deploy::deploy(
                            env,
                            key.as_bytes(),
                            caller,
                            instance,
                            rev,
                            publishing_binding,
                        )
                        .await
                    };
                    match result {
                        Ok(v) => Answer::Ok(v),
                        Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
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

async fn call_key(env: &Env) -> Option<String> {
    let binding = env.secret_store(CALLER_KEY).ok()?;
    secrets_store::read(&binding).await.ok().flatten()
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let key = match env.secret_store(CALLER_KEY) {
        Ok(binding) => secrets_store::read(&binding).await?,
        Err(_) => None,
    };
    let Some(key) = key else {
        let answer = Answer::error(
            FailureKind::Failed,
            "pigeon-sites has no caller key: CALLER_KEY is not set",
        );
        return Ok(Response::from_json(&answer)?.with_status(503));
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
        return apex::serve(req, &env, key.as_bytes(), WORKER).await;
    }
    if host.ends_with(&format!(".{zone}")) {
        return public::serve(req, &env, key.as_bytes(), WORKER, &host).await;
    }
    pigeon_contract::serve(req, &env, &Sites, key.as_bytes()).await
}
