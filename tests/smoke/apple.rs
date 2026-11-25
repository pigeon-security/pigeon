//! The Apple platform: App Store Connect, signing, releases, APNs and design tokens.

use super::*;

/// App Store Connect (docs/plans/ios.md, phase 1): the ids' forms, the token's header and claims,
/// a .p8 told from other PEM, and the four slots the console shows.
#[test]
fn apple_ids_token_and_slots() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::apple::{self, Apple};
    use pigeon_core::apple as asc;

    assert!(asc::is_apple_id("A1B2C3D4E5"));
    for bad in ["a1b2c3d4e5", "A1B2C3D4E", "A1B2C3D4E56", "A1B2C3D4E-"] {
        assert!(!asc::is_apple_id(bad), "{bad}");
    }
    assert!(asc::is_issuer_id("57246542-96fe-1a63-e053-0824d011072a"));
    for bad in [
        "57246542-96fe-1a63-e053",
        "57246542x96fe-1a63-e053-0824d011072a",
        "not-a-uuid-at-all-x",
    ] {
        assert!(!asc::is_issuer_id(bad), "{bad}");
    }

    // The token: ES256 with the key id, ten minutes, App Store Connect's audience.
    let input = asc::jwt_signing_input("A1B2C3D4E5", "57246542-96fe-1a63-e053-0824d011072a", 1_000);
    let (h, c) = input.split_once('.').unwrap();
    let header: Value =
        serde_json::from_slice(&pigeon_core::webauthn::unb64url(h).unwrap()).unwrap();
    let claims: Value =
        serde_json::from_slice(&pigeon_core::webauthn::unb64url(c).unwrap()).unwrap();
    assert_eq!(
        header,
        json!({ "alg": "ES256", "kid": "A1B2C3D4E5", "typ": "JWT" })
    );
    assert_eq!(
        claims,
        json!({ "iss": "57246542-96fe-1a63-e053-0824d011072a", "iat": 1_000, "exp": 1_600, "aud": "appstoreconnect-v1" })
    );

    // Only a PKCS#8 PEM of a .p8's size is taken; a certificate, an RSA key or text is not.
    assert!(asc::p8_der("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----").is_none());
    assert!(
        asc::p8_der("-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----")
            .is_none()
    );
    assert!(asc::p8_der("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----").is_none());
    assert!(asc::p8_der("hello").is_none());

    // An error from Apple names its status and code, never more than 200 characters.
    let e = asc::api_error(
        401,
        r#"{"errors":[{"status":"401","code":"NOT_AUTHORIZED","title":"Authentication credentials are missing or invalid."}]}"#,
    );
    assert_eq!(
        e,
        "App Store Connect 401 NOT_AUTHORIZED: Authentication credentials are missing or invalid."
    );
    assert_eq!(asc::api_error(500, "<html>"), "App Store Connect 500");

    // The console's slots: the four the person fills, in order, all for Workers.
    let apple_cap = Apple;
    let slots: Vec<String> = apple_cap.secrets().into_iter().map(|s| s.name).collect();
    assert_eq!(slots, apple::SLOTS.map(String::from).to_vec());
    assert_eq!(
        slots,
        [
            "APPLE_TEAM_ID",
            "APPLE_ASC_ISSUER_ID",
            "APPLE_ASC_KEY_ID",
            "APPLE_ASC_KEY",
            "APNS_KEY_ID",
            "APNS_KEY"
        ]
    );
    let tools: Vec<&str> = apple_cap.tools().iter().map(|t| t.name).collect();
    assert_eq!(
        tools,
        [
            "ios_releases",
            "apple_status",
            "apns_register",
            "apns_unregister",
            "apns_status",
            "apns_send"
        ]
    );
    assert_eq!(apple_cap.routes(), ["/apple/", "/app/oauth"]);
    // Phase 5: the AASA names only dev.pigeon.app under the team, for passkeys and for the
    // /app/oauth callback only, and nothing for a bad team id;
    // the return page never echoes what it was called with.
    assert_eq!(
        apple::aasa_document("A1B2C3D4E5").unwrap(),
        json!({
            "applinks": { "details": [{
                "appIDs": ["A1B2C3D4E5.dev.pigeon.app"],
                "components": [{ "/": "/app/oauth*" }],
            }] },
            "webcredentials": { "apps": ["A1B2C3D4E5.dev.pigeon.app"] },
        })
    );
    assert!(apple::aasa_document("not-a-team").is_none());
    assert!(apple::aasa_document("").is_none());
    assert!(!apple::APP_RETURN_PAGE.contains('{'));
    assert_eq!(asc::BUNDLE_ID, "dev.pigeon.app");
}

/// Phase 2 (docs/plans/ios.md): the CSR's DER, laid out byte for byte; the bodies sent to Apple;
/// the console writes' passkey purposes; and an identity's record, sealed key and all, which
/// round-trips through the Directory and is never shown.
#[test]
fn apple_signing_identity_requests_and_records() {
    use pigeon_capabilities::apple::{self, Identity, Profile};
    use pigeon_core::apple::{b64, csr, unb64};

    // A stand-in public key and signature; the layout around them is what is checked.
    let spki = [0x30, 0x03, 0x02, 0x01, 0x05];
    let info = csr::request_info(&spki, apple::COMMON_NAME);
    let mut want = vec![0x30, 0x2c, 0x02, 0x01, 0x00]; // info, version 0
    want.extend([0x30, 0x20, 0x31, 0x1e, 0x30, 0x1c]); // subject: one RDN, one attribute
    want.extend([0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x15]); // commonName, UTF8String
    want.extend(b"pigeon dev.pigeon.app");
    want.extend(spki);
    want.extend([0xa0, 0x00]); // no attributes
    assert_eq!(info, want);

    let req = csr::request(&info, &[0xab; 256]);
    assert_eq!(req[..4], [0x30, 0x82, 0x01, 0x42]); // long-form length, 322 bytes
    assert_eq!(req[4..50], info[..]);
    assert_eq!(
        req[50..65],
        [
            0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b, 0x05,
            0x00
        ]
    ); // sha256WithRSAEncryption, NULL
    assert_eq!(req[65..70], [0x03, 0x82, 0x01, 0x01, 0x00]); // BIT STRING, no unused bits
    assert_eq!(req.len(), 70 + 256);
    assert_eq!(csr::tlv(0x04, &[0; 200])[..3], [0x04, 0x81, 0xc8]);

    // Apple takes standard base64 with padding; base64url is refused when reading.
    assert_eq!(b64(&[0xfb, 0xff, 0x01]), "+/8B");
    assert_eq!(b64(&[1, 2, 3, 4, 5]), "AQIDBAU=");
    assert_eq!(unb64("AQID\nBAU=").unwrap(), [1, 2, 3, 4, 5]);
    assert_eq!(unb64("+/8B").unwrap(), [0xfb, 0xff, 0x01]);
    assert!(unb64("-_8B").is_none());

    // The bodies: a DISTRIBUTION certificate from the CSR, an App Store profile for one
    // certificate and the bundle id.
    let body = apple::certificate_request(&req);
    assert_eq!(body["data"]["type"], "certificates");
    assert_eq!(
        body["data"]["attributes"]["certificateType"],
        "DISTRIBUTION"
    );
    assert_eq!(
        unb64(body["data"]["attributes"]["csrContent"].as_str().unwrap()).unwrap(),
        req
    );
    let body = apple::profile_request("pigeon dev.pigeon.app 2026-11-22", "BUNDLE1", "CERT1");
    assert_eq!(
        body,
        json!({ "data": { "type": "profiles",
            "attributes": { "name": "pigeon dev.pigeon.app 2026-11-22", "profileType": "IOS_APP_STORE" },
            "relationships": {
                "bundleId": { "data": { "type": "bundleIds", "id": "BUNDLE1" } },
                "certificates": { "data": [{ "type": "certificates", "id": "CERT1" }] },
            } } })
    );

    // Adopt adds only what is missing.
    assert_eq!(
        apple::missing(
            &apple::CAPABILITIES,
            &["PUSH_NOTIFICATIONS".into(), "IN_APP_PURCHASE".into()]
        ),
        ["ASSOCIATED_DOMAINS"]
    );
    assert!(
        apple::missing(&apple::CAPABILITIES, &apple::CAPABILITIES.map(String::from)).is_empty()
    );

    assert_eq!(apple::seal_aad("CERT1"), "apple:identity:CERT1");

    // The record: the sealed key and the bytes round-trip, the expiry is the earlier of the
    // certificate's and the profile's, and what is shown holds none of the secret parts.
    let sealed =
        pigeon_core::webauthn::b64url(&pigeon_core::crypto::join_sealed(&[7; 12], &[9; 40]));
    let (iv, ct) =
        pigeon_core::crypto::split_sealed(&pigeon_core::webauthn::unb64url(&sealed).unwrap())
            .map(|(a, b)| (a.to_vec(), b.to_vec()))
            .unwrap();
    assert_eq!((iv, ct), (vec![7; 12], vec![9; 40]));
    assert!(pigeon_core::crypto::split_sealed(&[0; 27]).is_none());
    let id = Identity {
        id: "CERT1".into(),
        serial: "ABC123".into(),
        expires: "2027-11-22T10:00:00.000+00:00".into(),
        certificate: "CERTIFICATE-BYTES".into(),
        sealed_key: sealed.clone(),
        made_by: "willow".into(),
        made_ms: 1,
        profile: Some(Profile {
            id: "PROF1".into(),
            uuid: "u-1".into(),
            name: "pigeon dev.pigeon.app 2026-11-22".into(),
            expires: "2027-11-20T10:00:00.000+00:00".into(),
            content: "PROFILE-BYTES".into(),
        }),
    };
    let back: Identity = serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();
    assert_eq!(back, id);
    assert_eq!(
        id.expires_ms(),
        pigeon_core::expiry::parse_time("2027-11-20T10:00:00Z")
    );
    let shown = id.shown().to_string();
    for secret in [sealed.as_str(), "CERTIFICATE-BYTES", "PROFILE-BYTES"] {
        assert!(!shown.contains(secret), "{secret}");
    }
    assert!(shown.contains("ABC123") && shown.contains("PROF1"));
}

/// Phase 4 (docs/plans/ios.md): a release's build number, its steps in order, the bodies that
/// upload the IPA, and the upload parts Apple asks for, checked before a byte is sent.
#[test]
fn apple_release_numbers_steps_and_upload() {
    use pigeon_capabilities::apple::release::{
        self, file_request, may, next_number, upload_request,
    };
    use pigeon_core::apple::{is_version, parts, pkcs8_pem};

    // Above Apple's highest (35 for the old app) and above pigeon's last, whichever is higher.
    assert_eq!(next_number(None, Some(35)), 36);
    assert_eq!(next_number(Some(40), Some(35)), 41);
    assert_eq!(next_number(None, None), 1);

    // Start, then the identity once, then the IPA once.
    assert!(may("started", "identity").is_ok());
    assert!(may("signing", "identity").is_err());
    assert!(may("signing", "upload").is_ok());
    for state in ["started", "uploading", "processing", "complete", "failed"] {
        assert!(may(state, "upload").is_err(), "{state}");
    }
    assert!(release::open("processing") && !release::open("complete"));
    // Apple's upload state is an object; its errors and warnings are kept as the detail.
    let st = json!({ "state": "FAILED", "errors": [{ "code": "90000", "description": "Bad bundle" }],
        "warnings": [{ "code": "W1", "description": "Old SDK" }] });
    assert_eq!(release::upload_state(&st), "FAILED");
    assert_eq!(release::upload_state(&json!("COMPLETE")), "COMPLETE");
    assert_eq!(
        release::upload_problems(&st),
        "errors: 90000 Bad bundle; warnings: W1 Old SDK"
    );
    assert_eq!(release::JOB, "ios-release");

    assert!(is_version("0.2.0") && is_version("1") && is_version("10.4"));
    for bad in ["", "1.2.3.4", "v1", "1..2", "1.2-beta"] {
        assert!(!is_version(bad), "{bad}");
    }

    assert_eq!(
        upload_request("APP1", "0.2.0", 36),
        json!({ "data": { "type": "buildUploads",
            "attributes": { "cfBundleShortVersionString": "0.2.0", "cfBundleVersion": "36", "platform": "IOS" },
            "relationships": { "app": { "data": { "type": "apps", "id": "APP1" } } } } })
    );
    let f = file_request("UP1", 1234);
    assert_eq!(f["data"]["attributes"]["uti"], "com.apple.ipa");
    assert_eq!(f["data"]["attributes"]["assetType"], "ASSET");
    assert_eq!(f["data"]["attributes"]["fileSize"], 1234);
    assert_eq!(
        f["data"]["relationships"]["buildUpload"]["data"]["id"],
        "UP1"
    );

    // Parts must be https, PUT or POST, in order, covering the file exactly.
    let op = |o: u64, l: u64| {
        json!({ "method": "PUT", "url": "https://upload.example/x", "offset": o, "length": l,
        "requestHeaders": [{ "name": "Content-Type", "value": "application/octet-stream" }] })
    };
    let ok = parts(&json!([op(100, 50), op(0, 100)]), 150).unwrap();
    assert_eq!((ok[0].offset, ok[0].length, ok[1].offset), (0, 100, 100));
    assert_eq!(
        ok[0].headers,
        [(
            "Content-Type".to_string(),
            "application/octet-stream".to_string()
        )]
    );
    assert!(parts(&json!([op(0, 100)]), 150).is_err());
    assert!(parts(&json!([op(0, 100), op(90, 60)]), 150).is_err());
    assert!(
        parts(
            &json!([{ "method": "GET", "url": "https://x", "offset": 0, "length": 1 }]),
            1
        )
        .is_err()
    );
    assert!(
        parts(
            &json!([{ "method": "PUT", "url": "http://x", "offset": 0, "length": 1 }]),
            1
        )
        .is_err()
    );
    assert!(parts(&Value::Null, 1).is_err());

    let pem = pkcs8_pem(&[0x30; 100]);
    assert!(
        pem.starts_with("-----BEGIN PRIVATE KEY-----\n")
            && pem.ends_with("-----END PRIVATE KEY-----\n")
    );
    assert!(pem.lines().all(|l| l.len() <= 64));
    assert_eq!(pigeon_core::apple::p8_der(&pem).unwrap(), vec![0x30; 100]);
}

/// A release is admitted at main's head only at start; later steps trust the job's own record.
#[test]
fn apple_release_admits_by_record() {
    use pigeon_capabilities::apple::release::{EXPIRY, Release, admit};

    let now = 10_000_000_000;
    let rec = |job: &str, commit: &str, state: &str, updated: i64| Release {
        job: job.into(),
        commit: commit.into(),
        build_number: 7,
        state: state.into(),
        version: None,
        certificate: None,
        upload: None,
        processing: None,
        detail: None,
        started_ms: updated,
        updated_ms: updated,
    };

    // a. Start at main's head; then main moves and the later steps still pass.
    assert!(admit("start", "j1", "A", Some("A"), None, now).is_ok());
    let started = rec("j1", "A", "started", now);
    let signing = rec("j1", "A", "signing", now);
    assert!(admit("identity", "j1", "A", Some("B"), Some(&started), now).is_ok());
    assert!(admit("upload", "j1", "A", Some("B"), Some(&signing), now).is_ok());
    assert!(admit("start", "j1", "A", None, Some(&started), now).is_ok());

    // b. A commit that was never main's head is refused, and so is a step with no record.
    assert!(admit("start", "j3", "C", Some("A"), None, now).is_err());
    assert!(admit("identity", "j3", "C", Some("C"), None, now).is_err());
    assert!(admit("upload", "j3", "C", Some("C"), None, now).is_err());

    // c. Another job's record does not carry over.
    for step in ["identity", "upload"] {
        assert!(admit(step, "j2", "A", Some("A"), Some(&signing), now).is_err());
    }
    assert!(admit("start", "j2", "A", Some("B"), None, now).is_err());
    assert!(admit("start", "j2", "A", Some("B"), Some(&started), now).is_err());

    // d. A record of another commit, a finished record, or an old one is refused.
    let other = rec("j1", "B", "started", now);
    assert!(admit("identity", "j1", "A", None, Some(&other), now).is_err());
    for state in ["complete", "failed"] {
        let over = rec("j1", "A", state, now);
        assert!(admit("identity", "j1", "A", None, Some(&over), now).is_err());
        assert!(admit("upload", "j1", "A", None, Some(&over), now).is_err());
    }
    let old = rec("j1", "A", "signing", now - EXPIRY - 1);
    assert!(admit("upload", "j1", "A", None, Some(&old), now).is_err());
    let edge = rec("j1", "A", "signing", now - EXPIRY);
    assert!(admit("upload", "j1", "A", None, Some(&edge), now).is_ok());
    assert!(admit("other", "j1", "A", Some("A"), Some(&started), now).is_err());
}

/// The release step ensures a profile with Push: the pure parts.
#[test]
fn apple_release_profile_has_push() {
    use pigeon_capabilities::apple::{CAPABILITIES, missing, profile_has_push};

    let with = b"\x30\x82junk<plist><key>aps-environment</key><string>production</string></plist>";
    let without = b"\x30\x82junk<plist><key>get-task-allow</key><false/></plist>";
    assert!(profile_has_push(&pigeon_core::apple::b64(with)));
    assert!(!profile_has_push(&pigeon_core::apple::b64(without)));
    assert!(!profile_has_push("not base64 !!"));
    assert!(CAPABILITIES.contains(&"PUSH_NOTIFICATIONS"));
    assert_eq!(
        missing(&CAPABILITIES, &["ASSOCIATED_DOMAINS".to_string()]),
        vec!["PUSH_NOTIFICATIONS"]
    );
}

/// APNs, the pure parts (docs/plans/ios.md, §3): signing input, hosts, payload limit, error
/// table, token form and the rate limit.
#[test]
fn apple_apns_payload_errors_and_registry() {
    use pigeon_core::apns::{self, Action, Limited, PushType};

    let input = apns::signing_input("ABC1234567", "TEAM123456", 1000);
    assert_eq!(input.split('.').count(), 2);
    assert!(apns::token_fresh(0, 49 * 60));
    assert!(!apns::token_fresh(0, 50 * 60));
    assert!(!apns::may_refresh(0, 19 * 60));
    assert!(apns::may_refresh(0, 20 * 60));

    assert_eq!(
        apns::host("development"),
        Some("api.sandbox.push.apple.com")
    );
    assert_eq!(apns::host("production"), Some("api.push.apple.com"));
    assert_eq!(apns::host("x"), None);
    assert!(apns::is_device_token(&"ab".repeat(32)));
    assert!(!apns::is_device_token("zz"));
    assert!(!apns::is_device_token(&"g".repeat(64)));

    let p = apns::payload(PushType::Alert, Some(("t", "b")), None).unwrap();
    assert!(p.contains("\"alert\""));
    let bg = apns::payload(PushType::Background, None, None).unwrap();
    assert!(bg.contains("\"content-available\":1"));
    let big = json!({ "k": "x".repeat(5000) });
    assert!(apns::payload(PushType::Alert, Some(("t", "b")), Some(&big)).is_err());
    let nested = json!({ "k": { "a": "b" } });
    assert!(apns::payload(PushType::Alert, Some(("t", "b")), Some(&nested)).is_err());

    let h = apns::headers("pigeon", PushType::Background, Some("c")).unwrap();
    assert!(h.contains(&("apns-topic", "dev.pigeon.app".to_string())));
    assert!(h.contains(&("apns-priority", "5".to_string())));
    assert!(apns::headers("other", PushType::Alert, None).is_err());

    assert_eq!(apns::action(200, ""), Action::Ok);
    assert_eq!(apns::action(400, "BadDeviceToken"), Action::MarkDevice);
    assert_eq!(apns::action(400, "BadTopic"), Action::StopReport);
    assert_eq!(
        apns::action(403, "ExpiredProviderToken"),
        Action::RefreshToken
    );
    assert_eq!(apns::action(410, "Unregistered"), Action::DeleteIfNewer);
    assert_eq!(apns::action(429, "TooManyRequests"), Action::BackOff);
    assert_eq!(apns::action(503, "ServiceUnavailable"), Action::Retry);

    let burst: Vec<u64> = vec![1000; 30];
    assert_eq!(apns::rate_limit(&burst, 1010), Err(Limited::Minute));
    assert_eq!(apns::rate_limit(&burst, 1100), Ok(()));
    let day: Vec<u64> = (0..500).map(|i| i * 100).collect();
    assert_eq!(apns::rate_limit(&day, 50_000), Err(Limited::Day));
}

#[test]
fn apple_apns_send_token_and_actions() {
    use pigeon_core::apns;

    // The provider token is reused only when it exists, matches the key, is fresh and is not
    // forced.
    assert!(apns::reuse_token(Some(1000), true, 1000 + 49 * 60, false));
    assert!(!apns::reuse_token(Some(1000), true, 1000 + 50 * 60, false));
    assert!(!apns::reuse_token(Some(1000), false, 1100, false));
    assert!(!apns::reuse_token(Some(1000), true, 1100, true));
    assert!(!apns::reuse_token(None, true, 1100, false));

    // A gone token drops the row when APNs's time is later than the row's, or is missing.
    assert!(apns::drop_on_gone(None, 5000));
    assert!(apns::drop_on_gone(Some(6000), 5000));
    assert!(!apns::drop_on_gone(Some(5000), 5000));
    assert!(!apns::drop_on_gone(Some(4000), 5000));

    // Core binds the provider key, and the bindings sit in core's file only.
    let wrangler = include_str!("../../src/platform/wrangler.toml");
    for name in ["APNS_KEY", "APNS_KEY_ID"] {
        assert!(wrangler.contains(&format!("binding = \"{name}\"")));
        assert!(wrangler.contains(&format!("secret_name = \"{name}\"")));
    }
}

#[test]
fn platform_design_tokens_for_ios() {
    use pigeon_core::design::swift;

    // The committed Swift file is what the generator writes now.
    let out = swift::generated().unwrap();
    assert!(
        include_str!("../../ios/Pigeon/Design/Tokens.swift") == out,
        "ios/Pigeon/Design/Tokens.swift drifted from tokens.css: run the design_swift example"
    );

    // Spot values.
    assert!(out.ends_with("}\n") && !out.ends_with("\n\n"));
    assert!(out.contains(
        "static let background = dynamic(light: (0xF4, 0xEE, 0xE3, 1.0), dark: (0x14, 0x12, 0x1A, 1.0))"
    ));
    assert!(out.contains("static let accentSoft = dynamic("));
    assert!(out.contains("static let radiusCard: CGFloat = 14"));
    assert!(out.contains(
        "static let typeEyebrow = TypeToken(weight: 600, size: 12, lineHeight: 1.3, mono: true)"
    ));

    // Errors name the token.
    let css = |dark: &str, light: &str| {
        format!(":root {{ {dark} }}\n:root[data-theme=\"light\"] {{ {light} }}\n")
    };
    let e = swift::tokens(&css("--a: #000000; --b: #111111;", "--a: #FFFFFF;")).unwrap_err();
    assert!(e.contains("--b"), "{e}");
    let e = swift::tokens(&css("--blur: 4em;", "")).unwrap_err();
    assert!(e.contains("--blur"), "{e}");
    assert!(swift::tokens(&css("--a: #000000;", "--a: #FFFFFF;")).is_ok());
}
