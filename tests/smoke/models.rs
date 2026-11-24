//! The model gateway: requests, turns, cache tokens and cost.

use super::*;

#[test]
fn models_request() {
    use pigeon_models::jobs::{self, State};
    use pigeon_models::stream::Reading;
    use pigeon_models::{Dialect, Provider, ask};
    let provider = Provider {
        name: "anthropic",
        gateway: "pigeon",
        route: pigeon_models::Route::Byok,
        slug: "anthropic",
        alias: "default",
        dialect: Dialect::Anthropic,
        ids: pigeon_models::ModelIds::Claude,
        default_model: "claude-haiku-5-5",
        key_help: "",
        billing: None,
    };
    // The request, in the common shape and then as the provider takes it.
    let a = ask(
        &json!({ "prompt": "hi", "system": "be brief", "max_tokens": 99_999 }),
        &provider,
    )
    .unwrap();
    assert_eq!(a.model, "claude-haiku-5-5");
    assert_eq!(a.max_tokens, 64_000);
    let body = Dialect::Anthropic.body(&a);
    assert_eq!(
        body["messages"],
        json!([{ "role": "user", "content": "hi" }])
    );
    assert_eq!(body["system"], "be brief");
    assert!(ask(&json!({}), &provider).is_err());
    assert_eq!(Dialect::Anthropic.path(), "v1/messages");

    // The stubbed upstream streams its answer, split anywhere.
    let stream = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-haiku-5-5\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hidden\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    for chunk in [1, 7, 4096] {
        let mut r = Reading::default();
        for piece in stream.as_bytes().chunks(chunk) {
            r.feed(Dialect::Anthropic, piece);
        }
        r.end(Dialect::Anthropic);
        let reply = r.reply();
        assert_eq!(reply.text, "Hello", "chunks of {chunk}");
        assert_eq!((reply.tokens_in, reply.tokens_out), (Some(12), Some(7)));
        assert!(r.done && r.error.is_none());
    }
    // A mid-stream error is kept; junk is ignored.
    let mut r = Reading::default();
    r.feed(Dialect::Anthropic, b": c\ndata: not json\n\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\r\n\r\n");
    assert_eq!(r.error.as_deref(), Some("overloaded_error: Overloaded"));
    // A whole (non-streamed) answer reads the same; a non-answer is an error.
    let whole = json!({ "model": "m", "content": [{ "type": "text", "text": "ok" }], "stop_reason": "end_turn", "usage": { "input_tokens": 1, "output_tokens": 1 } });
    assert_eq!(Dialect::Anthropic.reply(&whole).unwrap().text, "ok");
    assert!(Dialect::Anthropic.reply(&json!({ "error": {} })).is_err());

    // As an async job: queued, running, succeeded; kept, and read only by its sender.
    assert_eq!(jobs::timeout_ms(&Value::Null), Ok(10 * 60_000));
    assert!(jobs::timeout_ms(&json!(15)).is_err());
    assert!(jobs::may_submit(jobs::MAX_OPEN).is_err());
    assert_eq!(jobs::to_start("default", jobs::MAX_RUNNING, 3), 0);
    assert!(
        jobs::allowed(State::Queued, State::Running)
            && jobs::allowed(State::Running, State::Succeeded)
    );
    assert!(!jobs::allowed(State::Succeeded, State::Running));
    assert!(jobs::lost(State::Running, Some(0), jobs::LOST_AFTER_MS + 1));
    assert!(jobs::may_read(Some("u1"), None, "u1") && jobs::may_read(Some("s1"), Some("p1"), "p1"));
    assert!(!jobs::may_read(Some("u1"), None, "u2"));
    assert_eq!(jobs::expires_at(0, Some(5)), 5 + jobs::KEPT_AFTER_READ_MS);
    let note = jobs::notify_body(
        "j1",
        State::Succeeded,
        "anthropic",
        "m",
        Some("policy-1"),
        Some(1),
        Some(2),
        0,
    );
    assert!(!note.to_string().contains("Hello"));
}

/// An AI agent's turn as a model job (docs/plans/ai-agents.md): the whole request checked, its stream
/// read into content blocks and cache usage, its lane and its cancel.
#[test]
fn models_turns_blocks_and_lanes() {
    use pigeon_models::Dialect;
    use pigeon_models::jobs::{self, State};
    use pigeon_models::stream::Reading;
    use pigeon_models::turn::{self, MAX_TOKENS};

    // The request: checked, given its model and bounds, always streamed.
    let req = json!({
        "system": [{ "type": "text", "text": "rules", "cache_control": { "type": "ephemeral" } }],
        "tools": [{ "name": "read_file", "input_schema": { "type": "object" } }],
        "messages": [{ "role": "user", "content": "go" }],
        "thinking": { "type": "adaptive" }
    });
    let t = turn::turn(
        &req,
        &json!(["compact-2026-01-12", "compact-2026-01-12"]),
        "claude-haiku-5-5",
        pigeon_models::Dialect::Anthropic,
        pigeon_models::ModelIds::Claude,
    )
    .unwrap();
    assert_eq!(t.model, "claude-haiku-5-5");
    assert_eq!(t.body["stream"], true);
    assert_eq!(t.body["max_tokens"], turn::DEFAULT_MAX_TOKENS);
    assert_eq!(t.beta.as_deref(), Some("compact-2026-01-12"));
    let refused = |r: Value, betas: Value| {
        turn::turn(
            &r,
            &betas,
            "claude-haiku-5-5",
            pigeon_models::Dialect::Anthropic,
            pigeon_models::ModelIds::Claude,
        )
        .unwrap_err()
    };
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }], "stream": false }),
            Value::Null
        )
        .contains("stream")
    );
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }], "service_tier": "x" }),
            Value::Null
        )
        .contains("service_tier")
    );
    assert!(refused(json!({ "messages": [] }), Value::Null).contains("empty"));
    assert!(
        refused(
            json!({ "messages": [{ "role": "system", "content": "x" }] }),
            Value::Null
        )
        .contains("messages[0]")
    );
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }], "model": "gpt-9" }),
            Value::Null
        )
        .contains("Claude")
    );
    assert!(refused(json!({ "messages": [{ "role": "user", "content": "x" }], "max_tokens": MAX_TOKENS + 1 }), Value::Null).contains("max_tokens"));
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }] }),
            json!(["files-api"])
        )
        .contains("files-api")
    );
    let big = "x".repeat(turn::MAX_BYTES);
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": big }] }),
            Value::Null
        )
        .contains("4 MiB")
    );

    // The stream: thinking with its signature, text, a tool call's input in pieces, cache usage.
    let stream = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1,\"cache_read_input_tokens\":1201,\"cache_creation_input_tokens\":231}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Reading.\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"src/\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"lib.rs\\\"}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"list\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":3}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":40}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    for chunk in [1, 13, 4096] {
        let mut r = Reading::default();
        for piece in stream.as_bytes().chunks(chunk) {
            r.feed(Dialect::Anthropic, piece);
        }
        r.end(Dialect::Anthropic);
        assert!(
            r.done && r.error.is_none(),
            "chunks of {chunk}: {:?}",
            r.error
        );
        assert_eq!(r.text, "Reading.");
        let c = r.content();
        assert_eq!(
            c[0],
            json!({ "type": "thinking", "thinking": "plan", "signature": "sig" })
        );
        assert_eq!(c[1], json!({ "type": "text", "text": "Reading." }));
        assert_eq!(c[2]["input"], json!({ "path": "src/lib.rs" }));
        assert_eq!(c[3]["input"], json!({}));
        assert_eq!(r.stop_reason.as_deref(), Some("tool_use"));
        let u = r.usage();
        assert_eq!(
            (
                u["cache_read_input_tokens"].as_u64(),
                u["cache_creation_input_tokens"].as_u64()
            ),
            (Some(1201), Some(231))
        );
        assert_eq!(u["output_tokens"], 40);
    }
    // A tool input that is not JSON is an error; an absurd block index too.
    let mut r = Reading::default();
    r.feed(Dialect::Anthropic, b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"x\",\"input\":{}}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{oops\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
    assert!(r.error.as_deref().is_some_and(|e| e.contains("not JSON")));
    let mut r = Reading::default();
    r.feed(Dialect::Anthropic, b"data: {\"type\":\"content_block_start\",\"index\":99999,\"content_block\":{\"type\":\"text\"}}\n\n");
    assert!(r.error.is_some() && r.blocks.is_empty());

    // Lanes: each with its own cap; an unknown one refused.
    assert_eq!(jobs::lane(&Value::Null), Ok("default"));
    assert_eq!(jobs::lane(&json!("ai-agents")), Ok("ai-agents"));
    assert!(jobs::lane(&json!("fast")).is_err());
    assert_eq!(jobs::to_start("ai-agents", jobs::MAX_RUNNING, 10), 10);
    assert_eq!(jobs::to_start("ai-agents", 10, 10), 6);
    assert_eq!(jobs::to_start("default", jobs::MAX_RUNNING, 10), 0);
    assert_eq!(jobs::to_start("nope", 0, 10), 0);
    // Cancel: from queued or running, never from an end.
    assert!(
        jobs::allowed(State::Queued, State::Cancelled)
            && jobs::allowed(State::Running, State::Cancelled)
    );
    assert!(!jobs::allowed(State::Cancelled, State::Running));
    assert!(
        jobs::may_cancel(State::Running).is_ok() && jobs::may_cancel(State::Succeeded).is_err()
    );
    assert_eq!(State::named("cancelled"), Some(State::Cancelled));
    // Retries before the first byte: overload and rate limits, a few times; never a bad request.
    assert!(jobs::retryable(529) && jobs::retryable(429) && !jobs::retryable(400));
    assert_eq!(
        (jobs::retry_wait_ms(1), jobs::retry_wait_ms(4)),
        (Some(1_000), None)
    );
}

/// A model job whose token the AI gateway refuses (401, code 2009: what killed every AI agent
/// run in flight while pigeon-model-gateway deployed) asks core for a new token and sends again
/// after a wait, a few times; the provider's own 401 and other refusals are not taken for it.
#[test]
fn models_gateway_refused_token_retried() {
    use pigeon_core::ai_gateway;
    use pigeon_models::jobs;
    let refused = json!({ "code": 2009, "message": "Unauthorized", "name": "AiGatewayError" });
    assert!(ai_gateway::refused_token(401, &refused));
    assert!(ai_gateway::refused_token(401, &json!([refused.clone()])));
    assert!(ai_gateway::refused_token(
        401,
        &json!({ "success": false, "errors": [{ "code": 2009, "message": "Unauthorized" }] })
    ));
    assert!(!ai_gateway::refused_token(500, &refused));
    assert!(!ai_gateway::refused_token(
        401,
        &json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } })
    ));
    assert!(!ai_gateway::refused_token(401, &json!("Unauthorized")));
    // Not one of the provider's retryable refusals: it has its own waits.
    assert!(!jobs::retryable(401));
    assert_eq!(
        (
            jobs::fresh_token_wait_ms(1),
            jobs::fresh_token_wait_ms(3),
            jobs::fresh_token_wait_ms(4)
        ),
        (Some(2_000), Some(30_000), None)
    );
}

/// The model gateway's cache tokens (docs/plans/observability.md, Open item 4): their prices
/// equal the AI agents' budget, a usage with and without the 5-minute/1-hour split is priced as
/// the budget prices it, and cache rows merge into the gateway's groups by key.
#[test]
fn models_cache_tokens_priced() {
    use pigeon_ai_agents::budget;
    use pigeon_core::ai_gateway::{Dimension, Filter, Usage};
    use pigeon_models::cache::{self, Row, Tokens};

    for m in [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable-5-2",
        "claude-sonnet-5-5-20261101",
        "gpt-9",
    ] {
        let (a, b) = (cache::price(m), budget::price(m));
        assert_eq!(
            (a.cache_read, a.cache_write_5m, a.cache_write_1h),
            (b.cache_read, b.cache_write_5m, b.cache_write_1h),
            "{m}"
        );
    }

    // With the split, and without it (all writes are 5-minute writes). Cache parts only.
    let split = json!({
        "input_tokens": 0, "output_tokens": 0,
        "cache_read_input_tokens": 1_000_000,
        "cache_creation_input_tokens": 300_000,
        "cache_creation": { "ephemeral_5m_input_tokens": 100_000, "ephemeral_1h_input_tokens": 200_000 }
    });
    let plain = json!({
        "cache_read_input_tokens": 1_000_000,
        "cache_creation_input_tokens": 300_000
    });
    for m in [
        "claude-opus-5-5",
        "claude-haiku-5-5",
        "claude-fable-5-2",
        "gpt-9",
    ] {
        for u in [&split, &plain] {
            assert_eq!(
                cache::cost_micros(m, cache::tokens(u)),
                budget::cost_micros(m, u)
            );
        }
    }
    assert_eq!(
        cache::tokens(&split),
        Tokens {
            read: 1_000_000,
            write_5m: 100_000,
            write_1h: 200_000
        }
    );
    assert_eq!(
        cache::tokens(&plain),
        Tokens {
            read: 1_000_000,
            write_5m: 300_000,
            write_1h: 0
        }
    );

    // The stream keeps the split when the provider sends it.
    let mut r = pigeon_models::stream::Reading::default();
    let ev = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"cache_read_input_tokens\":7,\"cache_creation_input_tokens\":30,\"cache_creation\":{\"ephemeral_5m_input_tokens\":10,\"ephemeral_1h_input_tokens\":20}}}}\n\n";
    r.feed(pigeon_models::Dialect::Anthropic, ev.as_bytes());
    assert_eq!(
        cache::tokens(&r.usage()),
        Tokens {
            read: 7,
            write_5m: 10,
            write_1h: 20
        }
    );
    assert_eq!(r.usage()["cache_creation_input_tokens"], 30);

    // The merge: one gateway group with cache rows, and a cache-only group.
    let tags = |task: &str| vec![("task".to_string(), task.to_string())];
    let rows: Vec<Row> = [
        ("claude-haiku-5-5", "ai-agent/a", &split),
        ("anthropic/claude-haiku.5-5", "ai-agent/b", &plain),
        ("claude-opus-5-5", "ai-agent/a", &plain),
    ]
    .iter()
    .map(|(m, t, u)| Row::new(1, "anthropic", m, &tags(t), u).unwrap())
    .collect();
    assert!(
        Row::new(
            1,
            "anthropic",
            "claude-haiku-5-5",
            &[],
            &json!({ "input_tokens": 5 })
        )
        .is_none()
    );
    let haiku = |m: &str| Usage {
        group: [
            ("provider".to_string(), Some("anthropic".to_string())),
            ("model".to_string(), Some(m.to_string())),
        ]
        .into_iter()
        .collect(),
        requests: 3,
        tokens_in: 10,
        cost: Some(0.5),
        ..Usage::default()
    };
    let by = [Dimension::Model];
    let (groups, all) = cache::merge(vec![haiku("claude-haiku-5-5")], &rows, &by);
    let micros = |m: &str, u: &Value| cache::cost_micros(m, cache::tokens(u));
    let h = micros("claude-haiku-5-5", &split) + micros("claude-haiku-5-5", &plain);
    let o = micros("claude-opus-5-5", &plain);
    assert_eq!(groups.len(), 2);
    assert_eq!(all.micros, h + o);
    assert_eq!((all.read, all.write), (3_000_000, 900_000));
    let find = |m: &str| groups.iter().find(|g| g["model"] == m).unwrap();
    let (gh, go) = (find("claude-haiku-5-5"), find("claude-opus-5-5"));
    assert_eq!(gh["cost"], 0.5);
    assert_eq!(gh["requests"], 3);
    assert_eq!(gh["cache_read_tokens"], 2_000_000);
    assert_eq!(gh["cache_write_tokens"], 600_000);
    // The gateway's cost already includes the cache: no cache cost is added.
    assert!(gh.get("cache_cost").is_none());
    assert_eq!(gh["cost_with_cache"], 0.5);
    assert_eq!(
        (go["requests"].as_u64(), go["cost"].is_null()),
        (Some(0), true)
    );
    assert_eq!(go["cost_with_cache"], 0.0);
    assert!(go.get("cache_cost").is_none());
    // Grouped by task, the keys are the task tag's.
    let (by_task, _) = cache::merge(vec![], &rows, &[Dimension::Task]);
    assert_eq!(by_task.len(), 2);
    assert!(
        by_task
            .iter()
            .any(|g| g["task"] == "ai-agent/a" && g["cache_read_tokens"] == 2_000_000)
    );
    // Merging two rows of one key equals merging their sum, as the SQL sum feeds it.
    let (a, b) = (rows[0].clone(), rows[2].clone());
    let b = Row {
        model: a.model.clone(),
        ..b
    };
    let sum = Row {
        cache_read: a.cache_read + b.cache_read,
        cache_write_5m: a.cache_write_5m + b.cache_write_5m,
        cache_write_1h: a.cache_write_1h + b.cache_write_1h,
        cost_micros: a.cost_micros + b.cost_micros,
        ..a.clone()
    };
    assert_eq!(
        cache::merge(vec![], &[a, b], &by),
        cache::merge(vec![], &[sum], &by)
    );

    // The filter: provider, model in either form, caller, task, task prefix; failures keep none.
    let keep = |f: &Filter, p: Option<&str>| rows.iter().filter(|r| cache::keeps(r, f, p)).count();
    assert_eq!(keep(&Filter::default(), None), 3);
    let f = |m: &str| Filter {
        model: Some(m.into()),
        ..Filter::default()
    };
    assert_eq!(keep(&f("anthropic/claude-haiku-5.5"), None), 2);
    assert_eq!(
        keep(
            &Filter {
                task: Some("ai-agent/a".into()),
                ..Filter::default()
            },
            None
        ),
        2
    );
    assert_eq!(keep(&Filter::default(), Some("ai-agent/")), 3);
    assert_eq!(keep(&Filter::default(), Some("ci/")), 0);
    assert_eq!(
        keep(
            &Filter {
                failed: true,
                ..Filter::default()
            },
            None
        ),
        0
    );
    assert_eq!(
        keep(
            &Filter {
                provider: Some("openai".into()),
                ..Filter::default()
            },
            None
        ),
        0
    );
}

/// The gateway's own per-request cost: one price table for the budget and the cache rows,
/// `models_cost` checks and answer, and `models_usage` no longer adding cache cost.
#[test]
fn models_gateway_log_cost() {
    use pigeon_ai_agents::budget;
    use pigeon_core::ai_gateway::{Log, LogLookup, cost_answer};
    use pigeon_core::model_prices;
    use pigeon_models::cache;

    // One table: budget, cache and core agree, and canonical names match.
    for m in [
        "claude-opus-5-5",
        "anthropic/claude-opus-5.5",
        "claude-sonnet-5-5-20261101",
        "Anthropic/Claude-Haiku-5.5",
        "claude-fable-5-2",
        "gpt-9",
    ] {
        let core = model_prices::price(m);
        assert_eq!(budget::price(m), core, "{m}");
        assert_eq!(cache::price(m), core, "{m}");
    }
    assert_eq!(
        model_prices::price("anthropic/claude-opus-5.5"),
        model_prices::price("claude-opus-5-5")
    );
    assert_ne!(
        model_prices::price("anthropic/claude-opus-5.5"),
        model_prices::price("gpt-9")
    );
    let usage = json!({ "input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 30 });
    assert_eq!(
        budget::cost_micros("anthropic/claude-haiku-5.5", &usage),
        model_prices::cost_micros("claude-haiku-5-5", &usage)
    );
    assert_eq!(
        budget::estimate_micros("claude-haiku-5-5", 1_000, 2_000),
        model_prices::estimate_micros("claude-haiku-5-5", 1_000, 2_000)
    );

    // models_cost input: 1 to 50 ids of [A-Za-z0-9_-], 1 to 100 characters.
    let ok = pigeon_models::log_ids(&json!({ "log_ids": ["a-b_C9", "0123"] })).unwrap();
    assert_eq!(ok, ["a-b_C9", "0123"]);
    let fifty: Vec<String> = (0..50).map(|n| format!("id{n}")).collect();
    assert!(pigeon_models::log_ids(&json!({ "log_ids": fifty })).is_ok());
    let fifty_one: Vec<String> = (0..51).map(|n| format!("id{n}")).collect();
    for bad in [
        json!({}),
        json!({ "log_ids": "abc" }),
        json!({ "log_ids": [] }),
        json!({ "log_ids": fifty_one }),
        json!({ "log_ids": [""] }),
        json!({ "log_ids": ["a".repeat(101)] }),
        json!({ "log_ids": ["a/b"] }),
        json!({ "log_ids": ["a b"] }),
        json!({ "log_ids": ["a?x=1"] }),
        json!({ "log_ids": ["../x"] }),
        json!({ "log_ids": [7] }),
    ] {
        assert!(pigeon_models::log_ids(&bad).is_err(), "{bad}");
    }
    assert!(pigeon_models::log_ids(&json!({ "log_ids": ["a".repeat(100)] })).is_ok());
    assert!(pigeon_models::TOOLS.contains(&"models_cost"));
    assert_eq!(
        pigeon_models::purposes("models_cost"),
        pigeon_models::purposes("models_usage")
    );

    // The answer: found, found without a cost, not found, failed; in request order.
    let ids: Vec<String> = ["one", "two", "three", "four"].map(String::from).to_vec();
    let found = Log {
        model: "anthropic/claude-opus-5.5".into(),
        tokens_in: Some(2),
        tokens_out: Some(234),
        cost: Some(0.83),
        ..Log::default()
    };
    let free = Log {
        model: "m".into(),
        ..Log::default()
    };
    let lookups = [
        LogLookup::Found(found),
        LogLookup::NotFound,
        LogLookup::Found(free),
        LogLookup::Failed("boom".into()),
    ];
    let answer = cost_answer(&ids, &lookups);
    let logs = answer["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 4);
    assert_eq!(
        logs.iter().map(|l| l["log_id"].clone()).collect::<Vec<_>>(),
        ["one", "two", "three", "four"]
    );
    assert_eq!(logs[0]["found"], true);
    assert_eq!(logs[0]["cost_usd"], 0.83);
    assert_eq!(logs[0]["model"], "anthropic/claude-opus-5.5");
    assert_eq!(
        (
            logs[0]["tokens_in"].as_u64(),
            logs[0]["tokens_out"].as_u64()
        ),
        (Some(2), Some(234))
    );
    assert_eq!(
        (logs[1]["found"].clone(), logs[1]["cost_usd"].is_null()),
        (json!(false), true)
    );
    assert!(logs[1].get("error").is_none());
    assert_eq!(
        (logs[2]["found"].clone(), logs[2]["cost_usd"].is_null()),
        (json!(true), true)
    );
    assert_eq!(logs[3]["found"], false);
    assert_eq!(logs[3]["error"], "boom");

    // models_usage: cost_with_cache equals cost, and there is no cache_cost.
    let usage = pigeon_core::ai_gateway::Usage {
        group: [
            ("provider".to_string(), Some("anthropic".to_string())),
            ("model".to_string(), Some("claude-haiku-5-5".to_string())),
        ]
        .into_iter()
        .collect(),
        requests: 1,
        cost: Some(0.25),
        ..Default::default()
    };
    let rows = [cache::Row::new(
        1,
        "anthropic",
        "claude-haiku-5-5",
        &[],
        &json!({ "cache_read_input_tokens": 1_000_000 }),
    )
    .unwrap()];
    let (groups, totals) = cache::merge(
        vec![usage],
        &rows,
        &[pigeon_core::ai_gateway::Dimension::Model],
    );
    assert_eq!(groups[0]["cost"], 0.25);
    assert_eq!(groups[0]["cost_with_cache"], groups[0]["cost"]);
    assert!(groups[0].get("cache_cost").is_none());
    assert_eq!(groups[0]["cache_read_tokens"], 1_000_000);
    assert_eq!(totals.read, 1_000_000);
}

/// A model id is checked by the rule of its provider: a Workers AI id is accepted there, an
/// OpenAI-style plain id is refused with `@` or `/`, and the refusal names the provider's rule.
#[test]
fn models_id_rule_per_provider() {
    use pigeon_models::{Dialect, ModelIds, turn};
    let msgs = json!([{ "role": "user", "content": "x" }]);
    let with = |model: &str, ids: ModelIds| {
        turn::turn(
            &json!({ "model": model, "messages": msgs }),
            &Value::Null,
            "gpt-6.2-sol",
            Dialect::OpenAi,
            ids,
        )
    };
    let kimi = "@cf/moonshotai/kimi-k2.7-code";
    assert_eq!(with(kimi, ModelIds::WorkersAi).unwrap().model, kimi);
    assert!(with("@cf/zai-org/glm-5.3", ModelIds::WorkersAi).is_ok());
    // OpenAI's plain rule refuses `@` and `/`, and says its own rule.
    let e = with(kimi, ModelIds::Plain).unwrap_err();
    assert!(e.starts_with(turn::REFUSED), "{e}");
    assert!(e.contains("plain model id"), "{e}");
    assert!(with("gpt-6.2-sol", ModelIds::Plain).is_ok());
    // Workers AI takes exactly `@cf/<org>/<model>`.
    for bad in [
        "gpt-6.2-sol",
        "@cf/moonshotai",
        "@cf/a/b/c",
        "cf/a/b",
        "@cf//b",
        "@cf/a/b c",
        "@cf/a/b:c",
    ] {
        let e = with(bad, ModelIds::WorkersAi).unwrap_err();
        assert!(e.contains("`@cf/<org>/<model>`"), "{bad}: {e}");
    }
    assert!(with(&format!("@cf/a/{}", "b".repeat(100)), ModelIds::WorkersAi).is_err());
}

/// The capability table shapes OpenAI requests: gpt-6-luna with tools on Chat Completions sends
/// reasoning effort `none`, and an unknown model gets the safe row (no effort, `auto` and `none`
/// only, chat).
#[test]
fn models_capability_table_openai_rows() {
    use pigeon_models::capabilities::{self, Endpoint};
    use pigeon_models::openai;
    let req = |model: &str, extra: Value| {
        let mut r = json!({
            "model": model,
            "tools": [{ "name": "f", "input_schema": {} }],
            "messages": [{ "role": "user", "content": "x" }],
            "output_config": { "effort": "high" }
        });
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        openai::request(&r).unwrap()
    };
    // Luna: tools and effort do not go together on chat; a dated name and a prefix are read.
    assert_eq!(req("gpt-6-luna", json!({}))["reasoning_effort"], "none");
    assert_eq!(
        req("gpt-6-luna-2026-10-01", json!({}))["reasoning_effort"],
        "none"
    );
    assert_eq!(req("gpt-6.2-sol", json!({}))["reasoning_effort"], "high");
    let no_tools = openai::request(&json!({
        "model": "gpt-6-luna", "output_config": { "effort": "low" },
        "messages": [{ "role": "user", "content": "x" }]
    }))
    .unwrap();
    assert_eq!(no_tools["reasoning_effort"], "low");
    // An unknown model: the safe row, so no effort and a forced choice becomes auto.
    let safe = capabilities::of("gpt-6-lunar");
    assert_eq!(safe, capabilities::SAFE);
    assert_eq!(
        capabilities::of("@cf/moonshotai/kimi-k2.7-code"),
        capabilities::SAFE
    );
    assert_eq!(safe.endpoint, Endpoint::Chat);
    assert!(safe.tools && !safe.effort && safe.auto && safe.none && !safe.any && !safe.tool);
    let out = req("mystery-1", json!({ "tool_choice": { "type": "any" } }));
    assert!(out.get("reasoning_effort").is_none());
    assert_eq!(out["tool_choice"], "auto");
    assert_eq!(
        req("mystery-1", json!({ "tool_choice": { "type": "none" } }))["tool_choice"],
        "none"
    );
    assert!(out["tools"].is_array());
}

/// A model that does not take `tool_choice` kinds `tool` and `any` gets `auto` on Anthropic, and
/// keeps `disable_parallel_tool_use`; a model that takes them keeps its choice.
#[test]
fn models_anthropic_tool_choice_downgraded() {
    use pigeon_models::anthropic;
    let req = |model: &str, choice: Value| {
        anthropic::request(&json!({
            "model": model,
            "max_tokens": 1000,
            "tools": [{ "name": "f", "input_schema": {} }],
            "tool_choice": choice,
            "messages": [{ "role": "user", "content": "x" }]
        }))
    };
    let forced = json!({ "type": "tool", "name": "f", "disable_parallel_tool_use": true });
    // Opus 5.5 takes auto and none only (Anthropic's tool use guide).
    let out = req("claude-opus-5-5", forced.clone());
    assert_eq!(
        out["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": true })
    );
    assert_eq!(
        req("claude-opus-5-5", json!({ "type": "any" }))["tool_choice"],
        json!({ "type": "auto" })
    );
    assert_eq!(
        req("claude-opus-5-5", json!({ "type": "none" }))["tool_choice"],
        json!({ "type": "none" })
    );
    // An unknown Claude gets the safe row.
    assert_eq!(
        req("claude-new-1", json!({ "type": "any" }))["tool_choice"],
        json!({ "type": "auto" })
    );
    // Sonnet 5.5 refuses forced tool use too; Haiku 5.5, Opus 5 and Sonnet 4.5 take every kind.
    assert_eq!(
        req("claude-sonnet-5-5", forced.clone())["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": true })
    );
    for m in [
        "claude-haiku-5-5",
        "claude-opus-5",
        "claude-sonnet-4-5-20250929",
    ] {
        assert_eq!(req(m, forced.clone())["tool_choice"], forced);
    }
    assert_eq!(out["max_tokens"], 1000);
}

/// A history that ends an assistant message in a thinking block is replayed to Anthropic without
/// it; a message left empty is dropped; other messages are unchanged.
#[test]
fn models_anthropic_replay_drops_trailing_thinking() {
    use pigeon_models::anthropic;
    let think = json!({ "type": "thinking", "thinking": "t", "signature": "s" });
    let redacted = json!({ "type": "redacted_thinking", "data": "d" });
    let text = json!({ "type": "text", "text": "hi" });
    let user = json!({ "role": "user", "content": "go" });
    let out = anthropic::request(&json!({
        "model": "claude-sonnet-5-5",
        "messages": [
            user.clone(),
            { "role": "assistant", "content": [text.clone(), think.clone(), redacted.clone()] },
            user.clone(),
            { "role": "assistant", "content": [think.clone()] },
            user.clone(),
            { "role": "assistant", "content": [think.clone(), text.clone()] },
        ]
    }));
    let m = out["messages"].as_array().unwrap();
    assert_eq!(m.len(), 5);
    assert_eq!(m[1]["content"], json!([text.clone()]));
    assert_eq!(m[2], user);
    assert_eq!(m[3], user);
    assert_eq!(m[4]["content"], json!([think, text]));
}
