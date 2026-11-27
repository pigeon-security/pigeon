//! One signed ingest run per Durable Object. The stored lifecycle never contains credentials.

use pigeon_core::containers::{self, Start as ContainerStart};
use pigeon_ingest::container_job::{
    self, MAX_RUNTIME_MS, Outcome, SPOOL_BYTES, Start, Terminal, TerminalNotice,
};
use pigeon_ingest::manifest::{self, Counters, RunState, Status};
use pigeon_ingest::protocol::{self, StopRequest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::wasm_bindgen::JsValue;
use worker::{
    DurableObject, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result, State, Url,
    wasm_bindgen,
};

use crate::{fail, json_answer, now_ms};

const IMAGE: &str = "ingest";
const INSTANCE: &str = "standard-4";
const RUNNER: &str = "/usr/local/bin/pigeon-ingest-runner";
const POLL_MS: i64 = 30_000;
const START_GRACE_MS: i64 = 120_000;
const KEY: &str = "run";

#[derive(Deserialize, Serialize)]
struct Run {
    id: String,
    started_ms: i64,
    deadline_ms: i64,
    requested_stop: Option<Outcome>,
    terminal: Option<Terminal>,
    #[serde(default)]
    report_to_jobs: bool,
    #[serde(default)]
    audit_stored: bool,
}

#[worker::durable_object]
pub struct IngestJob {
    state: State,
    env: Env,
}

impl DurableObject for IngestJob {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        match req.path().as_str() {
            "/ready" => json_answer(
                200,
                json!({
                    "container": containers::present(&self.state),
                    "image_revision": self.image_revision(),
                    "jobs_callback": self.callback_origin().is_some(),
                }),
            ),
            "/start" => {
                let Ok(start) = req.json::<Start>().await else {
                    return json_answer(400, json!({ "error": "not a start request" }));
                };
                self.start(start).await
            }
            "/status" => {
                let Some(run) = self.run().await? else {
                    return json_answer(404, json!({ "error": "unknown run" }));
                };
                self.answer(&run)
            }
            "/stop" => {
                let Ok(stop) = req.json::<StopRequest>().await else {
                    return json_answer(400, json!({ "error": "not a stop request" }));
                };
                self.stop(&stop.run).await
            }
            _ => json_answer(404, json!({ "error": "not found" })),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let Some(mut run) = self.run().await? else {
            return Response::empty();
        };
        if run.terminal.is_none() {
            self.reconcile(&mut run).await?;
        }
        self.deliver(&mut run).await?;
        Response::empty()
    }
}

impl IngestJob {
    fn image_revision(&self) -> Option<String> {
        self.env
            .var("INGEST_IMAGE_REVISION")
            .ok()
            .map(|value| value.to_string())
            .filter(|revision| container_job::valid_revision(revision))
    }

    fn callback_origin(&self) -> Option<String> {
        let origin = self.env.var("PIGEON_PLATFORM_ORIGIN").ok()?.to_string();
        let url = Url::parse(&origin).ok()?;
        (url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && !origin.ends_with('/'))
        .then_some(origin)
    }

    async fn run(&self) -> Result<Option<Run>> {
        self.state.storage().get(KEY).await
    }

    async fn save(&self, run: &Run) -> Result<()> {
        self.state.storage().put(KEY, run).await
    }

    fn answer(&self, run: &Run) -> Result<Response> {
        let state = if run.terminal.is_some() {
            "ended"
        } else if containers::running(&self.state) {
            "running"
        } else {
            "starting"
        };
        json_answer(
            200,
            json!({ "state": state, "terminal": run.terminal, "audit_stored": run.audit_stored }),
        )
    }

    async fn start(&self, start: Start) -> Result<Response> {
        if let Some(run) = self.run().await? {
            return self.answer(&run);
        }
        let began = now_ms();
        let deadline_ms = start.deadline_ms.unwrap_or(began + MAX_RUNTIME_MS);
        if !container_job::valid_deadline(began, deadline_ms) {
            return json_answer(400, json!({ "error": "invalid container deadline" }));
        }
        if let Some(revision) = &start.image_revision
            && (self.image_revision().as_ref() != Some(revision)
                || revision != &start.request.job.commit)
        {
            return json_answer(
                409,
                json!({ "error": "the approved runner image has changed" }),
            );
        }
        if start.report_to_jobs
            && (start.image_revision.is_none() || self.callback_origin().is_none())
        {
            return json_answer(
                409,
                json!({ "error": "the Data Worker needs an approved image and Jobs callback origin" }),
            );
        }
        if !containers::present(&self.state) {
            return json_answer(
                409,
                json!({ "error": "this Data Worker has no ingest container" }),
            );
        }
        let mut env =
            protocol::start_env(&start.request).map_err(|_| fail("invalid ingest job"))?;
        env.push(("SPOOL_BYTES".into(), SPOOL_BYTES.to_string()));
        let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let run = Run {
            id: start.request.job.run,
            started_ms: began,
            deadline_ms,
            requested_stop: None,
            terminal: None,
            report_to_jobs: start.report_to_jobs,
            audit_stored: false,
        };
        // Cloudflare's storage input gate serializes this claim with other starts. Set the alarm
        // before asking the provider, so a lost start response still has a reconciliation path.
        let storage = self.state.storage();
        storage.put(KEY, &run).await?;
        storage.set_alarm(POLL_MS).await?;
        if containers::running(&self.state) {
            // An older deployment could already be running this named object.
            return self.answer(&run);
        }
        if containers::start(
            &self.state,
            &ContainerStart {
                image: IMAGE,
                instance: INSTANCE,
                env: &pairs,
                entrypoint: &[RUNNER],
                internet: true,
            },
        )
        .is_err()
        {
            // Keep the alarm and claim even after a start error. A lost response does not prove
            // the compute was never created.
            return json_answer(
                503,
                json!({ "error": "runner start could not be confirmed" }),
            );
        }
        containers::keep_for(&self.state, deadline_ms - began).await?;
        json_answer(202, json!({ "state": "starting" }))
    }

    async fn stop(&self, id: &str) -> Result<Response> {
        let Some(mut run) = self.run().await? else {
            // Legacy deployments did not persist a run.
            containers::destroy(&self.state, "stopped by pigeon").await?;
            return json_answer(200, json!({ "state": "stopped" }));
        };
        if run.id != id {
            return json_answer(404, json!({ "error": "unknown run" }));
        }
        if run.terminal.is_none() {
            run.requested_stop = Some(Outcome::Cancelled);
            self.save(&run).await?;
            self.reconcile(&mut run).await?;
        }
        self.deliver(&mut run).await?;
        self.answer(&run)
    }

    async fn reconcile(&self, run: &mut Run) -> Result<()> {
        let now = now_ms();
        let storage = self.state.storage();
        storage.set_alarm(POLL_MS).await?;
        if !containers::present(&self.state) {
            // A removed binding does not prove the compute is gone.
            return Ok(());
        }
        if now >= run.deadline_ms && run.requested_stop.is_none() {
            run.requested_stop = Some(Outcome::TimedOut);
            self.save(run).await?;
        }
        if containers::running(&self.state) {
            if run.requested_stop.is_some() {
                containers::destroy(&self.state, "ingest run ended").await?;
            } else {
                containers::keep_for(&self.state, run.deadline_ms - now).await?;
                return Ok(());
            }
        }
        if containers::running(&self.state) {
            return Ok(());
        }
        let status = self.read_status(&run.id).await?;
        let reported = match status.as_ref().map(|status| status.state) {
            Some(RunState::Done) => Some(Outcome::Succeeded),
            Some(RunState::Failed) => Some(Outcome::Failed),
            Some(RunState::Stopped) => Some(Outcome::Cancelled),
            _ => None,
        };
        let outcome = run.requested_stop.or(reported);
        if reported.is_none() && now < run.started_ms + START_GRACE_MS {
            return Ok(());
        }
        // Recheck after fetching R2 so a concurrent stop cannot overwrite an already persisted
        // terminal record, and retain a stop requested while the status was being read.
        let Some(current) = self.run().await? else {
            return Err(fail("missing ingest run"));
        };
        if current.terminal.is_some() {
            *run = current;
            return Ok(());
        }
        run.terminal = Some(Terminal {
            run: run.id.clone(),
            started_ms: run.started_ms,
            finished_ms: now_ms().max(run.started_ms),
            outcome: current
                .requested_stop
                .or(outcome)
                .unwrap_or(Outcome::Failed),
            counters: status.map_or_else(Counters::default, |status| status.counters),
        });
        self.save(run).await
    }

    async fn deliver(&self, run: &mut Run) -> Result<()> {
        let Some(terminal) = run.terminal.as_ref() else {
            return Ok(());
        };
        let storage = self.state.storage();
        if !run.report_to_jobs || run.audit_stored {
            return storage.delete_alarm().await;
        }
        storage.set_alarm(POLL_MS).await?;
        let origin = self
            .callback_origin()
            .ok_or_else(|| fail("ingest Jobs callback origin is unavailable"))?;
        let key = self.env.secret("DATA_KEY")?.to_string();
        let notice = TerminalNotice {
            instance: self.env.var("INSTANCE")?.to_string(),
            terminal: terminal.clone(),
        };
        let body = serde_json::to_string(&notice)?;
        let now = now_ms();
        let path = container_job::TERMINAL_PATH;
        let message = protocol::signing_message("POST", path, now, body.as_bytes());
        let mac = pigeon_core::sign::hmac_sha256(key.as_bytes(), message.as_bytes());
        let signature = format!("t={now},v1={}", pigeon_core::sign::hex(&mac));
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        headers.set(protocol::SIGNATURE_HEADER, &signature)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_redirect(worker::RequestRedirect::Manual)
            .with_body(Some(JsValue::from_str(&body)));
        let res = Fetch::Request(Request::new_with_init(&format!("{origin}{path}"), &init)?)
            .send()
            .await?;
        if res.status_code() != 204 {
            return Ok(());
        }
        run.audit_stored = true;
        self.save(run).await?;
        storage.delete_alarm().await
    }

    async fn read_status(&self, id: &str) -> Result<Option<Status>> {
        let bucket = self.env.bucket("DATA")?;
        let Some(object) = bucket.get(manifest::status_key(id)).execute().await? else {
            return Ok(None);
        };
        let Some(body) = object.body() else {
            return Ok(None);
        };
        let status = serde_json::from_str::<Status>(&body.text().await?).ok();
        Ok(status.filter(|status| status.run == id))
    }
}
