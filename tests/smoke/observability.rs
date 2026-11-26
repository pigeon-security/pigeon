//! Observability settings and monitors on every Worker.

use super::*;

/// Every Worker in the map keeps Cloudflare Observability on with the plan's settings
/// (docs/plans/observability.md): logs on, invocation logs off, traces set, container logs off.
#[test]
fn core_observability_on_every_worker() {
    use pigeon_core::lanes;
    // Workers whose traces are off, and why. Every other Worker traces.
    let traces_off = [(
        "core",
        "serves the enrolment and OAuth URLs, whose codes sit in the query string",
    )];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let map = lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
    assert!(!map.workers.is_empty());
    for w in &map.workers {
        let path = root.join(&w.dir).join("wrangler.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let t: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let o = t
            .get("observability")
            .unwrap_or_else(|| panic!("{}: no [observability]", w.name));
        assert_eq!(
            o.get("enabled").and_then(|v| v.as_bool()),
            Some(true),
            "{}",
            w.name
        );
        let rate = o
            .get("head_sampling_rate")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
        assert!(
            rate.is_some_and(|r| r > 0.0 && r <= 1.0),
            "{}: head_sampling_rate {rate:?} is not in (0, 1]",
            w.name
        );
        assert_eq!(
            o.get("logs")
                .and_then(|l| l.get("invocation_logs"))
                .and_then(|v| v.as_bool()),
            Some(false),
            "{}: invocation logs keep whole URLs",
            w.name
        );
        let traces = o
            .get("traces")
            .and_then(|tr| tr.get("enabled"))
            .and_then(|v| v.as_bool());
        let off = traces_off.iter().find(|(n, _)| *n == w.name);
        assert_eq!(traces, Some(off.is_none()), "{}: traces {:?}", w.name, off);
        for c in t
            .get("containers")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
        {
            assert_eq!(
                c.get("observability")
                    .and_then(|o| o.get("enabled"))
                    .and_then(|v| v.as_bool()),
                Some(false),
                "{}: container logs must be off",
                w.name
            );
        }
    }
}

#[test]
fn core_observability_monitors_plan() {
    use pigeon_capabilities::observability::{
        absent_types, available_types, plan_monitors, shape_report,
    };
    use serde_json::json;

    let available = json!({ "Workers": [
        { "type": "workers_observability_alert", "filter_options": [{ "Key": "services" }] },
        { "type": "billing_usage_alert", "filter_options": [{ "Key": "product" }] },
    ] });
    let existing = json!([{ "name": "pigeon: Workers errors", "mechanisms": { "email": [] },
        "filters": { "services": ["x"] } }, { "name": "other" }]);
    let scripts = vec!["core".to_string(), "lander".to_string()];

    // An existing pigeon policy is not planned again, and billing watches are not planned
    // until their names are checked against the live API.
    assert!(plan_monitors(&available, &existing, &scripts).is_empty());
    let none = json!([]);
    let planned = plan_monitors(&available, &none, &scripts);
    assert_eq!(planned.len(), 1);
    assert!(
        planned
            .iter()
            .all(|p| p["alert_type"] != "billing_usage_alert")
    );
    // Filters name only the given scripts.
    assert_eq!(
        planned[0]["filters"],
        json!({ "services": ["core", "lander"] })
    );
    // The absent real-time type is skipped and reported.
    assert_eq!(
        absent_types(&available),
        vec!["workers_observability_real_time_issue"]
    );

    // An answer of an unknown shape is told apart from an account that lacks the types: it
    // recognises no type at all, which the plan refuses rather than calling it in place.
    assert!(available_types(&json!({ "result": "unexpected" })).is_empty());
    assert!(!available_types(&available).is_empty());

    // The read-only shape check: field names and counts, no values.
    let report = shape_report(&available, &existing);
    assert_eq!(report["available_alerts"]["groups"], json!(["Workers"]));
    assert_eq!(report["available_alerts"]["types_recognised"], 2);
    assert_eq!(
        report["available_alerts"]["filter_keys"]["billing_usage_alert"],
        json!(["product"])
    );
    assert_eq!(report["policies"]["count"], 2);
    assert_eq!(
        report["policies"]["pigeon"],
        json!(["pigeon: Workers errors"])
    );
    assert_eq!(report["policies"]["mechanisms_fields"], json!(["email"]));

    // Live shape (2026-11-23): `status` is the only filter key of the errors alert, and the
    // real-time type has none. A required key with listed values gets all of them; no guess.
    let live = json!({ "Workers": [
        { "type": "workers_observability_alert", "filter_options": [
            { "Key": "status", "AvailableValues": [{ "Value": "success" }, { "Value": "error" }] }] },
        { "type": "workers_observability_real_time_issue", "filter_options": [] },
    ] });
    let planned = plan_monitors(&live, &none, &scripts);
    assert_eq!(planned.len(), 2);
    assert_eq!(
        planned[0]["filters"],
        json!({ "status": ["success", "error"] })
    );
    assert_eq!(planned[1]["filters"], json!({}));
    // With no values listed, nothing is guessed: the policy is not planned.
    let bare = json!({ "Workers": [
        { "type": "workers_observability_alert", "filter_options": [{ "Key": "status" }] } ] });
    assert!(plan_monitors(&bare, &none, &scripts).is_empty());
    // The check report shows option values, not just keys.
    assert_eq!(
        shape_report(&live, &none)["available_alerts"]["filter_values"]["workers_observability_alert"]
            ["status"],
        json!(["success", "error"])
    );
    assert!(
        !report.to_string().contains("\"x\""),
        "a filter value leaked: {report}"
    );
}

#[test]
fn core_observability_monitors_filters() {
    use pigeon_capabilities::observability::{
        filter_options, plan_line, plan_monitors_with, policy_body, shape_report, validate_filters,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    let none = json!([]);
    let scripts = vec!["core".to_string()];
    // Cloudflare's documented shape: values are `{ID, Description}`.
    let available = json!({ "Workers": [
        { "type": "workers_observability_alert", "filter_options": [
            { "Key": "status", "Range": "1-n", "ComparisonOperator": "==",
              "AvailableValues": [{ "ID": "PENDING", "Description": "p" }, { "ID": "FIRING", "Description": "f" }] },
            { "Key": "slo", "Range": "0-1", "ComparisonOperator": ">=", "AvailableValues": null }] },
        { "type": "workers_observability_real_time_issue", "filter_options": [] },
    ] });

    // ID-shaped values are parsed, with Range and operator.
    let options = filter_options(&available);
    let status = &options["workers_observability_alert"][0];
    assert_eq!(status.ids, vec!["PENDING", "FIRING"]);
    assert!(status.required());
    assert!(!options["workers_observability_alert"][1].required());
    let report = shape_report(&available, &none);
    assert_eq!(
        report["available_alerts"]["filter_options"]["workers_observability_alert"]["status"],
        json!({ "range": "1-n", "comparison_operator": "==", "ids": ["PENDING", "FIRING"] })
    );

    // A required key with IDs gets all of them; the optional key is left out.
    let plan = plan_monitors_with(&available, &none, &scripts, &BTreeMap::new());
    assert!(plan.needs_filters.is_empty());
    assert_eq!(plan.policies.len(), 2);
    assert_eq!(
        plan.policies[0]["filters"],
        json!({ "status": ["PENDING", "FIRING"] })
    );
    let line = plan_line(&plan.policies[0]);
    assert!(line.contains("PENDING") && line.contains("Workers Observability Monitor"));
    let line = plan_line(&plan.policies[1]);
    assert!(line.contains("Workers Issues automation") && line.contains("every Worker"));

    // The real-time policy posts no `filters` key at all.
    let body = policy_body(&plan.policies[1]);
    assert!(body.get("filters").is_none(), "{body}");
    assert!(policy_body(&plan.policies[0]).get("filters").is_some());

    // A required key with no IDs: not planned, and listed with how to pass it.
    let bare = json!({ "Workers": [
        { "type": "workers_observability_alert", "filter_options": [{ "Key": "status", "Range": "1-n" }] } ] });
    let plan = plan_monitors_with(&bare, &none, &scripts, &BTreeMap::new());
    assert!(plan.policies.is_empty());
    assert_eq!(plan.needs_filters.len(), 1);
    assert_eq!(plan.needs_filters[0]["key"], "status");
    assert_eq!(
        plan.needs_filters[0]["alert_type"],
        "workers_observability_alert"
    );
    assert!(
        plan.needs_filters[0]["how"]
            .as_str()
            .unwrap()
            .contains("filters:")
    );

    // Explicit filters win, once validated.
    let given = json!({ "workers_observability_alert": { "status": ["FIRING"] } });
    let explicit = validate_filters(&available, &given).unwrap();
    let plan = plan_monitors_with(&available, &none, &scripts, &explicit);
    assert_eq!(plan.policies[0]["filters"], json!({ "status": ["FIRING"] }));
    let free = validate_filters(
        &bare,
        &json!({ "workers_observability_alert": { "status": ["x"] } }),
    )
    .unwrap();
    let plan = plan_monitors_with(&bare, &none, &scripts, &free);
    assert_eq!(plan.policies[0]["filters"], json!({ "status": ["x"] }));

    // Bad input is an error, never a panic.
    let bad = |v: serde_json::Value| validate_filters(&available, &v).unwrap_err();
    assert!(bad(json!({ "workers_observability_alert": { "nope": ["a"] } })).contains("nope"));
    let e = bad(json!({ "workers_observability_alert": { "status": ["GONE"] } }));
    assert!(e.contains("PENDING") && e.contains("FIRING"), "{e}");
    assert!(bad(json!({ "other_type": {} })).contains("other_type"));
    assert!(bad(json!({ "workers_observability_alert": { "status": "FIRING" } })).contains("list"));
    assert!(bad(json!({ "workers_observability_alert": { "status": [1] } })).contains("string"));
    assert!(bad(json!({ "workers_observability_alert": { "status": [""] } })).contains("string"));
    assert!(bad(json!({ "workers_observability_alert": { "status": [] } })).contains("1 to 20"));
    assert!(bad(json!("x")).contains("object"));
    let many: Vec<String> = (0..21).map(|i| i.to_string()).collect();
    assert!(
        validate_filters(
            &bare,
            &json!({ "workers_observability_alert": { "status": many } })
        )
        .is_err()
    );
}

#[test]
fn core_observability_spike_and_ci_price() {
    use pigeon_capabilities::observability::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    let hour = 1_795_428_000_000_i64; // 2026-11-23T10:00:00Z
    let row = |script: &str, at: &str, requests: f64, errors: f64| {
        json!({ "sum": { "requests": requests, "errors": errors },
                "dimensions": { "scriptName": script, "status": "success", "datetimeHour": at } })
    };
    let answer = json!({ "data": { "viewer": { "accounts": [{ "workersInvocationsAdaptive": [
        row("core", "2026-11-23T10:00:00Z", 30.0, 1.0),
        row("core", "2026-11-23T10:00:00Z", 10.0, 4.0),
        row("core", "2026-11-23T11:00:00Z", 50.0, 10.0),
        row("core", "not a date", 99.0, 99.0),
        row("other", "2026-11-23T09:00:00Z", 5.0, 0.0),
    ] }] } } });
    let hourly = fold_hourly(&answer);
    assert_eq!(hourly["core"].len(), 2);
    assert_eq!(hourly["core"][&hour], (40.0, 5.0));
    assert_eq!(hourly["core"][&(hour + 3_600_000)], (50.0, 10.0));
    assert!(hourly["other"].contains_key(&(hour - 3_600_000)));

    let deploy = |id: i64, state: &str, at: i64| json!({ "id": id, "landing": "l1", "commit": "abc", "state": state, "finished_at": at });
    // 90 requests and 15 errors after: 16.7% against a 5% window rate is a spike.
    let deploys = [
        deploy(1, "succeeded", hour + 600_000),
        deploy(2, "failed", hour + 600_000),
        deploy(3, "succeeded", hour + 7_200_000 * 5),
    ];
    let flagged = spike_after_deploy(&hourly["core"], &deploys, 0.05);
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    assert_eq!(flagged[0]["deploy"], 1);
    assert_eq!(flagged[0]["requests_after"], 90.0);
    assert_eq!(flagged[0]["errors_after"], 15.0);
    // Below thresholds: the same deploy at a window rate of 10% is not flagged.
    assert!(spike_after_deploy(&hourly["core"], &deploys, 0.10).is_empty());

    // CI price: a small class is priced, an unknown one is listed as unpriced.
    let seconds = BTreeMap::from([
        ("linux-amd64-small".to_string(), 100.0),
        ("mystery".to_string(), 50.0),
    ]);
    let (total, by_class, unpriced) = ci_price(&seconds);
    let want = 100.0 * (2.0 * 0.000020 + 8.0 * 0.0000025 + 16.0 * 0.00000007);
    assert!((total - want).abs() < 1e-12, "{total} vs {want}");
    assert_eq!(by_class.len(), 1);
    assert_eq!(unpriced, vec!["mystery".to_string()]);
}

#[test]
fn core_observability_monitors_updates() {
    use pigeon_capabilities::observability::{
        monitor_left_alone, plan_monitor_updates, update_line,
    };
    use serde_json::json;

    let wanted = vec![json!({ "name": "pigeon: Workers errors",
        "alert_type": "workers_observability_alert",
        "filters": { "status": ["error", "fail"] }, "enabled": true })];
    let email = "person@example.com";
    let policy = |filters: Value, enabled: bool| {
        json!([{ "id": "p1", "name": "pigeon: Workers errors",
            "alert_type": "workers_observability_alert", "enabled": enabled,
            "description": "kept", "alert_interval": "5m",
            "filters": filters, "mechanisms": { "email": [{ "id": email }] } }])
    };

    // Identical (order differs): nothing to do.
    let same = policy(json!({ "status": ["fail", "error"] }), true);
    assert!(plan_monitor_updates(&wanted, &same).is_empty());

    // Filters differ: one update with the diff; delivery and extra fields are kept.
    let differ = policy(json!({ "status": ["error"] }), true);
    let up = plan_monitor_updates(&wanted, &differ);
    assert_eq!(up.len(), 1);
    assert_eq!(up[0]["id"], "p1");
    assert_eq!(
        up[0]["changes"]["filters"]["to"],
        json!({ "status": ["error", "fail"] })
    );
    assert!(up[0]["changes"]["enabled"].is_null());
    assert_eq!(up[0]["body"]["mechanisms"]["email"][0]["id"], email);
    assert_eq!(up[0]["body"]["description"], "kept");
    assert_eq!(up[0]["body"]["alert_interval"], "5m");
    assert_eq!(up[0]["body"]["enabled"], true);
    assert_eq!(
        up[0]["body"]["filters"],
        json!({ "status": ["error", "fail"] })
    );

    // Disabled: an update; a missing filters list counts as `{}`.
    let off = policy(json!({ "status": ["error", "fail"] }), false);
    let up = plan_monitor_updates(&wanted, &off);
    assert_eq!(up.len(), 1);
    assert_eq!(
        up[0]["changes"]["enabled"],
        json!({ "from": false, "to": true })
    );
    assert!(up[0]["changes"]["filters"].is_null());
    let bare = json!([{ "id": "p1", "name": "pigeon: Workers errors",
        "alert_type": "workers_observability_alert", "enabled": true }]);
    assert!(plan_monitor_updates(&wanted, &bare)[0]["changes"]["filters"].is_object());

    // A policy that is not pigeon's is never touched, even with the same alert type.
    let other = json!([{ "id": "p2", "name": "Workers errors",
        "alert_type": "workers_observability_alert", "enabled": false, "filters": {} }]);
    let named = vec![json!({ "name": "Workers errors",
        "alert_type": "workers_observability_alert", "filters": {}, "enabled": true })];
    assert!(plan_monitor_updates(&named, &other).is_empty());
    assert!(plan_monitor_updates(&wanted, &other).is_empty());

    // A pigeon policy of another type is left alone and reported.
    let odd = json!([{ "id": "p3", "name": "pigeon: Workers errors",
        "alert_type": "something_else", "enabled": false, "filters": {} }]);
    assert!(plan_monitor_updates(&wanted, &odd).is_empty());
    assert_eq!(
        monitor_left_alone(&wanted, &odd),
        vec!["`pigeon: Workers errors` exists as something_else; left alone"]
    );

    // Wanted delivery is merged in; counts show, ids do not.
    let mut with_mail = wanted.clone();
    with_mail[0]["mechanisms"] = json!({ "email": [{ "id": "other@example.com" }] });
    let up = plan_monitor_updates(&with_mail, &same);
    assert_eq!(up.len(), 1);
    assert_eq!(
        up[0]["changes"]["mechanisms"],
        json!({ "from": { "email": 1 }, "to": { "email": 2 } })
    );
    assert_eq!(
        up[0]["body"]["mechanisms"]["email"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // No change or diff text carries an existing email id.
    let all = plan_monitor_updates(&with_mail, &off);
    let text = format!("{} {}", all[0]["changes"], update_line(&all[0]));
    assert!(!text.contains("example.com"), "an id leaked: {text}");
    assert!(update_line(&all[0]).contains("delivery email 1 -> 2"));
    assert!(update_line(&all[0]).contains("enabled false -> true"));
}

#[test]
fn core_observability_monitors_email() {
    use pigeon_capabilities::observability::{
        email_line, email_mechanisms, plan_line, plan_monitor_updates, policy_body, update_line,
        validate_email, with_email,
    };
    use serde_json::json;

    // Validation: a normal address passes (trimmed); bad ones fail without being echoed.
    assert_eq!(
        validate_email("  me@example.com ").as_deref(),
        Ok("me@example.com")
    );
    let bad = [
        "no-at.example.com",
        "two@@example.com",
        "a@b@example.com",
        "@example.com",
        "me@localhost",
        "me @example.com",
        "me@exa,mple.com",
        "me@exa;mple.com",
        "me@exam\u{7}ple.com",
        "a@",
        "",
        &format!("{}@example.com", "x".repeat(250)),
    ];
    for input in bad {
        let err = validate_email(input).expect_err(input);
        assert!(err.starts_with("`email` is not a valid address: "), "{err}");
        if input.len() > 2 {
            assert!(!err.contains(input), "the input was echoed: {err}");
        }
    }

    let mail = "new@example.com";
    let other = "old@example.com";
    assert_eq!(
        email_mechanisms(Some(mail)),
        json!({ "email": [{ "id": mail }] })
    );
    assert_eq!(email_mechanisms(None), json!({}));

    // Planned creates carry the mechanism and the body posts it.
    let plain = vec![
        json!({ "name": "pigeon: Workers errors", "alert_type": "workers_observability_alert",
            "filters": { "status": ["error"] }, "enabled": true }),
        json!({ "name": "pigeon: Workers real-time issues",
            "alert_type": "workers_observability_real_time_issue",
            "filters": {}, "enabled": true }),
    ];
    assert_eq!(policy_body(&plain[0])["mechanisms"], json!({}));
    assert_eq!(with_email(plain.clone(), None), plain);
    let wanted = with_email(plain, Some(mail));
    for p in &wanted {
        assert_eq!(p["mechanisms"], json!({ "email": [{ "id": mail }] }));
        assert_eq!(policy_body(p)["mechanisms"], p["mechanisms"]);
    }

    // An existing policy: missing the address -> update keeping the other email; holding it -> none.
    let existing = |ids: &[&str], id: &str| {
        let list: Vec<Value> = ids.iter().map(|i| json!({ "id": i })).collect();
        json!([{ "id": id, "name": "pigeon: Workers errors",
            "alert_type": "workers_observability_alert", "enabled": true,
            "filters": { "status": ["error"] }, "mechanisms": { "email": list } }])
    };
    let up = plan_monitor_updates(&wanted, &existing(&[other], "p1"));
    assert_eq!(up.len(), 1);
    let ids: Vec<&str> = up[0]["body"]["mechanisms"]["email"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(ids, vec![other, mail]);
    assert!(plan_monitor_updates(&wanted, &existing(&[other, mail], "p1")).is_empty());

    // The address is in exactly one plan line and in no create or update line.
    let mut lines = vec![email_line(mail)];
    lines.extend(wanted.iter().map(plan_line));
    lines.extend(up.iter().map(update_line));
    assert_eq!(lines.iter().filter(|l| l.contains(mail)).count(), 1);
    assert!(lines[0].contains(mail));
    assert!(!up.iter().any(|u| update_line(u).contains(other)));
}

#[test]
fn core_observability_summary_folds() {
    use pigeon_capabilities::observability::*;
    use serde_json::json;

    // Invocations: two statuses of one script fold into one entry.
    let answer = json!({ "data": { "viewer": { "accounts": [{ "workersInvocationsAdaptive": [
        { "sum": { "requests": 90.0, "errors": 0.0 }, "quantiles": { "cpuTimeP50": 1000.0, "cpuTimeP99": 5000.0, "wallTimeP50": 2000.0, "wallTimeP99": 9000.0 },
          "dimensions": { "scriptName": "core", "status": "success" } },
        { "sum": { "requests": 10.0, "errors": 10.0 }, "quantiles": { "cpuTimeP50": 1000.0, "cpuTimeP99": 8000.0, "wallTimeP50": 2000.0, "wallTimeP99": 3000.0 },
          "dimensions": { "scriptName": "core", "status": "exception" } },
    ] }] } } });
    let scripts = fold_invocations(&answer);
    let core = &scripts["core"];
    assert_eq!(core.requests, 100.0);
    assert_eq!(core.errors, 10.0);
    assert!((core.error_rate() - 0.1).abs() < 1e-9);
    assert_eq!(core.cpu_p99_us, 8000.0);
    assert_eq!(core.by_status["exception"], 10.0);
    assert!((core.cpu_ms_estimate() - 100.0).abs() < 1e-9);

    // Errors: top five per Worker, messages cut to 200 characters.
    let long = "x".repeat(500);
    let mut rows = vec![json!({ "count": 3.0, "groups": [
        { "key": "$metadata.service", "value": "core" },
        { "key": "$metadata.message", "value": long } ] })];
    for i in 0..6 {
        rows.push(json!({ "count": i as f64, "groups": [
            { "key": "$metadata.service", "value": "core" },
            { "key": "$metadata.message", "value": format!("m{i}") } ] }));
    }
    let errors = fold_errors(&json!({ "calculations": [{ "aggregates": rows }] }));
    assert_eq!(errors["core"].len(), 5);
    assert_eq!(errors["core"][0]["message"], "m5");
    let cut_one = errors["core"]
        .iter()
        .find(|e| e["count"] == 3.0)
        .map(|e| e["message"].as_str().unwrap_or_default().chars().count());
    assert_eq!(cut_one, Some(200));
    assert_eq!(cut(&long).chars().count(), 200);

    // Redaction: a message comes back without URL queries, emails or token-like strings, and
    // the words around them stay.
    let leaky = "GET https://pigeon.dev/app/oauth?code=Zx9-abc&state=1 failed for ada@example.com: \
                 Bearer eyJhbGciOi.J9eyJzdWIiOiIx.SflKxwRJSMeKKF2QT4 (account \
                 0123456789abcdef0123456789abcdef) at src/lib.rs:232 v4.166.0";
    let clean = redact(leaky);
    for gone in [
        "code=",
        "Zx9-abc",
        "ada@",
        "example.com",
        "eyJhbGciOi",
        "0123456789abcdef",
    ] {
        assert!(!clean.contains(gone), "{gone} left in {clean}");
    }
    for kept in [
        "GET https://pigeon.dev/app/oauth?[query]",
        "failed for [email]",
        "Bearer [redacted]",
        "(account [redacted])",
        "src/lib.rs:232",
        "v4.166.0",
    ] {
        assert!(clean.contains(kept), "{kept} missing from {clean}");
    }
    let rows = vec![json!({ "count": 1.0, "groups": [
        { "key": "$metadata.service", "value": "pigeon-proxy" },
        { "key": "$metadata.message", "value": leaky } ] })];
    let errors = fold_errors(&json!({ "calculations": [{ "aggregates": rows }] }));
    assert_eq!(errors["pigeon-proxy"][0]["message"], json!(cut(&clean)));

    // Logs coverage: the Workers with any events, so one whose logs are off is told apart from
    // one with no errors.
    let seen = fold_services(&json!({ "calculations": [{ "aggregates": [
        { "count": 9.0, "groups": [{ "key": "$metadata.service", "value": "pigeon-platform" }] },
        { "count": 2.0, "groups": [{ "key": "$metadata.service", "value": "pigeon-sites" }] } ] }] }));
    assert!(seen.contains("pigeon-platform") && seen.contains("pigeon-sites"));
    assert!(!seen.contains("pigeon-proxy"));
    assert!(fold_services(&json!({})).is_empty());

    // Billing: two units of one service stay apart; the account name never comes out.
    let billing = fold_billing(&json!([
        { "ServiceName": "Workers", "ConsumedUnit": "requests", "ConsumedQuantity": "100", "BilledCost": 1.0, "BillingAccountName": "Secret Co" },
        { "ServiceName": "Workers", "ConsumedUnit": "requests", "ConsumedQuantity": 50, "BilledCost": 0.5, "BillingAccountName": "Secret Co" },
        { "ServiceName": "Workers", "ConsumedUnit": "cpu_ms", "ConsumedQuantity": 7, "BilledCost": 0.25 },
    ]));
    assert_eq!(billing.len(), 2);
    let requests = billing.iter().find(|b| b["ConsumedUnit"] == "requests");
    assert_eq!(
        requests.map(|b| b["ConsumedQuantity"].as_f64()),
        Some(Some(150.0))
    );
    assert_eq!(requests.map(|b| b["BilledCost"].as_f64()), Some(Some(1.5)));
    assert!(!json!(billing).to_string().contains("Secret"));

    // An empty ConsumedUnit (as live rows often have) falls back to the charge description, so
    // two meters of one service still stay apart.
    let billing = fold_billing(&json!([
        { "ServiceName": "Durable Objects", "ConsumedUnit": "", "ChargeDescription": "Requests", "ConsumedQuantity": 9, "BilledCost": 0.0 },
        { "ServiceName": "Durable Objects", "ConsumedUnit": "", "ChargeDescription": "Duration", "ConsumedQuantity": 4, "BilledCost": 0.0 },
    ]));
    assert_eq!(billing.len(), 2, "{billing:?}");
    assert!(billing.iter().any(|b| b["ConsumedUnit"] == "Duration"));

    // The Workers estimate: 2M requests and 10M CPU ms.
    assert!((workers_estimate(2e6, 1e7) - 0.8).abs() < 1e-9);

    // Agent runs: only the window's runs, counts and dollars.
    let runs = json!({ "runs": [
        { "created_at": 1500, "spent": { "cost": "$1.5" }, "brief": "secret brief" },
        { "created_at": 1600, "spent": { "cost": "$0.25" } },
        { "created_at": 100, "spent": { "cost": "$9" } },
    ] });
    assert_eq!(fold_agent_runs(&runs, 1000, 2000), Ok((2, 1.75)));
    assert!(fold_agent_runs(&json!({}), 0, 1).is_err());

    // CI jobs: Containers only, finished in the window, seconds per class.
    let jobs = json!({ "jobs": [
        { "class": "linux-amd64-small", "connector": "containers", "finished_at": 1500, "duration_ms": 30000 },
        { "class": "linux-amd64-small", "connector": "containers", "finished_at": 1600, "duration_ms": 30000 },
        { "class": "linux-amd64-small", "connector": "buildkite", "finished_at": 1600, "duration_ms": 99000 },
        { "class": "linux-amd64-medium", "connector": "containers", "finished_at": 10, "duration_ms": 99000 },
    ] });
    let (seconds, listed) = fold_ci_jobs(&jobs, 1000, 2000).unwrap_or_default();
    assert_eq!(listed, 4);
    assert_eq!(seconds.len(), 1);
    assert_eq!(seconds["linux-amd64-small"], 60.0);
}

/// The "AI agent runs" line (docs/plans/agent-costs.md): a run's own cost is read from the
/// numeric `cost_usd` when the AI agents Worker gives it, else from the `cost` string, and the
/// window's turns not yet read from AI Gateway are counted.
#[test]
fn core_observability_agent_cost_lines() {
    use pigeon_capabilities::observability::{fold_agent_runs, unreconciled_agent_turns};
    use serde_json::json;

    let runs = json!({ "runs": [
        { "created_at": 1500, "spent": { "cost": "$1.5", "cost_usd": 1.25, "unreconciled_turns": 2 } },
        { "created_at": 1600, "spent": { "cost": "$0.25" } },
        { "created_at": 1700, "spent": { "cost": 0.5, "unreconciled_turns": 1 } },
        { "created_at": 100, "spent": { "cost_usd": 9.0, "unreconciled_turns": 7 } },
    ] });
    assert_eq!(fold_agent_runs(&runs, 1000, 2000), Ok((3, 2.0)));
    assert_eq!(unreconciled_agent_turns(&runs, 1000, 2000), 3);
    assert_eq!(unreconciled_agent_turns(&json!({}), 0, 1), 0);
}

/// The `cost_daily` buckets: the gateway's logs fold by day, family and run; billing rows land on
/// their day or spread over their period; stored months merge with live days; totals add up.
#[test]
fn platform_cost_daily_buckets() {
    use pigeon_capabilities::cost::*;
    use pigeon_core::ai_gateway::Log;
    use pigeon_models::daily::daily_by_run;
    use std::collections::BTreeMap;

    let today = "2026-03-10";
    let near = |a: &Value, b: f64| (a.as_f64().unwrap_or(f64::NAN) - b).abs() < 1e-9;
    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|l| {
                l.iter()
                    .filter_map(|x| x["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    // Models: families, run attribution, unattributed; outside the window or free adds nothing.
    let log = |at: &str, model: &str, cost: Option<f64>, task: Option<&str>| {
        let mut l = Log {
            created_at: at.into(),
            model: model.into(),
            cost,
            ..Log::default()
        };
        if let Some(t) = task {
            l.metadata.insert("task".into(), t.into());
        }
        l
    };
    let opus = "anthropic/claude-opus-4";
    let logs = vec![
        log(
            "2026-03-10T01:00:00Z",
            opus,
            Some(1.0),
            Some("ai-agent/r1/turn1"),
        ),
        log(
            "2026-03-10T02:00:00Z",
            "anthropic/claude-sonnet-4",
            Some(0.5),
            Some("ai-agent/r2/turn1"),
        ),
        log(
            "2026-03-09T03:00:00Z",
            "anthropic/claude-haiku-4",
            Some(0.25),
            None,
        ),
        log(
            "2026-03-09T04:00:00Z",
            "other/model-x",
            Some(0.125),
            Some("ai-agent/r1/turn2"),
        ),
        log("2026-01-01T00:00:00Z", opus, Some(9.0), None),
        log("2026-03-10T05:00:00Z", opus, None, None),
    ];
    let folded = daily_by_run(&logs, today, 31);
    assert_eq!(folded.days.len(), 31);
    assert_eq!(folded.by_family["Opus"]["2026-03-10"], 1.0);
    assert_eq!(folded.by_family["Other"]["2026-03-09"], 0.125);
    assert_eq!(folded.by_run["r1"]["2026-03-09"], 0.125);
    assert_eq!(folded.by_run["r2"]["2026-03-10"], 0.5);
    assert_eq!(folded.unattributed["2026-03-09"], 0.25);
    assert!(!folded.by_family["Opus"].contains_key("2026-01-01"));
    let models = json!(folded);

    // Roles: a planner, an executor, and a run the list does not know falls to other.
    let roles = roles_of(&json!({ "runs": [
        { "run": "r1", "role": "planner" }, { "run": "r2", "role": "executor" } ] }));
    let live = live_days(None, Some(&models), Some(&roles), None);
    assert!(near(&live["2026-03-10"]["roles"]["planner"], 1.0));
    assert!(near(&live["2026-03-10"]["roles"]["executor"], 0.5));
    assert!(near(&live["2026-03-09"]["roles"]["other"], 0.25));
    let none = BTreeMap::new();
    let live_other = live_days(None, Some(&models), Some(&none), None);
    assert!(near(&live_other["2026-03-10"]["roles"]["other"], 1.5));
    assert!(live_days(None, Some(&models), None, None)["2026-03-10"]["roles"].is_null());

    // Billing: day rows on their day, a period row spread up to today, the account name dropped.
    let rows = json!([
        { "ServiceName": "Workers Paid", "BilledCost": 9.0, "ChargePeriodStart": "2026-03-01T00:00:00Z", "ChargePeriodEnd": "2026-04-01T00:00:00Z", "BillingAccountName": "Secret Co" },
        { "ServiceName": "Workers", "BilledCost": 2.0, "ChargePeriodStart": "2026-03-09T00:00:00Z", "ChargePeriodEnd": "2026-03-10T00:00:00Z" },
        { "ServiceName": "Containers", "BilledCost": 0.5, "ChargePeriodStart": "2026-03-10T00:00:00Z", "ChargePeriodEnd": "2026-03-11T00:00:00Z" },
        { "ServiceName": "R2 Storage", "BilledCost": "1.5", "ChargePeriodStart": "2026-03-08T00:00:00Z", "ChargePeriodEnd": "2026-03-09T00:00:00Z" },
        { "ServiceName": "Durable Objects", "BilledCost": 0.3, "ChargePeriodStart": "2026-03-10T00:00:00Z", "ChargePeriodEnd": "2026-03-11T00:00:00Z" },
    ]);
    assert_eq!(line_of("Workers Paid"), "Workers");
    assert_eq!(line_of("Durable Objects"), "Other");
    let billing = billing_days(&rows, today);
    assert!(!json!(billing.days).to_string().contains("Secret"));
    let nine = &billing.days["2026-03-09"];
    assert!(near(&nine["cloudflare"]["Workers"], 2.9));
    assert_eq!(nine["granularity"]["Workers"], "period");
    assert_eq!(billing.days["2026-03-08"]["granularity"]["R2"], "day");
    assert!(!billing.days.contains_key("2026-03-11"));

    // Assemble 7d: order, Other lines, totals, notes, granularity.
    let live = live_days(Some(&billing), Some(&models), Some(&roles), None);
    let stored: BTreeMap<String, Value> = BTreeMap::from([
        (
            "2026-02".to_string(),
            json!({ "2026-02-27": {
            "cloudflare": { "Workers": 1.0 }, "granularity": { "Workers": "day" },
            "models": { "Opus": 2.0 }, "roles": { "planner": 2.0 } } }),
        ),
        (
            "2026-03".to_string(),
            json!({ "2026-03-09": {
            "models": { "Opus": 5.0 }, "roles": { "planner": 5.0 } } }),
        ),
    ]);
    let week = assemble("7d", today, &stored, &live, false, false, vec![], vec![]);
    assert_eq!(week["days"].as_array().map(Vec::len), Some(7));
    assert_eq!(week["days"][0], "2026-03-04");
    assert_eq!(week["stored_from"], "2026-02-27");
    let lines = &week["cloudflare"]["lines"];
    assert_eq!(
        names(lines),
        [
            "CI Containers",
            "Workers",
            "Workers AI",
            "D1",
            "R2",
            "Other"
        ]
    );
    assert_eq!(lines[1]["granularity"], "period");
    assert_eq!(lines[4]["granularity"], "day");
    assert_eq!(lines[4]["kind"], "billed");
    assert!(near(&week["cloudflare"]["total"], 10.6));
    assert!(near(&week["ai_gateway"]["total"], 1.875));
    assert!(near(&week["total"], 12.475));
    let note = week["notes"][0].as_str().unwrap_or_default();
    assert!(note.contains("per billing period"));
    // Live wins for a day it covers: stored Opus 5.0 on 03-09 is replaced.
    assert!(
        week["ai_gateway"]["by_model"][0]["by_day"]
            .get("2026-03-09")
            .is_none()
    );
    assert_eq!(
        names(&week["ai_gateway"]["by_model"]),
        ["Opus", "Sonnet", "Haiku", "OpenAI", "Other"]
    );
    assert_eq!(
        names(&week["ai_gateway"]["by_role"]),
        ["planner", "executor", "other"]
    );

    // A truncated listing never overwrites the stored gateway values; Cloudflare's still land.
    let cut = assemble("7d", today, &stored, &live, true, false, vec![], vec![]);
    assert!(near(
        &cut["ai_gateway"]["by_model"][0]["by_day"]["2026-03-09"],
        5.0
    ));
    assert_eq!(cut["truncated"], true);
    let closed = closed_by_month(&live, today);
    let month = merge_month(&stored["2026-03"], &closed["2026-03"], true);
    assert!(near(&month["2026-03-09"]["models"]["Opus"], 5.0));
    assert!(near(&month["2026-03-09"]["cloudflare"]["Workers"], 2.9));
    let month = merge_month(&stored["2026-03"], &closed["2026-03"], false);
    assert!(month["2026-03-09"]["models"]["Opus"].is_null());
    assert!(!closed["2026-03"].contains_key(today));

    // All: from the first stored day to today.
    let all = assemble("all", today, &stored, &live, false, false, vec![], vec![]);
    assert_eq!(all["days"].as_array().map(Vec::len), Some(12));
    assert_eq!(all["days"][0], "2026-02-27");
    assert!(near(
        &all["ai_gateway"]["by_model"][0]["by_day"]["2026-02-27"],
        2.0
    ));
    assert!(near(
        &all["ai_gateway"]["by_model"][0]["by_day"]["2026-03-10"],
        1.0
    ));
    assert!(near(&all["total"], 12.475 + 2.7 + 3.0));
    // With nothing stored, `all` is the 7d window; `today` is one day.
    let empty = assemble(
        "all",
        today,
        &BTreeMap::new(),
        &BTreeMap::new(),
        false,
        false,
        vec![],
        vec![],
    );
    assert_eq!(empty["days"].as_array().map(Vec::len), Some(7));
    assert_eq!(empty["stored_from"], Value::Null);
    assert_eq!(empty["total"], 0.0);
    let one = assemble("today", today, &stored, &live, false, false, vec![], vec![]);
    assert_eq!(one["days"], json!([today]));
    assert!(near(&one["ai_gateway"]["total"], 1.5));
}

/// `cost_daily` lists the gateway's logs only for days no stored month settles: a closed day's
/// logs do not change, so a day stored with the gateway's reading (even one with no AI cost) is
/// not listed again. Listing every gateway for 7 days took longer than the console's 10-second
/// read limit, so Cost answered 504 on web and iOS. The stored days and a today-only live read
/// still assemble into the full week.
#[test]
fn platform_cost_daily_reads_only_unsettled_days() {
    use pigeon_capabilities::cost::*;
    use pigeon_core::ai_gateway::Log;
    use pigeon_models::daily::daily_by_run;
    use std::collections::BTreeMap;

    let today = "2026-03-10";
    let near = |a: &Value, b: f64| (a.as_f64().unwrap_or(f64::NAN) - b).abs() < 1e-9;
    let opus = |at: &str, cost: f64| Log {
        created_at: at.into(),
        model: "anthropic/claude-opus-4".into(),
        cost: Some(cost),
        ..Log::default()
    };

    // Nothing stored: the whole range is listed; `today` is always one day.
    let none = BTreeMap::new();
    assert_eq!(live_window(today, 7, &none), 7);
    assert_eq!(live_window(today, 1, &none), 1);

    // A full 7-day read marks every day it listed, also the days with no cost.
    let week_logs = vec![
        opus("2026-03-05T10:00:00Z", 2.0),
        opus("2026-03-09T10:00:00Z", 3.0),
    ];
    let models = json!(daily_by_run(&week_logs, today, 7));
    let live = live_days(None, Some(&models), None, None);
    for day in ["2026-03-04", "2026-03-06", "2026-03-09", today] {
        assert_eq!(live[day][GATEWAY_READ], true, "{day}");
    }
    let closed = closed_by_month(&live, today);
    assert!(!closed["2026-03"].contains_key(today));
    let stored: BTreeMap<String, Value> = BTreeMap::from([(
        "2026-03".to_string(),
        merge_month(&Value::Null, &closed["2026-03"], false),
    )]);
    // Every closed day is settled now: the next read lists today alone.
    assert_eq!(live_window(today, 7, &stored), 1);
    // `all` lists back to the oldest of its 31 days that is not stored yet.
    assert_eq!(live_window(today, 31, &stored), 31);

    // A closed day left unsettled (a failed read stored only billing) is listed again, with
    // everything after it.
    let mut gap = stored.clone();
    gap.get_mut("2026-03").unwrap()["2026-03-07"] = json!({ "cloudflare": { "Workers": 1.0 } });
    assert_eq!(live_window(today, 7, &gap), 4);
    // A day stored before the mark, with gateway cost in it, counts as settled.
    assert!(gateway_settled(&json!({ "models": { "Opus": 1.0 } })));
    assert!(gateway_settled(&json!({ "gateway_details": {} })));
    assert!(!gateway_settled(&json!({ "cloudflare": {} })));
    assert!(!gateway_settled(&Value::Null));

    // The next read lists today only, and the answer still has the whole week.
    let today_logs = vec![opus("2026-03-10T01:00:00Z", 0.5)];
    let models = json!(daily_by_run(&today_logs, today, 1));
    let live = live_days(None, Some(&models), None, None);
    assert_eq!(live.keys().collect::<Vec<_>>(), [today]);
    let week = assemble("7d", today, &stored, &live, false, false, vec![], vec![]);
    let opus_days = &week["ai_gateway"]["by_model"][0]["by_day"];
    assert!(near(&opus_days["2026-03-05"], 2.0));
    assert!(near(&opus_days["2026-03-09"], 3.0));
    assert!(near(&opus_days[today], 0.5));
    assert!(near(&week["ai_gateway"]["total"], 5.5));

    // A stored day read from a cut-short listing keeps the answer's warning.
    let days: Vec<String> = serde_json::from_value(week["days"].clone()).unwrap();
    assert!(!stored_truncated(&days, &stored));
    let mut cut = stored.clone();
    cut.get_mut("2026-03").unwrap()["2026-03-09"]["gateway_details"] =
        json!({ "pigeon": { "total": 3.0, "models": {}, "truncated": true } });
    assert!(stored_truncated(&days, &cut));

    // The months a read needs: the first stored (for `stored_from`) and the range's.
    let index = vec!["2025-12".to_string(), "2026-02".into(), "2026-03".into()];
    assert_eq!(
        wanted_months("7d", &index, today, 7),
        ["2025-12", "2026-03"]
    );
    assert_eq!(wanted_months("7d", &index, "2026-03-03", 7), index);
    assert_eq!(wanted_months("all", &index, today, 31), index);
    assert!(wanted_months("today", &[], today, 1).is_empty());
}

#[test]
fn core_observability_monitors_billing() {
    use pigeon_capabilities::observability::{
        billing_notes, billing_policies, plan_line, plan_monitor_updates, plan_monitors_with,
        update_line, validate_billing,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    let scripts = vec!["core".to_string()];
    let listed = json!({ "Billing": [
        { "type": "billing_usage_alert", "filter_options": [
            { "Key": "product", "Range": "1-n", "ComparisonOperator": "==",
              "AvailableValues": [{ "ID": "worker_requests", "Description": "r" }, { "ID": "r2_storage", "Description": "s" }] },
            { "Key": "limit", "Range": "1-1", "ComparisonOperator": "==", "AvailableValues": null }] },
        { "type": "workers_observability_real_time_issue", "filter_options": [] },
    ] });
    let unlisted = json!({ "Billing": [
        { "type": "billing_usage_alert", "filter_options": [
            { "Key": "product", "AvailableValues": null }, { "Key": "limit", "AvailableValues": null }] },
    ] });
    let absent = json!({ "Workers": [
        { "type": "workers_observability_real_time_issue", "filter_options": [] }] });
    let existing = json!([{ "id": "b1", "name": "pigeon: billing worker_requests",
        "alert_type": "billing_usage_alert", "enabled": true,
        "filters": { "product": ["worker_requests"], "limit": ["50"] }, "mechanisms": {} }]);

    // No `billing`: nothing billing-related is planned or updated, even for an existing policy.
    let none = validate_billing(&listed, &json!(null)).unwrap();
    assert!(none.is_empty() && billing_policies(&none).is_empty());
    let plan = plan_monitors_with(&listed, &json!([]), &scripts, &BTreeMap::new());
    assert!(
        plan.policies
            .iter()
            .all(|p| p["alert_type"] != "billing_usage_alert")
    );
    assert!(plan_monitor_updates(&plan.policies, &existing).is_empty());
    assert!(billing_notes(&listed, &none).is_empty());

    // Valid `billing`: one policy per product with product and limit filters.
    let ok = validate_billing(
        &listed,
        &json!({ "worker_requests": "100", "r2_storage": "2.5" }),
    )
    .unwrap();
    let wanted = billing_policies(&ok);
    assert_eq!(wanted.len(), 2);
    let w = wanted
        .iter()
        .find(|p| p["name"] == "pigeon: billing worker_requests")
        .unwrap();
    assert_eq!(w["alert_type"], "billing_usage_alert");
    assert_eq!(
        w["filters"],
        json!({ "product": ["worker_requests"], "limit": ["100"] })
    );
    assert_eq!(w["enabled"], true);
    let line = plan_line(w);
    assert!(line.contains("worker_requests usage passes 100"), "{line}");
    assert!(billing_notes(&listed, &ok).is_empty());

    // A product not listed: the error names the allowed IDs.
    let err = validate_billing(&listed, &json!({ "nope": "1" })).unwrap_err();
    assert!(
        err.contains("worker_requests") && err.contains("r2_storage"),
        "{err}"
    );

    // Bad limits and bad product shapes are errors.
    for bad in ["-1", "1e3", "", "abc", "1.", "123456789012345678901"] {
        assert!(
            validate_billing(&listed, &json!({ "worker_requests": bad })).is_err(),
            "{bad}"
        );
    }
    assert!(validate_billing(&listed, &json!({ "worker_requests": 100 })).is_err());
    assert!(validate_billing(&unlisted, &json!({ "Bad-Name": "1" })).is_err());
    let many: serde_json::Map<String, serde_json::Value> =
        (0..11).map(|i| (format!("p{i}"), json!("1"))).collect();
    assert!(validate_billing(&unlisted, &json!(many)).is_err());

    // No listed IDs: any well-formed product passes, with a note.
    let free = validate_billing(&unlisted, &json!({ "worker_requests": "100" })).unwrap();
    assert_eq!(billing_notes(&unlisted, &free).len(), 1);

    // An existing policy with another limit: one update with a filters diff, no create.
    let wanted =
        billing_policies(&validate_billing(&listed, &json!({ "worker_requests": "100" })).unwrap());
    let updates = plan_monitor_updates(&wanted, &existing);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["id"], "b1");
    assert_eq!(
        updates[0]["changes"]["filters"]["to"],
        json!({ "product": ["worker_requests"], "limit": ["100"] })
    );
    assert!(update_line(&updates[0]).contains("pigeon: billing worker_requests"));
    // Same limit: nothing to do.
    let same =
        billing_policies(&validate_billing(&listed, &json!({ "worker_requests": "50" })).unwrap());
    assert!(plan_monitor_updates(&same, &existing).is_empty());

    // The billing type absent from the account, with `billing` given: an error.
    let err = validate_billing(&absent, &json!({ "worker_requests": "100" })).unwrap_err();
    assert!(err.contains("billing_usage_alert"), "{err}");
    // Absent without `billing` is no error.
    assert!(validate_billing(&absent, &json!(null)).unwrap().is_empty());
}

#[test]
fn core_observability_errors_args_and_folding() {
    use pigeon_capabilities::observability::*;
    use serde_json::json;

    // Arguments: defaults, caps and refusals.
    let a = errors_args(&json!({})).unwrap();
    assert_eq!((a.worker.clone(), a.hours, a.limit), (None, 24, 5));
    let a = errors_args(&json!({ "worker": "core", "hours": 9999, "limit": 99 })).unwrap();
    assert_eq!(
        (a.worker.as_deref(), a.hours, a.limit),
        (Some("core"), 168, 20)
    );
    assert_eq!(
        errors_args(&json!({ "hours": 0, "limit": 0 }))
            .unwrap()
            .hours,
        1
    );
    for bad in [
        json!({ "worker": 3 }),
        json!({ "worker": "" }),
        json!({ "hours": "x" }),
        json!({ "limit": -1 }),
    ] {
        assert!(errors_args(&bad).is_err(), "{bad}");
    }

    // Folding: counts, by_status and capped messages, per Worker.
    let metrics = fold_invocations(
        &json!({ "data": { "viewer": { "accounts": [{ "workersInvocationsAdaptive": [
        { "sum": { "requests": 10.0, "errors": 2.0 }, "quantiles": {},
          "dimensions": { "scriptName": "core", "status": "exception" } } ] }] } } }),
    );
    let rows: Vec<_> = (0..4)
        .map(|i| {
            json!({ "count": i as f64, "groups": [
            { "key": "$metadata.service", "value": "core" },
            { "key": "$metadata.message", "value": format!("m{i} {}", "x".repeat(300)) } ] })
        })
        .collect();
    let messages = fold_errors_top(&json!({ "calculations": [{ "aggregates": rows }] }), 2);
    let known = vec!["core".to_string(), "pigeon-proxy".to_string()];
    let ask = errors_args(&json!({ "worker": "core", "limit": 2 })).unwrap();
    let out = errors_answer(&metrics, &messages, &known, &ask);
    assert_eq!(out["workers"].as_array().unwrap().len(), 1);
    assert_eq!(out["workers"][0]["errors"], 2.0);
    assert_eq!(out["workers"][0]["by_status"]["exception"], 10.0);
    let top = out["workers"][0]["top_errors"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    assert!(top[0]["message"].as_str().unwrap().chars().count() <= 200);
    assert!(
        out["run_filter"]
            .as_str()
            .unwrap()
            .contains("no run filter")
    );
    assert!(out.get("known_workers").is_none());

    // An unknown Worker is an empty result that lists the known names, not an error.
    let ask = errors_args(&json!({ "worker": "nope" })).unwrap();
    let out = errors_answer(&metrics, &messages, &known, &ask);
    assert!(out["workers"].as_array().unwrap().is_empty());
    assert_eq!(out["known_workers"], json!(["core", "pigeon-proxy"]));

    // No worker: every known Worker, quiet ones included.
    let all = errors_answer(
        &metrics,
        &messages,
        &known,
        &errors_args(&json!({})).unwrap(),
    );
    assert_eq!(all["workers"].as_array().unwrap().len(), 2);
}

/// `cloudflare_calls` answers totals by class and each path with its classes, most called first,
/// within `limit`; its `hours` and `limit` are clamped.
#[test]
fn core_cloudflare_calls_tool_shape() {
    use pigeon_capabilities::observability::{CALLS_TOOL, Observability, calls_args};
    use pigeon_core::api_calls::{Count, view};
    let map = pigeon_core::lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
    let obs = Observability { map };
    let tool = pigeon_capabilities::Capability::tools(&obs)
        .into_iter()
        .find(|t| t.name == CALLS_TOOL)
        .expect("the tool is listed");
    assert!(tool.description.starts_with("Read-only."));
    assert_eq!(calls_args(&json!({})), (24, 50));
    assert_eq!(calls_args(&json!({ "hours": 999, "limit": 0 })), (168, 1));

    let count = |method: &str, path: &str, class: &str, n: u64| Count {
        method: method.into(),
        path: path.into(),
        class: class.into(),
        n,
    };
    let rows = vec![
        count("DELETE", "/accounts/{id}/tokens/{id}", "2xx", 40),
        count("GET", "/accounts/{id}/tokens", "2xx", 9),
        count("GET", "/accounts/{id}/tokens", "429", 3),
        count("POST", "/accounts/{id}/tokens", "2xx", 41),
        count("POST", "/accounts/{id}/tokens", "5xx", 1),
        count("GET", "/accounts/{id}/workers/services/{id}", "429", 2),
    ];
    let v = view(&rows, 24, 3);
    assert_eq!(v["hours"], 24);
    assert_eq!(v["total"], 96);
    assert_eq!(
        v["by_class"],
        json!({ "2xx": 90, "4xx": 0, "429": 5, "5xx": 1, "other": 0, "error": 0 })
    );
    assert_eq!(v["paths_shown"], 3);
    let paths = v["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 3);
    assert_eq!(
        paths[0],
        json!({ "method": "POST", "path": "/accounts/{id}/tokens", "total": 42,
                "2xx": 41, "4xx": 0, "429": 0, "5xx": 1, "other": 0, "error": 0 })
    );
    assert_eq!(paths[1]["method"], "DELETE");
    assert_eq!(
        (
            paths[2]["method"].as_str(),
            paths[2]["total"].as_u64(),
            paths[2]["429"].as_u64()
        ),
        (Some("GET"), Some(12), Some(3))
    );
}

/// `observability_summary` mints one token holding its three sources' permissions, and revokes
/// it once, whether the sources answered or failed.
#[test]
fn core_observability_summary_mints_one_token() {
    use super::core::token_block_on;
    use pigeon_capabilities::observability::{OneToken, SUMMARY_PERMISSIONS, with_one_token};
    use std::cell::RefCell;
    #[derive(Default)]
    struct Counting {
        opened: RefCell<Vec<Vec<String>>>,
        closed: RefCell<Vec<String>>,
        refuse: bool,
    }
    impl OneToken for Counting {
        async fn open(&self, permissions: &[&str]) -> worker::Result<(Option<String>, String)> {
            if self.refuse {
                return Err(worker::Error::RustError("no capacity".into()));
            }
            self.opened
                .borrow_mut()
                .push(permissions.iter().map(|p| p.to_string()).collect());
            let n = self.opened.borrow().len();
            Ok((Some(format!("id-{n}")), format!("value-{n}")))
        }
        async fn close(&self, id: &str) -> worker::Result<()> {
            self.closed.borrow_mut().push(id.to_string());
            Ok(())
        }
    }
    let mut want: Vec<&str> = SUMMARY_PERMISSIONS.to_vec();
    want.sort_unstable();
    assert_eq!(
        want,
        [
            "Account Analytics Read",
            "Billing Read",
            "Workers Observability Write"
        ]
    );

    // Three sources, as the summary runs them, all under one token.
    let api = Counting::default();
    let three = |t: String| async move {
        let source = |name: &str| format!("{name} as {t}");
        (
            Ok::<_, String>(source("metrics")),
            Ok::<_, String>(source("logs")),
            source("billing"),
        )
    };
    let shared = token_block_on(with_one_token(&api, &SUMMARY_PERMISSIONS, three)).unwrap();
    assert_eq!(api.opened.borrow().len(), 1);
    assert_eq!(api.opened.borrow()[0].len(), 3);
    assert_eq!(*api.closed.borrow(), ["id-1"]);
    assert_eq!(shared.value.0.unwrap(), "metrics as value-1");
    assert_eq!(shared.value.2, "billing as value-1");
    assert!(shared.revoke_failed.is_none());

    // Sources that fail still revoke the token.
    let failing = |_t: String| async {
        (
            Err::<(), _>("metrics: 500".to_string()),
            Err::<(), _>("logs: 500".to_string()),
        )
    };
    let shared = token_block_on(with_one_token(&api, &SUMMARY_PERMISSIONS, failing)).unwrap();
    assert!(shared.value.0.is_err() && shared.value.1.is_err());
    assert_eq!(api.opened.borrow().len(), 2);
    assert_eq!(*api.closed.borrow(), ["id-1", "id-2"]);

    // A mint that fails runs nothing and revokes nothing.
    let refusing = Counting {
        refuse: true,
        ..Counting::default()
    };
    let ran = std::cell::Cell::new(false);
    let err = token_block_on(with_one_token(
        &refusing,
        &SUMMARY_PERMISSIONS,
        |_t| async {
            ran.set(true);
        },
    ))
    .err()
    .unwrap();
    assert!(err.contains("not minted"), "{err}");
    assert!(!ran.get() && refusing.closed.borrow().is_empty());
}
