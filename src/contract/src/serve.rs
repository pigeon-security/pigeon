//! The capability side: a trait to implement, and the function that serves core's requests.

use std::pin::Pin;

use serde::de::DeserializeOwned;
use worker::js_sys::Uint8Array;
use worker::{Env, Request, RequestInit, Response};

use crate::caller::{HEADER, Refused, Signed, WORKER_CONTEXT_VERSION};
use crate::keys::Keys;
use crate::now_ms;
use crate::wire::{
    Answer, CallRequest, EventsRequest, FailureKind, Manifest, OPS, SecretRequest, WorkerManifest,
};

/// An answer in flight.
pub type Answering<'a> = Pin<Box<dyn Future<Output = Answer> + 'a>>;

/// A capability Worker's side of the contract. Everything is JSON in and out; the caller is
/// already verified.
pub trait Service {
    /// What this Worker offers.
    fn manifest(&self) -> Manifest;

    /// Runs a tool for `caller`.
    fn call<'a>(&'a self, env: &'a Env, caller: &'a Signed, req: CallRequest) -> Answering<'a>;

    /// Answers a forwarded request (a webhook or a page, which check their own callers); by
    /// default 404.
    fn fetch<'a>(
        &'a self,
        _env: &'a Env,
        _req: Request,
    ) -> Pin<Box<dyn Future<Output = worker::Result<Response>> + 'a>> {
        Box::pin(std::future::ready(Response::error("not found", 404)))
    }

    /// Acts on a batch of queue messages; by default they are ignored.
    fn events<'a>(&'a self, _env: &'a Env, _req: EventsRequest) -> Answering<'a> {
        Box::pin(std::future::ready(Answer::Ok(serde_json::Value::Null)))
    }

    /// Checks, acts on or dates one of its secrets for `caller`; by default the op is unknown.
    fn secret<'a>(
        &'a self,
        _env: &'a Env,
        _caller: &'a Signed,
        _req: SecretRequest,
    ) -> Answering<'a> {
        Box::pin(std::future::ready(unknown_op("secret")))
    }
}

/// Serves one request from core. It routes by op, checks the caller's signature (on every op
/// but `manifest`, `fetch` and `events`, which carry no caller), parses the body and answers
/// JSON. An unknown op answers kind `unknown_op` with HTTP 200, so core can tell skew from
/// failure. The caller is checked with this Worker's own key, then the shared one
/// ([`Keys::verify`]). A bad signature answers `unauthorized` with 401, and a body that does not parse
/// answers `bad_request` with 400. `fetch` requests are handed on as they came, without the
/// `/fetch` prefix.
pub async fn serve(
    mut req: Request,
    env: &Env,
    service: &dyn Service,
    keys: &Keys,
) -> worker::Result<Response> {
    let url = req.url()?;
    let trimmed = url.path().trim_start_matches('/');
    let (op, rest) = trimmed.split_once('/').unwrap_or((trimmed, ""));
    if !OPS.contains(&op) || (!rest.is_empty() && op != "fetch") {
        let answer = Answer::error(FailureKind::UnknownOp, format!("unknown op {op}"));
        return reply(&answer, 200);
    }
    if op == "fetch" {
        let mut target = format!("https://capability/{rest}");
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        let forwarded = forward(&mut req, &target).await?;
        return service.fetch(env, forwarded).await;
    }
    let header = req.headers().get(HEADER)?;
    let method = req.method().to_string();
    let agent = trimmed_agent(req.headers().get("User-Agent")?.as_deref());
    let body = req.bytes().await?;
    match run(op, header.as_deref(), &body, env, service, keys).await {
        Ok(answer) => reply(&answer, 200),
        Err(rejection) => {
            let answer = match rejection.reason {
                Some(reason) => {
                    // One line per refusal: who asked and why, never the header itself.
                    worker::console_error!(
                        "refused 401 reason={reason} op={op} method={method} route=/{trimmed} user_agent={agent:?}"
                    );
                    Answer::refused(reason, rejection.message)
                }
                None => Answer::error(rejection.kind, rejection.message),
            };
            reply(&answer, rejection.status)
        }
    }
}

/// A request turned away, with the HTTP status that says so.
struct Rejection {
    kind: FailureKind,
    message: String,
    status: u16,
    /// The machine-readable reason of a refused caller.
    reason: Option<&'static str>,
}

impl Rejection {
    fn new(kind: FailureKind, message: impl Into<String>, status: u16) -> Self {
        Self {
            kind,
            message: message.into(),
            status,
            reason: None,
        }
    }
}

/// The User-Agent cut to 64 characters, or an empty string when there is none.
fn trimmed_agent(agent: Option<&str>) -> String {
    agent.unwrap_or("").chars().take(64).collect()
}

/// Runs every op but `fetch`.
async fn run(
    op: &str,
    header: Option<&str>,
    body: &[u8],
    env: &Env,
    service: &dyn Service,
    keys: &Keys,
) -> Result<Answer, Rejection> {
    match op {
        "manifest" => serde_json::to_value(WorkerManifest {
            manifest: service.manifest(),
            worker_context_version: Some(WORKER_CONTEXT_VERSION),
        })
        .map(Answer::Ok)
        .map_err(|e| {
            Rejection::new(
                FailureKind::Failed,
                format!("the manifest does not serialize: {e}"),
                500,
            )
        }),
        "call" => {
            let (caller, request) = signed_body::<CallRequest>(keys, header, op, body, service)?;
            caller
                .check_instance(request.args["instance"].as_str())
                .map_err(unauthorized)?;
            Ok(service.call(env, &caller, request).await)
        }
        "secret" => {
            let (caller, request) = signed_body::<SecretRequest>(keys, header, op, body, service)?;
            caller.check_instance(None).map_err(unauthorized)?;
            Ok(service.secret(env, &caller, request).await)
        }
        "events" => {
            let request = parse::<EventsRequest>(body)?;
            Ok(service.events(env, request).await)
        }
        _ => Err(Rejection::new(
            FailureKind::UnknownOp,
            format!("unknown op {op}"),
            200,
        )),
    }
}

/// Checks the caller, then parses the body.
fn signed_body<T: DeserializeOwned>(
    keys: &Keys,
    header: Option<&str>,
    op: &str,
    body: &[u8],
    service: &dyn Service,
) -> Result<(Signed, T), Rejection> {
    let signed = keys
        .verify(header, op, body, now_ms(), &service.manifest().worker)
        .map_err(unauthorized)?;
    Ok((signed, parse(body)?))
}

fn unauthorized(r: Refused) -> Rejection {
    Rejection {
        reason: Some(r.reason()),
        ..Rejection::new(FailureKind::Unauthorized, r.to_string(), 401)
    }
}

/// Parses a JSON body.
fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Rejection> {
    serde_json::from_slice(body).map_err(|e| {
        Rejection::new(
            FailureKind::BadRequest,
            format!("the body does not parse: {e}"),
            400,
        )
    })
}

/// The answer for an op this Worker does not serve.
fn unknown_op(what: &str) -> Answer {
    Answer::error(FailureKind::UnknownOp, format!("{what} is not served here"))
}

/// `req` rebuilt for `target`, with the same method, headers and body.
async fn forward(req: &mut Request, target: &str) -> worker::Result<Request> {
    let body = req.bytes().await?;
    let mut init = RequestInit::new();
    init.with_method(req.method())
        .with_headers(req.headers().clone());
    if !body.is_empty() {
        init.with_body(Some(Uint8Array::from(body.as_slice()).into()));
    }
    Request::new_with_init(target, &init)
}

/// `answer` as a JSON response with `status`.
fn reply(answer: &Answer, status: u16) -> worker::Result<Response> {
    Ok(Response::from_json(answer)?.with_status(status))
}
