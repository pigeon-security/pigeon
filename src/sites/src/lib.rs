//! pigeon-sites: the sites Worker (docs/plans/workers-split.md). It renders consumer
//! repositories with the `blog@1` template ([`pigeon_blog`]) and, next, deploys instance sites.
//! A capability Worker: it has no public surface, and answers only core over a Service Binding,
//! through [`pigeon_contract::serve`], which checks the caller core signed. It asks core for
//! what it must not hold itself (a linked account's token) over its `PLATFORM` binding. Built
//! for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-sites builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use std::collections::BTreeSet;

use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, Service, Signed,
    ToolSpec,
};
use pigeon_core::secrets_store;
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

/// `site_render`: a consumer repository's content rendered with `blog@1`, answering what the
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
    Ok(match pigeon_blog::render(&input) {
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
                tools: vec![ToolSpec {
                    name: "site_render".into(),
                    description: "Render a consumer repository's content with template blog@1, \
                                  as a dry run: every path the site would hold, with its size \
                                  and type, and the template's digest; or the problems with \
                                  the content. Nothing is written. Served by pigeon-sites."
                        .into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "site_toml": { "type": "string", "description": "The text of .pigeon/site.toml." },
                            "posts": { "type": "array", "description": "content/posts/*.md as [name, text] pairs." },
                            "pages": { "type": "array", "description": "content/pages/*.md as [name, text] pairs." },
                            "statics": { "type": "array", "items": { "type": "string" }, "description": "Paths under static/." },
                            "base_url": { "type": "string", "description": "Such as https://example.com." },
                            "commit": { "type": "string", "description": "The short commit, for the footer." },
                            "commit_year": { "type": "integer" }
                        },
                        "required": ["site_toml"]
                    }),
                }],
                ..CapabilityManifest::default()
            }],
        }
    }

    fn call<'a>(
        &'a self,
        _env: &'a Env,
        _caller: &'a Signed,
        req: CallRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move {
            match req.tool.as_str() {
                "site_render" => match render(&req.args) {
                    Ok(v) => Answer::Ok(v),
                    Err(why) => Answer::error(FailureKind::BadRequest, why),
                },
                other => Answer::error(FailureKind::UnknownTool, format!("no tool `{other}`")),
            }
        })
    }
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
    pigeon_contract::serve(req, &env, &Sites, key.as_bytes()).await
}
