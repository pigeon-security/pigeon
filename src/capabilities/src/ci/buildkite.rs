//! The Buildkite connector: a job is a build of one pigeon pipeline, on Buildkite's hosted
//! agents. The build carries the job in its env and in its `pigeon_job` meta-data; the
//! pipeline's steps (see [`steps_yaml`]) pick the queue for the job's class with
//! `if: build.env("PIGEON_QUEUE") == …`, skip Buildkite's checkout, and run the platform's
//! bootstrap. The bootstrap exchanges the job's Buildkite OIDC token for a job token at
//! `/ci/token`, so no secret passes through build settings, then runs the same runner as
//! Containers: it fetches the commit through the git proxy and reports on `/ci/runner/{job}`.
//!
//! Webhooks (`build.running`, `build.finished`) are verified with their HMAC signature before
//! anything in them is read, and only builds the gateway started, matched by handle, are acted
//! on. A build that ends without its runner having reported is an infrastructure failure, which
//! the gateway retries elsewhere once.

use pigeon_core::buildkite::{self as api, Pipeline};
use pigeon_core::buildkite_hook::{self as hook, Event};
use pigeon_core::oidc::{self, Expect};
use pigeon_core::secrets_store;
use serde_json::{Map, Value, json};
use worker::{Error, Result, js_sys};

use super::class::{Class, Offered, Size};
use super::config::{Arch, Os};
use super::route::Availability;
use super::state::Report;
use super::{Connector, Cx, Dispatched, Hook, Pending, Run, Seen};
use crate::Slot;
use crate::expiry::{self, Expiry};

/// The connector's name.
pub const NAME: &str = "buildkite";
/// The Secrets Store bindings of its two secrets.
pub const API_TOKEN: &str = "BUILDKITE_API_TOKEN";
pub const WEBHOOK_TOKEN: &str = "BUILDKITE_WEBHOOK_TOKEN";
/// Buildkite's OIDC issuer, shared by every organization, and its keys.
pub const ISSUER: &str = "https://agent.buildkite.com";
pub const JWKS: &str = "https://agent.buildkite.com/.well-known/jwks";
/// The API token's scopes the connector needs.
pub const SCOPES: [&str; 2] = ["read_builds", "write_builds"];
/// How far a webhook's timestamp may be from now, either way.
const HOOK_WINDOW_SECS: i64 = 300;

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn now_secs() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// A cluster queue and the class it runs.
pub struct Queue {
    /// The queue's key in the cluster, such as `pigeon-linux-small`.
    pub key: &'static str,
    pub offered: Offered,
}

/// The Buildkite connector's settings.
pub struct Buildkite {
    /// The organization's and the pipeline's slugs.
    pub org: String,
    pub pipeline: String,
    /// The queues jobs go to, by class.
    pub queues: Vec<Queue>,
    /// Whether only tokens from Buildkite's hosted agents are exchanged.
    pub hosted_only: bool,
}

/// A queue running `class` with these real limits.
pub fn queue(key: &'static str, class: Class, cpu: u32, memory_gb: u32, disk_gb: u32) -> Queue {
    Queue {
        key,
        offered: Offered {
            class,
            cpu,
            memory_gb,
            disk_gb,
            // Hosted jobs may run 8 hours; the gateway's own limit is lower.
            max_timeout_min: 360,
            network_none: false,
        },
    }
}

/// A Linux amd64 class of `size`, with or without docker.
pub fn linux(size: Size, docker: bool) -> Class {
    Class {
        os: Os::Linux,
        arch: Arch::Amd64,
        size,
        docker,
        gpu: false,
    }
}

/// The pipeline's steps, to paste into its YAML steps editor: one step per queue, chosen by
/// the build's `PIGEON_QUEUE`, each skipping Buildkite's checkout and running the platform's
/// bootstrap. It holds no `$`, so nothing in it is interpolated.
pub fn steps_yaml(queues: &[&str], host: &str) -> String {
    let mut out = String::from("steps:\n");
    for q in queues {
        out.push_str(&format!(
            "  - label: \":pigeon: {q}\"\n    key: \"{q}\"\n    if: build.env(\"PIGEON_QUEUE\") == \"{q}\"\n    agents:\n      queue: \"{q}\"\n    checkout:\n      skip: true\n    command: \"curl -fsSL --retry 3 https://{host}/ci/buildkite.sh | sh\"\n"
        ));
    }
    out
}

/// The handle of build `number` with UUID `id`.
pub fn handle(number: u64, id: &str) -> String {
    format!("{number}/{id}")
}

/// A handle's build number.
fn number_of(handle: &str) -> Result<u64> {
    handle
        .split_once('/')
        .and_then(|(n, _)| n.parse().ok())
        .ok_or_else(|| bad("not a Buildkite build handle"))
}

/// What a build in `state` that has ended says, its runner not having reported: cancelled, or
/// a lost runner. `None` while it has not ended.
fn ended(state: &str) -> Option<Report> {
    match state {
        "scheduled" | "running" | "waiting" | "blocked" | "failing" | "creating" => None,
        "canceled" | "canceling" => Some(Report::Cancelled),
        other => Some(Report::Infra {
            why: format!("the Buildkite build ended {other} without its runner reporting"),
        }),
    }
}

/// The Buildkite build and job a verified OIDC token vouches for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vouched {
    pub build_id: String,
    pub build_number: u64,
    pub job_id: String,
}

fn exact_claim(
    claims: &Map<String, Value>,
    name: &str,
    want: &str,
) -> std::result::Result<(), String> {
    match claims.get(name).and_then(Value::as_str) {
        Some(s) if s == want => Ok(()),
        Some(_) => Err(format!("claim `{name}` does not match the expected value")),
        None => Err(format!("claim `{name}` is missing or not a string")),
    }
}

fn uuid_claim(claims: &Map<String, Value>, name: &str) -> std::result::Result<String, String> {
    let s = claims
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("claim `{name}` is missing or not a string"))?;
    if s.is_empty() || s.len() > 64 || !s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(format!("claim `{name}` is not a valid UUID"));
    }
    Ok(s.to_owned())
}

fn build_number_claim(claims: &Map<String, Value>) -> std::result::Result<u64, String> {
    const BAD: &str = "claim `build_number` is not a positive integer";
    let n = match claims.get("build_number") {
        None => return Err("claim `build_number` is missing".to_owned()),
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| BAD.to_owned())?,
        Some(Value::String(s)) => {
            if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(BAD.to_owned());
            }
            s.parse::<u64>().map_err(|_| BAD.to_owned())?
        }
        Some(_) => return Err(BAD.to_owned()),
    };
    if n < 1 {
        return Err(BAD.to_owned());
    }
    Ok(n)
}

/// Checks the Buildkite claims of an OIDC token whose signature, issuer, audience and times are
/// already verified. Every organization shares the issuer, so the organization and pipeline
/// claims are what keep another organization's token out.
pub fn vouched(
    claims: &Map<String, Value>,
    org: &str,
    pipeline: &str,
    hosted_only: bool,
) -> std::result::Result<Vouched, String> {
    exact_claim(claims, "organization_slug", org)?;
    exact_claim(claims, "pipeline_slug", pipeline)?;
    if hosted_only {
        exact_claim(claims, "runner_environment", "buildkite-hosted")?;
    }
    let build_id = uuid_claim(claims, "build_id")?;
    let job_id = uuid_claim(claims, "job_id")?;
    let build_number = build_number_claim(claims)?;
    Ok(Vouched {
        build_id,
        build_number,
        job_id,
    })
}

/// The audience a Buildkite OIDC token for the exchange must name.
pub fn audience(host: &str) -> String {
    format!("https://{host}/ci/token")
}

/// Secret `binding`'s value, when it is bound and set.
async fn secret(cx: &Cx<'_>, binding: &str) -> Option<String> {
    let store = cx.env.secret_store(binding).ok()?;
    secrets_store::read(&store)
        .await
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// What Buildkite's answer to `GET /v2/access-token` says of the token's expiry. A token it
/// refuses with 401 has expired or been revoked; one whose answer names no expiry is reported
/// as unknown, since Buildkite does not promise to name one.
fn buildkite_expiry(answer: Result<Option<String>>) -> Expiry {
    match answer {
        Ok(Some(at)) => expiry::from_field(Some(&at)),
        Ok(None) => Expiry::Unknown {
            why: "Buildkite's answer names no expiry".into(),
        },
        Err(e) if e.to_string().contains(": 401") => Expiry::Refused {
            why: "Buildkite refuses the API token (401): it has expired or been revoked".into(),
        },
        Err(e) => Expiry::Unknown {
            why: format!("the check failed: {e}"),
        },
    }
}

impl Buildkite {
    fn api<'a>(&'a self, token: &'a str) -> Pipeline<'a> {
        Pipeline {
            token,
            org: &self.org,
            pipeline: &self.pipeline,
        }
    }

    async fn token(&self, cx: &Cx<'_>) -> Result<String> {
        secret(cx, API_TOKEN)
            .await
            .ok_or_else(|| bad("BUILDKITE_API_TOKEN is not set"))
    }

    /// The queue of the class routing picked for a job.
    fn queue_for(&self, class: &Class) -> Option<&Queue> {
        self.queues.iter().find(|q| q.offered.class == *class)
    }

    /// Verifies a Buildkite OIDC token for the exchange and answers the build it vouches for.
    pub async fn exchange_claims(&self, cx: &Cx<'_>, jwt: &str) -> Result<Vouched> {
        let jwks = api::json(JWKS).await?;
        let aud = audience(cx.host);
        let claims = oidc::verify(
            jwt,
            &jwks,
            &Expect {
                issuer: ISSUER,
                audience: &aud,
                now: now_secs(),
                leeway: 60,
                max_lifetime: 3_600,
            },
        )
        .map_err(|e| bad(&format!("the OIDC token: {e}")))?;
        vouched(&claims, &self.org, &self.pipeline, self.hosted_only).map_err(|e| bad(&e))
    }
}

impl Connector for Buildkite {
    fn name(&self) -> &'static str {
        NAME
    }

    fn offers(&self) -> Vec<Offered> {
        self.queues.iter().map(|q| q.offered.clone()).collect()
    }

    fn secrets(&self) -> Vec<Slot> {
        vec![
            Slot {
                name: API_TOKEN.into(),
                purpose: format!(
                    "A Buildkite API access token for organization `{}`, with the scopes \
                     read_builds and write_builds and nothing else (Buildkite: Personal \
                     Settings, API Access Tokens, New API Access Token). The CI gateway uses \
                     it to start, read and cancel builds of pipeline `{}`.",
                    self.org, self.pipeline
                ),
                scopes: &[secrets_store::WORKERS],
            },
            Slot {
                name: WEBHOOK_TOKEN.into(),
                purpose: format!(
                    "The token of pipeline `{}`'s webhook notification to \
                     /ci/hooks/buildkite, sent as an HMAC signature in X-Buildkite-Signature \
                     (Buildkite: the notification's Token field). The CI gateway checks every \
                     delivery with it before reading it.",
                    self.pipeline
                ),
                scopes: &[secrets_store::WORKERS],
            },
        ]
    }

    /// The API token's expiry is what `GET /v2/access-token` answers in `expires_at`; the
    /// webhook token is a shared signing key, which never expires.
    fn secret_expiry<'a>(&'a self, cx: &'a Cx<'a>, name: &'a str) -> Pending<'a, Expiry> {
        Box::pin(async move {
            if name == WEBHOOK_TOKEN {
                return Ok(Expiry::Never);
            }
            if name != API_TOKEN {
                return Ok(Expiry::Unknown {
                    why: "Buildkite does not use this secret".into(),
                });
            }
            let token = self.token(cx).await?;
            Ok(buildkite_expiry(api::expires_at(&token).await))
        })
    }

    fn available<'a>(&'a self, cx: &'a Cx<'a>) -> Pending<'a, Availability> {
        Box::pin(async move {
            let (Some(token), Some(_)) =
                (secret(cx, API_TOKEN).await, secret(cx, WEBHOOK_TOKEN).await)
            else {
                return Ok(Availability::default());
            };
            if self.org.is_empty() || self.queues.is_empty() {
                return Ok(Availability::default());
            }
            let healthy = match api::scopes(&token).await {
                Ok(scopes) => SCOPES.iter().all(|s| scopes.iter().any(|h| h == s)),
                Err(e) => {
                    worker::console_error!("ci: buildkite: {e}");
                    false
                }
            };
            Ok(Availability {
                configured: true,
                healthy,
                free: None,
                queued: 0,
            })
        })
    }

    fn dispatch<'a>(&'a self, cx: &'a Cx<'a>, run: &'a Run) -> Pending<'a, Dispatched> {
        Box::pin(async move {
            let queue = self
                .queue_for(&run.class)
                .ok_or_else(|| bad(&format!("no Buildkite queue runs {}", run.class)))?;
            let token = self.token(cx).await?;
            let api = self.api(&token);
            let mut env = json!({
                "PIGEON_JOB": run.job,
                "PIGEON_QUEUE": queue.key,
                "PIGEON_COMMIT": run.commit,
                "PIGEON_SCRIPT": run.script,
                "PIGEON_TIMEOUT_MIN": run.timeout_min.to_string(),
                "PIGEON_GIT_URL": run.git_url,
                "PIGEON_RUNNER_URL": run.runner_url,
                "PIGEON_TOKEN_URL": audience(cx.host),
            });
            if run.pass_token {
                env["PIGEON_PASS_TOKEN"] = json!("1");
            }
            let build = api
                .create(&json!({
                    "commit": run.commit,
                    "branch": format!("pigeon-ci/{}", run.job),
                    "message": format!("{} ({}, attempt {})", run.name, run.job, run.attempt),
                    "env": env,
                    "meta_data": { "pigeon_job": run.job },
                    "ignore_pipeline_branch_filters": true,
                }))
                .await?;
            let id = build["id"]
                .as_str()
                .ok_or_else(|| bad("Buildkite answered a build without its id"))?;
            let number = build["number"]
                .as_u64()
                .ok_or_else(|| bad("Buildkite answered a build without its number"))?;
            Ok(Dispatched {
                handle: handle(number, id),
                url: build["web_url"].as_str().map(String::from),
            })
        })
    }

    fn status<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, Option<Report>> {
        Box::pin(async move {
            let token = self.token(cx).await?;
            let build = self.api(&token).get(number_of(handle)?).await?;
            let state = build["state"].as_str().unwrap_or("");
            Ok(match state {
                "running" => Some(Report::Started),
                s => ended(s),
            })
        })
    }

    fn cancel<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, ()> {
        Box::pin(async move {
            let token = self.token(cx).await?;
            self.api(&token).cancel(number_of(handle)?).await
        })
    }

    fn webhook<'a>(&'a self, cx: &'a Cx<'a>, h: &'a Hook) -> Pending<'a, Option<Seen>> {
        Box::pin(async move {
            let key = secret(cx, WEBHOOK_TOKEN).await.unwrap_or_default();
            hook::verify(
                h.header("x-buildkite-signature"),
                &h.body,
                key.as_bytes(),
                now_secs(),
                HOOK_WINDOW_SECS,
            )
            .map_err(|e| bad(&e))?;
            Ok(seen(
                hook::read(h.header("x-buildkite-event"), &h.body).map_err(|e| bad(&e))?,
            ))
        })
    }

    fn exchange<'a>(&'a self, cx: &'a Cx<'a>, jwt: &'a str) -> Pending<'a, Option<String>> {
        Box::pin(async move {
            let v = self.exchange_claims(cx, jwt).await?;
            Ok(Some(handle(v.build_number, &v.build_id)))
        })
    }
}

/// What a verified delivery says of one of the gateway's builds: none for a ping, another
/// event, or a build without the `pigeon_job` meta-data the gateway sets.
fn seen(event: Event) -> Option<Seen> {
    let (build, report) = match event {
        Event::Running(b) => (b, Some(Report::Started)),
        Event::Finished(b) => {
            let r = ended(&b.state);
            (b, r)
        }
        Event::Ping | Event::Other(_) => return None,
    };
    build.job.as_ref()?;
    Some(Seen {
        handle: handle(build.number, &build.id),
        report: report?,
        url: build.web_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigeon_core::buildkite_hook::Build;

    const B: &str = "0190a1b2-c3d4-e5f6-0718-293a4b5c6d7e";
    const J: &str = "0190a1b2-c3d4-e5f6-0718-293a4b5c6d7f";

    fn good() -> Map<String, Value> {
        let Value::Object(m) = json!({
            "organization_slug": "acme", "pipeline_slug": "pigeon", "build_id": B,
            "build_number": 42, "job_id": J, "runner_environment": "buildkite-hosted",
        }) else {
            unreachable!()
        };
        m
    }

    fn with(k: &str, v: Value) -> Map<String, Value> {
        let mut m = good();
        m.insert(k.to_owned(), v);
        m
    }

    fn without(k: &str) -> Map<String, Value> {
        let mut m = good();
        m.remove(k);
        m
    }

    fn check(m: &Map<String, Value>, hosted: bool) -> std::result::Result<Vouched, String> {
        vouched(m, "acme", "pigeon", hosted)
    }

    #[test]
    fn a_token_from_our_pipeline_vouches_for_its_build() {
        let want = Vouched {
            build_id: B.into(),
            build_number: 42,
            job_id: J.into(),
        };
        assert_eq!(check(&good(), true), Ok(want.clone()));
        assert_eq!(check(&with("build_number", json!("42")), true), Ok(want));
    }

    #[test]
    fn another_organization_or_pipeline_is_refused() {
        let e = check(&with("organization_slug", json!("evil")), false).unwrap_err();
        assert!(e.contains("organization_slug") && !e.contains("evil"));
        assert!(
            check(&with("pipeline_slug", json!("other")), false)
                .unwrap_err()
                .contains("pipeline_slug")
        );
        assert!(check(&with("organization_slug", json!(5)), false).is_err());
    }

    #[test]
    fn build_and_job_ids_must_be_uuids_and_the_number_positive() {
        assert!(check(&without("build_id"), false).is_err());
        for bad in [format!("{B}/x"), format!("{B} x"), String::new()] {
            assert!(check(&with("build_id", json!(bad)), false).is_err());
        }
        for bad in [json!(0), json!(-1), json!("4x"), json!("0"), json!(1.5)] {
            assert!(check(&with("build_number", bad), false).is_err());
        }
        assert!(check(&without("build_number"), false).is_err());
        assert!(check(&without("job_id"), false).is_err());
    }

    #[test]
    fn hosted_only_refuses_other_agents() {
        let self_hosted = with("runner_environment", json!("self-hosted"));
        assert!(check(&self_hosted, true).is_err());
        assert!(check(&without("runner_environment"), true).is_err());
        assert!(check(&self_hosted, false).is_ok());
    }

    #[test]
    fn handles_carry_the_number_first() {
        assert_eq!(handle(7, "abc"), "7/abc");
        assert_eq!(number_of("7/abc").unwrap(), 7);
        assert!(number_of("abc").is_err());
    }

    #[test]
    fn the_steps_pick_a_queue_skip_checkout_and_hold_no_dollar() {
        let y = steps_yaml(
            &["pigeon-linux-small", "pigeon-linux-medium"],
            "platform.pigeon.dev",
        );
        assert!(!y.contains('$'));
        assert_eq!(y.matches("- label:").count(), 2);
        assert!(y.contains("if: build.env(\"PIGEON_QUEUE\") == \"pigeon-linux-medium\""));
        assert!(y.contains("checkout:\n      skip: true"));
        assert!(y.contains("https://platform.pigeon.dev/ci/buildkite.sh"));
    }

    fn build(state: &str, job: Option<&str>) -> Build {
        Build {
            id: B.into(),
            number: 9,
            state: state.into(),
            web_url: Some("https://buildkite.com/acme/pigeon/builds/9".into()),
            job: job.map(String::from),
        }
    }

    #[test]
    fn only_our_builds_are_seen_and_an_unreported_end_is_lost() {
        assert_eq!(seen(Event::Ping), None);
        assert_eq!(seen(Event::Other("job.started".into())), None);
        assert_eq!(seen(Event::Finished(build("passed", None))), None);
        let s = seen(Event::Running(build("running", Some("j1")))).unwrap();
        assert_eq!(
            (s.handle.as_str(), &s.report),
            (&*format!("9/{B}"), &Report::Started)
        );
        assert!(matches!(
            seen(Event::Finished(build("passed", Some("j1"))))
                .unwrap()
                .report,
            Report::Infra { .. }
        ));
        assert_eq!(
            seen(Event::Finished(build("canceled", Some("j1"))))
                .unwrap()
                .report,
            Report::Cancelled
        );
        assert_eq!(seen(Event::Finished(build("blocked", Some("j1")))), None);
    }

    #[test]
    fn the_api_tokens_expiry_comes_from_access_token() {
        assert_eq!(
            buildkite_expiry(Ok(Some("2026-12-21T00:00:00.000Z".into()))),
            Expiry::At {
                at_ms: 1_797_811_200_000
            }
        );
        assert!(matches!(buildkite_expiry(Ok(None)), Expiry::Unknown { .. }));
        let refused = Err(Error::RustError(
            "Buildkite refused the API token: 401 Authentication required".into(),
        ));
        assert!(matches!(buildkite_expiry(refused), Expiry::Refused { .. }));
        let down = Err(Error::RustError(
            "Buildkite refused the API token: 503 ".into(),
        ));
        assert!(matches!(buildkite_expiry(down), Expiry::Unknown { .. }));
    }
}
