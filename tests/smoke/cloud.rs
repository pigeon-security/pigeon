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
        assert!(
            transport::allowed_request(&req.method, &req.url),
            "a call to {}",
            req.url
        );
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
    fn names<'a>(&'a self) -> Fut<'a, Vec<String>> {
        Box::pin(async move {
            if self.fail_exists.get() {
                return Err(worker::Error::RustError(
                    "no secrets_store credential".into(),
                ));
            }
            Ok(self.names.borrow().clone())
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
               SCW_DEFAULT_ORGANIZATION_ID, SCW_DEFAULT_PROJECT_ID on the app's Access page (Credentials)";
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
        "Scaleway is not set up: set SCW_SECRET_KEY on the app's Access page (Credentials)"
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

#[test]
fn cloud_gateway_reads_keys_buckets_and_objects() {
    // A key list: the secret Scaleway should never send is dropped if it does.
    let fx = Fx::new(|req| {
        let (host, path, q) = split(&req.url);
        match (host.as_str(), path.as_str()) {
            ("api.scaleway.com", "/iam/v1alpha1/api-keys") => json_res(
                200,
                json!({ "total_count": 1, "api_keys": [{
                    "access_key": NEW_KEY, "secret_key": "LEAKLEAKLEAK", "application_id": APP,
                    "description": "backups", "created_at": "2013-05-01T00:00:00Z",
                    "expires_at": "2014-05-01T00:00:00Z", "default_project_id": PROJ,
                    "editable": true, "deletable": true, "managed": false,
                    "creation_ip": "203.0.113.9",
                }] }),
            ),
            (_, "/iam/v1alpha1/applications") => json_res(
                200,
                json!({ "applications": [{ "id": APP, "name": "pigeon-backups", "tags": ["pigeon"] }] }),
            ),
            (_, "/iam/v1alpha1/policies") => json_res(
                200,
                json!({ "policies": [{ "id": POLICY, "name": "pigeon-backups", "application_id": APP }] }),
            ),
            ("s3.fr-par.scw.cloud", "/") => text_res(200, BUCKETS_XML),
            ("s3.nl-ams.scw.cloud", "/") => text_res(
                200,
                "<ListAllMyBucketsResult><Buckets><Bucket><Name>logs</Name>\
                 <CreationDate>2021-01-02T03:04:05.000Z</CreationDate></Bucket></Buckets>\
                 </ListAllMyBucketsResult>",
            ),
            ("s3.pl-waw.scw.cloud", "/") => text_res(
                403,
                "<Error><Code>AccessDenied</Code><Message>no</Message></Error>",
            ),
            ("s3.fr-par.scw.cloud", "/photos") if q.contains("continuation-token=tok1") => {
                text_res(
                    200,
                    "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>b.txt</Key>\
                 <Size>3</Size><ETag>\"e2\"</ETag><LastModified>2020-01-01T00:00:00.000Z</LastModified>\
                 </Contents></ListBucketResult>",
                )
            }
            ("s3.fr-par.scw.cloud", "/photos") => text_res(
                200,
                "<ListBucketResult><IsTruncated>true</IsTruncated>\
                 <NextContinuationToken>tok1</NextContinuationToken><Contents><Key>a.txt</Key>\
                 <Size>12</Size><ETag>\"e1\"</ETag><LastModified>2020-01-01T00:00:00.000Z</LastModified>\
                 </Contents><CommonPrefixes><Prefix>dir/</Prefix></CommonPrefixes></ListBucketResult>",
            ),
            ("s3.fr-par.scw.cloud", "/photos/dir/a%20b.txt") => HttpResponse {
                status: 200,
                headers: vec![
                    ("Content-Length".into(), "12".into()),
                    ("ETag".into(), "\"e1\"".into()),
                    (
                        "Last-Modified".into(),
                        "Tue, 01 Jan 2020 00:00:00 GMT".into(),
                    ),
                    ("Content-Type".into(), "text/plain".into()),
                ],
                body: String::new(),
            },
            _ => nothing(req),
        }
    });

    let keys = fx.run("cloud_tokens_list", json!({})).unwrap();
    let text = keys.to_string();
    assert!(
        !text.contains("LEAKLEAKLEAK") && !text.contains("secret_key"),
        "{text}"
    );
    assert_eq!(keys["total_count"], 1);
    assert_eq!(keys["api_keys"][0]["access_key"], NEW_KEY);
    assert_eq!(keys["api_keys"][0]["application"]["name"], "pigeon-backups");
    assert_eq!(keys["api_keys"][0]["policies"][0]["id"], POLICY);
    let first = &fx.log()[0];
    assert_eq!(first.header("X-Auth-Token"), Some(SECRET));
    assert!(first.url.contains(&format!("organization_id={ORG}")));
    assert_eq!(first.method, "GET");
    // The paging arguments are checked.
    assert!(
        fx.run("cloud_tokens_list", json!({ "page_size": 101 }))
            .is_err()
    );

    // Buckets across the three regions: one fails and is reported beside the others.
    let all = fx.run("cloud_buckets_list", json!({})).unwrap();
    let names: Vec<String> = all["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            format!(
                "{}/{}",
                b["region"].as_str().unwrap(),
                b["name"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(names, ["fr-par/photos", "nl-ams/logs"]);
    assert_eq!(all["errors"][0]["region"], "pl-waw");
    assert!(
        all["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("AccessDenied")
    );
    // One region named: one request, and its failure is the error.
    let before = fx.log().len();
    let one = fx
        .run("cloud_buckets_list", json!({ "region": "nl-ams" }))
        .unwrap();
    assert_eq!(fx.log().len(), before + 1);
    assert_eq!(one["buckets"].as_array().unwrap().len(), 1);
    let denied = fx.run("cloud_buckets_list", json!({ "region": "pl-waw" }));
    assert!(denied.unwrap_err().contains("AccessDenied"));
    // A region that is not one of the three never reaches the network.
    let before = fx.log().len();
    assert!(
        fx.run("cloud_buckets_list", json!({ "region": "evil.com/x" }))
            .is_err()
    );
    assert_eq!(fx.log().len(), before);

    // Objects, with a paging token.
    let args = json!({ "region": "fr-par", "bucket": "photos", "prefix": "a", "delimiter": "/", "max_keys": 1 });
    let page = fx.run("cloud_objects_list", args).unwrap();
    assert_eq!(page["objects"][0]["key"], "a.txt");
    assert_eq!(page["prefixes"][0], "dir/");
    assert_eq!(page["next_token"], "tok1");
    let url = fx.log().last().unwrap().url.clone();
    for part in ["list-type=2", "max-keys=1", "prefix=a", "delimiter=%2F"] {
        assert!(url.contains(part), "{url}");
    }
    let args = json!({ "region": "fr-par", "bucket": "photos", "token": "tok1" });
    let next = fx.run("cloud_objects_list", args).unwrap();
    assert_eq!(next["objects"][0]["key"], "b.txt");
    assert!(next["next_token"].is_null());
    let args = json!({ "region": "fr-par", "bucket": "photos", "max_keys": 1001 });
    assert!(fx.run("cloud_objects_list", args).is_err());
    let args = json!({ "region": "fr-par", "bucket": "Bad_Bucket" });
    assert!(fx.run("cloud_objects_list", args).is_err());

    // Head.
    let args = json!({ "region": "fr-par", "bucket": "photos", "key": "dir/a b.txt" });
    let head = fx.run("cloud_object_head", args).unwrap();
    assert_eq!(head["size"], 12);
    assert_eq!(head["etag"], "e1");
    assert_eq!(head["content_type"], "text/plain");
    assert_eq!(fx.log().last().unwrap().method, "HEAD");
    let args = json!({ "region": "fr-par", "bucket": "photos", "key": "nope" });
    let missing = fx.run("cloud_object_head", args).unwrap_err();
    assert!(missing.contains("404"), "{missing}");

    // Every request went to an allowed host, and the host allowlist holds.
    assert!(fx.log().iter().all(|r| transport::allowed(&r.url)));
    for url in [
        "https://api.scaleway.com/iam/v1alpha1/api-keys",
        "https://s3.fr-par.scw.cloud/",
        "https://s3.nl-ams.scw.cloud/b?x=1",
        "https://s3.pl-waw.scw.cloud",
    ] {
        assert!(transport::allowed(url), "{url}");
    }
    for url in [
        "http://api.scaleway.com/x",
        "https://example.com/",
        "https://api.scaleway.com.evil.com/",
        "https://s3.fr-par.scw.cloud.evil.com/",
        "https://evil.com/s3.fr-par.scw.cloud",
        "https://user@api.scaleway.com/",
        "https://api.scaleway.com@evil.com/",
        "https://evil.com@api.scaleway.com/",
        "https://api.scaleway.com:8443/",
        "https://api.scaleway.com\\@evil.com/",
        "https://API.scaleway.com/",
        "https://photos.s3.fr-par.scw.cloud/",
        "https://s3.us-east-1.scw.cloud/",
        "ftp://api.scaleway.com/",
        "//api.scaleway.com/",
        "api.scaleway.com",
        "",
    ] {
        assert!(!transport::allowed(url), "{url}");
    }
    // The Worker's transport refuses before it fetches (so this needs no runtime).
    let refused = block(transport::FetchTransport.send(HttpRequest {
        method: "GET".into(),
        url: "https://user:pw@evil.com/x?k=v".into(),
        headers: vec![("X-Auth-Token".into(), SECRET.into())],
        body: String::new(),
    }));
    assert_eq!(
        refused.unwrap_err().to_string(),
        "the cloud gateway does not call evil.com"
    );
    // A request's Debug shows no header value.
    let req = iam::request("GET", "/api-keys", &[], None, &iam::SecretKey::new(SECRET));
    assert!(!format!("{req:?}").contains(SECRET));
}

const NO_VERSIONS: &str = "<ListVersionsResult><Name>newbucket</Name><MaxKeys>1</MaxKeys>\
    <IsTruncated>false</IsTruncated></ListVersionsResult>";
const ONE_VERSION: &str = "<ListVersionsResult><Name>newbucket</Name><MaxKeys>1</MaxKeys>\
    <IsTruncated>true</IsTruncated><Version><Key>a</Key></Version></ListVersionsResult>";

/// A Scaleway holding at most one bucket, `newbucket`, whose state the scenario moves.
fn bucket_world(
    exists: Rc<Cell<bool>>,
    full: Rc<Cell<bool>>,
) -> impl Fn(&HttpRequest) -> HttpResponse {
    move |req| {
        let (_, path, q) = split(&req.url);
        match (req.method.as_str(), path.as_str()) {
            ("GET", "/") if exists.get() => text_res(
                200,
                "<ListAllMyBucketsResult><Buckets><Bucket><Name>newbucket</Name>\
                 <CreationDate>2013-05-24T00:00:00.000Z</CreationDate></Bucket></Buckets>\
                 </ListAllMyBucketsResult>",
            ),
            ("GET", "/") => text_res(
                200,
                "<ListAllMyBucketsResult><Buckets></Buckets></ListAllMyBucketsResult>",
            ),
            ("GET", "/newbucket") if q.contains("versions") => {
                text_res(200, if full.get() { ONE_VERSION } else { NO_VERSIONS })
            }
            ("PUT", "/newbucket") => {
                exists.set(true);
                text_res(200, "")
            }
            ("DELETE", "/newbucket") => {
                exists.set(false);
                text_res(204, "")
            }
            _ => nothing(req),
        }
    }
}

#[test]
fn cloud_gateway_buckets_plan_then_apply() {
    let (exists, full) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let fx = Fx::new(bucket_world(exists.clone(), full.clone()));
    let args = json!({ "region": "fr-par", "bucket": "newbucket" });

    // Planning changes nothing and makes one approval, whose payload is the plan.
    let planned = fx.run("cloud_buckets_plan_create", args.clone()).unwrap();
    assert!(fx.sent("PUT").is_empty() && !exists.get());
    assert_eq!(planned["approval"], "ap1");
    assert!(planned["plan"][0].as_str().unwrap().contains("newbucket"));
    let rec = fx.appr.recs.borrow()[0].clone();
    assert_eq!(rec.kind, "cloud_change");
    assert_eq!(rec.subject, "cloud:scaleway:buckets:create");
    assert_eq!(
        rec.payload,
        json!({
            "provider": "scaleway", "kind": "buckets", "action": "create",
            "args": { "region": "fr-par", "bucket": "newbucket" },
            "before": { "exists": false },
        })
    );
    assert!(!fx.appr.lines.borrow()[0].is_empty());

    // Not approved yet: nothing happens.
    let waiting = fx.run("cloud_apply", json!({ "plan": "ap1" })).unwrap_err();
    assert!(waiting.contains("waiting for the person"), "{waiting}");
    assert!(fx.sent("PUT").is_empty() && fx.appr.taken.borrow().is_empty());
    assert!(fx.run("cloud_apply", json!({ "plan": "nope" })).is_err());
    assert!(fx.run("cloud_apply", json!({})).is_err());

    // Approved: the bucket is made private, the approval spent, and nothing differs.
    fx.approve("ap1");
    let done = fx.run("cloud_apply", json!({ "plan": "ap1" })).unwrap();
    assert_eq!(done["created"]["bucket"], "newbucket");
    assert_eq!(done["differences"], json!([]));
    assert!(exists.get());
    let puts = fx.sent("PUT");
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].url, "https://s3.fr-par.scw.cloud/newbucket");
    assert_eq!(puts[0].header("x-amz-acl"), Some("private"));
    assert_eq!(
        puts[0].header("x-amz-content-sha256"),
        Some(pigeon_core::s3::EMPTY_SHA256)
    );
    assert!(
        puts[0]
            .header("authorization")
            .unwrap()
            .contains("SignedHeaders=host;x-amz-acl;")
    );
    assert_eq!(*fx.appr.taken.borrow(), ["ap1"]);
    // An approval is used once.
    let again = fx.run("cloud_apply", json!({ "plan": "ap1" })).unwrap_err();
    assert!(again.contains("used"), "{again}");
    assert_eq!(fx.sent("PUT").len(), 1);
    // The bucket exists now: a second plan to create it is refused.
    let e = fx
        .run("cloud_buckets_plan_create", args.clone())
        .unwrap_err();
    assert!(e.contains("already exists"), "{e}");

    // Drift: planned while absent, made by someone else before the apply. Nothing is changed
    // and the approval is not spent.
    exists.set(false);
    let planned = fx.run("cloud_buckets_plan_create", args.clone()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx.approve(&id);
    exists.set(true);
    let puts = fx.sent("PUT").len();
    let e = fx.run("cloud_apply", json!({ "plan": id })).unwrap_err();
    assert!(
        e.contains("no longer holds") && e.contains("plan again"),
        "{e}"
    );
    assert_eq!(fx.sent("PUT").len(), puts);
    assert_eq!(fx.appr.taken.borrow().len(), 1);

    // Delete: refused when the bucket holds anything, even an old version.
    full.set(true);
    let e = fx
        .run("cloud_buckets_plan_delete", args.clone())
        .unwrap_err();
    assert!(e.contains("not empty"), "{e}");
    let versions = fx
        .log()
        .into_iter()
        .rev()
        .find(|r| r.url.contains("versions"))
        .unwrap();
    assert!(
        versions.url.contains("max-keys=1") && versions.url.contains("versions="),
        "{}",
        versions.url
    );
    assert!(fx.sent("DELETE").is_empty());
    // Empty at plan time, then something lands before the apply: refused again.
    full.set(false);
    let planned = fx.run("cloud_buckets_plan_delete", args.clone()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx.approve(&id);
    full.set(true);
    let e = fx.run("cloud_apply", json!({ "plan": id })).unwrap_err();
    assert!(e.contains("not empty") && e.contains("plan again"), "{e}");
    assert!(fx.sent("DELETE").is_empty() && exists.get());
    // Empty at both: deleted, and the apply reads the bucket gone.
    full.set(false);
    let planned = fx.run("cloud_buckets_plan_delete", args.clone()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx.approve(&id);
    let done = fx.run("cloud_apply", json!({ "plan": id })).unwrap();
    assert_eq!(done["differences"], json!([]));
    assert_eq!(fx.sent("DELETE").len(), 1);
    assert!(!exists.get());
    let e = fx.run("cloud_buckets_plan_delete", args).unwrap_err();
    assert!(e.contains("does not exist"), "{e}");

    // Names and regions are checked before anything is read.
    let before = fx.log().len();
    for bucket in [
        "ab",
        "Abc",
        "a_b",
        "-abc",
        "abc-",
        "a.b.c",
        "xn--abc",
        &"a".repeat(64),
        "a b",
    ] {
        let args = json!({ "region": "fr-par", "bucket": bucket });
        assert!(
            fx.run("cloud_buckets_plan_create", args).is_err(),
            "{bucket}"
        );
    }
    let args = json!({ "region": "us-east-1", "bucket": "newbucket" });
    assert!(fx.run("cloud_buckets_plan_create", args).is_err());
    assert_eq!(fx.log().len(), before);
    // A public bucket is not an option: an `acl` argument changes nothing.
    exists.set(false);
    let args = json!({ "region": "fr-par", "bucket": "newbucket", "acl": "public-read" });
    let planned = fx.run("cloud_buckets_plan_create", args).unwrap();
    assert!(planned["args"].get("acl").is_none());
}

/// The state of the mocked IAM, which a scenario moves.
struct World {
    key_gone: Cell<bool>,
    app_gone: Cell<bool>,
    app_tag: RefCell<String>,
    other_key: Cell<bool>,
    description: RefCell<String>,
}

fn world() -> Rc<World> {
    Rc::new(World {
        key_gone: Cell::new(false),
        app_gone: Cell::new(false),
        app_tag: RefCell::new("pigeon".into()),
        other_key: Cell::new(false),
        description: RefCell::new("pigeon cloud gateway: backups".into()),
    })
}

/// A Scaleway IAM with one application, one policy and one key, `NEW_KEY`.
fn token_world(w: Rc<World>) -> impl Fn(&HttpRequest) -> HttpResponse {
    move |req| {
        let (_, path, _) = split(&req.url);
        let path = path
            .strip_prefix("/iam/v1alpha1")
            .unwrap_or(&path)
            .to_string();
        let key = |secret: bool| {
            let mut k = json!({
                "access_key": NEW_KEY, "application_id": APP, "default_project_id": PROJ2,
                "description": *w.description.borrow(), "expires_at": "2013-06-23T00:00:00Z",
                "editable": true, "deletable": true, "managed": false,
            });
            if secret {
                k["secret_key"] = json!(NEW_SECRET);
            }
            k
        };
        match (req.method.as_str(), path.as_str()) {
            ("GET", "/applications") => json_res(200, json!({ "applications": [] })),
            ("POST", "/applications") => {
                json_res(200, json!({ "id": APP, "name": "pigeon-backups" }))
            }
            ("GET", p) if p == format!("/applications/{APP}") && !w.app_gone.get() => json_res(
                200,
                json!({ "id": APP, "name": "pigeon-backups", "tags": [w.app_tag.borrow().clone()] }),
            ),
            ("DELETE", p) if p == format!("/applications/{APP}") => {
                w.app_gone.set(true);
                text_res(204, "")
            }
            ("POST", "/policies") => json_res(200, json!({ "id": POLICY })),
            ("GET", "/policies") => json_res(
                200,
                json!({ "policies": [{ "id": POLICY, "name": "pigeon-backups", "application_id": APP }] }),
            ),
            ("DELETE", p) if p == format!("/policies/{POLICY}") => text_res(204, ""),
            ("POST", "/api-keys") => json_res(200, key(true)),
            ("GET", "/api-keys") => {
                let mut keys = vec![key(false)];
                if w.other_key.get() {
                    keys.push(
                        json!({ "access_key": "SCWOTHERKEY0000000000", "application_id": APP }),
                    );
                }
                json_res(200, json!({ "api_keys": keys, "total_count": keys.len() }))
            }
            ("GET", p) if p == format!("/api-keys/{NEW_KEY}") && !w.key_gone.get() => {
                json_res(200, key(false))
            }
            ("DELETE", p) if p == format!("/api-keys/{NEW_KEY}") => {
                w.key_gone.set(true);
                text_res(204, "")
            }
            _ => nothing(req),
        }
    }
}

fn token_args() -> Value {
    json!({
        "name": "backups",
        "permission_sets": ["ObjectStorageReadOnly", "ObjectStorageFullAccess"],
        "project_ids": [PROJ2],
        "slot": "CLOUD_SCW_BACKUPS",
    })
}

#[test]
fn cloud_gateway_creates_a_key_and_never_shows_its_secret() {
    let w = world();
    let fx = Fx::new(token_world(w.clone()));

    // What a plan refuses, before any call goes out.
    let with = |k: &str, v: Value| {
        let mut a = token_args();
        a[k] = v;
        a
    };
    let refused = [
        with("permission_sets", json!(["IAMManager"])),
        with(
            "permission_sets",
            json!(["ObjectStorageFullAccess", "OrganizationManager"]),
        ),
        with("permission_sets", json!(["IAMFullAccess"])),
        with("permission_sets", json!([])),
        with("permission_sets", json!("ObjectStorageFullAccess")),
        with("organization_id", json!(ORG)),
        with("project_ids", json!(["not-a-uuid"])),
        with("expires_days", json!(0)),
        with("expires_days", json!(366)),
        with("expires_days", json!("30")),
        with("name", json!("Bad Name")),
        with("name", json!("")),
        with("slot", json!("SCW_ACCESS_KEY")),
        with("slot", json!("CLOUD_")),
        with("slot", json!("cloud_x")),
        with("slot", json!("CLOUD_A-B")),
        with("slot", json!("CLOUD_SCW_SECRET_KEY ")),
        with("slot", json!(format!("CLOUD_{}", "A".repeat(57)))),
    ];
    for args in refused {
        assert!(
            fx.run("cloud_tokens_plan_create", args.clone()).is_err(),
            "{args}"
        );
    }
    assert!(fx.log().is_empty(), "a refused plan reads nothing");
    // The longest slot is fine, and an existing slot is not.
    let mut ok = token_args();
    ok["slot"] = json!(format!("CLOUD_{}", "A".repeat(56)));
    assert!(fx.run("cloud_tokens_plan_create", ok).is_ok());
    fx.store.names.borrow_mut().push("CLOUD_TAKEN".into());
    let e = fx
        .run(
            "cloud_tokens_plan_create",
            with("slot", json!("CLOUD_TAKEN")),
        )
        .unwrap_err();
    assert!(e.contains("already exists"), "{e}");
    // A sink that cannot be written to stops the plan.
    fx.store.fail_exists.set(true);
    assert!(
        fx.run("cloud_tokens_plan_create", token_args())
            .unwrap_err()
            .contains("cannot be checked")
    );
    fx.store.fail_exists.set(false);

    // The plan: normalised arguments, a default of 30 days, the approval's payload.
    let planned = fx.run("cloud_tokens_plan_create", token_args()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    let rec = fx
        .appr
        .recs
        .borrow()
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .unwrap();
    assert_eq!(rec.subject, "cloud:scaleway:tokens:create");
    assert_eq!(
        rec.payload["args"]["permission_sets"],
        json!(["ObjectStorageFullAccess", "ObjectStorageReadOnly"])
    );
    assert_eq!(rec.payload["args"]["expires_days"], 30);
    assert_eq!(rec.payload["args"]["project_ids"], json!([PROJ2]));
    assert_eq!(
        rec.payload["before"],
        json!({ "application_exists": false, "slot_exists": false })
    );
    assert!(fx.sent("POST").is_empty(), "a plan creates nothing");

    // Apply: application, policy, key, then the slot.
    fx.approve(&id);
    let done = fx.run("cloud_apply", json!({ "plan": id })).unwrap();
    let text = done.to_string();
    assert!(
        !text.contains(NEW_SECRET) && !text.contains("secret_key"),
        "{text}"
    );
    assert_eq!(done["created"]["slot"], "CLOUD_SCW_BACKUPS");
    assert_eq!(done["created"]["access_key"], NEW_KEY);
    assert_eq!(done["created"]["application"]["name"], "pigeon-backups");
    assert_eq!(done["created"]["expires_at"], "2013-06-23T00:00:00Z");
    assert_eq!(done["differences"], json!([]));
    let posts: Vec<String> = fx.sent("POST").iter().map(|r| split(&r.url).1).collect();
    assert_eq!(
        posts,
        [
            "/iam/v1alpha1/applications",
            "/iam/v1alpha1/policies",
            "/iam/v1alpha1/api-keys"
        ]
    );
    let bodies: Vec<Value> = fx
        .sent("POST")
        .iter()
        .map(|r| serde_json::from_str(&r.body).unwrap())
        .collect();
    assert_eq!(bodies[0]["name"], "pigeon-backups");
    assert_eq!(bodies[0]["tags"], json!(["pigeon"]));
    assert_eq!(bodies[0]["organization_id"], ORG);
    // One rule, naming the projects: never an organization-wide one.
    assert_eq!(
        bodies[1]["rules"],
        json!([{ "permission_set_names": ["ObjectStorageFullAccess", "ObjectStorageReadOnly"], "project_ids": [PROJ2] }])
    );
    assert_eq!(bodies[1]["application_id"], APP);
    assert_eq!(bodies[2]["application_id"], APP);
    assert_eq!(bodies[2]["default_project_id"], PROJ2);
    assert_eq!(bodies[2]["expires_at"], "2013-06-23T00:00:00Z");
    // The secret is in the sink as ACCESS:SECRET, under the named slot, and nowhere else.
    let written = fx.store.written.borrow().clone();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].0, "CLOUD_SCW_BACKUPS");
    assert_eq!(written[0].1, format!("{NEW_KEY}:{NEW_SECRET}"));
    assert!(!written[0].2.contains(NEW_SECRET));
    let keys = pigeon_core::s3::parse_keys(&written[0].1).unwrap();
    assert_eq!(keys.access_key_id, NEW_KEY);
    assert_eq!(keys.secret_access_key, NEW_SECRET);
    let approvals = format!("{:?}{:?}", fx.appr.recs.borrow(), fx.appr.lines.borrow());
    assert!(!approvals.contains(NEW_SECRET));
    assert!(!format!("{:?}", fx.log()).contains(NEW_SECRET));
    // Debug of the new key hides the secret.
    let made =
        iam::parse_new_key(&json!({ "access_key": NEW_KEY, "secret_key": NEW_SECRET })).unwrap();
    assert!(!format!("{made:?}").contains(NEW_SECRET));
    assert!(!format!("{:?}", made.secret).contains(NEW_SECRET));
    // A secret key Scaleway sends on a read is never copied.
    let read = iam::parse_key(&json!({ "access_key": NEW_KEY, "secret_key": NEW_SECRET })).unwrap();
    assert!(!format!("{read:?}{}", read.to_json()).contains(NEW_SECRET));
    let mut v = json!({ "a": [{ "secret_key": "x", "keep": 1 }], "secret_key": "y" });
    iam::strip_secrets(&mut v);
    assert_eq!(v, json!({ "a": [{ "keep": 1 }] }));

    // The slot cannot be written: the new key is deleted at once, and the error hides the
    // secret even though the sink's error held it.
    let w2 = world();
    let fx2 = Fx::new(token_world(w2.clone()));
    let planned = fx2.run("cloud_tokens_plan_create", token_args()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx2.approve(&id);
    fx2.store.fail_write.set(true);
    let e = fx2.run("cloud_apply", json!({ "plan": id })).unwrap_err();
    assert!(!e.contains(NEW_SECRET) && !e.contains(NEW_KEY), "{e}");
    assert!(e.contains("was deleted"), "{e}");
    assert!(w2.key_gone.get(), "the new key was deleted");
    assert!(
        fx2.sent("DELETE")
            .iter()
            .any(|r| r.url.ends_with(&format!("/api-keys/{NEW_KEY}")))
    );
    assert!(fx2.store.written.borrow().is_empty());
    assert!(!format!("{:?}", fx2.log()).contains(NEW_SECRET));

    // A sink that cannot be written to when the plan is applied: nothing is created.
    let fx3 = Fx::new(token_world(world()));
    let planned = fx3.run("cloud_tokens_plan_create", token_args()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx3.approve(&id);
    fx3.store.fail_exists.set(true);
    let e = fx3.run("cloud_apply", json!({ "plan": id })).unwrap_err();
    assert!(e.contains("cannot"), "{e}");
    assert!(fx3.sent("POST").is_empty());
}

#[test]
fn cloud_gateway_deletes_keys_with_care() {
    let w = world();
    let fx = Fx::new(token_world(w.clone()));

    // The gateway's own key is never deleted, and no call is made to find that out.
    let e = fx
        .run("cloud_tokens_plan_delete", json!({ "access_key": OWN }))
        .unwrap_err();
    assert!(e.contains("own API key"), "{e}");
    assert!(fx.log().is_empty());
    // Something that is not an access key is refused as well.
    for bad in [
        "",
        "SCW",
        "scwnewkey00000000000",
        "SCWNEWKEY0000000000",
        "../api-keys",
    ] {
        assert!(
            fx.run("cloud_tokens_plan_delete", json!({ "access_key": bad }))
                .is_err(),
            "{bad}"
        );
    }
    assert!(fx.log().is_empty());
    // An apply of a plan naming the own key is refused too, whatever the approval says.
    let forged = json!({
        "provider": "scaleway", "kind": "tokens", "action": "delete",
        "args": { "access_key": OWN, "with_application": false }, "before": {},
    });
    fx.appr.recs.borrow_mut().push(Record {
        id: "forged".into(),
        kind: KIND.into(),
        subject: "cloud:scaleway:tokens:delete".into(),
        state: "approved".into(),
        payload: forged,
        expires_ms: NOW,
    });
    assert!(fx.run("cloud_apply", json!({ "plan": "forged" })).is_err());
    assert!(fx.sent("DELETE").is_empty() && fx.appr.taken.borrow().is_empty());

    // A key alone.
    let planned = fx
        .run("cloud_tokens_plan_delete", json!({ "access_key": NEW_KEY }))
        .unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx.approve(&id);
    let done = fx.run("cloud_apply", json!({ "plan": id })).unwrap();
    assert_eq!(done["differences"], json!([]));
    assert_eq!(fx.sent("DELETE").len(), 1);
    assert!(w.key_gone.get() && !w.app_gone.get());

    // With its application: only a `pigeon` one, and only when it holds no other key.
    let w = world();
    let fx = Fx::new(token_world(w.clone()));
    let args = json!({ "access_key": NEW_KEY, "with_application": true });
    *w.app_tag.borrow_mut() = "someone-else".into();
    let e = fx
        .run("cloud_tokens_plan_delete", args.clone())
        .unwrap_err();
    assert!(e.contains("not tagged `pigeon`"), "{e}");
    *w.app_tag.borrow_mut() = "pigeon".into();
    w.other_key.set(true);
    let e = fx
        .run("cloud_tokens_plan_delete", args.clone())
        .unwrap_err();
    assert!(e.contains("another API key"), "{e}");
    assert!(fx.sent("DELETE").is_empty());
    w.other_key.set(false);
    let planned = fx.run("cloud_tokens_plan_delete", args.clone()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    let rec = fx.appr.recs.borrow()[0].clone();
    assert_eq!(rec.payload["before"]["policies"], json!([POLICY]));
    assert_eq!(
        rec.payload["before"]["application"]["name"],
        "pigeon-backups"
    );

    // Drift: the key's description changes between plan and apply. The apply names what
    // moved and does not spend the approval.
    fx.approve(&id);
    *w.description.borrow_mut() = "someone edited this".into();
    let e = fx.run("cloud_apply", json!({ "plan": id })).unwrap_err();
    assert!(
        e.contains("before.description") && e.contains("plan again"),
        "{e}"
    );
    assert!(fx.sent("DELETE").is_empty() && fx.appr.taken.borrow().is_empty());
    // Planned again, it goes through: the key, its policy and its application.
    let planned = fx.run("cloud_tokens_plan_delete", args).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx.approve(&id);
    let done = fx.run("cloud_apply", json!({ "plan": id })).unwrap();
    assert_eq!(done["differences"], json!([]));
    let deletes: Vec<String> = fx.sent("DELETE").iter().map(|r| split(&r.url).1).collect();
    assert_eq!(
        deletes,
        [
            format!("/iam/v1alpha1/api-keys/{NEW_KEY}"),
            format!("/iam/v1alpha1/policies/{POLICY}"),
            format!("/iam/v1alpha1/applications/{APP}"),
        ]
    );
    assert!(w.key_gone.get() && w.app_gone.get());
}

#[test]
fn cloud_gateway_redacts_what_scaleway_says() {
    use redact::redact as hide;
    // Known values, and anything shaped like an access key.
    let text =
        format!("bad {SECRET} for {OWN}; also SCWABCDEFGHIJKLMNOPQ and SCW123456789012345678X");
    let out = hide(&text, &[SECRET, OWN, ""]);
    assert_eq!(
        out,
        "bad <redacted> for <redacted>; also <redacted> and <redacted>"
    );
    // A value inside another is hidden whole, and a short SCW word stays.
    assert_eq!(
        hide("abc-secret-xyz", &["secret", "abc-secret"]),
        "<redacted>-xyz"
    );
    assert_eq!(
        hide("SCW is Scaleway, SCW123 too", &[]),
        "SCW is Scaleway, SCW123 too"
    );
    assert_eq!(hide("SCWabcdefghijklmnopq", &[]), "SCWabcdefghijklmnopq");
    assert_eq!(hide("", &["x"]), "");

    // Scaleway's error carries the keys: the tool's error does not.
    let leak = format!(
        "{{\"message\":\"denied for {OWN} with {SECRET}, new {NEW_SECRET} and SCWZZZZZZZZZZZZZZZZZ\"}}"
    );
    let fx = Fx::new(move |_| text_res(403, &leak));
    let e = fx.run("cloud_tokens_list", json!({})).unwrap_err();
    for hidden in [SECRET, OWN, "SCWZZZZZZZZZZZZZZZZZ"] {
        assert!(!e.contains(hidden), "{e}");
    }
    assert!(
        e.contains("Scaleway 403") && e.contains("<redacted>"),
        "{e}"
    );
    // S3's too.
    let body = format!("<Error><Code>Denied</Code><Message>{OWN} {SECRET}</Message></Error>");
    let fx = Fx::new(move |_| text_res(403, &body));
    let e = fx
        .run("cloud_buckets_list", json!({ "region": "fr-par" }))
        .unwrap_err();
    assert!(
        !e.contains(OWN) && !e.contains(SECRET) && e.contains("Denied"),
        "{e}"
    );
    // A creation whose key answer is not usable leaves no secret in the error, and the objects
    // it made are removed again.
    let w = world();
    let inner = token_world(w.clone());
    let fx = Fx::new(move |req| {
        if req.method == "POST" && req.url.ends_with("/api-keys") {
            return text_res(500, &format!("{{\"message\":\"boom {NEW_SECRET}\"}}"));
        }
        inner(req)
    });
    let planned = fx.run("cloud_tokens_plan_create", token_args()).unwrap();
    let id = planned["approval"].as_str().unwrap().to_string();
    fx.approve(&id);
    let e = fx.run("cloud_apply", json!({ "plan": id })).unwrap_err();
    assert!(e.contains("Scaleway 500"), "{e}");
    assert!(w.app_gone.get(), "the application was removed again");
    assert!(fx.store.written.borrow().is_empty());
    // A transport failure is redacted as well.
    struct Down;
    impl Transport for Down {
        fn send<'a>(&'a self, req: HttpRequest) -> Fut<'a, HttpResponse> {
            let why = format!(
                "connect failed for {}",
                req.header("X-Auth-Token").unwrap_or_default()
            );
            Box::pin(async move { Err(worker::Error::RustError(why)) })
        }
    }
    let fx = Fx::new(nothing);
    let ports = Ports {
        transport: &Down,
        ..fx.ports()
    };
    let e = block(tools::run_tool(&ports, "cloud_buckets_list", &json!({}))).unwrap();
    assert_eq!(e["buckets"], json!([]));
    assert!(!e["errors"].to_string().contains(SECRET), "{e}");
    let e = block(tools::run_tool(&ports, "cloud_tokens_list", &json!({})))
        .unwrap_err()
        .to_string();
    assert!(!e.contains(SECRET) && e.contains("connect failed"), "{e}");
}

#[test]
fn digitalocean_probe_confirms_team_without_exposing_provider_data() {
    use pigeon_cloud::compute::probe_digitalocean;

    const TEAM: &str = "sampleTeamIdOpaque7812";
    let net = Net {
        log: RefCell::new(Vec::new()),
        handler: Box::new(|request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.url, "https://api.digitalocean.com/v2/account");
            assert_eq!(request.header("Authorization"), Some("Bearer dummy-key"));
            assert!(request.body.is_empty());
            json_res(
                200,
                json!({"account": {
                    "uuid": ORG,
                    "team": {"uuid": TEAM},
                    "email": "private"
                }}),
            )
        }),
    };
    block(probe_digitalocean(&net, TEAM, "dummy-key")).unwrap();
    // A token's user ID or a case-folded team ID cannot authorize a team.
    for target in [ORG, &TEAM.to_ascii_lowercase()] {
        let err = block(probe_digitalocean(&net, target, "dummy-key"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("could not confirm the approved target"));
        assert!(!err.contains(TEAM) && !err.contains("private"));
    }
    assert!(block(probe_digitalocean(&net, "invalid team", "dummy-key")).is_err());
    assert!(block(probe_digitalocean(&net, TEAM, "")).is_err());
    assert!(block(probe_digitalocean(&net, TEAM, "bad\nkey")).is_err());
    assert_eq!(net.log.borrow().len(), 3);

    assert!(!transport::allowed(
        "https://api.digitalocean.com/v2/droplets"
    ));
    assert!(!transport::allowed(
        "https://api.digitalocean.com/v2/account?foo=1"
    ));
}

#[test]
fn digitalocean_probe_redacts_failures_and_requires_team_identity() {
    use pigeon_cloud::compute::probe_digitalocean;

    const SECRET: &str = "dummy-key";
    for (status, body, expected) in [
        (
            401,
            "provider echoed dummy-key and private data",
            "HTTP 401",
        ),
        (
            503,
            "provider echoed dummy-key and private data",
            "HTTP 503",
        ),
        (200, "invalid JSON with dummy-key", "invalid JSON"),
        (
            200,
            r#"{"account":{"uuid":"dummy-key"}}"#,
            "could not confirm the approved target",
        ),
    ] {
        let net = Net {
            log: RefCell::new(Vec::new()),
            handler: Box::new(move |_| text_res(status, body)),
        };
        let error = block(probe_digitalocean(&net, SECRET, SECRET))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected));
        assert!(!error.contains(SECRET));
        assert!(!error.contains("private data"));
    }

    struct Offline;
    impl Transport for Offline {
        fn send<'a>(&'a self, _req: HttpRequest) -> Fut<'a, HttpResponse> {
            Box::pin(async { Err(worker::Error::RustError("provider echoed dummy-key".into())) })
        }
    }
    let error = block(probe_digitalocean(&Offline, SECRET, SECRET))
        .unwrap_err()
        .to_string();
    assert!(error.contains("transport failed") && !error.contains(SECRET));
}

#[test]
fn digitalocean_spaces_verifies_team_key_grant_and_bounded_listing() {
    use pigeon_cloud::digitalocean_spaces::{Binding, probe};
    use pigeon_core::s3::Keys;

    let binding = Binding {
        region: "nyc3".into(),
        bucket: "fixture-bucket".into(),
        prefix: "inside/receipt ?".into(),
    };
    let keys = Keys {
        access_key_id: "dummy-access-id".into(),
        secret_access_key: "dummy-secret-key".into(),
    };
    let request = |permission, prefix, meta_status, team_id| {
        let permission = String::from(permission);
        let prefix = String::from(prefix);
        let team_id = String::from(team_id);
        Net {
            log: RefCell::new(Vec::new()),
            handler: Box::new(move |req| {
                assert_eq!(req.method, "GET");
                assert!(req.body.is_empty());
                match req.url.as_str() {
                    "https://api.digitalocean.com/v2/account" => {
                        assert_eq!(
                            req.header("Authorization"),
                            Some("Bearer dummy-account-token")
                        );
                        json_res(200, json!({"account":{"team":{"uuid":team_id}}}))
                    }
                    "https://api.digitalocean.com/v2/spaces/keys/dummy-access-id" => {
                        assert_eq!(
                            req.header("Authorization"),
                            Some("Bearer dummy-account-token")
                        );
                        assert!(!format!("{req:?}").contains("dummy-access-id"));
                        json_res(
                            meta_status,
                            json!({"key":{
                                "access_key":"dummy-access-id",
                                "grants":[{"bucket":"fixture-bucket","permission":permission}],
                                "secret_key":"dummy-secret-key"
                            }}),
                        )
                    }
                    "https://nyc3.digitaloceanspaces.com/fixture-bucket?list-type=2&max-keys=1&prefix=inside%2Freceipt%20%3F" =>
                    {
                        let auth = req.header("Authorization").unwrap();
                        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=dummy-access-id/"));
                        assert!(!auth.contains("dummy-account-token"));
                        assert!(!format!("{req:?}").contains("dummy-access-id"));
                        text_res(
                            200,
                            &format!(
                                "<ListBucketResult><Contents><Key>{prefix}item</Key><Size>5</Size></Contents></ListBucketResult>"
                            ),
                        )
                    }
                    _ => panic!("unexpected provider route"),
                }
            }),
        }
    };
    let verify = |net: &Net| {
        block(probe(
            net,
            "approved-team",
            "dummy-account-token",
            &binding,
            &keys,
            NOW,
        ))
    };
    let good = request("read", "inside/receipt ?", 200, "approved-team");
    verify(&good).unwrap();
    assert_eq!(good.log.borrow().len(), 3);
    for (permission, prefix, status, team_id, calls) in [
        ("readwrite", "inside/receipt ?", 200, "approved-team", 2),
        ("fullaccess", "inside/receipt ?", 200, "approved-team", 2),
        ("unknown", "inside/receipt ?", 200, "approved-team", 2),
        ("read", "inside/receipt ?", 403, "approved-team", 2),
        ("read", "inside/receipt ?", 200, "another-team", 1),
        ("read", "outside/", 200, "approved-team", 3),
    ] {
        let net = request(permission, prefix, status, team_id);
        let error = verify(&net).unwrap_err().to_string();
        assert_eq!(net.log.borrow().len(), calls);
        for value in [
            "dummy-access-id",
            "dummy-secret-key",
            "dummy-account-token",
            "outside/",
            "another-team",
        ] {
            assert!(!error.contains(value));
        }
    }

    for prohibited in [
        "https://api.digitalocean.com/v2/spaces/keys",
        "https://api.digitalocean.com/v2/spaces/keys/anything?evil=1",
        "https://nyc3.digitaloceanspaces.com/",
        "https://nyc3.digitaloceanspaces.com/a-bucket?list-type=2&max-keys=2",
        "https://nyc3.digitaloceanspaces.com/a-bucket/object?list-type=2&max-keys=1",
        "https://nyc3.digitaloceanspaces.com/a-bucket?list-type=2&max-keys=1&continuation-token=1",
        "https://elsewhere.digitaloceanspaces.com/a-bucket?list-type=2&max-keys=1",
    ] {
        assert!(!transport::allowed(prohibited));
    }
}

#[test]
fn digitalocean_ingest_recovers_resources_and_waits_for_volume_detach() {
    use pigeon_cloud::digitalocean_ingest::{
        Created, Removal, Target, create_droplet, create_volume, find_droplet, find_volume,
        remove_droplet, remove_volume,
    };

    let run = "0123456789abcdef0123456789abcdef";
    let target = Target::new(run, "tor1").unwrap();
    let id = "01234567-89ab-cdef-0123-456789abcdef";
    let name = target.name().to_owned();
    let volume_reads = Cell::new(0);
    let droplet_reads = Cell::new(0);
    let net = Net {
        log: RefCell::new(Vec::new()),
        handler: Box::new(move |req| {
            let (_, path, query) = split(&req.url);
            assert_eq!(req.header("Authorization"), Some("Bearer test-key"));
            let description = format!("Pigeon ingest scratch for {name}");
            let volume = json!({
                "id": id, "name": name, "description": description, "region": {"slug":"tor1"}
            });
            let droplet = json!({
                "id": 42, "name": name, "region": {"slug":"tor1"}, "tags": ["pigeon-ingest"]
            });
            match (req.method.as_str(), path.as_str()) {
                ("POST", "/v2/volumes") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    assert_eq!(body["name"], name);
                    assert_eq!(body["size_gigabytes"], 256);
                    assert_eq!(body["filesystem_type"], "ext4");
                    text_res(503, "")
                }
                ("GET", "/v2/volumes") => {
                    assert_eq!(query, format!("name={name}&region=tor1&per_page=2"));
                    json_res(200, json!({"volumes":[volume]}))
                }
                ("POST", "/v2/droplets") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    assert_eq!(body["volumes"], json!([id]));
                    assert_eq!(body["region"], "tor1");
                    assert_eq!(body["user_data"], "bootstrap");
                    text_res(503, "")
                }
                ("GET", "/v2/droplets") => json_res(200, json!({"droplets":[droplet]})),
                ("GET", "/v2/droplets/42") => {
                    let n = droplet_reads.replace(droplet_reads.get() + 1);
                    if n == 0 {
                        json_res(200, json!({"droplet":droplet}))
                    } else {
                        text_res(404, "")
                    }
                }
                ("DELETE", "/v2/droplets/42") => text_res(204, ""),
                ("GET", path) if path == format!("/v2/volumes/{id}") => {
                    let mut volume = volume;
                    let n = volume_reads.replace(volume_reads.get() + 1);
                    if n == 2 {
                        return text_res(404, "");
                    }
                    // The provider documents a nullable attachment list.
                    volume["droplet_ids"] = if n == 0 { json!([42]) } else { Value::Null };
                    json_res(200, json!({"volume":volume}))
                }
                ("DELETE", path) if path == format!("/v2/volumes/{id}") => text_res(204, ""),
                _ => panic!("unexpected request"),
            }
        }),
    };
    let token = "test-key";
    assert_eq!(
        block(create_volume(&net, token, &target, 256)),
        Created::Unknown
    );
    assert_eq!(
        block(find_volume(&net, token, &target)).unwrap().as_deref(),
        Some(id)
    );
    assert_eq!(
        block(create_droplet(&net, token, &target, id, "bootstrap")),
        Created::Unknown
    );
    assert_eq!(block(find_droplet(&net, token, &target)).unwrap(), Some(42));
    assert_eq!(
        block(remove_droplet(&net, token, &target, 42)).unwrap(),
        Removal::Requested
    );
    assert_eq!(
        block(remove_volume(&net, token, &target, id)).unwrap(),
        Removal::Attached
    );
    assert_eq!(
        block(remove_droplet(&net, token, &target, 42)).unwrap(),
        Removal::Absent
    );
    assert_eq!(
        block(remove_volume(&net, token, &target, id)).unwrap(),
        Removal::Requested
    );
    assert_eq!(
        block(remove_volume(&net, token, &target, id)).unwrap(),
        Removal::Absent
    );
    assert_eq!(
        net.log
            .borrow()
            .iter()
            .filter(|req| req.method == "POST")
            .count(),
        2
    );
    assert_eq!(
        net.log
            .borrow()
            .iter()
            .filter(|req| req.method == "DELETE")
            .count(),
        2
    );
    assert!(!transport::allowed_request(
        "POST",
        &format!("https://api.digitalocean.com/v2/volumes/{id}/actions")
    ));
    assert!(!transport::allowed_request(
        "GET",
        &format!(
            "https://api.digitalocean.com/v2/volumes?name={}&region=elsewhere&per_page=2",
            target.name()
        )
    ));
}

#[test]
fn digitalocean_droplet_reconciles_ambiguous_create_and_confirms_owned_delete() {
    use pigeon_cloud::digitalocean_droplet::{Created, Removal, cleanup, create, reconcile};
    let name = "pigeon-health-11111111111111111111111111111111";
    let checks = Cell::new(0);
    let net = Net {
        log: RefCell::new(Vec::new()),
        handler: Box::new(move |req| {
            let (_, path, query) = split(&req.url);
            assert_eq!(req.header("Authorization"), Some("Bearer fake-provision"));
            let row = json!({"id":123,"name":name,"tags":["pigeon-health-check"]});
            match (req.method.as_str(), path.as_str()) {
                ("POST", "/v2/droplets") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    assert_eq!(body["name"], name);
                    assert_eq!(body["region"], "tor1");
                    assert_eq!(body["size"], "s-2vcpu-4gb");
                    assert_eq!(body["image"], "ubuntu-24-04-x64");
                    assert_eq!(body["tags"], json!(["pigeon-health-check"]));
                    assert!(!req.body.contains("fake-provision"));
                    text_res(503, "ambiguous")
                }
                ("GET", "/v2/droplets") => {
                    assert_eq!(
                        query,
                        format!("tag_name=pigeon-health-check&per_page=2&name={name}")
                    );
                    json_res(200, json!({"droplets":[row]}))
                }
                ("GET", "/v2/droplets/123") => {
                    checks.set(checks.get() + 1);
                    if checks.get() == 1 {
                        json_res(200, json!({"droplet":row}))
                    } else {
                        text_res(404, "")
                    }
                }
                ("DELETE", "/v2/droplets/123") => text_res(204, ""),
                _ => panic!("unexpected request"),
            }
        }),
    };
    assert!(matches!(
        block(create(
            &net,
            "fake-provision",
            name,
            "fixed health bootstrap"
        )),
        Created::Unknown
    ));
    let recovered = block(reconcile(&net, "fake-provision", name))
        .unwrap()
        .unwrap();
    assert!(matches!(
        block(cleanup(&net, "fake-provision", name, recovered)).unwrap(),
        Removal::Requested
    ));
    assert!(matches!(
        block(cleanup(&net, "fake-provision", name, recovered)).unwrap(),
        Removal::Absent
    ));
    assert_eq!(
        net.log
            .borrow()
            .iter()
            .filter(|req| req.method == "POST")
            .count(),
        1
    );
    assert_eq!(
        net.log
            .borrow()
            .iter()
            .filter(|req| req.method == "DELETE")
            .count(),
        1
    );

    let other = Net {
        log: RefCell::new(Vec::new()),
        handler: Box::new(|_| {
            json_res(
                200,
                json!({"droplet":{"id":123,"name":"not-ours","tags":["pigeon-health-check"]}}),
            )
        }),
    };
    assert!(block(cleanup(&other, "fake-provision", name, 123)).is_err());
    assert_eq!(other.log.borrow().len(), 1);

    for (method, url) in [
        (
            "DELETE",
            "https://api.digitalocean.com/v2/droplets?tag_name=pigeon-health-check",
        ),
        ("GET", "https://api.digitalocean.com/v2/droplets"),
        ("PATCH", "https://api.digitalocean.com/v2/droplets/123"),
        ("POST", "https://api.digitalocean.com/v2/account"),
        ("DELETE", "https://api.digitalocean.com/v2/spaces/keys/fake"),
    ] {
        assert!(!transport::allowed_request(method, url));
    }
}

mod droplet_observations {
    use super::*;
    use pigeon_cloud::digitalocean_droplet::{
        DropletStatus, Observation, compute_estimate, observe,
    };
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    const NAME: &str = "pigeon-health-00000000000000000000000000000001";

    struct FakeRead(HttpResponse);

    impl Transport for FakeRead {
        fn send<'a>(&'a self, req: HttpRequest) -> Fut<'a, HttpResponse> {
            assert_eq!(req.method, "GET");
            assert_eq!(req.url, "https://api.digitalocean.com/v2/droplets/42");
            Box::pin(std::future::ready(Ok(self.0.clone())))
        }
    }

    fn observe_fake(status: u16, body: Value) -> worker::Result<Observation> {
        let transport = FakeRead(HttpResponse {
            status,
            headers: vec![],
            body: body.to_string(),
        });
        let mut future = std::pin::pin!(observe(&transport, "fake", NAME, 42));
        let Poll::Ready(result) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("fake transport must answer immediately");
        };
        result
    }

    #[test]
    fn observation_checks_ownership_before_returning_status() {
        let mut body = json!({"droplet": {"id":42, "name":NAME, "tags":["pigeon-health-check"], "status":"active"}});
        assert_eq!(
            observe_fake(200, body.clone()).unwrap(),
            Observation::Present(DropletStatus::Active)
        );
        body["droplet"]["tags"] = json!([]);
        assert!(observe_fake(200, body).is_err());
        assert_eq!(observe_fake(404, Value::Null).unwrap(), Observation::Absent);
        assert!(observe_fake(403, Value::Null).is_err());
    }

    #[test]
    fn compute_estimate_uses_elapsed_time_until_deletion_is_confirmed() {
        let estimate = compute_estimate(2_000, 1_802_000, None).unwrap();
        assert_eq!(estimate.duration_ms, 1_800_000);
        assert_eq!(estimate.estimated_usd_micros, 17_855);
        assert!(!estimate.deletion_confirmed);

        let estimate = compute_estimate(2_000, 7_202_000, Some(3_602_000)).unwrap();
        assert_eq!(estimate.duration_ms, 3_600_000);
        assert_eq!(estimate.estimated_usd_micros, 35_710);
        assert!(estimate.deletion_confirmed);
    }

    #[test]
    fn compute_estimate_handles_invalid_and_sub_cent_intervals() {
        assert_eq!(compute_estimate(2_000, 1_000, None), None);
        assert_eq!(compute_estimate(2_000, 3_000, Some(1_000)), None);
        assert_eq!(
            compute_estimate(2_000, 2_001, None)
                .unwrap()
                .estimated_usd_micros,
            1
        );
    }
}

mod droplet_job_history {
    use pigeon_capabilities::digitalocean::health::{Job, MAX_JOBS, Stage, update_history};
    use pigeon_core::oauth;
    use serde_json::json;

    /// Start from the persisted shape of a job created before diagnostics were added.
    fn legacy_job(id: &str) -> Job {
        serde_json::from_value(json!({
            "id":id, "instance":"example", "state":"waiting", "health":"pending",
            "started_ms":1, "deadline_ms":100, "next_ms":99, "lease_until_ms":0,
            "expected_version":"expected-version", "droplet":101,
            "callback_hash":oauth::hash("test-bearer"),
            "credential":{"team":"private-team","sealed":"private-ciphertext"}
        }))
        .unwrap()
    }

    #[test]
    fn reports_are_ordered_idempotent_and_redacted() {
        let mut job = legacy_job("one");
        let token = "test-bearer";
        assert!(!job.callback("wrong", "started", Some(Stage::ImageBuild), None, 2));
        assert!(job.callback(token, "started", None, None, 3));
        assert!(job.callback(token, "started", Some(Stage::ImageBuild), None, 4));
        assert!(job.callback(token, "started", Some(Stage::ImageBuild), None, 5));
        assert!(job.callback(token, "failed", Some(Stage::PackageInstall), None, 6));
        assert_eq!(
            job.public_with_events()["events"].as_array().unwrap().len(),
            2
        );
        assert!(!job.callback(token, "succeeded", Some(Stage::VersionCheck), None, 7));
        assert!(job.callback(
            token,
            "result",
            Some(Stage::VersionCheck),
            Some("expected-version"),
            8
        ));
        assert!(!job.callback(token, "started", Some(Stage::VersionCheck), None, 9));
        assert_eq!(job.public_with_events()["health"], "passed");
        assert_eq!(job.public_with_events()["provider_state"], "cleanup");
        assert_eq!(job.public_with_events()["stage"], "version_check");
        let public = job.public_with_events();
        assert_eq!(public["events"][2]["outcome"], "succeeded");
        for name in [
            "private-team",
            "private-ciphertext",
            "callback_hash",
            "expected_version",
        ] {
            assert!(!public.to_string().contains(name));
        }
    }

    #[test]
    fn reports_distinguish_failed_stage_and_cancelled_job() {
        let mut job = legacy_job("one");
        assert!(job.callback(
            "test-bearer",
            "failed",
            Some(Stage::PackageInstall),
            None,
            4
        ));
        assert_eq!(job.public_with_events()["health"], "failed");
        assert_eq!(job.public_with_events()["stage"], "package_install");
        let mut cancelled = legacy_job("two");
        assert!(cancelled.schedule_cancel("two", 5));
        assert!(!cancelled.callback("test-bearer", "result", None, Some("expected-version"), 6));
        assert_eq!(
            cancelled.public_with_events()["events"][0]["outcome"],
            "cancelled"
        );
    }

    #[test]
    fn terminal_history_exports_independently_of_new_jobs() {
        let mut record = serde_json::to_value(legacy_job("one")).unwrap();
        record["state"] = json!("gone");
        record["health"] = json!("passed");
        record["created_ms"] = json!(1);
        record["confirmed_delete_ms"] = json!(61_001);
        let first: Job = serde_json::from_value(record).unwrap();
        let mut history = update_history(Vec::new(), &first);
        assert_eq!(history[0]["audit_status"], "pending");
        assert_eq!(history[0]["compute_estimate"]["duration_ms"], 61_000);
        assert_eq!(history[0]["compute_estimate"]["deletion_confirmed"], true);

        history[0]["audit_status"] = json!("stored");
        history = update_history(history, &first);
        assert_eq!(history[0]["audit_status"], "stored");
        let history = update_history(history, &legacy_job("two"));
        assert!(history[0].get("audit_status").is_none());
        assert_eq!(history[1]["audit_status"], "stored");
    }

    #[test]
    fn history_keeps_the_latest_jobs_and_updates_in_place() {
        let mut history = Vec::new();
        for index in 0..=MAX_JOBS {
            let job = legacy_job(&index.to_string());
            history = update_history(history, &job);
        }
        assert_eq!(history.len(), MAX_JOBS);
        assert_eq!(history[0]["id"], MAX_JOBS.to_string());
        assert_eq!(history.last().unwrap()["id"], "1");

        let mut last = legacy_job(&MAX_JOBS.to_string());
        last.schedule_cancel(&MAX_JOBS.to_string(), 9);
        let history = update_history(history, &last);
        assert_eq!(history.len(), MAX_JOBS);
        assert_eq!(history[0]["events"][0]["outcome"], "cancelled");
        assert_eq!(history[1]["id"], (MAX_JOBS - 1).to_string());
    }
}
