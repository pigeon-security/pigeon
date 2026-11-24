//! The cloud gateway: Scaleway's API keys, buckets and objects behind plans and approvals, run
//! against a mocked Scaleway. The mocks are a transport that records each request and answers
//! canned JSON or XML, slots, approvals and a secrets sink held in memory. Futures are driven
//! by hand: nothing here waits.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pigeon_cloud::approvals::{Approvals, Created, KIND, Record};
use pigeon_cloud::provider::Ports;
use pigeon_cloud::sink::Sink;
use pigeon_cloud::slots::{SlotState, Slots};
use pigeon_cloud::transport::{Fut, HttpRequest, HttpResponse, Transport};
use pigeon_cloud::{iam, redact, s3api, scaleway, tools, transport};

use super::*;

const OWN: &str = "SCWXXXXXXXXXXXXXXXXX";
const SECRET: &str = "22222222-2222-2222-2222-222222222222";
const ORG: &str = "11111111-1111-1111-1111-111111111111";
const PROJ: &str = "33333333-3333-3333-3333-333333333333";
const PROJ2: &str = "44444444-4444-4444-4444-444444444444";
const APP: &str = "55555555-5555-5555-5555-555555555555";
const POLICY: &str = "66666666-6666-6666-6666-666666666666";
const NEW_KEY: &str = "SCWNEWKEY00000000000";
const NEW_SECRET: &str = "99999999-9999-9999-9999-999999999999";
/// 2013-05-24T00:00:00Z, in milliseconds.
const NOW: i64 = 1_369_353_600_000;

/// Runs a future that never waits.
fn block<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match f.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => panic!("the future waited"),
    }
}

/// Scaleway's answer: JSON.
fn json_res(status: u16, v: Value) -> HttpResponse {
    text_res(status, &v.to_string())
}

/// Scaleway's answer: text, such as S3's XML.
fn text_res(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: Vec::new(),
        body: body.into(),
    }
}

/// A URL's host, path and query.
fn split(url: &str) -> (String, String, String) {
    let rest = url.strip_prefix("https://").unwrap();
    let (host, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    (host.into(), format!("/{path}"), query.into())
}

/// The recording transport.
struct Net {
    log: RefCell<Vec<HttpRequest>>,
    handler: Box<dyn Fn(&HttpRequest) -> HttpResponse>,
}

impl Transport for Net {
    fn send<'a>(&'a self, req: HttpRequest) -> Fut<'a, HttpResponse> {
        assert!(transport::allowed(&req.url), "a call to {}", req.url);
        self.log.borrow_mut().push(req.clone());
        let res = (self.handler)(&req);
        Box::pin(async move { Ok(res) })
    }
}

/// Slots held in memory.
struct StubSlots(RefCell<HashMap<String, SlotState>>);

impl Slots for StubSlots {
    fn read<'a>(&'a self, name: &'a str) -> Fut<'a, SlotState> {
        let state = self.0.borrow().get(name).cloned();
        Box::pin(async move { Ok(state.unwrap_or(SlotState::Unbound)) })
    }
}

/// The secrets sink in memory.
#[derive(Default)]
struct Store {
    names: RefCell<Vec<String>>,
    written: RefCell<Vec<(String, String, String)>>,
    fail_write: Cell<bool>,
    fail_exists: Cell<bool>,
}

impl Sink for Store {
    fn exists<'a>(&'a self, slot: &'a str) -> Fut<'a, bool> {
        Box::pin(async move {
            if self.fail_exists.get() {
                return Err(worker::Error::RustError(
                    "no secrets_store credential".into(),
                ));
            }
            Ok(self.names.borrow().iter().any(|n| n == slot))
        })
    }

    fn write<'a>(&'a self, slot: &'a str, value: &'a str, comment: &'a str) -> Fut<'a, ()> {
        Box::pin(async move {
            if self.fail_write.get() {
                // A sink error that holds the value: the gateway must still hide it.
                return Err(worker::Error::RustError(format!(
                    "Cloudflare refused {value}"
                )));
            }
            self.names.borrow_mut().push(slot.into());
            self.written
                .borrow_mut()
                .push((slot.into(), value.into(), comment.into()));
            Ok(())
        })
    }
}

/// Approvals in memory, with core's rules for take: approved, once, for this exact change.
#[derive(Default)]
struct Appr {
    recs: RefCell<Vec<Record>>,
    taken: RefCell<Vec<String>>,
    lines: RefCell<Vec<Vec<String>>>,
}

impl Approvals for Appr {
    fn create<'a>(
        &'a self,
        subject: &'a str,
        payload: &'a Value,
        lines: &'a [String],
    ) -> Fut<'a, Created> {
        Box::pin(async move {
            let id = format!("ap{}", self.recs.borrow().len() + 1);
            self.lines.borrow_mut().push(lines.to_vec());
            self.recs.borrow_mut().push(Record {
                id: id.clone(),
                kind: KIND.into(),
                subject: subject.into(),
                state: "pending".into(),
                payload: payload.clone(),
                expires_ms: NOW + 1_800_000,
            });
            Ok(Created {
                id,
                expires_ms: NOW + 1_800_000,
                existing: false,
            })
        })
    }

    fn get<'a>(&'a self, id: &'a str) -> Fut<'a, Record> {
        Box::pin(async move {
            let found = self.recs.borrow().iter().find(|r| r.id == id).cloned();
            found.ok_or_else(|| worker::Error::RustError("no such approval".into()))
        })
    }

    fn take<'a>(&'a self, id: &'a str, subject: &'a str, payload: &'a Value) -> Fut<'a, ()> {
        Box::pin(async move {
            let mut recs = self.recs.borrow_mut();
            let rec = recs
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| worker::Error::RustError("no such approval".into()))?;
            if rec.state != "approved" || rec.subject != subject || &rec.payload != payload {
                return Err(worker::Error::RustError(
                    "the approval was not taken: it does not match".into(),
                ));
            }
            rec.state = "used".into();
            self.taken.borrow_mut().push(id.into());
            Ok(())
        })
    }
}

/// Everything a scenario needs: the stubs and the caller.
struct Fx {
    net: Net,
    slots: StubSlots,
    store: Store,
    appr: Appr,
    caller: Value,
}

impl Fx {
    /// A gateway whose four slots are bound and set, against `handler`.
    fn new(handler: impl Fn(&HttpRequest) -> HttpResponse + 'static) -> Fx {
        let slots = [
            (scaleway::ACCESS_KEY, OWN),
            (scaleway::SECRET_KEY, SECRET),
            (scaleway::ORGANIZATION_ID, ORG),
            (scaleway::PROJECT_ID, PROJ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), SlotState::Set(v.to_string())))
        .collect();
        Fx {
            net: Net {
                log: RefCell::default(),
                handler: Box::new(handler),
            },
            slots: StubSlots(RefCell::new(slots)),
            store: Store::default(),
            appr: Appr::default(),
            caller: json!({ "user": { "id": "u1", "name": "ada", "kind": "person" }, "acting_for": null }),
        }
    }

    fn ports(&self) -> Ports<'_> {
        Ports {
            transport: &self.net,
            slots: &self.slots,
            approvals: &self.appr,
            sink: &self.store,
            now_ms: NOW,
            caller: &self.caller,
        }
    }

    /// Runs a tool: its answer, or its error's text.
    fn run(&self, tool: &str, args: Value) -> Result<Value, String> {
        block(tools::run_tool(&self.ports(), tool, &args)).map_err(|e| e.to_string())
    }

    /// The person approves approval `id`.
    fn approve(&self, id: &str) {
        for r in self.recs_mut().iter_mut().filter(|r| r.id == id) {
            r.state = "approved".into();
        }
    }

    fn recs_mut(&self) -> std::cell::RefMut<'_, Vec<Record>> {
        self.appr.recs.borrow_mut()
    }

    /// The requests so far.
    fn log(&self) -> Vec<HttpRequest> {
        self.net.log.borrow().clone()
    }

    /// The requests so far whose method is `method`.
    fn sent(&self, method: &str) -> Vec<HttpRequest> {
        self.log()
            .into_iter()
            .filter(|r| r.method == method)
            .collect()
    }
}

/// A handler that answers 404 to everything.
fn nothing(_: &HttpRequest) -> HttpResponse {
    json_res(404, json!({ "message": "not found" }))
}

const BUCKETS_XML: &str = "<?xml version=\"1.0\"?><ListAllMyBucketsResult><Buckets>\
    <Bucket><Name>photos</Name><CreationDate>2020-01-02T03:04:05.000Z</CreationDate></Bucket>\
    </Buckets></ListAllMyBucketsResult>";

#[test]
fn cloud_gateway_latent_until_scaleway_is_set_up() {
    let fx = Fx::new(nothing);
    // Nothing is bound and the sink holds nothing: the person sets all four.
    fx.slots.0.borrow_mut().clear();
    let all = "Scaleway is not set up: set SCW_ACCESS_KEY, SCW_SECRET_KEY, \
               SCW_DEFAULT_ORGANIZATION_ID, SCW_DEFAULT_PROJECT_ID on the Access tab";
    let calls: [(&str, Value); 9] = [
        ("cloud_tokens_list", json!({})),
        ("cloud_tokens_plan_create", json!({ "name": "a" })),
        ("cloud_tokens_plan_delete", json!({ "access_key": NEW_KEY })),
        ("cloud_buckets_list", json!({})),
        (
            "cloud_objects_list",
            json!({ "region": "fr-par", "bucket": "photos" }),
        ),
        (
            "cloud_object_head",
            json!({ "region": "fr-par", "bucket": "photos", "key": "a" }),
        ),
        (
            "cloud_buckets_plan_create",
            json!({ "region": "fr-par", "bucket": "photos" }),
        ),
        (
            "cloud_buckets_plan_delete",
            json!({ "region": "fr-par", "bucket": "photos" }),
        ),
        ("cloud_apply", json!({ "plan": "ap1" })),
    ];
    for (tool, args) in &calls {
        assert_eq!(fx.run(tool, args.clone()).unwrap_err(), all, "{tool}");
    }
    assert!(
        fx.log().is_empty(),
        "no call goes out while Scaleway is not set up"
    );

    // The status never errors: it says what is missing, never a value.
    let status = fx.run("cloud_status", json!({})).unwrap();
    let p = &status["providers"][0];
    assert_eq!(p["provider"], "scaleway");
    assert_eq!(p["ready"], false);
    assert_eq!(p["why"], all);
    assert_eq!(p["slots"].as_array().unwrap().len(), 4);
    assert_eq!(
        p["slots"][0],
        json!({ "name": "SCW_ACCESS_KEY", "bound": false, "set": false })
    );
    assert_eq!(p["hosts"], json!(transport::HOSTS));
    assert!(p["check"].is_null());

    // Set in the Secrets Store but not bound yet.
    fx.store
        .names
        .borrow_mut()
        .extend(scaleway::SLOTS.map(String::from));
    let unbound = "Scaleway is not set up: SCW_ACCESS_KEY, SCW_SECRET_KEY, \
                   SCW_DEFAULT_ORGANIZATION_ID, SCW_DEFAULT_PROJECT_ID are set but not bound \
                   yet: ask an agent for docs/plans/cloud-gateway.md phase 6";
    assert_eq!(
        fx.run("cloud_buckets_list", json!({})).unwrap_err(),
        unbound
    );
    let status = fx.run("cloud_status", json!({})).unwrap();
    assert_eq!(
        status["providers"][0]["slots"][1],
        json!({ "name": "SCW_SECRET_KEY", "bound": false, "set": true })
    );

    // Bound, and one still empty: only that one is named.
    for (k, v) in [
        (scaleway::ACCESS_KEY, OWN),
        (scaleway::ORGANIZATION_ID, ORG),
        (scaleway::PROJECT_ID, PROJ),
    ] {
        fx.slots
            .0
            .borrow_mut()
            .insert(k.into(), SlotState::Set(v.into()));
    }
    fx.slots
        .0
        .borrow_mut()
        .insert(scaleway::SECRET_KEY.into(), SlotState::Unset);
    assert_eq!(
        fx.run("cloud_buckets_list", json!({})).unwrap_err(),
        "Scaleway is not set up: set SCW_SECRET_KEY on the Access tab"
    );
    // A sink that fails counts a slot it cannot see as not set.
    fx.slots.0.borrow_mut().clear();
    fx.store.fail_exists.set(true);
    assert_eq!(fx.run("cloud_object_head", json!({})).unwrap_err(), all);
    // Debug never shows a slot's value.
    assert!(!format!("{:?}", SlotState::Set("hush".into())).contains("hush"));

    // The manifest: one capability, ten tools with schemas, four workers-scoped slots.
    use pigeon_contract::Service;
    let m = pigeon_cloud::CloudGateway::new().manifest();
    assert_eq!(m.worker, "pigeon-cloud-gateway");
    let cap = &m.capabilities[0];
    assert_eq!(cap.name, "cloud");
    assert_eq!(cap.tools.len(), 10);
    assert!(
        cap.tools
            .iter()
            .all(|t| !t.description.is_empty() && t.input_schema["type"] == "object")
    );
    assert_eq!(
        cap.secrets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        scaleway::SLOTS
    );
    assert!(
        cap.secrets
            .iter()
            .all(|s| s.scopes == ["workers"] && !s.purpose.is_empty())
    );
    assert_eq!(cap.footprint["externals"][0]["name"], "Scaleway");
    assert_eq!(
        cap.footprint["externals"][0]["detail"],
        "api.scaleway.com, s3.fr-par.scw.cloud, s3.nl-ams.scw.cloud, s3.pl-waw.scw.cloud"
    );
    assert!(!cap.versions.is_empty());
    let names: Vec<&str> = cap.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, tools::TOOLS);

    // Checking a secret looks at its form and never repeats it.
    assert!(scaleway::check_secret("SCW_ACCESS_KEY", OWN).is_ok());
    assert!(scaleway::check_secret("SCW_SECRET_KEY", SECRET).is_ok());
    assert!(scaleway::check_secret("SCW_DEFAULT_PROJECT_ID", PROJ).is_ok());
    for (name, value) in [
        ("SCW_ACCESS_KEY", "SCWshort"),
        ("SCW_ACCESS_KEY", SECRET),
        ("SCW_SECRET_KEY", "hunter2hunter2"),
        ("SCW_DEFAULT_ORGANIZATION_ID", "not-a-uuid"),
    ] {
        let e = scaleway::check_secret(name, value).unwrap_err().to_string();
        assert!(!e.contains(value), "{e}");
    }
}

#[test]
fn cloud_gateway_sigv4_matches_the_published_examples() {
    use pigeon_core::s3::{self, Endpoint, Keys};
    let keys = Keys {
        access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
    };
    let aws = Endpoint {
        base: "https://examplebucket.s3.amazonaws.com".into(),
        region: "us-east-1".into(),
    };
    let date = "20130524T000000Z";
    let sig = |headers: &[(String, String)]| -> (String, String) {
        let auth = &headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1;
        let (head, sig) = auth.rsplit_once("Signature=").unwrap();
        (head.to_string(), sig.to_string())
    };
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());

    // GET Object.
    let h = s3::sign(
        "GET",
        &aws,
        "/test.txt",
        &[],
        &[pair("range", "bytes=0-9")],
        s3::EMPTY_SHA256,
        &keys,
        date,
    );
    let (head, got) = sig(&h);
    assert_eq!(
        got,
        "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
    assert!(head.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));

    // PUT Object, with a body hash and a storage class.
    let body_hash = "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072";
    let h = s3::sign(
        "PUT",
        &aws,
        &format!("/{}", s3::uri_encode("test$file.text", false)),
        &[],
        &[
            pair("date", "Fri, 24 May 2013 00:00:00 GMT"),
            pair("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ],
        body_hash,
        &keys,
        date,
    );
    let (head, got) = sig(&h);
    assert_eq!(
        got,
        "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
    );
    assert!(
        head.contains(
            "SignedHeaders=date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class"
        )
    );

    // GET Bucket Lifecycle.
    let h = s3::sign(
        "GET",
        &aws,
        "/",
        &[pair("lifecycle", "")],
        &[],
        s3::EMPTY_SHA256,
        &keys,
        date,
    );
    assert_eq!(
        sig(&h).1,
        "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
    );

    // GET Bucket (List Objects).
    let q = [pair("max-keys", "2"), pair("prefix", "J")];
    let h = s3::sign("GET", &aws, "/", &q, &[], s3::EMPTY_SHA256, &keys, date);
    assert_eq!(
        sig(&h).1,
        "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
    );

    // The cloud crate's own request: the same signer, a Scaleway region, path-style, no host
    // header (fetch sets it), and the query as signed.
    let req = s3api::request("GET", "fr-par", "/photos", &q, &[], &keys, NOW).unwrap();
    assert_eq!(
        req.url,
        "https://s3.fr-par.scw.cloud/photos?max-keys=2&prefix=J"
    );
    assert!(req.header("host").is_none());
    assert_eq!(req.header("x-amz-date"), Some("20130524T000000Z"));
    let auth = req.header("authorization").unwrap();
    assert!(auth.contains("Credential=AKIAIOSFODNN7EXAMPLE/20130524/fr-par/s3/aws4_request"));
    assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
    // The signature is the one core makes for that host, region and path.
    let scw = Endpoint {
        base: "https://s3.fr-par.scw.cloud".into(),
        region: "fr-par".into(),
    };
    let h = s3::sign(
        "GET",
        &scw,
        "/photos",
        &q,
        &[],
        s3::EMPTY_SHA256,
        &keys,
        date,
    );
    assert!(auth.ends_with(&sig(&h).1));
    // A region is never part of a host unless it is one of the three.
    for bad in ["", "evil.com", "fr-par.evil.com/x", "FR-PAR", "us-east-1"] {
        assert!(
            s3api::request("GET", bad, "/", &[], &[], &keys, NOW).is_err(),
            "{bad}"
        );
    }
    // PUT and DELETE sign an empty body and carry the ACL.
    let acl = [pair("x-amz-acl", "private")];
    let req = s3api::request("PUT", "nl-ams", "/bkt", &[], &acl, &keys, NOW).unwrap();
    assert_eq!(req.header("x-amz-content-sha256"), Some(s3::EMPTY_SHA256));
    assert_eq!(req.header("x-amz-acl"), Some("private"));
    assert!(
        req.header("authorization")
            .unwrap()
            .contains("x-amz-acl;x-amz-content-sha256")
    );
}
