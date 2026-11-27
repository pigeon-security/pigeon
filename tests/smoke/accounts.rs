//! Accounts: secrets, expiry, storage, and linked Cloudflare and GitHub accounts.

use super::proxy::webhook_and_oidc_key;
use super::*;

#[test]
fn secrets_write_and_check() {
    use pigeon_capabilities::Slot;
    use pigeon_capabilities::expiry::Expiry;
    use pigeon_capabilities::secrets::{cloudflare_expiry, listing};
    let slot = |name: &str| Slot {
        name: name.into(),
        purpose: format!("{name} purpose"),
        scopes: &[pigeon_core::secrets_store::WORKERS],
    };
    // What the secrets page lists: declared slots first, set or missing, then the others.
    let declared = vec![
        ("secrets".to_string(), slot("CF_PARENT_TOKEN")),
        ("mirror".to_string(), slot("GITHUB_MIRROR_TOKEN")),
    ];
    let stored = vec![
        ("ZETA".to_string(), None),
        (
            "CF_PARENT_TOKEN".to_string(),
            Some("2026-11-01".to_string()),
        ),
        ("ALPHA".to_string(), None),
    ];
    let listed = listing(&declared, &stored);
    let names: Vec<&str> = listed.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        ["CF_PARENT_TOKEN", "GITHUB_MIRROR_TOKEN", "ALPHA", "ZETA"]
    );
    assert!(listed[0].set && !listed[1].set);
    assert_eq!(listed[1].owner, "mirror");
    // The check before it is written: the issuer's answer (stubbed Cloudflare verify).
    assert_eq!(
        cloudflare_expiry(&json!({ "status": "active", "expires_on": "2026-12-21T00:00:00Z" })),
        Expiry::At {
            at_ms: 1_797_811_200_000
        }
    );
    assert_eq!(
        cloudflare_expiry(&json!({ "status": "active", "expires_on": null })),
        Expiry::Never
    );
    for status in [json!("expired"), json!("disabled"), Value::Null] {
        let v = json!({ "status": status, "expires_on": "2026-12-21T00:00:00Z" });
        assert!(
            matches!(cloudflare_expiry(&v), Expiry::Refused { .. }),
            "{v}"
        );
    }
}

#[test]
fn expiry_readings() {
    use pigeon_capabilities::expiry::{self, Expiry, Row, Status};
    const DAY: i64 = 86_400_000;
    let at = 1_797_811_200_000; // 2026-12-21T00:00:00Z
    // The issuers' time shapes; junk is refused rather than misread.
    for t in [
        "2026-12-21T00:00:00Z",
        "2026-12-21T00:00:00.000Z",
        "2026-12-21 00:00:00 UTC",
        "2026-12-21T02:00:00+02:00",
        "2026-12-20 17:00:00 -0700",
        "2026-12-21",
    ] {
        assert_eq!(expiry::parse_time(t), Some(at), "{t}");
    }
    for t in [
        "",
        "2026-13-01",
        "2026-12-21T25:00:00Z",
        "2026-12-21T00:00:00 PST",
        "tomorrow",
        "2026-12-21T00:00:00+2",
    ] {
        assert_eq!(expiry::parse_time(t), None, "{t}");
    }
    assert_eq!(expiry::from_field(None), Expiry::Never);
    assert!(matches!(
        expiry::from_field(Some("soon")),
        Expiry::Unknown { .. }
    ));
    assert_eq!(expiry::date(at), "2026-12-21");
    // Read against now: ok, soon (a week ahead), expired, refused, unknown.
    let now = at - 30 * DAY;
    assert_eq!(expiry::status(&Expiry::At { at_ms: at }, now), Status::Ok);
    assert_eq!(
        expiry::status(&Expiry::At { at_ms: at }, at - 7 * DAY),
        Status::Soon
    );
    assert_eq!(
        expiry::status(&Expiry::At { at_ms: at }, at),
        Status::Expired
    );
    assert_eq!(
        expiry::status(&Expiry::Refused { why: "401".into() }, now),
        Status::Expired
    );
    assert_eq!(expiry::days_left(at, at - DAY - 1), 1);
    let row = Row::new(
        "GITHUB_MIRROR_TOKEN",
        "secret",
        "mirror",
        Some(Expiry::At { at_ms: at }),
        Some(now),
        at - 3 * DAY,
    );
    assert_eq!(
        (row.status, row.expires.as_deref(), row.days_left),
        (Status::Soon, Some("2026-12-21"), Some(3))
    );
    assert!(row.needs_you());
    let missing = Row::new("X", "secret", "x", None, None, now);
    assert_eq!(missing.status, Status::Missing);
    // A row never carries a value.
    let shown = serde_json::to_value(&row).unwrap();
    assert!(shown.get("value").is_none() && !shown.to_string().contains("ghp_"));
    // GitHub's answer for the mirror's token (stubbed header).
    use pigeon_capabilities::mirror::github_expiry;
    assert_eq!(
        github_expiry(200, Some("2026-12-21 00:00:00 UTC")),
        Expiry::At { at_ms: at }
    );
    assert_eq!(github_expiry(200, None), Expiry::Never);
    assert!(matches!(github_expiry(401, None), Expiry::Refused { .. }));
    assert!(matches!(github_expiry(502, None), Expiry::Unknown { .. }));
}

#[test]
fn storage_reads_s3_compatible_accounts() {
    use pigeon_capabilities::data_read::{self, Account, Mode, Provider};
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

    // AWS's published header-auth example.
    let headers = s3::sign(
        "GET",
        &aws,
        "/test.txt",
        &[],
        &[("range".into(), "bytes=0-9".into())],
        s3::EMPTY_SHA256,
        &keys,
        date,
    );
    let auth = &headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .unwrap()
        .1;
    assert!(
        auth.ends_with(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ),
        "{auth}"
    );
    assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    // AWS's published query-string example, past the tool's cap, so the inner function.
    let url = s3::presign_url(&aws, "/test.txt", &keys, date, 86_400);
    assert!(
        url.ends_with(
            "X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        ),
        "{url}"
    );
    assert!(url.contains(
        "X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"
    ));
    assert!(s3::presign_get(&aws, "/test.txt", &keys, date, 86_400).is_err());
    assert!(s3::presign_get(&aws, "/test.txt", &keys, date, 0).is_err());
    assert!(s3::presign_get(&aws, "/test.txt", &keys, date, 3600).is_ok());

    // Encoding.
    assert_eq!(s3::uri_encode("a b/ü.txt", false), "a%20b/%C3%BC.txt");
    assert_eq!(s3::uri_encode("a b/ü~-._", true), "a%20b%2F%C3%BC~-._");
    assert_eq!(s3::object_path("bkt", "dir/a b"), "/bkt/dir/a%20b");

    // Keys.
    let k = s3::parse_keys(" AKID : sec/ret:x \n").unwrap();
    assert_eq!(k.access_key_id, "AKID");
    assert_eq!(k.secret_access_key, "sec/ret:x");
    assert!(!format!("{k:?}").contains("sec/ret"));
    for bad in ["", "nocolon", ":secret", "id:", "id:sec ret", "i d:secret"] {
        let e = s3::parse_keys(bad).unwrap_err().to_string();
        assert!(bad.len() < 4 || !e.contains(bad), "{e}");
    }
    assert!(
        !s3::parse_keys("id:sec ret")
            .unwrap_err()
            .to_string()
            .contains("sec ret")
    );

    // Recorded answers.
    let buckets = s3::parse_list_buckets(
        "<?xml version=\"1.0\"?><ListAllMyBucketsResult><Owner><ID>x</ID></Owner><Buckets>\
         <Bucket><Name>photos</Name><CreationDate>2020-01-02T03:04:05.000Z</CreationDate></Bucket>\
         <Bucket><Name>logs</Name><CreationDate>2021-01-02T03:04:05.000Z</CreationDate></Bucket>\
         </Buckets></ListAllMyBucketsResult>",
    )
    .unwrap();
    assert_eq!(buckets.len(), 2);
    assert_eq!(buckets[1].name, "logs");
    assert_eq!(buckets[0].created, "2020-01-02T03:04:05.000Z");
    assert!(s3::parse_list_buckets("<Error/>").is_err());

    let page = s3::parse_list_objects(
        "<ListBucketResult><Name>b</Name><Prefix>a/</Prefix><IsTruncated>true</IsTruncated>\
         <NextContinuationToken>tok/en==</NextContinuationToken>\
         <Contents><Key>a/one.txt</Key><LastModified>2020-01-01T00:00:00.000Z</LastModified>\
         <ETag>&quot;abc&quot;</ETag><Size>12</Size></Contents>\
         <CommonPrefixes><Prefix>a/sub/</Prefix></CommonPrefixes></ListBucketResult>",
    )
    .unwrap();
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].size, 12);
    assert_eq!(page.objects[0].etag, "abc");
    assert_eq!(page.prefixes, vec!["a/sub/".to_string()]);
    assert_eq!(page.next.as_deref(), Some("tok/en=="));
    let last = s3::parse_list_objects(
        "<ListBucketResult><IsTruncated>false</IsTruncated>\
         <Contents><Key>x &amp; y.txt</Key><Size>0</Size></Contents></ListBucketResult>",
    )
    .unwrap();
    assert_eq!(last.objects[0].key, "x & y.txt");
    assert_eq!(last.next, None);

    assert_eq!(
        s3::parse_error(
            403,
            "<Error><Code>AccessDenied</Code><Message>Access &lt;Denied&gt;</Message></Error>"
        ),
        "S3 403 AccessDenied: Access <Denied>"
    );
    assert_eq!(s3::parse_error(404, ""), "S3 404");
    assert!(
        s3::parse_error(
            500,
            &format!(
                "<Error><Code>X</Code><Message>{}</Message></Error>",
                "m".repeat(500)
            )
        )
        .len()
            <= 200
    );

    // Queries and dates.
    let q = s3::list_objects_query(Some("a/"), Some("/"), 5000, Some("t"));
    assert!(q.contains(&("max-keys".into(), "1000".into())));
    assert!(q.contains(&("list-type".into(), "2".into())));
    assert!(q.contains(&("continuation-token".into(), "t".into())));
    let q = s3::list_objects_query(None, None, 0, None);
    assert!(q.contains(&("max-keys".into(), "1".into())));
    assert_eq!(q.len(), 2);
    assert_eq!(s3::amz_date(1_369_353_600_000.0), "20130524T000000Z");
    assert_eq!(s3::amz_date(1_700_000_000_000.0), "20231114T221320Z");

    // The registry.
    let sw = Account::scaleway("personal", "fr-par");
    assert_eq!(sw.endpoint, "https://s3.fr-par.scw.cloud");
    assert_eq!(sw.id(), "scaleway:personal");
    assert_eq!(sw.binding(), "DATA_READ_SCALEWAY_PERSONAL");
    let docean = Account::digitalocean("old-one", "nyc3");
    assert_eq!(docean.endpoint, "https://nyc3.digitaloceanspaces.com");
    assert_eq!(docean.mode, Mode::ReadOnly);
    assert_eq!(docean.binding(), "DATA_READ_DIGITALOCEAN_OLD_ONE");
    let b2 = Account::backblaze("bk", "us-west-004");
    assert_eq!(b2.endpoint, "https://s3.us-west-004.backblazeb2.com");
    assert_eq!(b2.provider, Provider::Backblaze);
    assert!(data_read::validate(&[sw.clone(), docean.clone(), b2.clone()]).is_ok());
    assert!(data_read::validate(&[]).is_ok());

    let mut rw = docean.clone();
    rw.mode = Mode::ReadWrite;
    assert!(data_read::validate(&[rw]).is_err());
    for name in ["", "Bad", "-x", "x-", "a_b", &"a".repeat(33)] {
        assert!(
            data_read::validate(&[Account::scaleway(name, "fr-par")]).is_err(),
            "{name}"
        );
    }
    assert!(data_read::validate(&[sw.clone(), sw.clone()]).is_err());
    let mut slash = sw.clone();
    slash.endpoint.push('/');
    assert!(data_read::validate(&[slash]).is_err());
    let mut http = sw.clone();
    http.endpoint = "http://s3.fr-par.scw.cloud".into();
    assert!(data_read::validate(&[http]).is_err());
    let mut shortb = sw.clone();
    shortb.buckets = vec!["ab".into()];
    assert!(data_read::validate(&[shortb]).is_err());

    // Finding, and the setup text with no accounts.
    let e = data_read::find(&[], "scaleway:personal")
        .unwrap_err()
        .to_string();
    assert!(e.contains("DATA_READ_<PROVIDER>_<NAME>"));
    let e = data_read::find(std::slice::from_ref(&sw), "scaleway:other")
        .unwrap_err()
        .to_string();
    assert!(e.contains("scaleway:personal"));
    assert_eq!(
        data_read::find(std::slice::from_ref(&sw), "scaleway:personal")
            .unwrap()
            .name,
        "personal"
    );

    // Bucket limits.
    let mut limited = sw.clone();
    assert!(data_read::bucket_allowed(&limited, "anything"));
    limited.buckets = vec!["photos".into()];
    assert!(data_read::bucket_allowed(&limited, "photos"));
    assert!(!data_read::bucket_allowed(&limited, "logs"));
}

// ---- ingest runner ----

/// One request the fake S3 server saw.
#[derive(Clone, Debug)]
struct IngestRunnerRequest {
    method: String,
    /// The request target: path and query, as sent.
    target: String,
    /// Header names in lower case.
    headers: Vec<(String, String)>,
}

impl IngestRunnerRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// One stored object: its bytes and the headers that came with it.
#[derive(Clone, Default)]
struct IngestRunnerObject {
    bytes: Vec<u8>,
    /// `x-amz-meta-*`, `x-amz-checksum-sha256` and `content-type`, as sent.
    headers: Vec<(String, String)>,
}

/// An open multipart upload: its key, its headers and its parts by number.
type IngestRunnerUpload = (
    String,
    Vec<(String, String)>,
    std::collections::BTreeMap<u32, Vec<u8>>,
);

/// What the fake S3 server keeps and records.
#[derive(Default)]
struct IngestRunnerState {
    objects: std::collections::BTreeMap<String, IngestRunnerObject>,
    uploads: std::collections::BTreeMap<String, IngestRunnerUpload>,
    requests: Vec<IngestRunnerRequest>,
    /// Upload ids that were aborted.
    aborted: Vec<String>,
    /// Keys per list page.
    page_size: usize,
    next_upload: u32,
}

type IngestRunnerShared = std::sync::Arc<std::sync::Mutex<IngestRunnerState>>;

fn ingest_runner_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn ingest_runner_b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn ingest_runner_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Answers one request against the state: (status, headers, body).
fn ingest_runner_route(
    st: &mut IngestRunnerState,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    use sha2::{Digest, Sha256};
    let err = |code: u16, name: &str| {
        (
            code,
            vec![("content-type".to_string(), "application/xml".to_string())],
            format!("<Error><Code>{name}</Code><Message>fake {name}</Message></Error>")
                .into_bytes(),
        )
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let q: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (ingest_runner_unescape(k), ingest_runner_unescape(v))
        })
        .collect();
    let qv = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let rest = path.trim_start_matches('/');
    let (_bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    let key = ingest_runner_unescape(key);
    let kept: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| {
            k.starts_with("x-amz-meta-") || k == "x-amz-checksum-sha256" || k == "content-type"
        })
        .cloned()
        .collect();
    match method {
        "GET" if key.is_empty() => {
            let prefix = qv("prefix").unwrap_or_default();
            let after = qv("continuation-token")
                .or_else(|| qv("start-after"))
                .unwrap_or_default();
            let keys: Vec<&String> = st
                .objects
                .keys()
                .filter(|k| k.starts_with(&prefix) && k.as_str() > after.as_str())
                .collect();
            let page: Vec<&&String> = keys.iter().take(st.page_size).collect();
            let more = keys.len() > page.len();
            let mut xml = String::from("<ListBucketResult>");
            xml.push_str(&format!("<IsTruncated>{more}</IsTruncated>"));
            for k in &page {
                let size = st.objects[**k].bytes.len();
                let esc = k.replace('&', "&amp;");
                xml.push_str(&format!(
                    "<Contents><Key>{esc}</Key><Size>{size}</Size><ETag>\"e-{size}\"</ETag>\
                     <LastModified>2024-01-01T00:00:00.000Z</LastModified></Contents>"
                ));
            }
            if more && let Some(last) = page.last() {
                xml.push_str(&format!(
                    "<NextContinuationToken>{last}</NextContinuationToken>"
                ));
            }
            xml.push_str("</ListBucketResult>");
            (200, vec![], xml.into_bytes())
        }
        "GET" | "HEAD" => match st.objects.get(&key) {
            Some(o) => {
                let mut h = vec![("content-length".to_string(), o.bytes.len().to_string())];
                h.extend(
                    o.headers
                        .iter()
                        .filter(|(k, _)| k.starts_with("x-amz-meta-"))
                        .cloned(),
                );
                (
                    200,
                    h,
                    if method == "GET" {
                        o.bytes.clone()
                    } else {
                        vec![]
                    },
                )
            }
            None => err(404, "NoSuchKey"),
        },
        "POST" if qv("uploads").is_some() => {
            st.next_upload += 1;
            let id = format!("upload-{}", st.next_upload);
            st.uploads
                .insert(id.clone(), (key, kept, Default::default()));
            (
                200,
                vec![],
                format!(
                    "<InitiateMultipartUploadResult><UploadId>{id}</UploadId>\
                     </InitiateMultipartUploadResult>"
                )
                .into_bytes(),
            )
        }
        "POST" => {
            let Some(id) = qv("uploadId") else {
                return err(400, "InvalidRequest");
            };
            let Some((key, hdrs, parts)) = st.uploads.remove(&id) else {
                return err(404, "NoSuchUpload");
            };
            let xml = String::from_utf8_lossy(body).into_owned();
            let mut bytes = Vec::new();
            for chunk in xml.split("<PartNumber>").skip(1) {
                let n: u32 = chunk.split('<').next().unwrap_or("").parse().unwrap_or(0);
                if !chunk.contains(&format!("<ETag>\"etag-{n}\"</ETag>")) {
                    return err(400, "InvalidPart");
                }
                match parts.get(&n) {
                    Some(p) => bytes.extend_from_slice(p),
                    None => return err(400, "InvalidPart"),
                }
            }
            st.objects.insert(
                key,
                IngestRunnerObject {
                    bytes,
                    headers: hdrs,
                },
            );
            (200, vec![], b"<CompleteMultipartUploadResult/>".to_vec())
        }
        "PUT" if qv("partNumber").is_some() => {
            let (Some(id), Some(n)) = (qv("uploadId"), qv("partNumber")) else {
                return err(400, "InvalidRequest");
            };
            let n: u32 = n.parse().unwrap_or(0);
            match st.uploads.get_mut(&id) {
                Some(u) => {
                    u.2.insert(n, body.to_vec());
                    (
                        200,
                        vec![("etag".to_string(), format!("\"etag-{n}\""))],
                        vec![],
                    )
                }
                None => err(404, "NoSuchUpload"),
            }
        }
        "PUT" => {
            if let Some(want) = headers
                .iter()
                .find(|(k, _)| k == "x-amz-checksum-sha256")
                .map(|(_, v)| v)
                && *want != ingest_runner_b64(&Sha256::digest(body))
            {
                return err(400, "BadDigest");
            }
            st.objects.insert(
                key,
                IngestRunnerObject {
                    bytes: body.to_vec(),
                    headers: kept,
                },
            );
            (
                200,
                vec![("etag".to_string(), "\"put\"".to_string())],
                vec![],
            )
        }
        "DELETE" => match qv("uploadId") {
            Some(id) => {
                st.uploads.remove(&id);
                st.aborted.push(id);
                (204, vec![], vec![])
            }
            None => err(400, "InvalidRequest"),
        },
        _ => err(405, "MethodNotAllowed"),
    }
}

fn ingest_runner_serve(stream: std::net::TcpStream, state: IngestRunnerShared) {
    use std::io::{BufRead, BufReader, Read, Write};
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
            return;
        };
        let (method, target) = (method.to_string(), target.to_string());
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let len = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse::<usize>().ok());
        let chunked = headers.iter().any(|(k, _)| k == "transfer-encoding");
        let mut body = vec![0u8; len.unwrap_or(0)];
        let request = IngestRunnerRequest {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
        };
        if reader.read_exact(&mut body).is_err() {
            // The client gave up mid-body: nothing is stored.
            if let Ok(mut st) = state.lock() {
                st.requests.push(request);
            }
            return;
        }
        let (status, mut rh, rbody) = {
            let Ok(mut st) = state.lock() else {
                return;
            };
            st.requests.push(request);
            if chunked {
                (
                    411,
                    vec![],
                    b"<Error><Code>MissingContentLength</Code></Error>".to_vec(),
                )
            } else {
                ingest_runner_route(&mut st, &method, &target, &headers, &body)
            }
        };
        if !rh.iter().any(|(k, _)| k == "content-length") {
            rh.push(("content-length".into(), rbody.len().to_string()));
        }
        let mut head = format!("HTTP/1.1 {status} X\r\n");
        for (k, v) in &rh {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        if method != "HEAD" {
            bytes.extend_from_slice(&rbody);
        }
        if out.write_all(&bytes).is_err() {
            return;
        }
    }
}

/// Starts a fake S3 server on 127.0.0.1 and answers its base URL and its shared state. It keeps
/// objects with headers and records every request. It lists with ListObjectsV2 pagination
/// (`page_size` keys a page, 2 at first), and does GET, HEAD, PUT (checking
/// `x-amz-checksum-sha256`), and multipart create, part, complete and abort. Path-style:
/// `{base}/{bucket}/{key}`. It runs for the life of the test process.
fn ingest_runner_fake_s3() -> (String, IngestRunnerShared) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state: IngestRunnerShared = std::sync::Arc::new(std::sync::Mutex::new(IngestRunnerState {
        page_size: 2,
        ..Default::default()
    }));
    let shared = std::sync::Arc::clone(&state);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let st = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || ingest_runner_serve(stream, st));
        }
    });
    (base, state)
}

/// A reader that gives `data`, then fails instead of ending, like `engine::Verify` does when
/// the bytes do not match.
struct IngestRunnerFailingBody {
    data: std::io::Cursor<Vec<u8>>,
}

impl std::io::Read for IngestRunnerFailingBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.data.read(buf)?;
        if n == 0 {
            return Err(std::io::Error::other("the bytes do not match"));
        }
        Ok(n)
    }
}

fn ingest_runner_metrics(bytes: &[u8]) -> pigeon_ingest_runner::pigeon_ingest::metrics::Metrics {
    use pigeon_ingest_runner::pigeon_ingest::metrics::{Kind, Metrics};
    use sha2::{Digest, Sha256};
    Metrics {
        size: bytes.len() as u64,
        sha256: ingest_runner_hex(&Sha256::digest(bytes)),
        blake3: "b3".repeat(32),
        mime: "text/plain".into(),
        kind: Kind::Text,
        simhash: None,
    }
}

#[test]
fn ingest_runner_s3_source_r2_dest_and_spool() {
    use pigeon_core::s3::parse_keys;
    use pigeon_ingest_runner::pigeon_ingest::engine::{Dest, Source, Spool};
    use pigeon_ingest_runner::{DirSpool, LocalSource, R2Dest, S3Source};
    use sha2::{Digest, Sha256};
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};

    const SECRET: &str = "SECRETSECRETSECRET";
    let keys = || parse_keys(&format!("AKIDTEST:{SECRET}")).unwrap();
    let (base, state) = ingest_runner_fake_s3();
    let mut errors: Vec<String> = Vec::new();

    // The S3 source: paged listing with start-after, then continuation tokens.
    {
        let mut st = state.lock().unwrap();
        for k in ["a/1", "a/2", "b", "c/d e", "e&f"] {
            st.objects.insert(
                k.to_string(),
                IngestRunnerObject {
                    bytes: format!("bytes of {k}").into_bytes(),
                    headers: vec![],
                },
            );
        }
    }
    let src = S3Source::new(&base, "us-east-1", "src", "", keys());
    let p1 = src.list(Some("a/1"), None).unwrap();
    let k1: Vec<&str> = p1.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(k1, ["a/2", "b"]);
    assert_eq!(p1.objects[0].size, "bytes of a/2".len() as u64);
    let p2 = src.list(Some("a/1"), p1.next.as_deref()).unwrap();
    let k2: Vec<&str> = p2.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(k2, ["c/d e", "e&f"]);
    assert!(p2.next.is_none());
    {
        let st = state.lock().unwrap();
        let first = &st.requests[0];
        assert!(
            first.target.contains("start-after=a%2F1"),
            "{}",
            first.target
        );
        assert!(first.target.contains("max-keys=1000"));
        assert!(first.target.contains("list-type=2"));
        assert!(st.requests[1].target.contains("continuation-token=b"));
    }
    let mut got = Vec::new();
    src.open("c/d e").unwrap().read_to_end(&mut got).unwrap();
    assert_eq!(got, b"bytes of c/d e");
    let mut got = Vec::new();
    src.open("e&f").unwrap().read_to_end(&mut got).unwrap();
    assert_eq!(got, b"bytes of e&f");
    match src.open("missing") {
        Ok(_) => panic!("a missing object opened"),
        Err(e) => {
            assert!(e.to_string().contains("NoSuchKey"), "{e}");
            errors.push(e.to_string());
        }
    }

    // The local source: sorted bytewise, nested, symlinks not followed.
    let dir =
        std::env::temp_dir().join(format!("pigeon-smoke-ingest-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("a/y")).unwrap();
    std::fs::write(dir.join("b.txt"), b"bee").unwrap();
    std::fs::write(dir.join("A.txt"), b"capital").unwrap();
    std::fs::write(dir.join("a/z"), b"zed").unwrap();
    std::fs::write(dir.join("a/y/n.txt"), b"nested").unwrap();
    std::os::unix::fs::symlink(dir.join("b.txt"), dir.join("a/link")).unwrap();
    std::os::unix::fs::symlink(dir.join("a"), dir.join("dirlink")).unwrap();
    let local = LocalSource::new(dir.clone());
    let page = local.list(None, None).unwrap();
    let keys_local: Vec<&str> = page.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys_local, ["A.txt", "a/y/n.txt", "a/z", "b.txt"]);
    assert!(page.next.is_none());
    assert_eq!(page.objects[1].size, 6);
    let after = local.list(Some("a/y/n.txt"), None).unwrap();
    assert_eq!(after.objects.len(), 2);
    let mut got = String::new();
    local
        .open("a/y/n.txt")
        .unwrap()
        .read_to_string(&mut got)
        .unwrap();
    assert_eq!(got, "nested");
    assert!(local.open("a/link").is_err());
    assert!(local.open("dirlink/z").is_err());
    assert!(local.open("../outside").is_err());
    let _ = std::fs::remove_dir_all(&dir);

    // The R2 destination: head, a single PUT, multipart, abort.
    let dest = R2Dest::new(&base, "dst", keys()).with_multipart(10, 8);
    assert_eq!(dest.head("blobs/x").unwrap(), None);
    let small = b"hello blob";
    let m = ingest_runner_metrics(small);
    dest.put_blob("blobs/x", &mut Cursor::new(small.to_vec()), 10, &m)
        .unwrap();
    let head = dest.head("blobs/x").unwrap().unwrap();
    assert_eq!(head.size, 10);
    assert_eq!(head.sha256.as_deref(), Some(m.sha256.as_str()));
    assert_eq!(head.blake3.as_deref(), Some(m.blake3.as_str()));
    {
        let st = state.lock().unwrap();
        let o = &st.objects["blobs/x"];
        let h = |n: &str| {
            o.headers
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            h("x-amz-checksum-sha256"),
            Some(ingest_runner_b64(&Sha256::digest(small)))
        );
        assert_eq!(h("x-amz-meta-size").as_deref(), Some("10"));
        assert_eq!(h("x-amz-meta-mime").as_deref(), Some("text/plain"));
        let put = st
            .requests
            .iter()
            .find(|r| r.method == "PUT" && r.target.ends_with("/blobs/x"))
            .unwrap();
        assert_eq!(put.header("content-length"), Some("10"));
        assert_eq!(put.header("x-amz-content-sha256"), Some("UNSIGNED-PAYLOAD"));
    }
    // Bytes that do not match the digest are refused by the server: nothing stored.
    let wrong = ingest_runner_metrics(b"other bytes");
    let e = dest
        .put_blob(
            "blobs/bad",
            &mut Cursor::new(b"0123456789".to_vec()),
            10,
            &wrong,
        )
        .unwrap_err();
    errors.push(e.to_string());
    assert!(!state.lock().unwrap().objects.contains_key("blobs/bad"));
    // A body that fails at its end is an error. A single PUT may already be complete on the
    // server then; the checksum header is what keeps wrong bytes out (checked above).
    let e = dest
        .put_blob(
            "blobs/bad2",
            &mut IngestRunnerFailingBody {
                data: Cursor::new(small.to_vec()),
            },
            10,
            &m,
        )
        .unwrap_err();
    errors.push(e.to_string());

    // Multipart: 27 bytes in parts of 8.
    let big: Vec<u8> = (0..27u8).collect();
    let mb = ingest_runner_metrics(&big);
    dest.put_blob("blobs/big", &mut Cursor::new(big.clone()), 27, &mb)
        .unwrap();
    {
        let st = state.lock().unwrap();
        assert_eq!(st.objects["blobs/big"].bytes, big);
        assert!(st.uploads.is_empty());
        let parts = st
            .requests
            .iter()
            .filter(|r| r.method == "PUT" && r.target.contains("partNumber="))
            .count();
        assert_eq!(parts, 4);
        assert!(
            st.objects["blobs/big"]
                .headers
                .iter()
                .any(|(k, v)| k == "x-amz-meta-sha256" && *v == mb.sha256)
        );
    }
    assert_eq!(dest.head("blobs/big").unwrap().unwrap().size, 27);
    // Multipart aborts when the body fails at its end.
    let e = dest
        .put_blob(
            "blobs/torn",
            &mut IngestRunnerFailingBody {
                data: Cursor::new(big.clone()),
            },
            27,
            &mb,
        )
        .unwrap_err();
    errors.push(e.to_string());
    // And when it fails in the middle.
    let e = dest
        .put_blob(
            "blobs/torn2",
            &mut IngestRunnerFailingBody {
                data: Cursor::new(big[..12].to_vec()),
            },
            27,
            &mb,
        )
        .unwrap_err();
    errors.push(e.to_string());
    {
        let st = state.lock().unwrap();
        assert!(!st.objects.contains_key("blobs/torn"));
        assert!(!st.objects.contains_key("blobs/torn2"));
        assert_eq!(st.aborted.len(), 2);
        assert!(st.uploads.is_empty());
    }
    // Small objects.
    assert_eq!(dest.get_object("ingest/runs/r/status.json").unwrap(), None);
    dest.put_object(
        "ingest/runs/r/status.json",
        b"{\"state\":\"done\"}",
        "application/json",
    )
    .unwrap();
    assert_eq!(
        dest.get_object("ingest/runs/r/status.json")
            .unwrap()
            .as_deref(),
        Some(&b"{\"state\":\"done\"}"[..])
    );

    // Every request was signed, and no secret went anywhere.
    {
        let st = state.lock().unwrap();
        assert!(st.requests.len() > 20);
        for r in &st.requests {
            let auth = r.header("authorization").unwrap_or("");
            assert!(
                auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDTEST/"),
                "{r:?}"
            );
            assert!(!r.target.contains(SECRET));
            assert!(r.headers.iter().all(|(_, v)| !v.contains(SECRET)));
        }
    }
    assert!(errors.len() >= 5);
    assert!(errors.iter().all(|e| !e.contains(SECRET)));

    // The spool: accounting and removal; names are a counter.
    let sdir =
        std::env::temp_dir().join(format!("pigeon-smoke-ingest-spool-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sdir);
    let spool = DirSpool::new(sdir.clone(), 100).unwrap();
    assert_eq!(spool.free_bytes(), 100);
    let mut f1 = spool.create().unwrap();
    f1.write_all(&[7u8; 30]).unwrap();
    assert_eq!(spool.free_bytes(), 70);
    let mut f2 = spool.create().unwrap();
    f2.write_all(&[8u8; 50]).unwrap();
    assert_eq!(spool.free_bytes(), 20);
    f1.seek(SeekFrom::Start(0)).unwrap();
    f1.write_all(&[9u8; 10]).unwrap();
    assert_eq!(spool.free_bytes(), 20);
    f1.seek(SeekFrom::Start(0)).unwrap();
    let mut back = Vec::new();
    f1.read_to_end(&mut back).unwrap();
    assert_eq!(back.len(), 30);
    assert_eq!(&back[..10], &[9u8; 10]);
    let mut names: Vec<String> = std::fs::read_dir(&sdir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["spool-0.tmp", "spool-1.tmp"]);
    drop(f1);
    assert_eq!(spool.free_bytes(), 50);
    assert_eq!(std::fs::read_dir(&sdir).unwrap().count(), 1);
    drop(f2);
    assert_eq!(spool.free_bytes(), 100);
    assert_eq!(std::fs::read_dir(&sdir).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&sdir);
}

#[test]
fn linked_account_github_app_and_site_layout() {
    use std::collections::BTreeSet;

    use pigeon_core::cloudflare_oauth::{
        Client, authorize_url, callback_params, challenge, needs_refresh, read_tokens,
    };
    use pigeon_core::github_app::{
        jwt_signing_input, pkcs1_to_pkcs8, private_key_der, read_push, status_body, token_request,
        verify_webhook,
    };
    use pigeon_core::r2::{self, Manifest};
    use pigeon_core::sign::{hex, hmac_sha256};
    use pigeon_core::webauthn::unb64url;
    use pigeon_core::workers::{Route, RoutePlan, plan_route};
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::pkcs8::EncodePrivateKey;

    // (1) Linking: the authorize URL carries PKCE and state; the callback refuses errors and a
    // missing state; tokens are read without echoing them, and refresh keeps the old refresh
    // token when none comes back.
    let client = Client {
        client_id: "cid".into(),
        redirect_uri: "https://platform.example/linked/callback".into(),
        scopes: vec!["account.read".into(), "zone.read".into()],
    };
    let verifier = "v".repeat(43);
    let url = authorize_url(&client, "st8", &challenge(&verifier));
    assert!(
        url.starts_with("https://dash.cloudflare.com/oauth2/auth?"),
        "{url}"
    );
    for part in [
        "response_type=code",
        "client_id=cid",
        "state=st8",
        "code_challenge_method=S256",
        "scope=account.read%20zone.read%20offline_access",
        "redirect_uri=https%3A%2F%2Fplatform.example%2Flinked%2Fcallback",
    ] {
        assert!(url.contains(part), "{part} in {url}");
    }
    assert!(
        pigeon_core::oauth::pkce(&verifier, &challenge(&verifier)),
        "S256"
    );
    assert_eq!(
        callback_params("code=c1&state=st8"),
        Ok(("c1".to_string(), "st8".to_string()))
    );
    assert!(
        callback_params("error=access_denied&state=st8").is_err(),
        "denied"
    );
    assert!(callback_params("code=c1").is_err(), "no state");
    let t = read_tokens(
        r#"{"access_token":"at1","token_type":"Bearer","expires_in":3600,"refresh_token":"rt1"}"#,
        1_000.0,
        None,
    )
    .unwrap();
    assert_eq!(t.refresh.as_deref(), Some("rt1"));
    assert!(!needs_refresh(&t, 1_000.0), "fresh");
    assert!(
        needs_refresh(&t, 1_000.0 + 3_400_000.0),
        "under five minutes left"
    );
    let again = read_tokens(
        r#"{"access_token":"at2","token_type":"bearer","expires_in":60}"#,
        0.0,
        Some("rt1"),
    )
    .unwrap();
    assert_eq!(
        again.refresh.as_deref(),
        Some("rt1"),
        "the old refresh token is kept"
    );
    let refused = read_tokens(
        r#"{"error":"invalid_grant","error_description":"reused"}"#,
        0.0,
        Some("rt-secret"),
    )
    .unwrap_err();
    assert!(
        refused.contains("invalid_grant") && !refused.contains("rt-secret"),
        "{refused}"
    );
    assert!(
        !format!("{t:?}").contains("at1"),
        "Debug never shows a token"
    );

    // (2) The GitHub App: its key as GitHub hands it out (PKCS#1) becomes the PKCS#8 that
    // WebCrypto imports; a JWT is short-lived; only a correct signature over these bytes passes.
    let key = webhook_and_oidc_key();
    let pkcs1 = key.to_pkcs1_der().unwrap();
    let pkcs8 = key.to_pkcs8_der().unwrap();
    assert_eq!(
        pkcs1_to_pkcs8(pkcs1.as_bytes()),
        pkcs8.as_bytes(),
        "wrapped as rsa does"
    );
    let pem = |label: &str, der: &[u8]| {
        let b64 = pigeon_core::git::base64(der);
        let lines: Vec<&str> = b64
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        )
    };
    assert_eq!(
        private_key_der(&pem("RSA PRIVATE KEY", pkcs1.as_bytes())).as_deref(),
        Some(pkcs8.as_bytes())
    );
    assert_eq!(
        private_key_der(&pem("PRIVATE KEY", pkcs8.as_bytes())).as_deref(),
        Some(pkcs8.as_bytes())
    );
    assert!(private_key_der(&pem("PUBLIC KEY", pkcs8.as_bytes())).is_none());
    let input = jwt_signing_input("Iv1.abc", 1_000_000);
    let (h, c) = input.split_once('.').unwrap();
    let header: Value = serde_json::from_slice(&unb64url(h).unwrap()).unwrap();
    let claims: Value = serde_json::from_slice(&unb64url(c).unwrap()).unwrap();
    assert_eq!(header, json!({ "alg": "RS256", "typ": "JWT" }));
    assert_eq!(
        claims,
        json!({ "iat": 999_940, "exp": 1_000_540, "iss": "Iv1.abc" })
    );

    let secret = b"gh-hook";
    let body = br#"{"ref":"refs/heads/main","after":"abc123","deleted":false,"repository":{"full_name":"noisypigeon/pigeon"},"installation":{"id":42},"head_commit":{"message":"post"}}"#;
    let good = format!("sha256={}", hex(&hmac_sha256(secret, body)));
    assert!(verify_webhook(secret, body, Some(&good)));
    assert!(!verify_webhook(secret, b"{}", Some(&good)), "other bytes");
    assert!(!verify_webhook(b"other", body, Some(&good)), "other secret");
    assert!(!verify_webhook(secret, body, None), "unsigned");
    assert!(
        !verify_webhook(secret, body, Some(&good[7..])),
        "no sha256= prefix"
    );
    let push = read_push(Some("push"), body).unwrap().unwrap();
    assert_eq!(
        (
            push.repo.as_str(),
            push.git_ref.as_str(),
            push.after.as_str(),
            push.installation
        ),
        ("noisypigeon/pigeon", "refs/heads/main", "abc123", 42)
    );
    assert_eq!(read_push(Some("ping"), b"{}"), Ok(None));
    assert!(
        read_push(Some("push"), b"{}").is_err(),
        "a push without its fields"
    );
    assert_eq!(
        token_request(&["pigeon"], &[("contents", "read")]),
        json!({ "repositories": ["pigeon"], "permissions": { "contents": "read" } })
    );
    let long = "x".repeat(300);
    let status = status_body("success", Some("https://noisypigeon.com"), None, &long);
    assert_eq!(status["description"].as_str().unwrap().chars().count(), 140);
    assert!(status.get("log_url").is_none(), "absent urls are left out");

    // (3) The site's layout: content-addressed blobs, a manifest per commit, directory paths
    // resolved, nothing uploaded twice, and the live deploy never pruned.
    let files = vec![
        ("index.html".to_string(), b"<h1>home</h1>".to_vec()),
        ("/posts/a/index.html".to_string(), b"<p>a</p>".to_vec()),
        ("/img/p.jpg".to_string(), b"jpeg".to_vec()),
        ("/img/copy.jpg".to_string(), b"jpeg".to_vec()),
    ];
    let manifest: Manifest = r2::manifest_of(&files);
    assert!(manifest.contains_key("/index.html"), "paths start with /");
    assert_eq!(
        manifest["/img/p.jpg"].blob, manifest["/img/copy.jpg"].blob,
        "same bytes, one blob"
    );
    assert_eq!(
        manifest["/index.html"].content_type,
        "text/html; charset=utf-8"
    );
    let present: BTreeSet<String> = [manifest["/img/p.jpg"].blob.clone()].into();
    assert_eq!(
        r2::missing(&manifest, &present).len(),
        2,
        "only what the bucket lacks"
    );
    for (request, served) in [
        ("/", Some("/index.html")),
        ("/posts/a", Some("/posts/a/index.html")),
        ("/posts/a/", Some("/posts/a/index.html")),
        ("/img/p.jpg", Some("/img/p.jpg")),
        ("/nope", None),
        ("/posts/../index.html", None),
    ] {
        assert_eq!(
            r2::resolve(&manifest, request).as_deref(),
            served,
            "{request}"
        );
    }
    let deploys: Vec<(String, u64)> = (0..5).map(|i| (format!("c{i}"), i)).collect();
    let mut pruned = r2::prune(&deploys, "c0", 2);
    pruned.sort();
    assert_eq!(
        pruned,
        ["c1", "c2"],
        "the oldest beyond two, never the live one"
    );

    // (4) The cutover route: created once, never taking a pattern another Worker serves.
    let routes = vec![Route {
        id: "r1".into(),
        pattern: "noisypigeon.com/*".into(),
        script: Some("old-site".into()),
    }];
    assert_eq!(
        plan_route(&routes, "noisypigeon.com/*", "pigeon-site-blog"),
        RoutePlan::Conflict("old-site".into())
    );
    assert_eq!(
        plan_route(&[], "noisypigeon.com/*", "pigeon-site-blog"),
        RoutePlan::Create
    );
    let ours = vec![Route {
        script: Some("pigeon-site-blog".into()),
        ..routes[0].clone()
    }];
    assert_eq!(
        plan_route(&ours, "noisypigeon.com/*", "pigeon-site-blog"),
        RoutePlan::Present("r1".into())
    );
}

#[test]
fn linking_a_cloudflare_account() {
    // linked_scopes: each data-Worker need matched to Cloudflare's scope names, ignoring case;
    // a need no scope offers comes back empty.
    {
        use pigeon_capabilities::linked::needs_found;
        let listed = json!([
            { "id": "pipelines.write", "name": "Pipelines Write" },
            { "id": "email-routing.write", "name": "Email Routing Rules Write" },
            { "id": "zone.read", "name": "Zone Read" },
        ]);
        let found = needs_found(&listed, &["Pipelines", "email routing", "Workers AI"]);
        assert_eq!(found["Pipelines"][0]["id"], "pipelines.write");
        assert_eq!(
            found["email routing"][0]["name"],
            "Email Routing Rules Write"
        );
        assert_eq!(found["Workers AI"], json!([]));
    }

    use pigeon_capabilities::linked::{
        CAPABILITY_SCOPES, Link, PERMISSIONS, choose_account, optional_scopes,
        requested_permissions, scope_ids, scopes_to_add, status,
    };

    // The client asks for exactly the scopes pigeon needs, found by their permission names:
    // the base ones every link needs, then each enabled-on-linked capability's (M4).
    let listed = json!([
        { "id": "account-settings.read", "name": "Account Settings Read" },
        { "id": "zone.read", "name": "Zone Read" },
        { "id": "dns.write", "name": "DNS Write" },
        { "id": "workers-scripts.write", "name": "Workers Scripts Write" },
        { "id": "workers-routes.write", "name": "Workers Routes Write" },
        { "id": "r2.write", "name": "Workers R2 Storage Write" },
        { "id": "secrets-store.write", "name": "Secrets Store Write" },
        { "id": "d1.write", "name": "D1 Write" },
        { "id": "email-routing-rule.write", "name": "Email Routing Rules Write" },
        { "id": "email-routing-address.write", "name": "Email Routing Addresses Write" },
        { "id": "zone-settings.write", "name": "Zone Settings Write" },
        { "id": "zone.write", "name": "Zone Write" }
    ]);
    // The base scopes, then `secrets`', `data`'s and `email`'s (docs/plans/linked-data.md).
    assert_eq!(
        requested_permissions(),
        [
            &PERMISSIONS[..],
            &[
                "Secrets Store Write",
                "D1 Write",
                "Email Routing Rules Write",
                "Email Routing Addresses Write",
                "Zone Settings Write"
            ]
        ]
        .concat()
    );
    let (ids, missing) = scope_ids(&listed);
    assert_eq!(ids.len(), requested_permissions().len());
    assert!(
        missing.is_empty() && !ids.contains(&"zone.write".to_string()),
        "nothing more"
    );
    let (_, missing) = scope_ids(&json!([]));
    assert_eq!(
        missing.len(),
        requested_permissions().len(),
        "a missing scope is named, not skipped"
    );
    // Each capability's scope id is the one Cloudflare lists for its permission, and a person
    // may decline it at consent; the base ones are required.
    for c in CAPABILITY_SCOPES {
        let (ids, _) = scope_ids(&listed);
        assert!(ids.contains(&c.scope.to_string()), "{}", c.scope);
    }
    assert_eq!(
        optional_scopes(&ids),
        [
            "secrets-store.write",
            "d1.write",
            "email-routing-rule.write",
            "email-routing-address.write",
            "zone-settings.write"
        ]
    );
    // A client registered before the scope was added asks for it on the next linked_setup.
    let before: Vec<String> = ids[..PERMISSIONS.len()].to_vec();
    assert_eq!(
        scopes_to_add(&before)
            .iter()
            .map(|c| c.capability)
            .collect::<Vec<_>>(),
        ["secrets", "data", "data-email", "data-email", "data-email"]
    );
    assert!(scopes_to_add(&ids).is_empty());

    // The account a link takes: the one the record names, or the only one granted.
    let granted = vec![
        ("a".repeat(32), "noisypigeon".to_string()),
        ("b".repeat(32), "other".to_string()),
    ];
    let a = "a".repeat(32);
    assert_eq!(choose_account(Some(&a), &granted).unwrap().1, "noisypigeon");
    assert!(
        choose_account(Some(&"c".repeat(32)), &granted).is_err(),
        "not granted"
    );
    assert!(
        choose_account(None, &granted).is_err(),
        "several: the person chooses"
    );
    assert_eq!(choose_account(None, &granted[..1]).unwrap().0, a);
    assert!(choose_account(None, &[]).is_err());

    // What the console shows of a link never holds its sealed tokens.
    let l = Link {
        instance: "noisypigeon".into(),
        account_id: a.clone(),
        account_name: "noisypigeon".into(),
        linked_by: "Willow".into(),
        linked_ms: 1,
        expires_at_ms: 10.0,
        scope: None,
        sealed: "SEALED-TOKENS".into(),
        refreshed_ms: None,
        failed: None,
    };
    let shown = status(Some(&l), 5.0).to_string();
    assert!(
        shown.contains("linked") && !shown.contains("SEALED-TOKENS"),
        "{shown}"
    );
    assert_eq!(status(None, 0.0)["status"], "not linked");
}

#[test]
fn github_delivery_records_queue_and_replay_marker_atomically() {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use pigeon_capabilities::users::{PRUNE_DELIVERIES, RECORD_DELIVERY};
    use pigeon_core::github_app::delivery_fingerprint;
    use rusqlite::{Connection, params};

    const START: &str = "github:delivery:";
    const END: &str = "github:delivery;";
    const PUSH: &str = "github:push:example";
    const DEPLOY: &str = "github:deploy:example";

    fn deliver(
        db: &Connection,
        fingerprint: &str,
        push: &str,
        pending: Option<&str>,
        now: i64,
        limit: i64,
    ) -> rusqlite::Result<usize> {
        db.execute(PRUNE_DELIVERIES, params![START, END, now])?;
        let key = format!("{START}{fingerprint}");
        let mut statement = db.prepare(RECORD_DELIVERY)?;
        let rows = statement
            .query_map(
                params![
                    key,
                    now + 7,
                    now + 90,
                    PUSH,
                    push,
                    DEPLOY,
                    pending,
                    key,
                    now,
                    START,
                    END,
                    now,
                    limit,
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.len())
    }

    fn value(db: &Connection, key: &str) -> String {
        db.query_row("SELECT value FROM settings WHERE key = ?", [key], |row| {
            row.get(0)
        })
        .unwrap()
    }

    let dir = super::repo("github-delivery-db");
    let path = dir.join("directory.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER NOT NULL);",
    )
    .unwrap();

    let a = delivery_fingerprint(b"original authenticated body");
    let same_body_different_header = delivery_fingerprint(b"original authenticated body");
    let b = delivery_fingerprint(b"later authenticated body");
    let c = delivery_fingerprint(b"authenticated body for a nonpublishing ref");
    let barrier = Arc::new(Barrier::new(2));
    let mut counts = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let db = Connection::open(&path).unwrap();
            db.busy_timeout(Duration::from_secs(3)).unwrap();
            barrier.wait();
            deliver(&db, &a, "push A", Some("pending A"), 0, 2).unwrap()
        });
        let second = scope.spawn(|| {
            let db = Connection::open(&path).unwrap();
            db.busy_timeout(Duration::from_secs(3)).unwrap();
            barrier.wait();
            deliver(
                &db,
                &same_body_different_header,
                "push A",
                Some("pending A"),
                0,
                2,
            )
            .unwrap()
        });
        [first.join().unwrap(), second.join().unwrap()]
    });
    counts.sort_unstable();
    assert_eq!(counts, [0, 3]);
    assert_eq!(value(&db, PUSH), "push A");

    db.execute(
        "UPDATE settings SET value = 'done A' WHERE key = ?",
        [DEPLOY],
    )
    .unwrap();
    assert_eq!(
        deliver(&db, &a, "push A", Some("pending A"), 1, 2).unwrap(),
        0
    );
    assert_eq!(value(&db, DEPLOY), "done A");

    db.execute_batch(
        "CREATE TRIGGER fail_deploy BEFORE INSERT ON settings
         WHEN new.key = 'github:deploy:example'
         BEGIN SELECT raise(ABORT, 'simulated queue failure'); END;",
    )
    .unwrap();
    assert!(deliver(&db, &b, "push B", Some("pending B"), 2, 2).is_err());
    assert_eq!(value(&db, PUSH), "push A");
    assert_eq!(value(&db, DEPLOY), "done A");
    db.execute_batch("DROP TRIGGER fail_deploy").unwrap();
    assert_eq!(
        deliver(&db, &b, "push B", Some("pending B"), 2, 2).unwrap(),
        3
    );

    // Replay of A cannot overtake B, even when the cache is full.
    assert_eq!(
        deliver(&db, &a, "push A", Some("pending A"), 3, 2).unwrap(),
        0
    );
    assert_eq!(deliver(&db, &c, "push C", None, 3, 2).unwrap(), 0);
    assert_eq!(value(&db, PUSH), "push B");
    assert_eq!(value(&db, DEPLOY), "pending B");

    // Once A expires, a nonpublishing push records its fingerprint and leaves B queued.
    assert_eq!(deliver(&db, &c, "push C", None, 7, 2).unwrap(), 2);
    assert_eq!(
        deliver(&db, &c, "push C", Some("pending C"), 8, 2).unwrap(),
        0
    );
    assert_eq!(value(&db, PUSH), "push C");
    assert_eq!(value(&db, DEPLOY), "pending B");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn console_site_access_binds_visitor_list_and_invite_to_exact_selection() {
    use pigeon_capabilities::site_passkeys::console::matches_selection;
    use pigeon_capabilities::users::SITE_VISITORS_SQL;
    use pigeon_core::site_auth::binding;
    use rusqlite::{Connection, params};

    let host = "preview.example.test";
    let scope = binding("alpha", "blog", host);
    let moved = binding("beta", "blog", host);
    let other_host = binding("alpha", "blog", "other.example.test");
    assert!(matches_selection(&scope, "alpha", "blog", host));
    assert!(!matches_selection(&scope, "beta", "blog", host));
    assert!(!matches_selection(&scope, "alpha", "docs", host));
    assert!(!matches_selection(
        &scope,
        "alpha",
        "blog",
        "other.example.test"
    ));

    // An invitation is the console's `visitor-invite` write: bound to the exact selection,
    // the name and the person (console::write tests).

    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE users (id TEXT PRIMARY KEY, kind TEXT, name TEXT, site_scope TEXT, created_at INTEGER);
         CREATE TABLE passkeys (user_id TEXT);
         INSERT INTO passkeys VALUES ('one');
         INSERT INTO passkeys VALUES ('moved');",
    )
    .unwrap();
    let kind = format!("site:{host}");
    for (id, user_kind, name, binding) in [
        ("one", kind.as_str(), "Alex", Some(scope.as_str())),
        ("two", kind.as_str(), "Sam", Some(scope.as_str())),
        ("moved", kind.as_str(), "Other owner", Some(moved.as_str())),
        ("legacy", kind.as_str(), "Legacy visitor", None),
        (
            "other",
            "site:other.example.test",
            "Other host",
            Some(other_host.as_str()),
        ),
        ("platform", "standard", "Platform person", None),
    ] {
        db.execute(
            "INSERT INTO users VALUES (?, ?, ?, ?, 100)",
            params![id, user_kind, name, binding],
        )
        .unwrap();
    }
    let mut statement = db.prepare(SITE_VISITORS_SQL).unwrap();
    let visitors: Vec<_> = statement
        .query_map(params![kind, scope], |row| {
            Ok((
                row.get::<_, String>("name")?,
                row.get::<_, bool>("enrolled")?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(visitors, [("Alex".into(), true), ("Sam".into(), false)]);
}

#[test]
fn site_visitor_invites_and_enrolments_need_the_site_allowlist() {
    // The console and the MCP `users_invite` / `users_enrol` tools share this rule before any
    // site user is created or enrolled: the site's allowlist must list "site".
    use pigeon_capabilities::site_passkeys::{admit_new_visitors, allows_new_visitors};
    use pigeon_core::site_auth::{Config, allowed};

    let config = |allow: &[&str]| Config {
        allow: allow.iter().map(|entry| entry.to_string()).collect(),
    };
    let host = "preview.example.test";
    let visitor = format!("site:{host}");
    for closed in [config(&[]), config(&["owner"]), config(&["Site", "sites"])] {
        assert!(!allows_new_visitors(&closed));
        let refused = admit_new_visitors(&closed).unwrap_err().to_string();
        assert!(
            refused.contains("does not admit new site visitors"),
            "{refused}"
        );
        // Sign-in agrees: a new site user of this host would not get in either.
        assert!(!allowed(&closed, host, "new", &visitor));
    }
    for open in [config(&["site"]), config(&["owner", "site"])] {
        assert!(allows_new_visitors(&open));
        admit_new_visitors(&open).unwrap();
        assert!(allowed(&open, host, "new", &visitor));
        assert!(!allowed(&open, host, "new", "site:other.example.test"));
    }
    // Standard users are listed by id; "site" alone does not let them in.
    assert!(allowed(&config(&["owner"]), host, "owner", "standard"));
    assert!(!allowed(&config(&["site"]), host, "owner", "standard"));
}

#[test]
fn backblaze_ingest_stays_inside_the_approved_source() {
    use pigeon_ingest::backblaze::Source;
    use pigeon_ingest::manifest::SourceSpec;

    let mut source = Source {
        name: "archive".into(),
        region: "us-west-004".into(),
        bucket: "ArchiveBucket".into(),
        prefix: "export/".into(),
    };
    assert_eq!(
        source.bind(Some("export/2024/")).unwrap(),
        SourceSpec::S3 {
            account: "backblaze:archive".into(),
            endpoint: "https://s3.us-west-004.backblazeb2.com".into(),
            region: "us-west-004".into(),
            bucket: "ArchiveBucket".into(),
            prefix: "export/2024/".into(),
        }
    );
    assert!(source.bind(None).is_ok());
    assert!(source.bind(Some("")).is_err());
    assert!(source.bind(Some("export-other/")).is_err());
    assert!(source.bind(Some("export/\n")).is_err());
    assert!(
        source
            .bind(Some(&format!("export/{}", "a".repeat(1024))))
            .is_err()
    );
    source.region = "us-west-004.backblazeb2.com@bad".into();
    assert!(source.bind(None).is_err());
}

#[test]
fn cloudflare_ingest_runtime_bounds_deadlines_and_run_keys() {
    use pigeon_ingest::container_job::*;

    assert!(valid_run("20261127-backblaze-archive-c2dfab"));
    assert!(!valid_run("../../terminal"));
    assert!(!valid_run(&"a".repeat(97)));
    assert!(valid_revision(&"a".repeat(40)));
    assert!(!valid_revision(&"g".repeat(40)));
    assert!(valid_deadline(1_000, 1_000 + MAX_RUNTIME_MS));
    assert!(!valid_deadline(1_000, 1_000));
    assert!(!valid_deadline(1_000, 1_001 + MAX_RUNTIME_MS));
    assert!(!valid_deadline(i64::MIN, i64::MAX));
}
