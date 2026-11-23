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
    use pigeon_capabilities::console::writes::Write;
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

    // The write: approved by a passkey over its own purpose, bound to one write and one target.
    assert_eq!(Write::named("land-approve"), Some(Write::LandApprove));
    assert_eq!(Write::named("users-remove"), None);
    let (p1, p2) = (
        Write::LandApprove.purpose("L1"),
        Write::LandApprove.purpose("L2"),
    );
    assert_ne!(p1, p2);
    assert_ne!(p1, Write::DisconnectApp.purpose("L1"));
    assert!(!Write::Invite.purpose("Ada").contains("Ada"));
    let signed = b64url(p1.as_bytes());
    assert_eq!(attempt(&key, &signed, good, false, &signed), Some(1));
    assert_eq!(
        attempt(&key, &signed, good, false, &b64url(p2.as_bytes())),
        None
    );
    assert_eq!(
        Write::DisconnectApp.target(" a_B-9 "),
        Ok("a_B-9".to_string())
    );
    assert!(
        Write::RemovePasskey.target("a/b").is_err() && Write::RemovePasskey.target("").is_err()
    );
    assert_eq!(
        Write::Invite.target("  Ada Lovelace "),
        Ok("Ada Lovelace".to_string())
    );
    assert!(
        Write::Invite.target("a\nb").is_err() && Write::Invite.target(&"x".repeat(65)).is_err()
    );
    assert_eq!(Write::LandApproval.target(" on "), Ok("on".to_string()));
    assert!(
        Write::LandApproval.target("yes").is_err() && Write::LandApprove.target("L1/../x").is_err()
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
        Architecture, Source, Wrangler, built, describe, public, render,
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
    ];
    let hosted = Hosted {
        worker: "pigeon-platform",
        capabilities: &capabilities,
        sources: &paths,
    };
    let described = describe(&sources, &hosted);
    let drawing = render(&described);
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
    ] {
        assert!(has("pigeon-platform", to, binding), "core binds {binding}");
    }
    for from in [
        "pigeon-sites",
        "pigeon-model-gateway",
        "pigeon-ci-gateway",
        "pigeon-proxy",
        "pigeon-ai-agents",
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
    assert_eq!(externals, ["AI Gateway", "Buildkite"]);
    assert!(drawing.contains("Buildkite"));
    // The drawing names every link's binding and every external, in both views.
    let public_drawing = render(&public(&described));
    for view in [&drawing, &public_drawing] {
        for binding in ["SITES", "MODELS", "CI", "AI_AGENTS", "PLATFORM"] {
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
        render(&shown)
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
    assert!(first.html.contains("ci_status") || first.model.to_string().contains("ci_status"));
    assert!(!second.model.to_string().contains("ci_status"));
}

/// The Platform section sits on the Cost tab, adds nothing to the overview total, keeps a
/// ten-minute snapshot, and its script is on the page.
#[test]
fn core_console_platform_section() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::console::{page, sections::Tab, snapshot};
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
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].id, "platform");
    assert!(s[0].tab == Tab::Cost);
    assert!(s[0].cost.is_none());
    assert_eq!(snapshot::fresh_ms("platform"), Some(600_000));
    assert!(page::SCRIPTS.contains(&"/console/platform.js"));
}
