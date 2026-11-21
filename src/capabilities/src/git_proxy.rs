//! The git proxy: `/git/{namespace}/{repo}.git/` on the proxy's host, in front of the source
//! repository in Cloudflare Artifacts, for callers holding a proxy credential. Each request
//! gets a git token of its own, read for fetches and write for pushes, minted when it arrives
//! and revoked when its answer has been sent; the caller never sees it. Bodies stream both
//! ways.
//!
//! Pushes follow the person's policy (`.pigeon/policy.toml`, see [`crate::policy`]), checked on
//! the ref updates at the start of the push before anything is sent on: a branch under the
//! session prefix is pushed, rewritten or deleted (`session-branches`); the protected branch
//! moves only by a landing's fast-forward (`main-forward-only`), never by a push; any other ref
//! is not in policy. A refused push is answered the way git reports a refused ref, so
//! `git push` prints why. Every request is logged as one audit line, and every ref update's
//! policy decision as another. ([`not_forward`] stays for the fast-forward check a push of the
//! protected branch would need, should the policy ever allow one.)
//!
//! A CI runner fetches with its job token instead: only from this repository, only by
//! upload-pack, only the one commit its grant names and only until the job's deadline. Its
//! request is read whole first, so that every object it wants is checked; it never pushes.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use futures_channel::oneshot;
use futures_util::{Stream, StreamExt, TryStreamExt, stream};
use pigeon_core::Account;
use pigeon_core::artifacts::{self, Access};
use pigeon_core::git::{self, Command, Commands, Remote, ZERO};
use pigeon_core::mcp::Tool;
use pigeon_core::pack::{self, Walk};
use pigeon_core::policy::{Decision, Kind, Vars};
use serde_json::Value;
use worker::js_sys::Uint8Array;
use worker::wasm_bindgen::JsValue;
use worker::{
    ByteStream, Error, Fetch, Headers, Method, Request, RequestInit, Response, Result,
    console_error,
};

use crate::ci;
use crate::proxy::{self, Audit, Outcome, ProxyCaller};
use crate::{Call as ToolCall, Capability, Context, Footprint, Serve, unknown};

/// How long a git token minted for one request lives: the ceiling if its revoke is lost.
const TOKEN_TTL: u64 = 900;

/// How much of a push that moves the protected branch is read, at most.
pub const BUFFERED_MAX: usize = 16 * 1024 * 1024;

/// How long to wait before each retry of an upstream request refused with 401: a token minted
/// moments before may not have reached Artifacts yet.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// The git proxy for one repository.
pub struct GitProxy {
    /// The hostname it answers on; requests to any other are not found.
    pub host: String,
    pub namespace: String,
    pub repo: String,
    /// The branch that only moves forward, such as `refs/heads/main`.
    pub protected: String,
    /// The prefix every other pushed ref must have, such as `refs/heads/claude/`.
    pub prefix: String,
}

// ---- what a request may be ----

/// A git smart-HTTP service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// Its name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }

    fn from_name(s: &str) -> Option<Service> {
        match s {
            "git-upload-pack" => Some(Service::UploadPack),
            "git-receive-pack" => Some(Service::ReceivePack),
            _ => None,
        }
    }
}

/// A request the proxy forwards: a service's refs, or a call to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Refs(Service),
    Rpc(Service),
}

/// What request `method path?query` is on repository `namespace/repo`, compared exactly, with
/// nothing decoded: `GET .../info/refs?service=<service>` or `POST .../<service>`.
pub fn route(
    method: &str,
    path: &str,
    query: Option<&str>,
    namespace: &str,
    repo: &str,
) -> std::result::Result<Call, &'static str> {
    let rest = path
        .strip_prefix("/git/")
        .and_then(|r| r.strip_prefix(namespace))
        .and_then(|r| r.strip_prefix('/'))
        .and_then(|r| r.strip_prefix(repo))
        .and_then(|r| r.strip_prefix(".git"))
        .filter(|r| r.is_empty() || r.starts_with('/'))
        .ok_or("not this repository")?;
    if rest == "/info/refs" {
        let service = query
            .and_then(|q| q.strip_prefix("service="))
            .and_then(Service::from_name)
            .ok_or("not a git path")?;
        if method != "GET" {
            return Err("wrong method");
        }
        return Ok(Call::Refs(service));
    }
    let service = rest
        .strip_prefix('/')
        .and_then(Service::from_name)
        .ok_or("not a git path")?;
    if !matches!(query, None | Some("")) {
        return Err("not a git path");
    }
    if method != "POST" {
        return Err("wrong method");
    }
    Ok(Call::Rpc(service))
}

/// Whether `name` is a well-formed ref: git's `check-ref-format` rules, made stricter.
pub fn refname_ok(name: &str) -> bool {
    if name.len() > 255 || !name.starts_with("refs/") {
        return false;
    }
    if name.contains("..") || name.contains("@{") || name.contains("//") {
        return false;
    }
    if name.ends_with('/') || name.ends_with('.') {
        return false;
    }
    let bad_byte = name.bytes().any(|b| {
        b < 0x20 || b == 0x7f || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    });
    !bad_byte
        && name
            .split('/')
            .all(|c| !c.is_empty() && !c.starts_with('.') && !c.ends_with(".lock"))
}

/// How an allowed ref update is pushed.
#[derive(Debug, PartialEq, Eq)]
pub enum Rule {
    /// Streamed as it comes.
    Stream,
    /// Read whole first, to check it moves forward.
    FastForward,
}

/// The policy's names for `protected` and `prefix`: branch names without `refs/heads/`.
pub fn vars(protected: &str, prefix: &str) -> Vars {
    let short = |r: &str| r.strip_prefix("refs/heads/").unwrap_or(r).to_string();
    Vars {
        session_prefix: short(prefix),
        main: short(protected),
    }
}

/// What ref update `c` is to the policy: moving the protected branch, deleting a ref, or
/// pushing one (a rewrite of a branch reads as a push: the policy treats them alike), with its
/// target, the branch name without `refs/heads/`, or the whole name of any other ref.
pub fn update(c: &Command, protected: &str) -> (Kind, String) {
    let target = c
        .name
        .strip_prefix("refs/heads/")
        .unwrap_or(&c.name)
        .to_string();
    let kind = if c.name == protected {
        Kind::MoveMain
    } else if c.new == ZERO {
        Kind::DeleteBranch
    } else {
        Kind::PushBranch
    };
    (kind, target)
}

/// Whether ref update `c` is allowed, and how: the policy decides. Branches under the session
/// prefix are pushed, rewritten and deleted (`session-branches`); the protected branch moves
/// only by a landing (`main-forward-only`), never by a push; anything else is not in policy.
pub fn check(c: &Command, protected: &str, prefix: &str) -> std::result::Result<Rule, String> {
    if !refname_ok(&c.name) {
        return Err("not a valid ref name".into());
    }
    let (kind, target) = update(c, protected);
    let answer = crate::policy::decide(
        &vars(protected, prefix),
        &crate::policy::request("git push", kind, &target),
    );
    match answer.decision {
        Decision::Allow => Ok(Rule::Stream),
        Decision::Deny => Err(match answer.rule {
            Some(rule) => format!("{} (policy rule {rule})", answer.reason),
            None => format!("{} (.pigeon/policy.toml)", answer.reason),
        }),
    }
}

/// Every update's rule, or each refused update's ref and reason.
pub fn check_all(
    cs: &[Command],
    protected: &str,
    prefix: &str,
) -> std::result::Result<Vec<Rule>, Vec<(String, String)>> {
    let mut rules = Vec::with_capacity(cs.len());
    let mut refused = Vec::new();
    for c in cs {
        match check(c, protected, prefix) {
            Ok(r) => rules.push(r),
            Err(e) => refused.push((c.name.clone(), e)),
        }
    }
    if refused.is_empty() {
        Ok(rules)
    } else {
        Err(refused)
    }
}

// ---- streams ----

/// A body stream that tells `ended` when it has been read to its end, or dropped unread.
struct Ended {
    inner: Pin<Box<dyn Stream<Item = Result<Vec<u8>>>>>,
    ended: Option<oneshot::Sender<()>>,
}

impl Stream for Ended {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(None) = polled
            && let Some(tx) = self.ended.take()
        {
            let _ = tx.send(());
        }
        polled
    }
}

/// `s` as a JS `ReadableStream` of `Uint8Array`s, for a request body.
fn readable(s: impl Stream<Item = Result<Vec<u8>>> + 'static) -> JsValue {
    let js = s
        .map_ok(|chunk| JsValue::from(Uint8Array::from(chunk.as_slice())))
        .map_err(|e| JsValue::from(e.to_string()));
    wasm_streams::ReadableStream::from_stream(js)
        .into_raw()
        .into()
}

/// A request body as the proxy sends it on.
enum Body {
    None,
    /// Read whole, so the request can be retried.
    Bytes(Vec<u8>),
    /// Streamed, so it is sent once.
    Stream(JsValue),
}

/// What reading the start of a push found.
struct Push {
    commands: Commands,
    /// What has been read so far.
    head: Vec<u8>,
    /// The rest, unread.
    rest: ByteStream,
}

/// Reads a push's body until its ref updates are complete.
async fn read_commands(mut body: ByteStream) -> std::result::Result<Push, String> {
    let mut head = Vec::new();
    loop {
        if let Some(commands) = git::commands(&head)? {
            return Ok(Push {
                commands,
                head,
                rest: body,
            });
        }
        match body.next().await {
            Some(Ok(chunk)) => head.extend(chunk),
            Some(Err(_)) => return Err("the push's body could not be read".into()),
            None => return Err("the push ended before its ref updates did".into()),
        }
    }
}

/// Reads and drops what is left of a body, up to `cap` bytes.
async fn drain(rest: &mut ByteStream, cap: usize) {
    let mut read = 0usize;
    while read < cap {
        match rest.next().await {
            Some(Ok(chunk)) => read += chunk.len(),
            _ => return,
        }
    }
}

/// Reads the rest of a push, up to [`BUFFERED_MAX`] in all.
async fn read_rest(push: &mut Push) -> std::result::Result<(), String> {
    while let Some(chunk) = push.rest.next().await {
        let chunk = chunk.map_err(|_| "the push's body could not be read".to_string())?;
        if push.head.len() + chunk.len() > BUFFERED_MAX {
            return Err(format!(
                "a push that moves the protected branch can be {} MiB at most; push it in \
                 smaller steps",
                BUFFERED_MAX / 1024 / 1024
            ));
        }
        push.head.extend(chunk);
    }
    Ok(())
}

// ---- upstream ----

/// Sends one request upstream. Without a streamed body it is retried while Artifacts answers
/// 401, since the token was minted moments before.
async fn send(
    remote: &Remote,
    method: Method,
    url: &str,
    headers: &Headers,
    body: Body,
) -> Result<Response> {
    let mut waits = NOT_YET_ACTIVE_WAITS_MS.iter();
    let retry = !matches!(body, Body::Stream(_));
    let mut body = Some(body);
    loop {
        let h = Headers::new();
        for (k, v) in headers.entries() {
            h.set(&k, &v)?;
        }
        h.set("Authorization", &remote.authorization)?;
        let mut init = RequestInit::new();
        init.with_method(method.clone()).with_headers(h);
        match body.as_ref() {
            Some(Body::Bytes(b)) => {
                init.with_body(Some(Uint8Array::from(b.as_slice()).into()));
            }
            Some(Body::Stream(_)) => {
                if let Some(Body::Stream(s)) = body.take() {
                    init.with_body(Some(s));
                }
            }
            Some(Body::None) | None => {}
        }
        let response = Fetch::Request(Request::new_with_init(url, &init)?)
            .send()
            .await?;
        if response.status_code() == 401
            && retry
            && let Some(ms) = waits.next()
        {
            worker::Delay::from(std::time::Duration::from_millis(*ms)).await;
            continue;
        }
        return Ok(response);
    }
}

/// The headers sent on: only those git's protocol needs. The caller's `Authorization` and
/// everything else stay behind.
fn forwarded_headers(req: &Request, service: Service) -> Result<Headers> {
    let out = Headers::new();
    let mut names = vec!["Content-Type", "Accept", "Git-Protocol", "User-Agent"];
    if service == Service::UploadPack {
        names.push("Content-Encoding");
    }
    for name in names {
        if let Some(v) = req.headers().get(name)? {
            out.set(name, &v)?;
        }
    }
    Ok(out)
}

/// The commits `old`'s history doesn't reach, from the repository's branches: every ref tip
/// but `old` wanted, `old` had, commits only when the remote can filter. Their parents, by
/// commit.
async fn upstream_history(
    remote: &Remote,
    old: String,
) -> std::result::Result<HashMap<String, Vec<String>>, String> {
    let refs = git::refs(remote, Service::UploadPack.name())
        .await
        .map_err(|_| "the repository's refs could not be read".to_string())?;
    let mut wants: Vec<String> = refs
        .refs
        .values()
        .filter(|id| **id != old && git::is_id(id))
        .cloned()
        .collect();
    wants.sort();
    wants.dedup();
    if wants.is_empty() {
        return Ok(HashMap::new());
    }
    let got = git::fetch_filtered(remote, &refs.caps, &wants, &[old], Some("tree:0"))
        .await
        .map_err(|_| "the repository's history could not be read".to_string())?;
    Ok(pack::commits(&got.pack)?.parents)
}

/// The protected-branch updates in a push that don't move it forward, each with why; `None`
/// when every one does. `head` is the whole push body, `commands` its ref updates and `rules`
/// theirs. When the pushed pack doesn't settle it, `upstream(old)` is asked once for the
/// repository's history that `old` doesn't reach (see [`upstream_history`]); if `old` is an
/// ancestor of what the push builds on, the walk meets it there.
pub async fn not_forward<U, F>(
    head: &[u8],
    commands: &Commands,
    rules: &[Rule],
    prefix: &str,
    mut upstream: U,
) -> Option<Vec<(String, String)>>
where
    U: FnMut(String) -> F,
    F: std::future::Future<Output = std::result::Result<HashMap<String, Vec<String>>, String>>,
{
    let protected = commands
        .commands
        .iter()
        .zip(rules)
        .find(|(_, r)| **r == Rule::FastForward)
        .map(|(c, _)| c.name.clone())?;
    let fail = |why: String| Some(vec![(protected.clone(), why)]);
    let start = match git::pack_start(head, commands) {
        Ok(Some(s)) => s,
        Ok(None) => return fail("the push ended early".into()),
        Err(e) => return fail(e),
    };
    let pack = &head[start..];
    let (mut known, unresolved) = if pack.is_empty() {
        (HashMap::new(), 0)
    } else {
        match pack::commits(pack) {
            Ok(c) => (c.parents, c.unresolved),
            Err(e) => return fail(e),
        }
    };
    let mut fetched = false;
    for (c, rule) in commands.commands.iter().zip(rules) {
        if *rule != Rule::FastForward {
            continue;
        }
        let forward = match pack::walk(&c.old, &c.new, &known, false) {
            Walk::Forward => true,
            Walk::NotForward => false,
            Walk::Need(_) => {
                if !fetched {
                    fetched = true;
                    match upstream(c.old.clone()).await {
                        Ok(more) => known.extend(more),
                        Err(e) => return fail(e),
                    }
                }
                pack::walk(&c.old, &c.new, &known, true) == Walk::Forward
            }
        };
        if forward {
            continue;
        }
        return fail(if unresolved > 0 {
            format!(
                "{} could not be checked: the push stores objects as changes to others it \
                 doesn't carry. Push the commit to a branch under {prefix} first, then move \
                 {} to it",
                c.name, c.name
            )
        } else {
            format!(
                "{} only moves forward: push a commit that has the current one in its history",
                c.name
            )
        });
    }
    None
}

impl GitProxy {
    async fn serve(&self, cx: &Context<'_>, req: Request) -> Result<Response> {
        let method = req.method();
        let path = req.path();
        let mut caller = None;
        let answer = self.answer(cx, req, &mut caller).await;
        let (outcome, status, why) = match &answer {
            Ok((res, why)) => {
                let status = res.status_code();
                let outcome = match why {
                    Some(_) => Outcome::Refused,
                    None if (200..400).contains(&status) => Outcome::Forwarded,
                    None => Outcome::Failed,
                };
                (outcome, Some(status), why.clone())
            }
            Err(e) => (Outcome::Failed, Some(502), Some(e.to_string())),
        };
        proxy::audit(
            caller.as_ref(),
            &Audit {
                route: "git",
                method: method.as_ref(),
                path: &path,
                outcome,
                status,
                why: why.as_deref(),
            },
        );
        match answer {
            Ok((res, _)) => Ok(res),
            Err(e) => {
                console_error!("git proxy: {e}");
                Response::error("the git proxy failed; see the platform's logs", 502)
            }
        }
    }

    /// The answer, and why when it is a refusal.
    async fn answer(
        &self,
        cx: &Context<'_>,
        mut req: Request,
        caller: &mut Option<ProxyCaller>,
    ) -> Result<(Response, Option<String>)> {
        let refuse = |status: u16, why: &str| -> Result<(Response, Option<String>)> {
            Ok((Response::error(why, status)?, Some(why.to_string())))
        };
        let url = req.url()?;
        if url.host_str() != Some(self.host.as_str()) {
            return refuse(404, "not found");
        }
        // A CI job token fetches its one commit; anything else needs a proxy credential.
        let mut job_body = None;
        match bearer(&req)? {
            Some(t) if t.starts_with(ci::token::PREFIX) => {
                match self.job(cx, &mut req, &url, &t, caller).await? {
                    Ok(body) => job_body = body,
                    Err((status, why)) => return refuse(status, &why),
                }
            }
            _ => {
                *caller = match proxy::caller(cx.env, &req).await {
                    Ok(c) => Some(c),
                    // 403, not 401: a 401 would send git looking for a password to prompt for.
                    Err(e) => return refuse(403, &e.to_string()),
                };
            }
        }
        let call = match route(
            req.method().as_ref(),
            url.path(),
            url.query(),
            &self.namespace,
            &self.repo,
        ) {
            Ok(c) => c,
            Err(why) => return refuse(404, why),
        };
        let Some(worker) = cx.worker else {
            return Err(Error::RustError(
                "no request context to revoke tokens in".into(),
            ));
        };
        let service = match call {
            Call::Refs(s) | Call::Rpc(s) => s,
        };
        let headers = forwarded_headers(&req, service)?;
        // A push's ref updates are read and checked before any token is minted.
        let mut push = None;
        let mut rules = Vec::new();
        if call == Call::Rpc(Service::ReceivePack) {
            let encoding = req.headers().get("Content-Encoding")?;
            if encoding.is_some_and(|e| !e.eq_ignore_ascii_case("identity")) {
                return refuse(415, "the proxy takes pushes without content encoding");
            }
            let mut p = match read_commands(req.stream()?).await {
                Ok(p) => p,
                Err(why) => return refuse(400, &why),
            };
            // One audit line per policy decision, as the push asked it.
            let who = caller.as_ref().map_or("a CI job", |c| c.label.as_str());
            for c in &p.commands.commands {
                let (kind, target) = update(c, &self.protected);
                let req = crate::policy::request(who, kind, &target);
                let answer = crate::policy::decide(&vars(&self.protected, &self.prefix), &req);
                worker::console_log!("{}", crate::policy::audit_line("git_proxy", &req, &answer));
            }
            match check_all(&p.commands.commands, &self.protected, &self.prefix) {
                Ok(r) => rules = r,
                Err(refused) => {
                    // Read what git is still sending, so it reads the refusal rather than
                    // stopping at a closed connection.
                    drain(&mut p.rest, BUFFERED_MAX).await;
                    return refusal(&p.commands, &refused);
                }
            }
            push = Some(p);
        }
        let access = match service {
            Service::UploadPack => Access::Read,
            Service::ReceivePack => Access::Write,
        };
        let (repo, token) =
            artifacts::session(cx.account, &self.namespace, &self.repo, access, TOKEN_TTL).await?;
        let remote = Remote {
            url: repo.remote.trim_end_matches('/').to_string(),
            authorization: format!("Bearer {}", token.value),
        };
        // From here the token is revoked whatever happens: when the answer's body ends, or now.
        let (ended, done) = oneshot::channel::<()>();
        let account: Account = cx.account.clone();
        let (namespace, name, id) = (self.namespace.clone(), self.repo.clone(), token.id.clone());
        worker.wait_until(async move {
            let _ = done.await;
            match id {
                Some(id) => {
                    if let Err(e) = artifacts::revoke(&account, &namespace, &name, &id).await {
                        console_error!("git proxy: revoking git token {id}: {e}");
                    }
                }
                None => console_error!("git proxy: a git token came back without an id"),
            }
        });
        let body = match (call, push) {
            (Call::Refs(_), _) => Body::None,
            (Call::Rpc(Service::UploadPack), _) => match job_body {
                // A job's request, read whole to check its wants.
                Some(b) => Body::Bytes(b),
                None => match req.inner().body() {
                    Some(s) => Body::Stream(s.into()),
                    None => Body::None,
                },
            },
            (Call::Rpc(Service::ReceivePack), Some(mut p)) => {
                if rules.contains(&Rule::FastForward) {
                    if let Err(why) = read_rest(&mut p).await {
                        return refusal(&p.commands, &[(self.protected.clone(), why)]);
                    }
                    let upstream = |old: String| upstream_history(&remote, old);
                    if let Some(refused) =
                        not_forward(&p.head, &p.commands, &rules, &self.prefix, upstream).await
                    {
                        return refusal(&p.commands, &refused);
                    }
                    Body::Bytes(p.head)
                } else {
                    // Warm the token first: a streamed body can be sent only once.
                    git::refs(&remote, service.name()).await?;
                    let head = stream::once(async move { Ok::<_, Error>(p.head) });
                    Body::Stream(readable(head.chain(p.rest)))
                }
            }
            (Call::Rpc(Service::ReceivePack), None) => {
                return Err(Error::RustError("a push without its ref updates".into()));
            }
        };
        if let Body::Stream(_) = body
            && call == Call::Rpc(Service::UploadPack)
        {
            git::refs(&remote, service.name()).await?;
        }
        let (method, upstream_url) = match call {
            Call::Refs(s) => (
                Method::Get,
                format!("{}/info/refs?service={}", remote.url, s.name()),
            ),
            Call::Rpc(s) => (Method::Post, format!("{}/{}", remote.url, s.name())),
        };
        let mut upstream = send(&remote, method, &upstream_url, &headers, body).await?;
        let out = Headers::new();
        for name in ["Content-Type", "Cache-Control"] {
            if let Some(v) = upstream.headers().get(name)? {
                out.set(name, &v)?;
            }
        }
        let status = upstream.status_code();
        if matches!(status, 401 | 403) {
            // Passed on, a 401 would send git looking for a password; it is the proxy's
            // token that was refused, not the caller.
            return Err(Error::RustError(format!(
                "Artifacts answered {status} to the proxy's git token"
            )));
        }
        let body = Ended {
            inner: Box::pin(upstream.stream()?),
            ended: Some(ended),
        };
        Ok((
            Response::from_stream(body)?
                .with_status(status)
                .with_headers(out),
            None,
        ))
    }

    /// Checks a CI job token's request: a fetch of the grant's one commit of this repository,
    /// before its deadline, and nothing else; never a push. Sets `caller` once the grant is
    /// known. Answers the request body when it was read to check its wants, or the status and
    /// reason to refuse with.
    #[allow(clippy::type_complexity)]
    async fn job(
        &self,
        cx: &Context<'_>,
        req: &mut Request,
        url: &worker::Url,
        presented: &str,
        caller: &mut Option<ProxyCaller>,
    ) -> Result<std::result::Result<Option<Vec<u8>>, (u16, String)>> {
        let no = |status: u16, why: &str| Ok(Err((status, why.to_string())));
        if !ci::token::reaches(url.path()) {
            return no(403, "a job token only fetches its own commit");
        }
        let call = match route(
            req.method().as_ref(),
            url.path(),
            url.query(),
            &self.namespace,
            &self.repo,
        ) {
            Ok(c) => c,
            Err(why) => return no(404, why),
        };
        if !matches!(
            call,
            Call::Refs(Service::UploadPack) | Call::Rpc(Service::UploadPack)
        ) {
            return no(403, "a job token cannot push");
        }
        let Some(grant) = ci::job_grant(cx.env, presented).await? else {
            return no(403, "the job token is unknown or has expired");
        };
        *caller = Some(ProxyCaller {
            id: ci::token::lookup_hash(presented)
                .map(|h| h[..proxy::ID_LEN].to_string())
                .unwrap_or_default(),
            label: format!("ci job {}", grant.job),
            person: "ci".into(),
        });
        let repo = format!("{}/{}", self.namespace, self.repo);
        let now = worker::js_sys::Date::now() as i64;
        if !ci::token::allows_fetch(&grant, now, &repo, &grant.commit) {
            return no(
                403,
                "the job token does not reach this repository, or has expired",
            );
        }
        if call == Call::Refs(Service::UploadPack) {
            return Ok(Ok(None));
        }
        let encoding = req.headers().get("Content-Encoding")?;
        if encoding.is_some_and(|e| !e.eq_ignore_ascii_case("identity")) {
            return no(415, "a job's fetch must be sent without content encoding");
        }
        let declared = req
            .headers()
            .get("Content-Length")?
            .and_then(|n| n.parse::<usize>().ok());
        if declared.is_some_and(|n| n > git::UPLOAD_REQUEST_MAX) {
            return no(413, "a job's fetch request is too large");
        }
        let body = req.bytes().await?;
        let asked = match git::upload_request(&body) {
            Ok(a) => a,
            Err(why) => return no(400, &why),
        };
        match job_fetch_allowed(&asked, |want| {
            ci::token::allows_fetch(&grant, now, &repo, want)
        }) {
            Ok(()) => Ok(Ok(Some(body))),
            Err(why) => no(403, why),
        }
    }
}

/// Whether a job may make upload-pack request `asked`, given `allowed` for each wanted object:
/// a fetch (protocol v0, or v2's `fetch`) wanting at least one object, every one allowed, or a
/// v2 `ls-refs` wanting nothing. Any other v2 command is refused.
pub fn job_fetch_allowed(
    asked: &git::UploadRequest,
    allowed: impl Fn(&str) -> bool,
) -> std::result::Result<(), &'static str> {
    match asked.command.as_deref() {
        None | Some("fetch") => {
            if asked.wants.is_empty() {
                return Err("a job's fetch must want its commit");
            }
            if asked.wants.iter().all(|w| allowed(w)) {
                Ok(())
            } else {
                Err("a job token fetches only its own commit")
            }
        }
        Some("ls-refs") if asked.wants.is_empty() => Ok(()),
        _ => Err("a job token can only fetch"),
    }
}

/// The regular file at `path` (such as `.pigeon/ci.toml`) in `commit` of repository
/// `namespace/repo`, or `None` when there is none. Read with a git token minted for it and
/// revoked after: the commit alone, one commit deep. For the platform's own use, such as CI
/// reading a job's configuration.
pub async fn read_file(
    account: &Account,
    namespace: &str,
    repo: &str,
    commit: &str,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    let (found, token) =
        artifacts::session(account, namespace, repo, Access::Read, TOKEN_TTL).await?;
    let remote = Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let read = git::read_file(&remote, commit, path).await;
    let revoked = match &token.id {
        Some(id) => artifacts::revoke(account, namespace, repo, id).await,
        None => Err(Error::RustError(
            "a git token came back without an id".into(),
        )),
    };
    let file = read?;
    revoked?;
    Ok(file)
}

/// Whether `new` moves a branch at `old` forward, from `pack`: the commits `new` reaches that
/// `old` does not, as an upload-pack fetch wanting `new` and having `old` answers. A commit the
/// pack leaves out is one `old` already reaches.
pub fn moves_forward(old: &str, new: &str, pack: &[u8]) -> std::result::Result<bool, String> {
    if old == new {
        return Ok(true);
    }
    let known = if pack.is_empty() {
        HashMap::new()
    } else {
        pack::commits(pack)?.parents
    };
    Ok(pack::walk(old, new, &known, true) == Walk::Forward)
}

/// What [`fast_forward`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Moved {
    /// The branch moved from `from` to the commit.
    Moved { from: String },
    /// The branch was already at the commit.
    Already,
    /// It did not move, for this reason.
    Refused(String),
}

/// The tip of `branch` (a full ref) of repository `namespace/repo`, when it exists. Read with a
/// git token minted for it and revoked after.
pub async fn tip(
    account: &Account,
    namespace: &str,
    repo: &str,
    branch: &str,
) -> Result<Option<String>> {
    let (found, token) =
        artifacts::session(account, namespace, repo, Access::Read, TOKEN_TTL).await?;
    let remote = Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let read = git::refs(&remote, Service::UploadPack.name()).await;
    revoke(account, namespace, repo, &token).await?;
    Ok(read?.refs.get(branch).cloned())
}

/// The IDs of what each of `paths` names in `commit` of `namespace/repo`, read with a git token
/// minted for it and revoked after.
pub async fn path_ids(
    account: &Account,
    namespace: &str,
    repo: &str,
    commit: &str,
    paths: &[&str],
) -> Result<Vec<Option<String>>> {
    let (found, token) =
        artifacts::session(account, namespace, repo, Access::Read, TOKEN_TTL).await?;
    let remote = Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let read = git::path_ids(&remote, commit, paths).await;
    revoke(account, namespace, repo, &token).await?;
    read
}

/// Whether `new` is a fast-forward of `branch`'s tip in `namespace/repo` now; the tip, and why
/// not when it is not.
pub async fn forward_of(
    account: &Account,
    namespace: &str,
    repo: &str,
    branch: &str,
    new: &str,
) -> Result<(String, Option<String>)> {
    let (found, token) =
        artifacts::session(account, namespace, repo, Access::Read, TOKEN_TTL).await?;
    let remote = Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let checked = check_forward(&remote, branch, new).await;
    revoke(account, namespace, repo, &token).await?;
    checked
}

/// Whether `branch`'s tip in `namespace/repo` already has `commit` in its history (or is it):
/// a landing whose merge answer was lost finds its commit there even after main moved on.
pub async fn contains(
    account: &Account,
    namespace: &str,
    repo: &str,
    branch: &str,
    commit: &str,
) -> Result<bool> {
    let (found, token) =
        artifacts::session(account, namespace, repo, Access::Read, TOKEN_TTL).await?;
    let remote = Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let checked = async {
        let refs = git::refs(&remote, Service::UploadPack.name()).await?;
        let Some(tip) = refs.refs.get(branch).cloned() else {
            return Ok(false);
        };
        if tip == commit {
            return Ok(true);
        }
        let wants = [tip.clone()];
        let got = git::fetch_filtered(
            &remote,
            &refs.caps,
            &wants,
            &[commit.to_string()],
            Some("tree:0"),
        )
        .await?;
        moves_forward(commit, &tip, &got.pack).map_err(Error::RustError)
    }
    .await;
    revoke(account, namespace, repo, &token).await?;
    checked
}

/// [`forward_of`] on `remote`.
async fn check_forward(
    remote: &Remote,
    branch: &str,
    new: &str,
) -> Result<(String, Option<String>)> {
    let refs = git::refs(remote, Service::UploadPack.name()).await?;
    let Some(old) = refs.refs.get(branch).cloned() else {
        return Ok((ZERO.into(), Some(format!("{branch} does not exist"))));
    };
    if old == new {
        return Ok((old, None));
    }
    let wants = [new.to_string()];
    let got = git::fetch_filtered(
        remote,
        &refs.caps,
        &wants,
        std::slice::from_ref(&old),
        Some("tree:0"),
    )
    .await?;
    let forward = moves_forward(&old, new, &got.pack).map_err(Error::RustError)?;
    let why = (!forward).then(|| {
        format!("{new} is not a fast-forward of {branch} ({old}): rebase onto it and land again")
    });
    Ok((old, why))
}

/// Moves `branch` (a full ref, such as `refs/heads/main`) of `namespace/repo` to `new`, only
/// forward: the platform's own push, with a write token minted for it and revoked after. The
/// tip is read, `new` is checked to descend from it (commits only, walked as a push to the
/// protected branch is), then the ref is updated from that tip with an empty pack, since every
/// object is already there; Artifacts refuses the update if the tip moved in between.
pub async fn fast_forward(
    account: &Account,
    namespace: &str,
    repo: &str,
    branch: &str,
    new: &str,
) -> Result<Moved> {
    if !git::is_id(new) {
        return Ok(Moved::Refused(format!("not a commit id: {new}")));
    }
    let (found, token) =
        artifacts::session(account, namespace, repo, Access::Write, TOKEN_TTL).await?;
    let remote = Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let moved = async {
        let (old, why) = check_forward(&remote, branch, new).await?;
        if let Some(why) = why {
            return Ok(Moved::Refused(why));
        }
        if old == new {
            return Ok(Moved::Already);
        }
        let caps = git::refs(&remote, Service::ReceivePack.name()).await?.caps;
        let update = git::Update {
            name: branch.to_string(),
            old: old.clone(),
            new: new.to_string(),
        };
        let status = git::push(&remote, &caps, &[update], &pigeon_core::gitobj::pack(&[])).await?;
        if status.iter().any(|s| s == &format!("ok {branch}")) {
            Ok(Moved::Moved { from: old })
        } else {
            Ok(Moved::Refused(format!(
                "Artifacts refused the update: {}",
                status.join("; ")
            )))
        }
    }
    .await;
    revoke(account, namespace, repo, &token).await?;
    moved
}

/// Revokes a git token minted by `artifacts::session`.
async fn revoke(
    account: &Account,
    namespace: &str,
    repo: &str,
    token: &artifacts::GitToken,
) -> Result<()> {
    match &token.id {
        Some(id) => artifacts::revoke(account, namespace, repo, id).await,
        None => Err(Error::RustError(
            "a git token came back without an id".into(),
        )),
    }
}

/// The bearer token `req` carries, if any.
fn bearer(req: &Request) -> Result<Option<String>> {
    Ok(req.headers().get("Authorization")?.and_then(|h| {
        let (scheme, rest) = h.trim().split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| rest.trim().to_string())
    }))
}

/// A push refused as git reports it: 200 with every ref refused, or 403 when the client asked
/// for no report.
fn refusal(c: &Commands, refused: &[(String, String)]) -> Result<(Response, Option<String>)> {
    let why = refused
        .iter()
        .map(|(r, w)| format!("{r}: {w}"))
        .collect::<Vec<_>>()
        .join("; ");
    let body = git::refusal(&c.commands, &c.caps, refused);
    if body.is_empty() {
        return Ok((Response::error(why.clone(), 403)?, Some(why)));
    }
    let headers = Headers::new();
    headers.set("Content-Type", "application/x-git-receive-pack-result")?;
    headers.set("Cache-Control", "no-cache")?;
    Ok((Response::from_bytes(body)?.with_headers(headers), Some(why)))
}

impl Capability for GitProxy {
    fn name(&self) -> &'static str {
        "git_proxy"
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> ToolCall<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/git/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            repos: vec![format!("{}/{}", self.namespace, self.repo)],
            ..Footprint::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = "ns";
    const REPO: &str = "repo";
    const P: &str = "refs/heads/main";
    const X: &str = "refs/heads/claude/";

    fn r(m: &str, p: &str, q: Option<&str>) -> std::result::Result<Call, &'static str> {
        route(m, p, q, NS, REPO)
    }

    fn cmd(old: &str, new: &str, name: &str) -> Command {
        Command {
            old: old.to_string(),
            new: new.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn routes_only_the_four_git_calls() {
        let b = "/git/ns/repo.git";
        assert_eq!(
            r(
                "GET",
                &format!("{b}/info/refs"),
                Some("service=git-upload-pack")
            ),
            Ok(Call::Refs(Service::UploadPack))
        );
        assert_eq!(
            r(
                "GET",
                &format!("{b}/info/refs"),
                Some("service=git-receive-pack")
            ),
            Ok(Call::Refs(Service::ReceivePack))
        );
        assert_eq!(
            r("POST", &format!("{b}/git-upload-pack"), None),
            Ok(Call::Rpc(Service::UploadPack))
        );
        assert_eq!(
            r("POST", &format!("{b}/git-receive-pack"), Some("")),
            Ok(Call::Rpc(Service::ReceivePack))
        );
        let q = Some("service=git-upload-pack");
        assert_eq!(
            r("GET", "/git/ns/other.git/info/refs", q),
            Err("not this repository")
        );
        for (m, p, q) in [
            ("GET", "/git/ns/repo/info/refs", q),
            ("GET", "/git/ns/repo2.git/info/refs", q),
            ("GET", "/git/nsx/repo.git/info/refs", q),
            ("GET", "/git/ns/repo.git/x/info/refs", q),
            ("GET", "/git/ns/repo.git/info/refs/", q),
            (
                "GET",
                "/git/ns/repo.git/info/refs",
                Some("service=git-upload-pack&x=1"),
            ),
            (
                "GET",
                "/git/ns/repo.git/info/refs",
                Some("service=git-upload-archive"),
            ),
            ("GET", "/git/ns/repo.git/info/refs", None),
            ("POST", "/git/ns/repo.git/info/refs", q),
            ("GET", "/git/ns/repo.git/git-upload-pack", None),
            ("POST", "/git/ns/repo.git/git-receive-pack", Some("x")),
            ("GET", "/git/ns/repo.git/objects/info/packs", None),
            ("GET", "/git/ns/repo.git/HEAD", None),
        ] {
            assert!(r(m, p, q).is_err(), "{m} {p} {q:?}");
        }
    }

    #[test]
    fn ref_names_are_strict() {
        assert!(refname_ok("refs/heads/claude/x"));
        assert!(refname_ok("refs/heads/main"));
        for n in [
            "refs/heads/a..b",
            "refs/heads/a.lock",
            "refs/heads/.a",
            "refs/heads/a/",
            "refs/heads/a.",
            "refs/heads/a b",
            "refs/heads/a@{1}",
            "refs/heads/a~1",
            "refs/heads/a\\b",
            "HEAD",
            "refs/heads//a",
            "refs/heads/claude/../main",
        ] {
            assert!(!refname_ok(n), "{n}");
        }
    }

    #[test]
    fn push_rules() {
        let h = "a".repeat(40);
        let rule = |old: &str, new: &str, name: &str| check(&cmd(old, new, name), P, X);
        assert!(rule(&h, ZERO, P).is_err(), "delete main");
        assert_eq!(
            rule(&h, ZERO, "refs/heads/claude/x"),
            Ok(Rule::Stream),
            "delete a session branch"
        );
        assert!(rule(ZERO, &h, P).is_err(), "create main");
        let pushed = rule(&h, &h, P).unwrap_err();
        assert!(pushed.contains("main-forward-only"), "{pushed}");
        assert_eq!(rule(ZERO, &h, "refs/heads/claude/x"), Ok(Rule::Stream));
        assert_eq!(rule(&h, &h, "refs/heads/claude/a/b"), Ok(Rule::Stream));
        for name in [
            "refs/heads/claudex",
            "refs/heads/other",
            "refs/tags/v1",
            "refs/heads/Main",
            "refs/heads/main/x",
            "refs/notes/commits",
        ] {
            assert!(rule(ZERO, &h, name).is_err(), "{name}");
        }
    }

    /// Runs `git` in `dir`; its output.
    fn git(dir: &std::path::Path, args: &[&str], input: Option<&str>) -> Vec<u8> {
        use std::io::Write;
        use std::process::{Command as Run, Stdio};
        let mut child = Run::new("git")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // Closed at the end of this block, so git sees the input end.
        {
            let mut stdin = child.stdin.take().unwrap();
            if let Some(i) = input {
                stdin.write_all(i.as_bytes()).unwrap();
            }
        }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git {args:?}");
        out.stdout
    }

    /// A push body as git sends it: the update with capabilities, a flush, then the thin pack
    /// of `new` less `old`'s history.
    fn push_body(dir: &std::path::Path, old: &str, new: &str, name: &str) -> Vec<u8> {
        let line = format!("{old} {new} {name}\0report-status side-band-64k ofs-delta\n");
        let mut body = format!("{:04x}{line}0000", line.len() + 4).into_bytes();
        let revs = format!("{new}\n^{old}\n");
        body.extend(git(
            dir,
            &[
                "pack-objects",
                "--stdout",
                "--revs",
                "--thin",
                "-q",
                "--delta-base-offset",
            ],
            Some(&revs),
        ));
        body
    }

    fn commit(dir: &std::path::Path, body: &str) -> String {
        std::fs::write(dir.join("f"), body).unwrap();
        git(dir, &["add", "f"], None);
        git(dir, &["commit", "-q", "-m", "c"], None);
        String::from_utf8(git(dir, &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_string()
    }

    /// What the proxy decides about a push to main from `old` to `new`, from the body alone.
    fn decide(body: &[u8]) -> Walk {
        let c = git::commands(body).unwrap().unwrap();
        // The policy refuses every push to main; the fast-forward check is judged alone.
        assert!(check_all(&c.commands, P, X).is_err());
        let start = git::pack_start(body, &c).unwrap().unwrap();
        let known = pack::commits(&body[start..]).unwrap().parents;
        let u = &c.commands[0];
        pack::walk(&u.old, &u.new, &known, false)
    }

    /// Runs a future that never waits.
    fn now<F: std::future::Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = TaskContext::from_waker(std::task::Waker::noop());
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("the check waited"),
        }
    }

    /// What `not_forward` decides on a push of `new` to main from `old`, sending the commits
    /// `old` doesn't reach (`pack_from` set to `old`) or none (`pack_from` set to `new`), with
    /// the upstream history read from `dir`'s refs as an upload-pack fetch would give it.
    fn verdict(dir: &std::path::Path, old: &str, new: &str, pack_from: &str) -> Option<String> {
        let mut body = push_body(dir, pack_from, new, P);
        let line = format!("{old} {new} {P}\0report-status-v2 side-band-64k\n");
        let c = git::commands(&body).unwrap().unwrap();
        body.splice(
            ..c.end,
            format!("{:04x}{line}0000", line.len() + 4).into_bytes(),
        );
        let c = git::commands(&body).unwrap().unwrap();
        let rules = vec![Rule::FastForward];
        let upstream = |old: String| {
            let tips =
                String::from_utf8(git(dir, &["for-each-ref", "--format=%(objectname)"], None))
                    .unwrap();
            let mut revs: String = tips
                .lines()
                .filter(|t| *t != old)
                .map(|t| format!("{t}\n"))
                .collect();
            revs.push_str(&format!("^{old}\n"));
            let pack = git(
                dir,
                &["pack-objects", "--stdout", "--revs", "-q"],
                Some(&revs),
            );
            std::future::ready(pack::commits(&pack).map(|c| c.parents))
        };
        now(not_forward(&body, &c, &rules, X, upstream)).map(|r| r[0].1.clone())
    }

    #[test]
    fn main_moves_only_forward_against_real_history() {
        let dir = std::env::temp_dir().join(format!("pigeon-forward-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "t"],
        ] {
            git(&dir, args, None);
        }
        let one = commit(&dir, "one\n");
        let two = commit(&dir, "one\ntwo\n");
        git(&dir, &["checkout", "-q", "-b", "claude/feature"], None);
        let f1 = commit(&dir, "one\ntwo\nf1\n");
        // Upstream: main at `two`, claude/feature at `f1`.
        git(&dir, &["update-ref", "refs/heads/main", &two], None);
        git(&dir, &["checkout", "-q", "-b", "side", &one], None);
        let sibling = commit(&dir, "one\nsibling\n");
        // New commits on top of main, sent in the push.
        assert_eq!(verdict(&dir, &two, &f1, &two), None);
        // main moved to a branch tip already upstream: an empty pack, decided upstream.
        assert_eq!(verdict(&dir, &two, &f1, &f1), None);
        // A sibling of main's commit, and main moved back to its parent: refused.
        let refused = Some(format!(
            "{P} only moves forward: push a commit that has the current one in its history"
        ));
        assert_eq!(verdict(&dir, &two, &sibling, &one), refused);
        assert_eq!(verdict(&dir, &two, &one, &one), refused);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_push_to_main_is_judged_by_its_history() {
        let dir = std::env::temp_dir().join(format!("pigeon-git-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "t"],
        ] {
            git(&dir, args, None);
        }
        let base = commit(&dir, &"line\n".repeat(300));
        let ahead = commit(&dir, &format!("{}more\n", "line\n".repeat(300)));
        let further = commit(&dir, &format!("{}more\nagain\n", "line\n".repeat(300)));
        // New commits on top of main: forward, from the pack alone.
        assert_eq!(decide(&push_body(&dir, &base, &further, P)), Walk::Forward);
        // A rewrite: a commit beside `ahead`, not after it.
        git(&dir, &["reset", "-q", "--hard", &base], None);
        let beside = commit(&dir, "other\n");
        assert_eq!(
            decide(&push_body(&dir, &ahead, &beside, P)),
            Walk::Need(vec![base.clone()]),
            "the walk asks upstream about the commit the rewrite builds on"
        );
        // Moving main to a commit already upstream sends an empty pack: upstream must tell.
        let mut empty = push_body(&dir, &base, &base, P);
        let line = format!("{base} {ahead} {P}\0report-status\n");
        let c = git::commands(&empty).unwrap().unwrap();
        empty.splice(
            ..c.end,
            format!("{:04x}{line}0000", line.len() + 4).into_bytes(),
        );
        assert_eq!(decide(&empty), Walk::Need(vec![ahead.clone()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_job_fetches_only_what_it_is_allowed() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let req = |command: Option<&str>, wants: &[&String]| git::UploadRequest {
            command: command.map(String::from),
            wants: wants.iter().map(|w| (*w).clone()).collect(),
        };
        let only_a = |w: &str| w == a;
        assert_eq!(job_fetch_allowed(&req(None, &[&a]), only_a), Ok(()));
        assert_eq!(
            job_fetch_allowed(&req(Some("fetch"), &[&a]), only_a),
            Ok(())
        );
        assert_eq!(
            job_fetch_allowed(&req(Some("ls-refs"), &[]), only_a),
            Ok(())
        );
        for (command, wants) in [
            (None, vec![&a, &b]),
            (None, vec![&b]),
            (None, vec![]),
            (Some("fetch"), vec![]),
            (Some("fetch"), vec![&b]),
            (Some("ls-refs"), vec![&a]),
            (Some("object-info"), vec![&a]),
            (Some("bundle-uri"), vec![]),
        ] {
            assert!(
                job_fetch_allowed(&req(command, &wants), only_a).is_err(),
                "{command:?} {wants:?}"
            );
        }
    }

    #[test]
    fn a_landing_moves_main_only_forward_against_real_history() {
        let dir = std::env::temp_dir().join(format!("pigeon-ff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], None);
        git(&dir, &["config", "user.email", "t@example.com"], None);
        git(&dir, &["config", "user.name", "t"], None);
        let a = commit(&dir, "a");
        let b = commit(&dir, "b");
        let c = commit(&dir, "c");
        git(&dir, &["checkout", "-q", "-b", "side", &a], None);
        let d = commit(&dir, "d");
        // What Artifacts answers a fetch wanting `new` and having `old`: commits only.
        let pack = |new: &str, old: &str| {
            git(
                &dir,
                &["pack-objects", "--stdout", "--revs", "-q"],
                Some(&format!("{new}\n^{old}\n")),
            )
        };
        assert_eq!(moves_forward(&a, &c, &pack(&c, &a)), Ok(true));
        assert_eq!(moves_forward(&b, &c, &pack(&c, &b)), Ok(true));
        assert_eq!(moves_forward(&c, &c, &[]), Ok(true));
        // Backwards: nothing to send, and c is not an ancestor of a.
        assert_eq!(moves_forward(&c, &a, &pack(&a, &c)), Ok(false));
        // Sideways: d branches from a, so it is no fast-forward of c, nor c of d.
        assert_eq!(moves_forward(&c, &d, &pack(&d, &c)), Ok(false));
        assert_eq!(moves_forward(&d, &c, &pack(&c, &d)), Ok(false));
        assert!(moves_forward(&a, &c, b"PACK junk").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_refused_update_is_named() {
        let h = "a".repeat(40);
        let cs = [
            cmd(&h, &h, P),
            cmd(ZERO, &h, "refs/tags/v1"),
            cmd(&h, ZERO, "refs/heads/claude/y"),
        ];
        let refused = check_all(&cs, P, X).unwrap_err();
        let names: Vec<&str> = refused.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [P, "refs/tags/v1"]);
        assert_eq!(check_all(&cs[2..], P, X), Ok(vec![Rule::Stream]));
    }
}
