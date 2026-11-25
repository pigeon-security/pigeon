//! Capabilities hosted by another Worker, reached over a Cloudflare Service Binding. A remote
//! capability looks like any other `Capability` to core: dispatch, the tool list, console
//! sections, secrets, queue fan-out and the architecture page need no special case. Core asks
//! each bound Worker for its manifest and keeps it a minute, so a capability deploy needs no
//! core deploy. Calls carry the caller, signed with CALLER_KEY. A Worker that does not answer is
//! left out and shown as unreachable, rather than failing everything.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use pigeon_contract::{
    Answer, CallRequest, CapabilityManifest, EventsRequest, FailureKind, SecretOp, SecretRequest,
    SectionRequest, WorkerContext, WorkerManifest,
};
use pigeon_core::mcp::Tool;
use pigeon_core::{secrets_store, webauthn};
use serde_json::{Value, json};
use worker::{Env, Request, RequestInit, Result};

use crate::console::sections::{Section, Tab};
use crate::expiry::Expiry;
use crate::{Call, Capability, Context, Expiring, Footprint, Serve, Slot, users};

/// A capability Worker bound to this one: its Service Binding and its script name.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// The Service Binding's name in this Worker's environment, such as `SITES`.
    pub binding: &'static str,
    /// The bound Worker's script name, such as `pigeon-sites`.
    pub worker: &'static str,
}

/// The Secrets Store binding holding the key that signs callers (docs/plans/workers-split.md).
pub const CALLER_KEY: &str = "CALLER_KEY";

/// How long a manifest is kept in this isolate, in milliseconds.
pub const MANIFEST_TTL_MS: i64 = 60_000;

/// The address the bound Worker is asked at; the host is only a placeholder.
const BASE: &str = "https://capability";

/// A manifest (or why it could not be had), and when it was asked for.
struct Kept {
    at_ms: i64,
    worker: &'static str,
    manifest: std::result::Result<WorkerManifest, String>,
}

thread_local! {
    static KEPT: RefCell<HashMap<&'static str, Kept>> = RefCell::new(HashMap::new());
    static INTERNED: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
    static SCOPES: RefCell<HashMap<String, &'static [&'static str]>> = RefCell::new(HashMap::new());
}

fn now_ms() -> i64 {
    i64::try_from(worker::Date::now().as_millis()).unwrap_or(i64::MAX)
}

/// Leaks a string once per distinct value, so it can be `&'static`.
fn intern(s: &str) -> &'static str {
    INTERNED.with(|set| {
        let mut set = set.borrow_mut();
        if let Some(found) = set.get(s) {
            return *found;
        }
        let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
        set.insert(leaked);
        leaked
    })
}

/// Leaks a list of scopes once per distinct list.
pub(crate) fn intern_scopes(scopes: &[String]) -> &'static [&'static str] {
    let key = scopes.join(",");
    let found = SCOPES.with(|map| map.borrow().get(&key).copied());
    if let Some(found) = found {
        return found;
    }
    let list: Vec<&'static str> = scopes.iter().map(|s| intern(s)).collect();
    let leaked: &'static [&'static str] = Box::leak(list.into_boxed_slice());
    SCOPES.with(|map| map.borrow_mut().insert(key, leaked));
    leaked
}

fn failure(message: impl Into<String>) -> worker::Error {
    worker::Error::RustError(message.into())
}

fn json_of<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| failure(e.to_string()))
}

/// The capabilities of one binding's manifest, kept or asked for. A failure is kept a sixth as
/// long, so a Worker that is down does not slow every request.
async fn manifest_of(env: &Env, binding: &Binding) -> std::result::Result<WorkerManifest, String> {
    let now = now_ms();
    let fresh = KEPT.with(|kept| {
        let kept = kept.borrow();
        let entry = kept.get(binding.binding)?;
        let ttl = match entry.manifest {
            Ok(_) => MANIFEST_TTL_MS,
            Err(_) => MANIFEST_TTL_MS / 6,
        };
        if now - entry.at_ms < ttl {
            Some(entry.manifest.clone())
        } else {
            None
        }
    });
    if let Some(fresh) = fresh {
        return fresh;
    }
    let asked = match env.service(binding.binding) {
        Ok(fetcher) => pigeon_contract::manifest_with_context(&fetcher)
            .await
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    KEPT.with(|kept| {
        kept.borrow_mut().insert(
            binding.binding,
            Kept {
                at_ms: now_ms(),
                worker: binding.worker,
                manifest: asked.clone(),
            },
        );
    });
    asked
}

/// The capabilities behind `bindings`, from their manifests, each kept [`MANIFEST_TTL_MS`] in
/// this isolate. A binding whose manifest cannot be had is left out, and listed by
/// [`unreachable`].
pub async fn capabilities(env: &Env, bindings: &[Binding]) -> Vec<Box<dyn Capability>> {
    let mut out: Vec<Box<dyn Capability>> = Vec::new();
    for binding in bindings {
        if let Ok(offer) = manifest_of(env, binding).await {
            let contextual =
                offer.supports_worker_context() && offer.manifest.worker == binding.worker;
            for spec in offer.manifest.capabilities {
                let mut remote = Remote::new(binding, spec);
                remote.contextual = contextual;
                out.push(Box::new(remote));
            }
        }
    }
    out
}

/// The bound Workers whose last manifest could not be had, with why (for the architecture
/// page and `manifest_refresh`).
pub fn unreachable() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = KEPT.with(|kept| {
        kept.borrow()
            .values()
            .filter_map(|k| match &k.manifest {
                Err(why) => Some((k.worker.to_string(), why.clone())),
                Ok(_) => None,
            })
            .collect()
    });
    out.sort();
    out
}

/// Forgets every kept manifest, so the next request asks again (the `manifest_refresh` tool).
pub fn forget() {
    KEPT.with(|kept| kept.borrow_mut().clear());
}

/// One capability of a capability Worker.
pub struct Remote {
    binding: &'static str,
    worker: &'static str,
    name: &'static str,
    spec: CapabilityManifest,
    tools: Vec<Tool>,
    routes: Vec<&'static str>,
    contextual: bool,
}

impl Remote {
    /// Builds a bound Worker's capability from its manifest (used by the smoke suite too).
    pub fn new(binding: &Binding, spec: CapabilityManifest) -> Self {
        let tools = spec
            .tools
            .iter()
            .map(|t| Tool {
                name: intern(&t.name),
                description: intern(&t.description),
                input_schema: t.input_schema.clone(),
            })
            .collect();
        let routes = spec.routes.iter().map(|r| intern(r)).collect();
        Self {
            binding: binding.binding,
            worker: binding.worker,
            name: intern(&spec.name),
            spec,
            tools,
            routes,
            contextual: false,
        }
    }

    /// Asks the bound Worker; the caller is signed only when one is given.
    async fn ask(
        &self,
        env: &Env,
        op: &str,
        body: &Value,
        caller: Option<&Value>,
    ) -> Result<Answer> {
        let fetcher = env.service(self.binding)?;
        let nonce = webauthn::random()?;
        match caller {
            Some(caller) => {
                let key = caller_key(env).await?;
                pigeon_contract::ask(
                    &fetcher,
                    BASE,
                    op,
                    body,
                    Some((key.as_bytes(), caller)),
                    &nonce,
                )
                .await
            }
            None => pigeon_contract::ask(&fetcher, BASE, op, body, None, &nonce).await,
        }
    }

    /// An answer as a result, naming this Worker in errors.
    fn outcome(&self, answer: Answer) -> Result<Value> {
        match answer {
            Answer::Ok(value) => Ok(value),
            Answer::Error(f) => Err(failure(format!("{}: {}", self.worker, f.message))),
        }
    }

    async fn secret(
        &self,
        cx: &Context<'_>,
        op: SecretOp,
        name: &str,
        value: Option<String>,
        id: Option<String>,
    ) -> Result<Answer> {
        let body = json_of(&SecretRequest {
            capability: self.name.to_string(),
            op,
            name: name.to_string(),
            value,
            id,
        })?;
        self.ask(cx.env, "secret", &body, Some(&caller_json(cx)))
            .await
    }
}

async fn caller_key(env: &Env) -> Result<String> {
    let missing = || failure("the caller key (CALLER_KEY) is not set in the Secrets Store");
    let store = env.secret_store(CALLER_KEY).map_err(|_| missing())?;
    secrets_store::read(&store).await?.ok_or_else(missing)
}

/// Sends a background call after its workflow validated the current approved instance and
/// selected its caller. This path has no legacy transport.
pub async fn call_owned_instance(
    env: Env,
    binding: &Binding,
    capability: &str,
    tool: &str,
    args: Value,
    caller: Value,
    instance: &crate::instances::record::Instance,
) -> Result<Value> {
    if args["instance"].as_str() != Some(instance.id.as_str()) {
        return Err(failure(
            "background call arguments do not match the approved instance",
        ));
    }
    let offer = manifest_of(&env, binding).await.map_err(failure)?;
    let context = instance_context(
        binding.worker,
        offer.supports_worker_context() && offer.manifest.worker == binding.worker,
        &instance.id,
    )
    .map_err(failure)?;
    let fetcher = env.service(binding.binding)?;
    let key = caller_key(&env).await?;
    let nonce = webauthn::random()?;
    let body = json_of(&CallRequest {
        capability: Some(capability.to_string()),
        tool: tool.to_string(),
        args,
    })?;
    match pigeon_contract::ask_with_context(
        &fetcher,
        BASE,
        "call",
        &body,
        (key.as_bytes(), &caller),
        &nonce,
        &context,
    )
    .await?
    {
        Answer::Ok(value) => Ok(value),
        Answer::Error(error) => Err(failure(format!("{}: {}", binding.binding, error.message))),
    }
}

/// Select the scoped transport before sending. An unsupported receiver never gets a
/// legacy copy of an instance-bound call.
pub fn instance_context(
    worker: &str,
    supported: bool,
    instance: &str,
) -> std::result::Result<WorkerContext, String> {
    if !supported {
        return Err(format!(
            "{worker} does not advertise signed instance context"
        ));
    }
    Ok(WorkerContext::new(worker, Some(instance)))
}

fn caller_json(cx: &Context<'_>) -> Value {
    serde_json::to_value(cx.caller).unwrap_or(Value::Null)
}

/// An expiry as a capability Worker answers it: `{"kind": "at", "at_ms": n}`, `{"kind":
/// "never"}`, `{"kind": "refused", "why": s}` or `{"kind": "unknown", "why": s}`.
fn expiry_of(v: &Value) -> Expiry {
    let why = || {
        v.get("why")
            .and_then(Value::as_str)
            .unwrap_or("no reason given")
            .to_string()
    };
    match v.get("kind").and_then(Value::as_str) {
        Some("at") => match v.get("at_ms").and_then(Value::as_i64) {
            Some(at_ms) => Expiry::At { at_ms },
            None => Expiry::Unknown {
                why: "the answer had no at_ms".to_string(),
            },
        },
        Some("never") => Expiry::Never,
        Some("refused") => Expiry::Refused { why: why() },
        Some("unknown") => Expiry::Unknown { why: why() },
        _ => Expiry::Unknown {
            why: "the answer was not an expiry".to_string(),
        },
    }
}

impl Capability for Remote {
    fn worker(&self) -> Option<&str> {
        Some(self.worker)
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn tools(&self) -> Vec<Tool> {
        self.tools
            .iter()
            .map(|t| Tool {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema.clone(),
            })
            .collect()
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            let body = json_of(&CallRequest {
                capability: Some(self.name.to_string()),
                tool: tool.to_string(),
                args: args.clone(),
            })?;
            let caller = caller_json(cx);
            let answer = if let Some(scope) = cx.instance {
                let context = instance_context(self.worker, self.contextual, scope.instance())
                    .map_err(failure)?;
                let fetcher = cx.env.service(self.binding)?;
                let nonce = webauthn::random()?;
                let key = caller_key(cx.env).await?;
                pigeon_contract::ask_with_context(
                    &fetcher,
                    BASE,
                    "call",
                    &body,
                    (key.as_bytes(), &caller),
                    &nonce,
                    &context,
                )
                .await?
            } else {
                // Platform calls stay compatible with peers running the older envelope.
                self.ask(cx.env, "call", &body, Some(&caller)).await?
            };
            match answer {
                Answer::Error(f) if matches!(f.kind, FailureKind::UnknownTool) => {
                    Err(crate::unknown(tool))
                }
                other => self.outcome(other),
            }
        })
    }

    fn wants(&self, event: &Value) -> bool {
        self.spec
            .wants
            .iter()
            .any(|pattern| pigeon_contract::wants(pattern, event))
    }

    fn on_events<'a>(&'a self, cx: &'a Context<'a>) -> Call<'a> {
        Box::pin(async move {
            let body = json_of(&EventsRequest {
                messages: Vec::new(),
            })?;
            let answer = self.ask(cx.env, "events", &body, None).await?;
            self.outcome(answer)
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        self.routes.clone()
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, mut req: Request) -> Serve<'a> {
        Box::pin(async move {
            let url = req.url()?;
            let mut target = format!("{BASE}/fetch{}", url.path());
            if let Some(query) = url.query() {
                target.push('?');
                target.push_str(query);
            }
            let bytes = req.bytes().await?;
            let mut init = RequestInit::new();
            init.with_method(req.method())
                .with_headers(req.headers().clone());
            if !bytes.is_empty() {
                init.with_body(Some(
                    worker::js_sys::Uint8Array::from(bytes.as_slice()).into(),
                ));
            }
            let forwarded = Request::new_with_init(&target, &init)?;
            let fetcher = cx.env.service(self.binding)?;
            fetcher.fetch_request(forwarded).await
        })
    }

    fn footprint(&self) -> Footprint {
        serde_json::from_value(self.spec.footprint.clone()).unwrap_or_default()
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        self.spec.versions.clone()
    }

    fn sections(&self) -> Vec<Section> {
        self.spec
            .sections
            .iter()
            .filter_map(|s| {
                let tab = Tab::ALL
                    .iter()
                    .copied()
                    .find(|t| t.path().trim_start_matches('/') == s.tab)?;
                Some(Section {
                    tab,
                    id: intern(&s.id),
                    title: intern(&s.title),
                    description: intern(&s.description),
                    order: s.order,
                    cost: s.cost.as_deref().map(intern),
                })
            })
            .collect()
    }

    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        id: &'a str,
        person: &'a users::User,
        query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let body = json_of(&SectionRequest {
                capability: self.name.to_string(),
                id: id.to_string(),
                query: query.clone(),
            })?;
            let user = serde_json::to_value(person).unwrap_or(Value::Null);
            let caller = json!({ "user": user, "acting_for": null });
            let answer = self.ask(cx.env, "section", &body, Some(&caller)).await?;
            self.outcome(answer)
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        self.spec
            .secrets
            .iter()
            .map(|s| Slot {
                name: s.name.clone(),
                purpose: s.purpose.clone(),
                scopes: intern_scopes(&s.scopes),
            })
            .collect()
    }

    fn check_secret<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move {
            let answer = self
                .secret(cx, SecretOp::Check, name, Some(value.to_string()), None)
                .await?;
            self.outcome(answer)
        })
    }

    fn secret_set<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, id: &'a str) -> Call<'a> {
        Box::pin(async move {
            let answer = self
                .secret(cx, SecretOp::Set, name, None, Some(id.to_string()))
                .await?;
            self.outcome(answer)
        })
    }

    fn secret_expiry<'a>(&'a self, cx: &'a Context<'a>, name: &'a str) -> Expiring<'a> {
        Box::pin(async move {
            Ok(
                match self.secret(cx, SecretOp::Expiry, name, None, None).await {
                    Ok(Answer::Ok(value)) => expiry_of(&value),
                    Ok(Answer::Error(f)) => Expiry::Unknown { why: f.message },
                    Err(e) => Expiry::Unknown { why: e.to_string() },
                },
            )
        })
    }
}
