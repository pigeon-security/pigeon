//! The console, mail, changelog and architecture views of the core Worker.

use super::*;

fn console_sign_in_and_a_write_assertion(
    key: &p256::ecdsa::SigningKey,
    challenge: &str,
    origin: &str,
    tamper: bool,
) -> pigeon_core::webauthn::Assertion {
    use p256::ecdsa::{Signature, signature::Signer};
    use pigeon_core::webauthn::{Assertion, b64url};
    use sha2::{Digest, Sha256};
    let client =
        format!(r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"{origin}"}}"#);
    let mut auth = Sha256::digest(b"platform.pigeon.dev").to_vec();
    auth.extend_from_slice(&[0x05, 0, 0, 0, 1]);
    let mut msg = auth.clone();
    msg.extend_from_slice(&Sha256::digest(client.as_bytes()));
    let sig: Signature = key.sign(&msg);
    if tamper {
        auth[36] ^= 0xff;
    }
    Assertion {
        id: b64url(b"cred-1"),
        client_data: b64url(client.as_bytes()),
        authenticator_data: b64url(&auth),
        signature: b64url(sig.to_der().as_bytes()),
    }
}

#[test]
fn console_sign_in_and_a_write() {
    use p256::ecdsa::SigningKey;
    use pigeon_capabilities::console::session::*;
    use pigeon_core::webauthn::{Credential, RelyingParty, assert as verify, b64url};

    // Sign-in: the passkey's assertion verifies; a forged one does not.
    let key = SigningKey::random(&mut rand::rngs::OsRng);
    let other = SigningKey::random(&mut rand::rngs::OsRng);
    let cred = Credential {
        id: b64url(b"cred-1"),
        point: key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec(),
        sign_count: 0,
    };
    let rp = RelyingParty {
        id: "platform.pigeon.dev",
        origin: "https://platform.pigeon.dev",
    };
    let good = "https://platform.pigeon.dev";
    let attempt = |k: &SigningKey, signed: &str, origin: &str, tamper: bool, expect: &str| {
        let a = console_sign_in_and_a_write_assertion(k, signed, origin, tamper);
        verify(&a, &cred, expect, &rp).ok()
    };
    let ch = "Y2hhbGxlbmdl";
    assert_eq!(attempt(&key, ch, good, false, ch), Some(1));
    assert_eq!(attempt(&other, ch, good, false, ch), None);
    assert_eq!(attempt(&key, "b3RoZXI", good, false, ch), None);
    assert_eq!(attempt(&key, ch, "https://evil.example", false, ch), None);
    assert_eq!(attempt(&key, ch, good, true, ch), None);

    // The session cookie, and the ones ignored.
    let c = cookie("pgn_ss_AbC-12_xyz");
    assert!(c.starts_with("__Host-pigeon_session=pgn_ss_AbC-12_xyz; "));
    for attr in [
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Max-Age=28800",
    ] {
        assert!(c.split("; ").any(|part| part == attr), "{attr}");
    }
    assert!(!c.contains("Domain"));
    assert!(clear().starts_with("__Host-pigeon_session=; ") && clear().ends_with("; Max-Age=0"));
    let t = "pgn_ss_AbC-12_xyz";
    assert_eq!(
        token_from(&format!("theme=dark;  {COOKIE}={t};lang=en")),
        Some(t)
    );
    let long = format!("{PREFIX}{}", "a".repeat(100 - PREFIX.len()));
    assert_eq!(token_from(&format!("{COOKIE}={long}")), Some(long.as_str()));
    assert_eq!(token_from(&format!("{COOKIE}={long}a")), None);
    for bad in [
        "pigeon_session=pgn_ss_AbC123".to_string(),
        format!("{COOKIE}=pgn_xx_AbC123"),
        format!("{COOKIE}=pgn_ss_AbC+123"),
        format!("{COOKIE}={PREFIX}"),
    ] {
        assert_eq!(token_from(&bad), None, "{bad}");
    }

    // Idle, too old, or from the future: not fresh.
    let now = 10 * MAX_MS;
    assert!(fresh(now - 1000, now - IDLE_MS + 1, now));
    assert!(!fresh(now - IDLE_MS, now - IDLE_MS, now));
    assert!(fresh(now - MAX_MS + 1, now, now));
    assert!(!fresh(now - MAX_MS, now, now));
    assert!(fresh(now + 60_000, now + 60_000, now));
    assert!(!fresh(now, now + 60_001, now));

    // A write comes from this origin, as JSON.
    let host = "platform.pigeon.dev";
    assert!(same_origin(Some(good), host));
    assert!(same_origin(Some("https://PLATFORM.pigeon.dev"), host));
    for bad in [
        "null",
        "http://platform.pigeon.dev",
        "https://platform.pigeon.dev:8443",
        "https://platform.pigeon.dev/",
        "https://evilplatform.pigeon.dev",
        "https://platform.pigeon.dev.evil.example",
    ] {
        assert!(!same_origin(Some(bad), host), "{bad}");
    }
    assert!(!same_origin(None, host));
    assert!(json_content(Some("Application/JSON; charset=utf-8")));
    assert!(!json_content(Some("application/jsonp")) && !json_content(None));

    // A write's passkey signs the challenge kept for its own purpose; another one's fails.
    use pigeon_capabilities::console::write::{action, digest, purpose};
    let a = action("land-approve").unwrap();
    let (p1, p2) = (
        purpose(a, &digest(a, "u1", &json!("L1"), &json!({}), None)),
        purpose(a, &digest(a, "u1", &json!("L2"), &json!({}), None)),
    );
    assert_ne!(p1, p2);
    let signed = b64url(p1.as_bytes());
    assert_eq!(attempt(&key, &signed, good, false, &signed), Some(1));
    assert_eq!(
        attempt(&key, &signed, good, false, &b64url(p2.as_bytes())),
        None
    );
}

/// The email parser (`pigeon_email`): raw MIME in, a typed message and its blobs out. It covers
/// headers kept in order, decoded subject and names, addresses with groups flattened, the
/// bodies, attachments stored once by content, signals, a JSON round trip, and bad input
/// returned as an error.
#[test]
fn email_parses_a_multipart_message() {
    use pigeon_email::{ContentRef, Error, Message, Role, Source, parse};

    let pdf = b"%PDF-1.4 test";
    let names = [
        "Received",
        "Received",
        "Authentication-Results",
        "Message-ID",
        "In-Reply-To",
        "References",
        "List-Id",
        "List-Unsubscribe",
        "Date",
        "Subject",
        "From",
        "To",
        "Cc",
        "MIME-Version",
        "Content-Type",
    ];
    let sample = [
        "Received: from mx.example.net (mx.example.net [192.0.2.1])",
        "\tby mail.example.org with ESMTPS; Mon, 23 Nov 2026 10:15:05 +0100",
        "Received: from client.example.net by mx.example.net; Mon, 23 Nov 2026 10:15:02 +0100",
        "Authentication-Results: mail.example.org; spf=pass smtp.mailfrom=example.com;",
        " dkim=pass header.d=example.com; dmarc=pass header.from=example.com",
        "Message-ID: <msg-1@example.com>",
        "In-Reply-To: <prev-1@example.com>",
        "References: <root-1@example.com> <prev-1@example.com>",
        "List-Id: Billing news <news.example.com>",
        "List-Unsubscribe: <mailto:unsub@example.com>",
        "Date: Mon, 23 Nov 2026 10:15:00 +0100",
        "Subject: =?UTF-8?B?UmVjaG51bmcgZsO8ciBOb3ZlbWJlcg==?=",
        "From: =?UTF-8?B?QsO8cm8gQmlsbGluZw==?= <Billing@Example.COM>",
        "To: Ann <ann@example.org>, bob@example.org",
        "Cc: team: cy@example.org, Di <di@example.net>;",
        "MIME-Version: 1.0",
        "Content-Type: multipart/mixed; boundary=\"outer\"",
        "",
        "--outer",
        "Content-Type: multipart/alternative; boundary=\"alt\"",
        "",
        "--alt",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Your invoice is attached.",
        "--alt",
        "Content-Type: text/html; charset=utf-8",
        "",
        "<p>Your invoice is attached.</p>",
        "--alt--",
        "--outer",
        "Content-Type: application/pdf; name=\"invoice.pdf\"",
        "Content-Disposition: attachment; filename=\"invoice.pdf\"",
        "Content-Transfer-Encoding: base64",
        "",
        "JVBERi0xLjQgdGVzdA==",
        "--outer",
        "Content-Type: image/png; name=\"logo.png\"",
        "Content-Disposition: inline; filename=\"logo.png\"",
        "Content-ID: <logo@x>",
        "Content-Transfer-Encoding: base64",
        "",
        "iVBORw0KGgo=",
        "--outer",
        "Content-Type: application/pdf; name=\"copy.pdf\"",
        "Content-Disposition: attachment; filename=\"copy.pdf\"",
        "Content-Transfer-Encoding: base64",
        "",
        "JVBERi0xLjQgdGVzdA==",
        "--outer--",
        "",
    ]
    .join("\r\n");
    let source = Source::EmailRouting {
        envelope_from: "bounce@example.com".into(),
        envelope_to: "Me@Example.ORG".into(),
    };
    let parsed = parse(sample.as_bytes(), source.clone()).unwrap();
    let m = &parsed.message;

    assert_eq!(m.schema, 1);
    assert_eq!(m.raw, ContentRef::of(sample.as_bytes()));
    let key = m.raw.key();
    assert_eq!(
        key,
        format!(
            "blobs/sha256/{}/{}/{}",
            &m.raw.sha256[0..2],
            &m.raw.sha256[2..4],
            m.raw.sha256
        )
    );
    assert_eq!(m.raw.sha256.len(), 64);

    // Every header is kept, in order; the folded Received is unfolded in `decoded`.
    let got: Vec<&str> = m.headers.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(got, names);
    assert!(m.headers[0].value.contains("\r\n\tby mail.example.org"));
    assert_eq!(
        m.headers[0].decoded.as_deref(),
        Some(
            "from mx.example.net (mx.example.net [192.0.2.1]) \
             by mail.example.org with ESMTPS; Mon, 23 Nov 2026 10:15:05 +0100"
        )
    );
    assert_eq!(m.headers[3].decoded, None);

    assert_eq!(m.subject.as_deref(), Some("Rechnung für November"));
    assert_eq!(m.date.as_deref(), Some("2026-11-23T09:15:00Z"));
    assert_eq!(m.message_id.as_deref(), Some("msg-1@example.com"));
    assert_eq!(m.in_reply_to, ["prev-1@example.com"]);
    assert_eq!(m.references, ["root-1@example.com", "prev-1@example.com"]);

    assert_eq!(m.from.len(), 1);
    assert_eq!(m.from[0].name.as_deref(), Some("Büro Billing"));
    assert_eq!(m.from[0].address, "Billing@Example.COM");
    assert_eq!(m.from[0].normalized, "billing@example.com");
    assert_eq!(m.from[0].domain, "example.com");
    assert_eq!(m.to.len(), 2);
    let cc: Vec<&str> = m.cc.iter().map(|a| a.normalized.as_str()).collect();
    assert_eq!(cc, ["cy@example.org", "di@example.net"]);
    let people = m.participants();
    for role in [Role::From, Role::To, Role::Cc] {
        assert!(people.iter().any(|(r, _)| *r == role));
    }
    assert!(
        people
            .iter()
            .any(|(r, a)| *r == Role::DeliveredTo && a.normalized == "me@example.org")
    );
    assert!(m.delivered_to.iter().any(|a| a.address == "Me@Example.ORG"));

    assert!(
        m.text
            .as_deref()
            .unwrap_or_default()
            .contains("Your invoice is attached.")
    );
    let html = m.html.as_ref().unwrap();
    let html_bytes = b"<p>Your invoice is attached.</p>";
    assert_eq!(html, &ContentRef::of(html_bytes));
    assert!(
        parsed
            .blobs
            .iter()
            .any(|(c, b)| c == html && b == html_bytes)
    );

    assert_eq!(m.attachments.len(), 3);
    let pdf_ref = ContentRef::of(pdf);
    let pdfs: Vec<_> = m
        .attachments
        .iter()
        .filter(|a| a.content_type == "application/pdf")
        .collect();
    assert_eq!(pdfs.len(), 2);
    assert!(pdfs.iter().all(|a| a.content == pdf_ref));
    assert_eq!(pdfs[0].filename.as_deref(), Some("invoice.pdf"));
    assert_eq!(pdfs[1].filename.as_deref(), Some("copy.pdf"));
    assert_eq!(
        parsed.blobs.iter().filter(|(c, _)| *c == pdf_ref).count(),
        1
    );
    // The HTML body, the PDF and the PNG: three distinct blobs.
    assert_eq!(parsed.blobs.len(), 3);
    let logo = m
        .attachments
        .iter()
        .find(|a| a.content_type == "image/png")
        .unwrap();
    assert_eq!(logo.content_id.as_deref(), Some("logo@x"));
    assert_eq!(logo.disposition.as_deref(), Some("inline"));

    let s = &m.taxonomy.signals;
    assert!(s.is_reply);
    assert!(!s.is_forward);
    assert!(s.has_attachments);
    assert_eq!(
        s.list_id.as_deref(),
        Some("Billing news <news.example.com>")
    );
    assert!(s.list_unsubscribe);
    for method in ["spf", "dkim", "dmarc"] {
        assert!(
            s.auth
                .iter()
                .any(|a| a.method == method && a.result == "pass")
        );
    }
    assert_eq!(m.taxonomy.doc_type, None);
    assert!(m.taxonomy.labels.is_empty());

    // The JSON round trip keeps the message, and parsing twice gives the same result.
    let json = serde_json::to_string(m).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, m);
    assert_eq!(parse(sample.as_bytes(), source).unwrap(), parsed);

    // Bad input is returned as an error, never a panic.
    assert_eq!(
        parse(b"", Source::Upload { name: None }).unwrap_err(),
        Error::Empty
    );
    // mail-parser finds no header section in binary junk, so parse returns NotMime.
    let junk = parse(b"\x00\x01 not mail", Source::Upload { name: None });
    assert!(matches!(junk, Err(Error::NotMime(_))));
}

#[test]
fn core_changelog_from_history() {
    use pigeon_capabilities::changelog::wants_event;
    use pigeon_core::changelog::{
        Entry, State, date_utc, landing_of, newest_days, render, subject, walk,
    };
    use pigeon_core::pack;
    use std::collections::BTreeMap;

    let dir = repo("changelog");
    // Commits at fixed times: two on 2026-11-21 UTC (the second at 00:30 +02:00 on the 22nd,
    // still the 21st in UTC), then two on 2026-11-22 UTC.
    let at = |message: &str, date: &str| -> String {
        std::fs::write(dir.join("f"), message).unwrap();
        git(&dir, &["add", "f"], None);
        let ok = Command::new("git")
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(ok.success());
        String::from_utf8(git(&dir, &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_string()
    };
    let a = at("first\n\nbody", "2026-11-21T09:00:00+00:00");
    let b = at("second", "2026-11-22T00:30:00+02:00");
    let c = at("third", "2026-11-22T08:00:00+00:00");
    let d = at("fourth", "2026-11-22T20:00:00+00:00");
    let all = |tip: &str| {
        pack::objects(&git(
            &dir,
            &["pack-objects", "--stdout", "--revs", "-q"],
            Some(&format!("{tip}\n")),
        ))
        .unwrap()
    };

    // Backfill: every commit, newest first, with its subject and committer time.
    let full = walk(&all(&d), &d, None).unwrap();
    let shas: Vec<&str> = full.iter().map(|e| e.sha.as_str()).collect();
    assert_eq!(shas, [d.as_str(), c.as_str(), b.as_str(), a.as_str()]);
    assert_eq!(full[3].subject, "first");
    assert_eq!(date_utc(full[2].unix), "2026-11-21");
    // Incremental: only what is new since the last processed commit, which joins the old
    // entries into the full list; a stop not on the history is an error.
    let old = walk(&all(&b), &b, None).unwrap();
    let since = pack::objects(&pack(&dir, &d, &b)).unwrap();
    let mut new = walk(&since, &d, Some(&b)).unwrap();
    assert_eq!(new.len(), 2);
    new.extend(old);
    assert_eq!(new, full);
    assert!(walk(&all(&d), &d, Some(&"0".repeat(40))).is_err());
    assert!(
        walk(&since, &d, None).is_err(),
        "a missing commit is an error"
    );

    // The render: dates newest first, short ids, a landing id only where a note names it.
    let mut landings = BTreeMap::new();
    landings.insert(d.clone(), "L1".to_string());
    let md = render(&full, &landings, None);
    let day22 = md.find("## 2026-11-22").unwrap();
    let day21 = md.find("## 2026-11-21").unwrap();
    assert!(md.starts_with("# Changelog of main\n") && day22 < day21);
    assert!(md.contains(&format!("- fourth (`{}`, landing `L1`)\n", &d[..7])));
    assert!(md.contains(&format!("- third (`{}`)\n", &c[..7])));
    assert!(md.find("- fourth").unwrap() < md.find("- third").unwrap());
    assert!(md[day21..].contains("- second") && md[day21..].contains("- first"));
    let one = render(&full, &landings, Some(1));
    assert!(one.contains("## 2026-11-22") && !one.contains("## 2026-11-21"));
    assert_eq!(newest_days(&md, 1).trim_end(), one.trim_end());
    assert_eq!(newest_days(&md, 5), md);

    // Notes, dates and subjects.
    assert_eq!(
        landing_of("landing L1 state=landed by=x test=- deploy=- approved_by=- at=t\n"),
        Some("L1".to_string())
    );
    assert_eq!(landing_of("junk"), None);
    assert_eq!(landing_of("landing state=x"), None);
    assert_eq!(date_utc(0), "1970-01-01");
    assert_eq!(date_utc(1_709_164_800), "2024-02-29");
    assert_eq!(date_utc(-1), "1969-12-31");
    let _ = date_utc(i64::MIN);
    assert_eq!(subject("  \n"), "(no subject)");
    assert_eq!(subject(&"x".repeat(150)).chars().count(), 101);
    let state = State {
        version: 1,
        main: d.clone(),
        entries: vec![Entry {
            sha: d.clone(),
            unix: 1,
            subject: "s".into(),
        }],
    };
    let back: State = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    assert_eq!(back, state);

    // It follows pushes to main and the landing notes of its repository, nothing else.
    let push = |r: &str| json!({ "payload": { "ref": r, "repo": "pigeon" } });
    assert!(wants_event("pigeon", "main", &push("refs/heads/main")));
    assert!(wants_event("pigeon", "main", &push("refs/notes/land")));
    assert!(!wants_event("pigeon", "main", &push("refs/heads/claude/x")));
    assert!(!wants_event(
        "pigeon",
        "main",
        &push("refs/notes/changelog")
    ));
    assert!(!wants_event(
        "pigeon",
        "main",
        &json!({ "payload": { "ref": "refs/heads/main", "repo": "other" } })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The architecture model draws one Worker for each entry of ci/workers.toml, each from its own
/// wrangler.toml, with its tools, Durable Object ownership, bindings and externals, and nothing
/// secret. Every Worker of the map is in the model and in the drawing.
#[test]
fn core_architecture_draws_every_worker_in_the_map() {
    use pigeon_capabilities::architecture::{
        Architecture, Source, Wrangler, built, describe, public,
    };
    use pigeon_capabilities::public_host::{PublicHost, Retire};
    use pigeon_capabilities::remote::{Binding, Remote};
    use pigeon_capabilities::{Capability, Hosted};
    use pigeon_contract::CapabilityManifest;

    // The repository's files, as core embeds them: the map, each Worker's wrangler.toml and Rust.
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while !root.join("ci/workers.toml").exists() {
        assert!(root.pop(), "ci/workers.toml is above the checkout");
    }
    fn rust(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() {
                rust(&p, root, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let map_text = std::fs::read_to_string(root.join("ci/workers.toml")).unwrap();
    let map = pigeon_core::lanes::parse(&map_text).expect("the map parses");
    let mut files: Vec<(String, String)> = vec![("ci/workers.toml".into(), map_text.clone())];
    for w in &map.workers {
        let toml = std::fs::read_to_string(root.join(&w.dir).join("wrangler.toml"))
            .unwrap_or_else(|_| panic!("{} has a wrangler.toml", w.dir));
        files.push((format!("{}/wrangler.toml", w.dir), toml));
    }
    // Every crate's Rust, as core's build.rs embeds it: a Worker is built from the libraries it
    // links too.
    let mut crates: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    crates.sort();
    for krate in crates {
        rust(&krate.join("src"), &root, &mut files);
    }
    let sources: Vec<Source> = files
        .iter()
        .map(|(path, text)| Source { path, text })
        .collect();
    let paths: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();

    // The capabilities core hosts: its own, and the capability Workers' from their manifests.
    let remote =
        |binding: &'static str, worker: &'static str, manifest: Value| -> Box<dyn Capability> {
            let spec: CapabilityManifest = serde_json::from_value(manifest).unwrap();
            Box::new(Remote::new(&Binding { binding, worker }, spec))
        };
    let tool = |name: &str| json!({ "name": name, "description": "A tool.", "inputSchema": { "type": "object" } });
    let proxy_host = PublicHost {
        zone: "pigeon.dev".into(),
        host: "proxy.pigeon.dev".into(),
        worker: "pigeon-proxy".into(),
        also: vec!["pigeon-platform".into()],
        retire: Retire::default(),
        tool: "setup_proxy_host",
    };
    let capabilities: Vec<Box<dyn Capability>> = vec![
        Box::new(Architecture),
        Box::new(proxy_host),
        remote(
            "MODELS",
            "pigeon-model-gateway",
            json!({
                "name": "models",
                "tools": [tool("models_submit")],
                "footprint": { "gateways": ["pigeon"], "externals": [
                    { "name": "AI Gateway", "kind": "model gateway", "detail": "gateway pigeon" }
                ] }
            }),
        ),
        remote(
            "CI",
            "pigeon-ci-gateway",
            json!({
                "name": "ci",
                "tools": [tool("ci_status")],
                "footprint": { "queues": ["pigeon-ci-events"], "externals": [
                    { "name": "Buildkite", "kind": "CI connector", "detail": "secret-detail" }
                ], "hostnames": [
                    { "host": "fake.pigeon.dev", "worker": "pigeon-ci-gateway", "by": "ci",
                      "access": ["fake-token"] },
                    { "host": "elsewhere.example.net", "worker": "pigeon-ci-gateway", "by": "ci" }
                ] }
            }),
        ),
        remote(
            "SITES",
            "pigeon-sites",
            json!({ "name": "sites", "tools": [tool("site_render")] }),
        ),
        remote(
            "AI_AGENTS",
            "pigeon-ai-agents",
            json!({ "name": "ai_agents", "tools": [tool("ai_agents_start")], "footprint": null }),
        ),
        // The cloud gateway: its real manifest entry, so its tools and outbound hosts are the ones
        // the Worker serves.
        remote(
            "CLOUD",
            "pigeon-cloud-gateway",
            serde_json::to_value(pigeon_cloud::capability()).unwrap(),
        ),
    ];
    let hosted = Hosted {
        worker: "pigeon-platform",
        capabilities: &capabilities,
        sources: &paths,
    };
    let described = describe(&sources, &hosted);
    let drawing = &built(&hosted).page;
    let model = serde_json::to_value(&described).unwrap();

    // One Worker per entry of the map, in its order, each in the drawing.
    let all = model["workers"].as_array().unwrap();
    let drawn: Vec<&str> = all.iter().map(|w| w["name"].as_str().unwrap()).collect();
    let scripts: Vec<&str> = map.workers.iter().map(|w| w.script.as_str()).collect();
    assert_eq!(drawn, scripts, "every Worker in ci/workers.toml, once");
    assert!(
        drawn.contains(&"pigeon-proxy"),
        "the proxy Worker has no binding from core"
    );
    for script in &scripts {
        assert!(drawing.contains(script), "{script} is in the drawing");
    }
    let worker = |name: &str| -> Value {
        all.iter()
            .find(|w| w["name"] == name)
            .unwrap_or_else(|| panic!("{name} is drawn"))
            .clone()
    };
    let strs = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    };

    // Tools are the owning Worker's; core merges them into /mcp.
    assert_eq!(
        strs(&worker("pigeon-model-gateway")["tools"]),
        ["models_submit"]
    );
    assert_eq!(strs(&worker("pigeon-ci-gateway")["tools"]), ["ci_status"]);
    assert_eq!(strs(&worker("pigeon-sites")["tools"]), ["site_render"]);
    assert_eq!(
        worker("pigeon-model-gateway")["merged_into"],
        "pigeon-platform"
    );
    let core_tools = strs(&worker("pigeon-platform")["tools"]);
    assert!(core_tools.contains(&"architecture_describe".into()));
    assert!(!core_tools.contains(&"models_submit".into()));
    assert_eq!(worker("pigeon-ci-gateway")["lane"], "ci-gateway");
    assert_eq!(
        worker("pigeon-ci-gateway")["deploy_job"],
        "deploy-ci-gateway"
    );

    // Durable Objects: each class under its host; a binding with `script_name` only reaches it.
    let owner = |w: &str, binding: &str| {
        worker(w)["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["binding"] == binding)
            .map(|o| o["owner"].as_str().unwrap().to_string())
    };
    for (w, binding, host) in [
        ("pigeon-platform", "CI_SCHEDULER", "pigeon-ci-gateway"),
        ("pigeon-platform", "MODEL_JOBS", "pigeon-model-gateway"),
        ("pigeon-platform", "CI_RUNNER", "pigeon-platform"),
        ("pigeon-ci-gateway", "CI_RUNNER", "pigeon-platform"),
        ("pigeon-ai-agents", "AI_AGENT_RUNS", "pigeon-ai-agents"),
    ] {
        assert_eq!(owner(w, binding).as_deref(), Some(host), "{w} {binding}");
    }
    // Core's declared bindings are all used somewhere it is built from.
    let unused = strs(&model["resources"]["unused"]);
    for linked in ["CI_RUNNER", "CI_SCHEDULER", "DIRECTORY", "LANDER"] {
        assert!(
            !unused.contains(&linked.to_string()),
            "{linked} is not unused: {unused:?}"
        );
    }
    // Whether a binding is used is read from every crate the Worker is built from, as its map
    // entry lists them: one named only in a library core links is used; one named only in a
    // crate core does not link is not.
    {
        let map = "version = 1\n[docs]\npaths = []\n[shared]\npaths = []\n[[worker]]\nname = \"core\"\nscript = \"pigeon-platform\"\ndir = \"src/platform\"\npaths = [\"src/platform/*\", \"src/lib/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy\"\n";
        let toml = "name = \"pigeon-platform\"\n\n[[secrets_store_secrets]]\nbinding = \"IN_LIB\"\nstore_id = \"s\"\nsecret_name = \"IN_LIB\"\n\n[[secrets_store_secrets]]\nbinding = \"ELSEWHERE\"\nstore_id = \"s\"\nsecret_name = \"ELSEWHERE\"\n";
        let files = [
            ("ci/workers.toml", map),
            ("src/platform/wrangler.toml", toml),
            ("src/platform/src/lib.rs", "pub fn serve() {}\n"),
            ("src/lib/src/lib.rs", "pub const KEY: &str = \"IN_LIB\";\n"),
            (
                "src/other/src/lib.rs",
                "pub const KEY: &str = \"ELSEWHERE\";\n",
            ),
        ];
        let sources: Vec<Source> = files
            .iter()
            .map(|(path, text)| Source { path, text })
            .collect();
        let none: Vec<Box<dyn Capability>> = Vec::new();
        let hosted = Hosted {
            worker: "pigeon-platform",
            capabilities: &none,
            sources: &files,
        };
        let model = serde_json::to_value(describe(&sources, &hosted)).unwrap();
        let unused = strs(&model["resources"]["unused"]);
        assert!(!unused.contains(&"IN_LIB".to_string()), "{unused:?}");
        assert!(unused.contains(&"ELSEWHERE".to_string()), "{unused:?}");
    }
    let objects = strs(&model["resources"]["objects"]);
    for want in [
        "CI_SCHEDULER (CiScheduler)",
        "MODEL_JOBS (ModelJobs)",
        "AI_AGENT_RUNS (AiAgentRuns)",
        "SANDBOXES (Sandbox)",
    ] {
        assert!(objects.iter().any(|o| o == want), "{want} is drawn");
    }
    assert_eq!(
        objects
            .iter()
            .filter(|o| o.starts_with("CI_RUNNER"))
            .count(),
        1,
        "CiRunner is drawn once, under core"
    );
    assert!(
        strs(&model["resources"]["containers"])
            .iter()
            .any(|c| c.starts_with("Sandbox (sandbox: pigeon-ai-agent-sandbox@sha256:"))
    );
    assert!(strs(&worker("pigeon-ai-agents")["containers"]).contains(&"Sandbox".into()));

    // Links: core's bindings, each other Worker's PLATFORM, and the cross-Worker objects.
    let links: Vec<(&str, &str, &str)> = model["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["from"].as_str().unwrap(),
                l["to"].as_str().unwrap(),
                l["via"].as_str().unwrap(),
            )
        })
        .collect();
    let has = |from: &str, to: &str, via: &str| links.contains(&(from, to, via));
    for (binding, to) in [
        ("SITES", "pigeon-sites"),
        ("MODELS", "pigeon-model-gateway"),
        ("CI", "pigeon-ci-gateway"),
        ("AI_AGENTS", "pigeon-ai-agents"),
        ("CLOUD", "pigeon-cloud-gateway"),
    ] {
        assert!(has("pigeon-platform", to, binding), "core binds {binding}");
    }
    for from in [
        "pigeon-sites",
        "pigeon-model-gateway",
        "pigeon-ci-gateway",
        "pigeon-proxy",
        "pigeon-ai-agents",
        "pigeon-cloud-gateway",
    ] {
        assert!(has(from, "pigeon-platform", "PLATFORM"), "{from}: PLATFORM");
    }
    assert!(has(
        "pigeon-platform",
        "pigeon-ci-gateway",
        "CI_SCHEDULER (CiScheduler)"
    ));
    assert!(has(
        "pigeon-ci-gateway",
        "pigeon-platform",
        "CI_RUNNER (CiRunner)"
    ));

    // Queues, secrets, the proxy host and externals.
    let queues = worker("pigeon-ci-gateway")["queues"].to_string();
    assert!(queues.contains("pigeon-ci-events") && queues.contains("produces"));
    assert!(strs(&worker("pigeon-sites")["secrets"]).contains(&"CALLER_KEY".into()));
    assert!(strs(&worker("pigeon-ci-gateway")["secrets"]).contains(&"BUILDKITE_API_TOKEN".into()));
    assert!(!strs(&worker("pigeon-sites")["secrets"]).contains(&"CF_PARENT_TOKEN".into()));
    assert!(strs(&worker("pigeon-platform")["secrets"]).contains(&"CF_PARENT_TOKEN".into()));
    let host = model["hostnames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "proxy.pigeon.dev")
        .unwrap();
    assert_eq!(host["worker"], "pigeon-proxy");
    let externals: Vec<&str> = model["externals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(externals, ["AI Gateway", "Buildkite", "Scaleway"]);
    // Scaleway is drawn with the four hosts the cloud gateway may call, and no others.
    let scaleway = model["externals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Scaleway")
        .unwrap();
    assert_eq!(
        scaleway["detail"],
        "api.scaleway.com, s3.fr-par.scw.cloud, s3.nl-ams.scw.cloud, s3.pl-waw.scw.cloud"
    );
    assert_eq!(
        strs(&worker("pigeon-cloud-gateway")["tools"]).len(),
        pigeon_cloud::tools::TOOLS.len()
    );
    assert!(strs(&worker("pigeon-cloud-gateway")["tools"]).contains(&"cloud_apply".into()));
    assert!(drawing.contains("Buildkite"));
    // The drawing names every link's binding and every external, in both views.
    let public_drawing = &built(&hosted).public_drawing;
    for view in [drawing, public_drawing] {
        for binding in ["SITES", "MODELS", "CI", "AI_AGENTS", "CLOUD", "PLATFORM"] {
            assert!(view.contains(binding), "{binding} labels an arrow");
        }
        for name in &externals {
            assert!(view.contains(name), "{name} is drawn");
        }
    }

    // Nothing secret or personal: not the account, the Secrets Store, or the App's client id.
    let core =
        Wrangler::read(&std::fs::read_to_string(root.join("src/platform/wrangler.toml")).unwrap());
    let text = format!("{model}{drawing}");
    for var in [
        "CF_ACCOUNT_ID",
        "CF_SECRETS_STORE_ID",
        "GITHUB_APP_CLIENT_ID",
        "GITHUB_APP_ID",
    ] {
        assert!(
            !text.contains(core.vars[var].as_str()),
            "{var} is not drawn"
        );
    }

    // The public view: nothing from any Worker's [vars], the account or the Secrets Store, no
    // email address, and none of the Access token names or external details.
    let shown = public(&described);
    let shown_text = format!(
        "{}{}",
        serde_json::to_string(&shown).unwrap(),
        built(&hosted).public_drawing
    );
    let mut secret: Vec<String> = vec!["fake-token".into(), "secret-detail".into()];
    for w in &map.workers {
        let toml = std::fs::read_to_string(root.join(&w.dir).join("wrangler.toml")).unwrap();
        secret.extend(Wrangler::read(&toml).vars.values().cloned());
        for line in toml.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if matches!(key.trim(), "account_id" | "store_id") {
                secret.push(value.trim().trim_matches('"').to_string());
            }
        }
    }
    // The zone and its hostnames are vars too, and are public by design: the drawing is of them.
    let zone = shown.resources.zone.clone().unwrap();
    for value in secret
        .iter()
        .filter(|v| !v.is_empty() && **v != zone && !v.ends_with(&format!(".{zone}")))
    {
        assert!(
            !shown_text.contains(value.as_str()),
            "{value} is not public"
        );
    }
    for (at, _) in shown_text.match_indices('@') {
        let before = shown_text[..at].chars().next_back().unwrap_or(' ');
        let after: String = shown_text[at + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '-')
            .collect();
        assert!(
            !(before.is_alphanumeric() && after.contains('.')),
            "an email shape near {after}"
        );
    }
    let hosts: Vec<&str> = shown.hostnames.iter().map(|h| h.host.as_str()).collect();
    assert!(hosts.contains(&"fake.pigeon.dev"), "{hosts:?}");
    assert!(!hosts.contains(&"elsewhere.example.net"), "off the zone");
    assert!(shown.hostnames.iter().any(|h| h.behind_access));
    assert!(text.contains("secret-detail") && text.contains("fake-token"));
    assert!(shown.resources.repos.is_empty() && shown.resources.mirrors.is_empty());

    // Without the map, the hosting Worker is still drawn, alone.
    let alone: Vec<Source> = sources
        .iter()
        .filter(|s| s.path != "ci/workers.toml")
        .map(|s| Source {
            path: s.path,
            text: s.text,
        })
        .collect();
    let fallback = serde_json::to_value(describe(&alone, &hosted)).unwrap();
    assert_eq!(fallback["workers"].as_array().unwrap().len(), 1);

    // The memo is kept while the capabilities stay, and rebuilt when they change.
    let first = built(&hosted);
    assert!(std::ptr::eq(first, built(&hosted)), "kept while unchanged");
    let fewer: Vec<Box<dyn Capability>> = vec![Box::new(Architecture)];
    let smaller = Hosted {
        worker: "pigeon-platform",
        capabilities: &fewer,
        sources: &paths,
    };
    let second = built(&smaller);
    assert!(
        !std::ptr::eq(first, second),
        "rebuilt for other capabilities"
    );
    assert!(first.page.contains("ci_status") || first.model.to_string().contains("ci_status"));
    assert!(!second.model.to_string().contains("ci_status"));
}

/// Observability's only console sections are the Cost area's views: `cost_daily`'s ranges. The
/// console's routes and redirects are checked in `console_decommissioned` (console.rs).
#[test]
fn core_console_cost_views() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::observability::Observability;
    use pigeon_core::lanes::Map;

    let o = Observability {
        map: Map {
            docs: vec![],
            shared: vec![],
            checks: vec![],
            core_first: None,
            workers: vec![],
        },
    };
    let s = o.sections();
    assert!(s.iter().all(|s| s.area == "cost"));
    let views: Vec<&str> = s.iter().map(|s| s.id).collect();
    assert_eq!(views, ["7d", "today", "all"]);
}

/// 2026-11-24T06:00:00Z in milliseconds since the epoch.
const TOKEN_NOW_MS: i64 = 1_795_500_000_000;

/// Runs a future that never waits to its end, with no runtime.
fn token_block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// A token list with a cap, a page size, failing deletes and a record of deletes.
struct MockTokens {
    tokens: std::cell::RefCell<Vec<pigeon_core::tokens::Listed>>,
    cap: usize,
    page_size: usize,
    fail_delete: Vec<&'static str>,
    deleted: std::cell::RefCell<Vec<String>>,
    next: std::cell::Cell<u32>,
    attempts: std::cell::Cell<u32>,
    /// While set, listing the tokens fails.
    fail_list: std::cell::Cell<bool>,
    lose_create_response: std::cell::Cell<bool>,
    suspend_create: std::cell::Cell<bool>,
}

impl MockTokens {
    fn new(cap: usize, page_size: usize) -> Self {
        Self {
            tokens: Default::default(),
            cap,
            page_size,
            fail_delete: Vec::new(),
            deleted: Default::default(),
            next: std::cell::Cell::new(100),
            attempts: std::cell::Cell::new(0),
            fail_list: std::cell::Cell::new(false),
            lose_create_response: std::cell::Cell::new(false),
            suspend_create: std::cell::Cell::new(false),
        }
    }

    fn add(&self, id: &str, name: &str, expires_on: Option<&str>, status: &str) {
        self.tokens.borrow_mut().push(pigeon_core::tokens::Listed {
            id: id.into(),
            name: name.into(),
            expires_on: expires_on.map(Into::into),
            status: status.into(),
        });
    }
}

impl pigeon_core::tokens::TokenApi for MockTokens {
    async fn list(&self, page: u32) -> worker::Result<Vec<pigeon_core::tokens::Listed>> {
        if self.fail_list.get() {
            return Err(worker::Error::RustError("list refused".into()));
        }
        let from = (page as usize - 1) * self.page_size;
        Ok(self
            .tokens
            .borrow()
            .iter()
            .skip(from)
            .take(self.page_size)
            .cloned()
            .collect())
    }

    async fn create(&self, body: &Value) -> worker::Result<pigeon_core::tokens::Minted> {
        self.attempts.set(self.attempts.get() + 1);
        if self.tokens.borrow().len() >= self.cap {
            return Err(worker::Error::RustError(
                r#"/accounts/acct/tokens: 400: [{"code":1001,"message":"Quota limit exceeded. You can create no more than 500 tokens"}]"#
                    .into(),
            ));
        }
        let n = self.next.get();
        self.next.set(n + 1);
        let id = format!("t{n}");
        let expires_on = body["expires_on"].as_str().unwrap_or_default();
        self.add(
            &id,
            body["name"].as_str().unwrap_or_default(),
            Some(expires_on),
            "active",
        );
        if self.suspend_create.get() {
            std::future::pending::<()>().await;
        }
        if self.lose_create_response.get() {
            return Err(worker::Error::RustError("create response timed out".into()));
        }
        Ok(pigeon_core::tokens::Minted {
            id,
            value: "v".into(),
            expires_on: expires_on.into(),
        })
    }

    async fn delete(&self, id: &str) -> worker::Result<()> {
        if self.fail_delete.contains(&id) {
            return Err(worker::Error::RustError("delete refused".into()));
        }
        self.tokens.borrow_mut().retain(|t| t.id != id);
        self.deleted.borrow_mut().push(id.into());
        Ok(())
    }
}

#[test]
fn core_token_minted_names() {
    use pigeon_core::tokens::{minted, name};
    let t = "2026-11-24T05:04:00Z";
    for (n, purpose) in [
        (format!("pigeon:worker-workers_ai:{t}"), "worker-workers_ai"),
        (format!("pigeon:upload worker:{t}"), "upload worker"),
        (format!("pigeon:artifacts:{t}"), "artifacts"),
    ] {
        assert_eq!(minted(&n), Some(purpose), "{n}");
    }
    assert_eq!(name("artifacts", t), format!("pigeon:artifacts:{t}"));
    assert_eq!(minted(&name("a b", t)), Some("a b"));
    for n in [
        "",
        "pigeon:",
        "pigeon:x",
        "pigeon:x:notatime",
        "my-own-token",
        "pigeon::2026-11-24T05:04:00Z",
        "pigeon:a:b:2026-11-24T05:04:00Z",
        "pigeon:x:2026-11-24T05:04:00Z-suffix",
        "pigeon:x:2026-11-24T05:04:00.000Z",
        "pigeon:x:2026-11-24 05:04:00Z",
        "pigeon:x:2026-11-24T05:04:00",
        "x:pigeon:x:2026-11-24T05:04:00Z",
        "pigeon:é:2026-11-24T05:04:00Z",
    ] {
        // The multi-byte purpose is a valid name; every other entry is not.
        if n.contains('é') {
            assert_eq!(minted(n), Some("é"));
        } else {
            assert_eq!(minted(n), None, "{n}");
        }
    }
}

#[test]
fn core_token_sweep_plan_and_counts() {
    use pigeon_core::tokens::{sweep, sweep_plan};
    let mock = MockTokens::new(500, 50);
    mock.add(
        "t1",
        "pigeon:a:2026-11-24T04:00:00Z",
        Some("2026-11-24T05:00:00Z"),
        "active",
    );
    mock.add(
        "t2",
        "pigeon:a:2026-11-24T05:50:00Z",
        Some("2026-11-24T06:05:00Z"),
        "active",
    );
    mock.add("t3", "pigeon:a:2026-11-24T04:00:00Z", None, "active");
    mock.add(
        "t4",
        "my-own-token",
        Some("2020-01-01T00:00:00Z"),
        "expired",
    );
    mock.add("t5", "pigeon:b:2026-11-24T04:00:00Z", None, "expired");
    mock.add(
        "t6",
        "pigeon:b:2026-11-24T04:00:00Z",
        Some("2026-11-24T05:59:59.500Z"),
        "active",
    );
    assert_eq!(
        sweep_plan(&mock.tokens.borrow(), TOKEN_NOW_MS),
        ["t1", "t5", "t6"]
    );
    let report = token_block_on(sweep(&mock, TOKEN_NOW_MS)).unwrap();
    assert_eq!(report.deleted, 3);
    assert_eq!(report.kept_pigeon, 2);
    assert_eq!(report.skipped_other, 1);
    assert!(report.failed.is_empty());
    assert_eq!(*mock.deleted.borrow(), ["t1", "t5", "t6"]);
}

#[test]
fn core_token_sweep_goes_on_after_a_failed_delete() {
    let mut mock = MockTokens::new(500, 50);
    mock.fail_delete = vec!["t1"];
    for id in ["t1", "t2", "t3"] {
        mock.add(
            id,
            "pigeon:a:2026-11-24T04:00:00Z",
            Some("2026-11-24T05:00:00Z"),
            "active",
        );
    }
    let report = token_block_on(pigeon_core::tokens::sweep(&mock, TOKEN_NOW_MS)).unwrap();
    assert_eq!(report.deleted, 2);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, "t1");
    assert_eq!(*mock.deleted.borrow(), ["t2", "t3"]);
}

#[test]
fn core_token_sweep_pages_through_the_list() {
    let mock = MockTokens::new(500, 2);
    for i in 0..7 {
        let name = if i % 2 == 0 {
            "pigeon:a:2026-11-24T04:00:00Z"
        } else {
            "other"
        };
        mock.add(
            &format!("t{i}"),
            name,
            Some("2026-11-24T05:00:00Z"),
            "active",
        );
    }
    let report = token_block_on(pigeon_core::tokens::sweep(&mock, TOKEN_NOW_MS)).unwrap();
    assert_eq!((report.deleted, report.skipped_other), (4, 3));
}

#[test]
fn core_token_sweep_frees_the_quota() {
    use pigeon_core::tokens::{TokenApi, name, sweep};
    let mock = MockTokens::new(3, 2);
    for id in ["t1", "t2", "t3"] {
        mock.add(
            id,
            "pigeon:a:2026-11-24T04:00:00Z",
            Some("2026-11-24T05:00:00Z"),
            "active",
        );
    }
    let body = json!({
        "name": name("a", "2026-11-24T06:00:00Z"),
        "expires_on": "2026-11-24T07:00:00Z",
    });
    let err = token_block_on(mock.create(&body))
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("1001") && err.contains("Quota limit exceeded"),
        "{err}"
    );
    let report = token_block_on(sweep(&mock, TOKEN_NOW_MS)).unwrap();
    assert_eq!(report.deleted, 3);
    let made = token_block_on(mock.create(&body)).unwrap();
    assert_eq!(made.expires_on, "2026-11-24T07:00:00Z");
    // The new token is live, so a second sweep keeps it.
    let report = token_block_on(sweep(&mock, TOKEN_NOW_MS)).unwrap();
    assert_eq!((report.deleted, report.kept_pigeon), (0, 1));
}

/// Polls two futures together, as a Durable Object's interleaved requests would run, until both
/// are ready.
fn token_join2<A: std::future::Future, B: std::future::Future>(
    a: A,
    b: B,
) -> (A::Output, B::Output) {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let (mut a, mut b) = (std::pin::pin!(a), std::pin::pin!(b));
    let (mut ra, mut rb) = (None, None);
    for _ in 0..1000 {
        if ra.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(&mut cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(&mut cx)
        {
            rb = Some(v);
        }
        if ra.is_some() && rb.is_some() {
            return (ra.take().unwrap(), rb.take().unwrap());
        }
    }
    panic!("the futures did not finish");
}

/// The body a mint would send: a name and an expiry 15 minutes on.
fn token_body(expires_on: &str) -> worker::Result<Value> {
    Ok(json!({
        "name": pigeon_core::tokens::name("worker-ai_gateway", "2026-11-24T06:00:00Z"),
        "expires_on": expires_on,
    }))
}

const TOKEN_EXPIRY: &str = "2026-11-24T06:15:00Z";

/// A lease key and caller hash for user `user` acting for nobody, with `permissions`.
fn token_keys(user: &str, permissions: &[&str]) -> (String, String) {
    use pigeon_capabilities::ledger::{caller_hash, lease_key};
    let caller = caller_hash(user, None);
    (lease_key(&caller, permissions), caller)
}

fn token_lease(
    ledger: &pigeon_capabilities::ledger::Ledger<impl pigeon_capabilities::ledger::Store>,
    api: &MockTokens,
    keys: &(String, String),
    now_ms: i64,
) -> worker::Result<pigeon_capabilities::ledger::Lease> {
    token_block_on(ledger.lease(
        api,
        &pigeon_capabilities::ledger::LeaseRequest {
            key: &keys.0,
            caller: &keys.1,
            purpose: "worker-ai_gateway",
            ttl_secs: 900,
        },
        now_ms,
        async || token_body(TOKEN_EXPIRY),
    ))
}

fn token_mints(api: &MockTokens) -> u32 {
    api.next.get() - 100
}

#[test]
fn core_token_ledger_two_leases_mint_once() {
    use pigeon_capabilities::ledger::{Ledger, lease_key};
    let (api, ledger) = (MockTokens::new(500, 50), Ledger::new((), Vec::new()));
    let keys = token_keys("u1", &["A", "B"]);
    let a = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).unwrap();
    let b = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS + 60_000).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.expires_at, TOKEN_NOW_MS + 900_000);
    assert_eq!(token_mints(&api), 1);
    // The order and repeats of the permissions do not change the key.
    assert_eq!(
        lease_key("c", &["B", "A", "A"]),
        lease_key("c", &["A", "B"])
    );
    assert_eq!(ledger.rows().len(), 1);
}

#[test]
fn core_token_ledger_other_caller_or_set_mints_apart() {
    use pigeon_capabilities::ledger::{Ledger, caller_hash, lease_key};
    let (api, ledger) = (MockTokens::new(500, 50), Ledger::new((), Vec::new()));
    let one = token_keys("u1", &["A"]);
    let other_caller = token_keys("u2", &["A"]);
    let other_set = token_keys("u1", &["A", "B"]);
    let a = token_lease(&ledger, &api, &one, TOKEN_NOW_MS).unwrap();
    let b = token_lease(&ledger, &api, &other_caller, TOKEN_NOW_MS).unwrap();
    let c = token_lease(&ledger, &api, &other_set, TOKEN_NOW_MS).unwrap();
    assert_eq!(token_mints(&api), 3);
    assert!(a.id != b.id && a.id != c.id && b.id != c.id);
    // Acting for a person is part of the caller.
    assert!(caller_hash("u1", Some("p")) != caller_hash("u1", None));
    assert!(lease_key(&caller_hash("u1", Some("p")), &["A"]) != one.0);
}

#[test]
fn core_token_ledger_a_nearly_expired_lease_mints_anew() {
    use pigeon_capabilities::ledger::Ledger;
    let (api, ledger) = (MockTokens::new(500, 50), Ledger::new((), Vec::new()));
    let keys = token_keys("u1", &["A"]);
    let first = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).unwrap();
    // 300 s left is still handed out; 299 s is not.
    let still = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS + 600_000).unwrap();
    assert_eq!(still.id, first.id);
    let next = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS + 601_000).unwrap();
    assert!(next.id != first.id);
    assert_eq!(token_mints(&api), 2);
    // The replaced token is left to expire: a taker may still be using it.
    assert!(api.deleted.borrow().is_empty());
    assert!(ledger.holds(&next.id) && ledger.holds(&first.id));
}

#[test]
fn core_token_ledger_concurrent_leases_mint_once() {
    use pigeon_capabilities::ledger::{LeaseRequest, Ledger};
    let (api, ledger) = (MockTokens::new(500, 50), Ledger::new((), Vec::new()));
    let keys = token_keys("u1", &["A"]);
    let ask = async || {
        ledger
            .lease(
                &api,
                &LeaseRequest {
                    key: &keys.0,
                    caller: &keys.1,
                    purpose: "worker-ai_gateway",
                    ttl_secs: 900,
                },
                TOKEN_NOW_MS,
                async || {
                    // Wait once, as a network call does, so the other lease runs meanwhile.
                    let mut yielded = false;
                    std::future::poll_fn(|cx| {
                        if yielded {
                            return std::task::Poll::Ready(());
                        }
                        yielded = true;
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    })
                    .await;
                    token_body(TOKEN_EXPIRY)
                },
            )
            .await
    };
    let (a, b) = token_join2(ask(), ask());
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(token_mints(&api), 1);
}

#[test]
fn core_token_ledger_an_orphan_row_is_revoked_and_replaced() {
    use pigeon_capabilities::ledger::{Ledger, Row};
    let api = MockTokens::new(500, 50);
    let keys = token_keys("u1", &["A"]);
    let name = "pigeon:worker-ai_gateway:2026-11-24T05:50:00Z";
    api.add("t50", name, Some(TOKEN_EXPIRY), "active");
    // The object restarted: a row, and no value in memory.
    let orphan = Row {
        id: "t50".into(),
        key: keys.0.clone(),
        name: name.into(),
        purpose: "worker-ai_gateway".into(),
        expires_at: TOKEN_NOW_MS + 600_000,
        caller: keys.1.clone(),
        released: false,
        created_at: None,
    };
    let ledger = Ledger::new((), vec![orphan]);
    let got = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).unwrap();
    assert_eq!(got.id, "t100");
    assert_eq!(*api.deleted.borrow(), ["t50"]);
    assert_eq!(ledger.rows().len(), 1);
    assert_eq!(ledger.rows()[0].id, "t100");
}

#[test]
fn core_token_ledger_a_quota_error_sweeps_once_and_retries_once() {
    use pigeon_capabilities::ledger::Ledger;
    // Full of expired pigeon tokens: the sweep frees room and the retry works.
    let api = MockTokens::new(2, 50);
    let mut rows = Vec::new();
    for id in ["t1", "t2"] {
        let mut row = gauge_row(id, TOKEN_NOW_MS - 1, false, None);
        row.name = "pigeon:a:2026-11-24T04:00:00Z".into();
        rows.push(row);
        api.add(
            id,
            "pigeon:a:2026-11-24T04:00:00Z",
            Some("2026-11-24T05:00:00Z"),
            "active",
        );
    }
    let ledger = Ledger::new((), rows);
    let keys = token_keys("u1", &["A"]);
    let got = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).unwrap();
    assert_eq!(got.id, "t100");
    assert_eq!(api.attempts.get(), 2);
    assert_eq!(*api.deleted.borrow(), ["t1", "t2"]);

    // Full of live tokens: one retry, then the error comes back unchanged in meaning.
    let (api, ledger) = (MockTokens::new(1, 50), Ledger::new((), Vec::new()));
    api.add(
        "t1",
        "pigeon:a:2026-11-24T05:59:00Z",
        Some(TOKEN_EXPIRY),
        "active",
    );
    let err = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS)
        .unwrap_err()
        .to_string();
    assert!(err.contains(pigeon_core::tokens::CAPACITY_ERROR), "{err}");
    assert_eq!(api.attempts.get(), 2);
    assert!(ledger.rows().is_empty());
}

#[test]
fn core_token_ledger_capacity_counts_external_tokens_and_sweeps_only_owned_rows() {
    use pigeon_capabilities::ledger::Ledger;
    let api = MockTokens::new(500, 3);
    for i in 0..9 {
        api.add(&format!("external{i}"), "outside", None, "active");
    }
    api.add(
        "lookalike",
        "pigeon:a:2026-11-24T04:00:00Z",
        Some("2026-11-24T05:00:00Z"),
        "expired",
    );
    let ledger = Ledger::new((), Vec::new());
    ledger.ceiling_set(10).unwrap();
    let error = token_lease(&ledger, &api, &token_keys("u1", &["A"]), TOKEN_NOW_MS).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(pigeon_core::tokens::CAPACITY_ERROR)
    );
    assert_eq!(api.attempts.get(), 0);
    assert!(api.deleted.borrow().is_empty());
    let gauge = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((gauge.live, gauge.account_total), (0, Some(10)));
    assert_eq!(gauge.last_sweep_succeeded_at_ms, Some(TOKEN_NOW_MS));
}

#[test]
fn core_token_ledger_charges_each_new_mint_against_the_cached_total() {
    use pigeon_capabilities::ledger::Ledger;
    let api = MockTokens::new(500, 50);
    for i in 0..9 {
        api.add(&format!("external{i}"), "outside", None, "active");
    }
    let ledger = Ledger::new((), Vec::new());
    ledger.ceiling_set(10).unwrap();
    let keys = token_keys("u1", &["A"]);
    let first = token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).unwrap();
    let gauge = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((gauge.live, gauge.account_total), (1, Some(9)));
    // A cache hit for an existing lease does not consume an additional slot.
    assert_eq!(
        token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).unwrap(),
        first
    );
    let other = token_keys("u2", &["A"]);
    assert!(token_lease(&ledger, &api, &other, TOKEN_NOW_MS).is_err());
    assert_eq!(api.attempts.get(), 1);

    api.fail_list.set(true);
    let later = TOKEN_NOW_MS + 16_000;
    assert!(token_lease(&ledger, &api, &other, later).is_err());
    assert_eq!(api.attempts.get(), 1);
    let gauge = token_block_on(ledger.gauge(&api, later)).unwrap();
    assert_eq!(gauge.account_total, None);
}

#[test]
fn core_token_ledger_recovers_an_ambiguous_create_across_restart() {
    use pigeon_capabilities::ledger::Ledger;
    let api = MockTokens::new(500, 50);
    let ledger = Ledger::new((), Vec::new());
    let keys = token_keys("u1", &["A"]);

    api.lose_create_response.set(true);
    assert!(token_lease(&ledger, &api, &keys, TOKEN_NOW_MS).is_err());
    assert_eq!(ledger.rows().len(), 1);
    assert_eq!(api.tokens.borrow().len(), 1);
    api.lose_create_response.set(false);

    let restarted = Ledger::new((), ledger.rows());
    let gauge = token_block_on(restarted.gauge(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((gauge.live, gauge.account_total), (1, Some(1)));
    let reaped = token_block_on(restarted.reap(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((reaped.deleted, reaped.forgotten), (1, 1));
    assert!(restarted.rows().is_empty());
    assert!(api.tokens.borrow().is_empty());

    // Cancellation or eviction after POST behaves the same way, even without a response error.
    api.suspend_create.set(true);
    {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};
        let request = pigeon_capabilities::ledger::LeaseRequest {
            key: &keys.0,
            caller: &keys.1,
            purpose: "worker-ai_gateway",
            ttl_secs: 900,
        };
        let mut mint =
            std::pin::pin!(
                restarted.lease(&api, &request, TOKEN_NOW_MS, async || token_body(
                    TOKEN_EXPIRY
                ))
            );
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(mint.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(restarted.rows().len(), 1);
        assert_eq!(api.tokens.borrow().len(), 1);
    }
    let reaped = token_block_on(restarted.reap(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!(reaped.deleted, 1);
    assert!(api.tokens.borrow().is_empty());
}

#[test]
fn core_token_ledger_failed_storage_commit_attempts_cleanup() {
    use pigeon_capabilities::ledger::{Ledger, Row, Store};
    struct RejectMinted;
    impl Store for RejectMinted {
        fn save(&self, row: &Row) -> worker::Result<()> {
            if row.id.starts_with("pending:") {
                Ok(())
            } else {
                Err(worker::Error::RustError("store unavailable".into()))
            }
        }
        fn remove(&self, _id: &str) -> worker::Result<()> {
            Ok(())
        }
    }
    let api = MockTokens::new(500, 50);
    let ledger = Ledger::new(RejectMinted, Vec::new());
    assert!(token_lease(&ledger, &api, &token_keys("u1", &["A"]), TOKEN_NOW_MS).is_err());
    assert_eq!(*api.deleted.borrow(), ["t100"]);
    assert!(ledger.rows().is_empty());
}

#[test]
fn core_token_transient_cleanup_retries_failed_revocation_and_checks_sweep_time() {
    use pigeon_capabilities::ledger::Ledger;
    let mut api = MockTokens::new(500, 50);
    let ledger = Ledger::new((), Vec::new());
    let token = token_block_on(ledger.mint_transient(
        &api,
        "artifacts",
        900,
        TOKEN_NOW_MS,
        token_body(TOKEN_EXPIRY).unwrap(),
    ))
    .unwrap();
    let later = TOKEN_NOW_MS + 1_000;
    // A transient call can still be active after the ledger restarts.
    let ledger = Ledger::new((), ledger.rows());
    assert_eq!(token_block_on(ledger.reap(&api, later)).unwrap().deleted, 0);
    api.fail_delete = vec!["t100"];
    assert!(token_block_on(ledger.revoke_transient(&api, &token.id)).is_err());
    assert!(ledger.rows()[0].released);
    let after_failure = later + 1_000;
    assert_eq!(
        token_block_on(ledger.reap(&api, after_failure))
            .unwrap()
            .failed,
        1
    );
    let gauge = token_block_on(ledger.gauge(&api, after_failure)).unwrap();
    assert_eq!(gauge.last_sweep_succeeded_at_ms, Some(later));
    assert_eq!((gauge.live, gauge.account_total), (1, Some(1)));
    api.fail_delete.clear();
    assert_eq!(
        token_block_on(ledger.reap(&api, after_failure))
            .unwrap()
            .deleted,
        1
    );
    assert!(ledger.rows().is_empty());
    let gauge = token_block_on(ledger.gauge(&api, after_failure)).unwrap();
    assert_eq!(gauge.last_sweep_succeeded_at_ms, Some(after_failure));
    assert_eq!((gauge.live, gauge.account_total), (0, Some(0)));
}

#[test]
fn core_token_ledger_release_revokes_one_callers_tokens_only() {
    use pigeon_capabilities::ledger::Ledger;
    let (mut api, ledger) = (MockTokens::new(500, 50), Ledger::new((), Vec::new()));
    let (a1, a2) = (token_keys("u1", &["A"]), token_keys("u1", &["B"]));
    let b1 = token_keys("u2", &["A"]);
    let t1 = token_lease(&ledger, &api, &a1, TOKEN_NOW_MS).unwrap();
    let t2 = token_lease(&ledger, &api, &a2, TOKEN_NOW_MS).unwrap();
    let t3 = token_lease(&ledger, &api, &b1, TOKEN_NOW_MS).unwrap();
    // A leased token is held: a taker's revoke of it answers `kept`.
    assert!(ledger.holds(&t1.id) && ledger.holds(&t3.id));
    assert!(!ledger.holds("t999"));
    let n = token_block_on(ledger.release(&api, &a1.1)).unwrap();
    assert_eq!(n, 2);
    assert_eq!(*api.deleted.borrow(), [t1.id.clone(), t2.id.clone()]);
    assert!(ledger.holds(&t3.id) && !ledger.holds(&t1.id));
    assert_eq!(ledger.rows().len(), 1);
    // A new lease for the released caller mints again.
    let again = token_lease(&ledger, &api, &a1, TOKEN_NOW_MS).unwrap();
    assert_eq!(again.id, "t103");

    // A revoke that fails leaves the row released and unexpired, for the sweeper.
    api.fail_delete = vec!["t103"];
    let n = token_block_on(ledger.release(&api, &a1.1)).unwrap();
    assert_eq!(n, 1);
    let stuck = ledger
        .rows()
        .into_iter()
        .find(|r| r.id == "t103")
        .expect("the row stays");
    assert!(stuck.released && stuck.expires_at > TOKEN_NOW_MS);
    assert!(!ledger.holds("t103"));
    // The sweeper retries it once the revoke works, and forgets the row.
    api.fail_delete.clear();
    let reaped = token_block_on(ledger.reap(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((reaped.deleted, reaped.forgotten, reaped.failed), (1, 1, 0));
    assert!(ledger.rows().iter().all(|r| r.id != "t103"));
}

#[test]
fn core_token_ledger_reap_keeps_other_names_and_forgets_the_expired() {
    use pigeon_capabilities::ledger::{Ledger, Row};
    let api = MockTokens::new(500, 50);
    let row = |id: &str, name: &str, expires_at: i64, released: bool| Row {
        id: id.into(),
        key: format!("k{id}"),
        name: name.into(),
        purpose: "p".into(),
        expires_at,
        caller: "c".into(),
        released,
        created_at: None,
    };
    let mine = "pigeon:a:2026-11-24T05:00:00Z";
    let ledger = Ledger::new(
        (),
        vec![
            row("t1", mine, TOKEN_NOW_MS + 1000, true),
            row("t2", "my-own-token", TOKEN_NOW_MS + 1000, true),
            row("t3", mine, TOKEN_NOW_MS - 1, false),
            row("t4", mine, TOKEN_NOW_MS + 1000, false),
        ],
    );
    let r = token_block_on(ledger.reap(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((r.deleted, r.forgotten, r.failed), (3, 4, 0));
    // Released, expired, and restarted shared leases are reclaimed. Other names are untouched.
    assert_eq!(*api.deleted.borrow(), ["t1", "t3", "t4"]);
    assert!(ledger.rows().is_empty());
}

/// A ledger row for the gauge scenarios: live until `expires_at`, minted at `created_at`.
fn gauge_row(
    id: &str,
    expires_at: i64,
    released: bool,
    created_at: Option<i64>,
) -> pigeon_capabilities::ledger::Row {
    pigeon_capabilities::ledger::Row {
        id: id.into(),
        key: format!("k{id}"),
        name: "pigeon:a:2026-11-24T05:50:00Z".into(),
        purpose: "p".into(),
        expires_at,
        caller: "c".into(),
        released,
        created_at,
    }
}

#[test]
fn core_token_gauge_arithmetic() {
    use pigeon_capabilities::ledger::{SweepGauge, live_now};
    let last = SweepGauge {
        count: 100,
        at_ms: TOKEN_NOW_MS,
    };
    let later = TOKEN_NOW_MS + 30_000;
    let rows = vec![
        // Minted after the list: counted on top of it.
        gauge_row("a", later + 600_000, false, Some(TOKEN_NOW_MS + 10_000)),
        gauge_row("b", later + 600_000, false, Some(TOKEN_NOW_MS + 20_000)),
        // Minted before the list (or before created_at existed): already in the count.
        gauge_row("c", later + 600_000, false, Some(TOKEN_NOW_MS - 1)),
        gauge_row("d", later + 600_000, false, None),
    ];
    assert_eq!(live_now(Some(&last), &rows, later), 102);
    // Never less than the ledger's live rows: a count of 1 with four live rows reads 4.
    let low = SweepGauge {
        count: 1,
        at_ms: TOKEN_NOW_MS + 60_000,
    };
    assert_eq!(live_now(Some(&low), &rows, later), 4);
    // Released rows still occupy capacity until deletion succeeds.
    let rows = vec![
        gauge_row("e", later + 1000, true, None),
        gauge_row("f", later - 1, false, None),
        gauge_row("g", later + 1000, false, None),
    ];
    assert_eq!(live_now(None, &rows, later), 2);
    assert_eq!(live_now(Some(&low), &rows, later), 2);
}

#[test]
fn core_token_gauge_count_live_counts_only_unexpired_minted_names() {
    let mock = MockTokens::new(500, 2);
    let live = Some("2026-11-24T06:30:00Z");
    let past = Some("2026-11-24T05:00:00Z");
    mock.add("t1", "pigeon:a:2026-11-24T05:00:00Z", live, "active");
    mock.add("t2", "pigeon:b:2026-11-24T05:00:00Z", live, "active");
    mock.add("t3", "pigeon:c:2026-11-24T05:00:00Z", past, "active");
    mock.add("t4", "pigeon:d:2026-11-24T05:00:00Z", live, "expired");
    mock.add("t5", "my-own-token", live, "active");
    mock.add("t6", "pigeon:notatime", live, "active");
    // No expiry: counted as live, like the sweep keeps it.
    mock.add("t7", "pigeon:e:2026-11-24T05:00:00Z", None, "active");
    let n = token_block_on(pigeon_core::tokens::count_live(&mock, TOKEN_NOW_MS)).unwrap();
    assert_eq!(n, 3);
    // Read-only.
    assert!(mock.deleted.borrow().is_empty());
    assert_eq!(mock.tokens.borrow().len(), 7);
}

#[test]
fn core_token_gauge_ceiling_bounds() {
    use pigeon_capabilities::ledger::{DEFAULT_SOFT_CEILING, Ledger, check_ceiling};
    assert_eq!(DEFAULT_SOFT_CEILING, 300);
    assert_eq!(check_ceiling(10), Ok(10));
    assert_eq!(check_ceiling(450), Ok(450));
    for bad in [9, 451, 0, -5, 500] {
        let why = check_ceiling(bad).unwrap_err();
        assert!(
            why.contains("from 10 to 450") && why.contains("hard cap is 500"),
            "{why}"
        );
    }
    let (api, ledger) = (MockTokens::new(500, 50), Ledger::new((), Vec::new()));
    assert!(ledger.ceiling_set(451).is_err());
    assert_eq!(ledger.ceiling_set(120).unwrap(), 120);
    let g = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((g.soft_ceiling, g.hard_cap), (120, 500));
}

#[test]
fn core_token_gauge_stale_refresh_and_fallback() {
    use pigeon_capabilities::ledger::Ledger;
    let api = MockTokens::new(500, 50);
    for i in 0..5 {
        api.add(
            &format!("t{i}"),
            "pigeon:a:2026-11-24T05:50:00Z",
            Some("2026-11-24T06:30:00Z"),
            "active",
        );
    }
    let ledger = Ledger::new((), Vec::new());
    // A fresh stored gauge is used as it is, with no list call.
    ledger.gauge_set(2, TOKEN_NOW_MS).unwrap();
    api.fail_list.set(true);
    let g = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS + 60_000)).unwrap();
    assert_eq!((g.live, g.source.as_str()), (2, "sweep"));
    // Older than 120 s: refreshed from the list.
    api.fail_list.set(false);
    let g = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS + 121_000)).unwrap();
    assert_eq!((g.live, g.source.as_str()), (5, "refreshed"));
    assert_eq!(g.as_of_ms, TOKEN_NOW_MS + 121_000);
    // Stale again and the list fails: the stored count, and it says so.
    api.fail_list.set(true);
    let g = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS + 400_000)).unwrap();
    assert_eq!((g.live, g.source.as_str()), (5, "stored (refresh failed)"));
    // Nothing stored and the list fails: the ledger's rows alone.
    let fresh = Ledger::new((), vec![gauge_row("x", TOKEN_NOW_MS + 5000, false, None)]);
    let g = token_block_on(fresh.gauge(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!(
        (g.live, g.source.as_str()),
        (1, "ledger rows (refresh failed)")
    );
}

#[test]
fn core_token_gauge_full_account_queues_then_sweep_frees_it() {
    use pigeon_ai_agents::Role::Executor;
    use pigeon_ai_agents::object::{Limits, queue_reason};
    use pigeon_capabilities::ledger::Ledger;
    // 500 live tokens: 250 expire at 06:05, 250 at 06:30. The account is at its cap.
    let api = MockTokens::new(500, 50);
    for i in 0..500 {
        let expires = if i < 250 {
            "2026-11-24T06:05:00Z"
        } else {
            "2026-11-24T06:30:00Z"
        };
        api.add(
            &format!("t{i}"),
            "pigeon:a:2026-11-24T05:00:00Z",
            Some(expires),
            "active",
        );
    }
    let ledger = Ledger::new((), Vec::new());
    let g = token_block_on(ledger.gauge(&api, TOKEN_NOW_MS)).unwrap();
    assert_eq!((g.live, g.soft_ceiling, g.hard_cap), (500, 300, 500));
    // The mint is blocked before POST.
    let err = token_lease(&ledger, &api, &token_keys("u1", &["A"]), TOKEN_NOW_MS)
        .unwrap_err()
        .to_string();
    assert!(err.contains(pigeon_core::tokens::CAPACITY_ERROR), "{err}");
    // A new run queues, with the reason; a gauge that is unknown does not hold it.
    let limits = Limits::default();
    let why = queue_reason(
        Executor,
        0,
        0,
        &limits,
        false,
        true,
        Some((g.live, g.soft_ceiling)),
    );
    assert_eq!(
        why.unwrap(),
        "queued: 500 live Cloudflare tokens, at or above the soft ceiling of 300 (waits until fewer than 300)"
    );
    assert_eq!(
        queue_reason(Executor, 0, 0, &limits, false, true, None),
        None
    );
    // Ten minutes on, the sweep deletes the expired 250 and stores what it kept.
    let later = TOKEN_NOW_MS + 600_000;
    let report = token_block_on(pigeon_core::tokens::sweep(&api, later)).unwrap();
    assert_eq!((report.deleted, report.kept_pigeon), (250, 250));
    ledger.gauge_set(report.kept_pigeon as u32, later).unwrap();
    let g = token_block_on(ledger.gauge(&api, later + 30_000)).unwrap();
    assert_eq!((g.live, g.account_total), (250, Some(250)));
    assert_eq!(
        queue_reason(
            Executor,
            0,
            0,
            &limits,
            false,
            true,
            Some((g.live, g.soft_ceiling))
        ),
        None
    );
}

#[test]
fn core_token_gauge_digest_entry_and_note() {
    use pigeon_capabilities::land::digest::tokens_entry;
    let below =
        tokens_entry(&json!({ "live": 10, "soft_ceiling": 300, "hard_cap": 500, "as_of": "t" }));
    assert_eq!(below["live"], 10);
    assert!(below.get("note").is_none());
    let at =
        tokens_entry(&json!({ "live": 300, "soft_ceiling": 300, "hard_cap": 500, "as_of": "t" }));
    assert!(
        at["note"]
            .as_str()
            .unwrap()
            .contains("at or above the soft ceiling of 300")
    );
}

/// Core's `approval` op for capability Workers: only kinds in the fixed table, the digest is
/// core's own, a Worker's wording is bounded, and an approval is read back only by the caller who
/// made it, as `expired` once it lapses.
#[test]
fn core_approval_op_for_capability_workers() {
    use pigeon_capabilities::split::{
        REMOTE_APPROVAL_KINDS, remote_digest, remote_kind, remote_plan, remote_view,
    };
    use pigeon_contract::{ApprovalRequest, CORE_OPS};
    use pigeon_core::approval::{self, Kind, State};

    assert!(CORE_OPS.contains(&"approval"));
    assert_eq!(REMOTE_APPROVAL_KINDS, ["cloud_change"]);
    assert!(!pigeon_capabilities::split::op_needs_bound("approval"));

    let ask = |v: Value| -> ApprovalRequest { serde_json::from_value(v).unwrap() };
    let create = ask(json!({
        "action": "create",
        "kind": "cloud_change",
        "subject": "cloud:scaleway:buckets:create",
        "payload": { "provider": "scaleway", "args": { "bucket": "b1" } },
        "lines": ["Create bucket `b1` in fr-par."],
    }));
    let planned = remote_plan(&create).unwrap();
    assert_eq!(planned.kind, Kind::Other("cloud_change".into()));
    assert_eq!(
        planned.summary,
        json!({ "plan": ["Create bucket `b1` in fr-par."] })
    );
    // The digest is core's own formula over the kind, subject and payload.
    let expected = approval::payload_digest(
        &Kind::Other("cloud_change".into()),
        "cloud:scaleway:buckets:create",
        &create.payload,
    );
    assert_eq!(planned.digest, expected);
    // `take` computes the same digest from the same plan, and another one from another plan.
    assert_eq!(remote_digest(&create).unwrap(), expected);
    let mut other = create.clone();
    other.payload["args"]["bucket"] = json!("b2");
    assert_ne!(remote_digest(&other).unwrap(), expected);

    // Kinds outside the table are refused, whatever the action: a Worker cannot make a tool call,
    // a draft, a plan of core's, or a kind nobody has heard of.
    for kind in [
        "tool_call",
        "instance_draft",
        "linked_plan",
        "observability_monitors",
        "",
    ] {
        let mut r = create.clone();
        r.kind = Some(kind.into());
        assert!(remote_kind(&r).is_err(), "{kind}");
        assert!(remote_plan(&r).is_err(), "{kind}");
        assert!(remote_digest(&r).is_err(), "{kind}");
    }
    let mut none = create.clone();
    none.kind = None;
    assert!(remote_plan(&none).is_err());

    // Bounded wording and payload.
    let broken = |edit: &dyn Fn(&mut ApprovalRequest)| {
        let mut r = create.clone();
        edit(&mut r);
        remote_plan(&r).is_err()
    };
    assert!(broken(&|r| r.subject = None));
    assert!(broken(&|r| r.subject = Some("two\nlines".into())));
    assert!(broken(&|r| r.subject = Some("x".repeat(121))));
    assert!(broken(&|r| r.lines.clear()));
    assert!(broken(&|r| r.lines = vec!["x".into(); 21]));
    assert!(broken(&|r| r.lines = vec!["  ".into()]));
    assert!(broken(&|r| r.lines = vec!["x".repeat(401)]));
    assert!(broken(&|r| r.payload = json!("a string")));
    assert!(broken(
        &|r| r.payload = json!({ "big": "x".repeat(17 * 1024) })
    ));

    // `get`: only the caller's own approval of a table kind, and a lapsed one reads expired.
    let record = |kind: Kind, caller: &str| {
        approval::new(
            "id1".into(),
            kind,
            planned.subject.clone(),
            planned.digest.clone(),
            planned.summary.clone(),
            create.payload.clone(),
            caller.into(),
            Some("person".into()),
            1_000,
        )
    };
    let mine = record(Kind::Other("cloud_change".into()), "u1");
    let view = remote_view(&mine, "u1", 2_000).unwrap();
    assert_eq!(view["state"], "pending");
    assert_eq!(view["kind"], "cloud_change");
    assert_eq!(view["payload"], create.payload);
    assert!(
        remote_view(&mine, "u2", 2_000).is_none(),
        "another caller's id says nothing"
    );
    assert!(
        remote_view(&record(Kind::ToolCall, "u1"), "u1", 2_000).is_none(),
        "a tool call's approval is not readable through this op"
    );
    let mut approved = mine.clone();
    approved.state = State::Approved;
    assert_eq!(
        remote_view(&approved, "u1", 2_000).unwrap()["state"],
        "approved"
    );
    let later = approved.expires_ms + 1;
    assert_eq!(
        remote_view(&approved, "u1", later).unwrap()["state"],
        "expired"
    );

    // The cloud gateway has its own caller key slot, as the other capability Workers do.
    assert_eq!(
        pigeon_capabilities::split::worker_key_name("cloud-gateway"),
        Some("CALLER_KEY_CLOUD_GATEWAY")
    );
}

/// Every tool core's smoke suite can build offline: the unit capabilities, the CI, models, cloud
/// and AI agents tools their Workers declare in their manifests.
fn core_served_tools() -> Vec<pigeon_core::mcp::Tool> {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::approvals::Approvals;
    use pigeon_capabilities::architecture::Architecture;
    use pigeon_capabilities::deploys::Deploys;
    use pigeon_capabilities::dispatch::Dispatch;
    use pigeon_capabilities::github::Github;
    use pigeon_capabilities::images::Images;
    use pigeon_capabilities::policy::Policy;
    use pigeon_capabilities::versioning::Versioning;
    use pigeon_contract::Service;
    use pigeon_core::mcp::Tool;

    let caps: Vec<Box<dyn Capability>> = vec![
        Box::new(Approvals),
        Box::new(Architecture),
        Box::new(Deploys),
        Box::new(Dispatch),
        Box::new(Github),
        Box::new(Images),
        Box::new(Versioning),
        Box::new(Policy {
            session_prefix: "claude/".into(),
            main: "main".into(),
        }),
    ];
    let mut tools: Vec<Tool> = caps.iter().flat_map(|c| c.tools()).collect();
    let ci = pigeon_ci::Ci {
        settings: pigeon_ci::pigeon::settings(pigeon_ci::Accounts::Parent("P".into())),
    };
    tools.extend(ci.tools());
    let models = pigeon_models::Models {
        gateways: Vec::new(),
        providers: Vec::new(),
        staged: Vec::new(),
        secrets_page: "https://x/secrets".into(),
        host: "h".into(),
        proxy_host: "p".into(),
    };
    tools.extend(models.tools());
    // The Workers whose tools core merges from their manifests: leaked names keep `Tool`'s
    // static strings; a test process is short.
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let manifests = [
        pigeon_ai_agents::service::Agents.manifest(),
        pigeon_cloud::CloudGateway::new().manifest(),
    ];
    for m in manifests {
        for cap in m.capabilities {
            for t in cap.tools {
                tools.push(Tool {
                    name: leak(&t.name),
                    description: leak(&t.description),
                    input_schema: t.input_schema,
                });
            }
        }
    }
    tools
}

/// The lint finds nothing in any tool core serves, and the conformance report counts them.
#[test]
fn core_every_served_tool_conforms_to_the_schema_rules() {
    let tools = core_served_tools();
    assert!(tools.len() > 40, "the tools were gathered");
    let report = pigeon_core::mcp::conformance(&tools);
    assert_eq!(report["violations"], json!([]), "no tool breaks a rule");
    assert_eq!(report["violation_count"], 0);
    assert_eq!(report["tools_checked"], tools.len());
    assert!(report["tools_list_bytes"].as_u64().unwrap() > 1000);
}

/// The lint catches an untyped property, an array with no items, a `$ref`, a bad name, an object
/// that declares nothing, and a top level that is not an object.
#[test]
fn core_schema_lint_catches_a_bad_schema() {
    use pigeon_core::mcp::lint_tool;
    let rules = |name: &str, schema: Value| -> Vec<(String, &'static str)> {
        lint_tool(name, &schema)
            .into_iter()
            .map(|v| (v.path, v.rule))
            .collect()
    };
    let good = json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "anyOf": [{ "type": "boolean" }, { "type": "string" }] },
            "c": { "type": "array", "items": { "type": "object", "additionalProperties": true } }
        }
    });
    assert!(rules("good-tool_1", good).is_empty());
    let bad = json!({
        "type": "object",
        "properties": {
            "untyped": { "description": "no type" },
            "list": { "type": "array" },
            "link": { "$ref": "#/defs/x" },
            "empty": { "type": "object" },
            "mixed": { "anyOf": [{ "type": "string" }, { "description": "no type" }] }
        }
    });
    let mut found = rules("bad name", bad);
    found.sort();
    assert_eq!(
        found,
        [
            ("".to_string(), "name"),
            ("/properties/empty".to_string(), "shape"),
            ("/properties/link".to_string(), "ref"),
            ("/properties/list".to_string(), "items"),
            ("/properties/mixed/anyOf/1".to_string(), "type"),
            ("/properties/untyped".to_string(), "type"),
        ]
    );
    // The top level must be an object, and may not be an anyOf.
    assert_eq!(
        rules("t", json!({ "type": "string" })),
        [("".into(), "top-type")]
    );
    let top_any = json!({ "anyOf": [{ "type": "object", "properties": {} }] });
    assert_eq!(rules("t", top_any), [("".to_string(), "top-type")]);
    assert_eq!(
        rules(
            &"x".repeat(65),
            json!({ "type": "object", "properties": {} })
        )
        .len(),
        1
    );
}

#[test]
fn core_artifacts_token_is_minted_once_and_a_401_mints_at_most_once_more() {
    use pigeon_core::artifacts::{TokenCache, with_cached_token};
    use std::cell::Cell;
    let cache = TokenCache::default();
    let mints = Cell::new(0u32);
    let expires = TOKEN_NOW_MS + 3_600_000;
    let mint = || async {
        mints.set(mints.get() + 1);
        Ok((format!("token-{}", mints.get()), expires))
    };
    let ok = |t: String| async move { Ok(t) };
    // Two requests share one token.
    let a = token_block_on(with_cached_token(&cache, "acc", TOKEN_NOW_MS, mint, ok)).unwrap();
    let b = token_block_on(with_cached_token(
        &cache,
        "acc",
        TOKEN_NOW_MS + 60_000,
        mint,
        ok,
    ))
    .unwrap();
    assert_eq!(
        (a.as_str(), b.as_str(), mints.get()),
        ("token-1", "token-1", 1)
    );
    // A 401 on the kept token drops it and mints once more.
    let uses = Cell::new(0u32);
    let refused_once = |t: String| {
        uses.set(uses.get() + 1);
        let first = uses.get() == 1;
        async move {
            if first {
                Err(worker::Error::RustError("/x: 401: refused".into()))
            } else {
                Ok(t)
            }
        }
    };
    let c = token_block_on(with_cached_token(
        &cache,
        "acc",
        TOKEN_NOW_MS,
        mint,
        refused_once,
    ))
    .unwrap();
    assert_eq!((c.as_str(), mints.get(), uses.get()), ("token-2", 2, 2));
    // A 401 that keeps coming mints once and stops.
    let always =
        |_: String| async { Err::<String, _>(worker::Error::RustError("/x: 401: no".into())) };
    assert!(token_block_on(with_cached_token(&cache, "acc", TOKEN_NOW_MS, mint, always)).is_err());
    assert_eq!(mints.get(), 3);
    // A token with under five minutes left is replaced.
    let d = token_block_on(with_cached_token(&cache, "acc", expires - 60_000, mint, ok)).unwrap();
    assert_eq!((d.as_str(), mints.get()), ("token-4", 4));
}

#[test]
fn scoped_mcp_discovery_marks_only_ci_reads() {
    use pigeon_core::mcp::{Server, Tool, answer};

    let server = Server {
        name: "fixture",
        version: "1",
        instructions: "",
    };
    let tools = [
        Tool::bare("ci_status", "Read a CI job"),
        Tool::bare("ci_jobs", "List CI jobs"),
        Tool::bare("call", "Dispatch a tool"),
    ];
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let mut reply = token_block_on(answer(&server, &tools, &request, async |_, _| {
        unreachable!("discovery must not execute a tool")
    }))
    .unwrap();
    pigeon_capabilities::ci::annotate_scoped_discovery(&mut reply);
    let wire: serde_json::Value =
        serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap();
    let tools = &wire["result"]["tools"];
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], true);
    assert!(tools[2].get("annotations").is_none());
}

#[test]
fn global_mcp_discovery_is_not_annotated_read_only() {
    use pigeon_capabilities::ci::annotate_discovery;
    use pigeon_core::mcp::{Server, Tool, answer};

    let server = Server {
        name: "fixture",
        version: "1",
        instructions: "",
    };
    let tools = [Tool::bare("ci_status", "Read a CI job")];
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let discover = || {
        token_block_on(answer(&server, &tools, &list, async |_, _| {
            unreachable!("discovery must not execute a tool")
        }))
        .unwrap()
    };
    let annotated = |reply: &Value| -> bool {
        let wire: Value = serde_json::from_slice(&serde_json::to_vec(reply).unwrap()).unwrap();
        wire["result"]["tools"][0]["annotations"]["readOnlyHint"] == true
    };

    // The global endpoint leaves discovery as the tools declare it.
    let mut global = discover();
    annotate_discovery(false, &list, &mut global);
    assert!(!annotated(&global));

    // A scoped endpoint marks the CI reads, but only when answering tools/list.
    let mut scoped = discover();
    annotate_discovery(true, &list, &mut scoped);
    assert!(annotated(&scoped));
    let mut other = discover();
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call"});
    annotate_discovery(true, &call, &mut other);
    assert!(!annotated(&other));
}

#[test]
fn contextual_ci_callbacks_read_only_their_own_instance() {
    use pigeon_capabilities::ci::ScopeRequest;
    use pigeon_capabilities::split::verify_ci_scope;
    use pigeon_contract::{Refused, Signed, WorkerContext};

    let signed = |context: Option<WorkerContext>| Signed {
        caller: json!({"user": {"kind": "standard", "id": "owner"}}),
        iat: 0,
        exp: 1,
        nonce: String::new(),
        context,
    };
    let read = |instance: &str| ScopeRequest::Read {
        instance: instance.into(),
    };
    let attribute = ScopeRequest::Attribute {
        repo: "example/project".into(),
        job: "test".into(),
    };

    let bound = signed(Some(WorkerContext::new("pigeon-ci-gateway", Some("one"))));
    assert_eq!(verify_ci_scope(&bound, &read("one")), Ok(()));
    assert_eq!(
        verify_ci_scope(&bound, &read("two")),
        Err(Refused::InstanceMismatch)
    );
    assert_eq!(
        verify_ci_scope(&bound, &attribute),
        Err(Refused::InstanceMismatch)
    );
    // A context bound to no instance reads nothing and attributes nothing.
    let unbound = signed(Some(WorkerContext::new("pigeon-ci-gateway", None)));
    assert_eq!(
        verify_ci_scope(&unbound, &read("one")),
        Err(Refused::InstanceMismatch)
    );
    assert_eq!(
        verify_ci_scope(&unbound, &attribute),
        Err(Refused::InstanceMismatch)
    );
    // Legacy envelopes carry no context and keep their earlier behavior.
    let legacy = signed(None);
    assert_eq!(verify_ci_scope(&legacy, &read("two")), Ok(()));
    assert_eq!(verify_ci_scope(&legacy, &attribute), Ok(()));
}

/// The Secrets Store listing keeps reading pages while a page comes back full, so a store with
/// more than one page of secrets is listed whole.
#[test]
fn secrets_store_listing_reads_every_full_page() {
    use pigeon_core::secrets_store::{PER_PAGE, more_pages};
    assert!(more_pages(PER_PAGE));
    assert!(!more_pages(PER_PAGE - 1));
    assert!(!more_pages(0));
}
