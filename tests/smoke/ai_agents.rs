//! The AI agents Worker: loop, budgets, cost and the sandbox.

use super::*;

/// An AI agent run's logic (docs/plans/ai-agents.md, phase 2): its next action from what it knows,
/// recovery from a failed turn, budgets and prices, the tools each role is offered and their
/// checks, and the turn it sends.
#[test]
fn ai_agents_loop_budgets_tools_and_turns() {
    use pigeon_ai_agents::budget::{self, Budget, Next, Over, Spent};
    use pigeon_ai_agents::run::{self, Action, Call, Job, Recovery, State, View, Wait};
    use pigeon_ai_agents::tools::{self, Kind};
    use pigeon_ai_agents::transcript::{self, Spec};
    use pigeon_ai_agents::{Role, task};

    // ---- the loop ----
    let view = |state: State| View {
        state,
        stop_asked: false,
        paused: false,
        job: None,
        calls: &[],
        turns: 0,
        stop_reason: None,
        reported: None,
        nudges: 0,
        user_last: false,
        last_turn: false,
        waiting: None,
    };
    assert_eq!(run::next(&view(State::Queued)), Action::Prepare);
    assert_eq!(
        run::next(&view(State::Running)),
        Action::Turn,
        "the first turn"
    );
    assert_eq!(
        run::next(&View {
            stop_asked: true,
            ..view(State::Waiting)
        }),
        Action::End(State::Stopped)
    );
    assert_eq!(
        run::next(&View {
            paused: true,
            ..view(State::Running)
        }),
        Action::Check {
            after_ms: run::CHECK_EVERY_MS
        }
    );
    assert_eq!(run::next(&view(State::Done)), Action::Idle);
    // A turn in flight is polled, often at first; read once it succeeds; retried once it ends.
    assert_eq!(
        run::next(&View {
            job: Some((Job::Pending, 0)),
            ..view(State::Running)
        }),
        Action::Poll { after_ms: 1_000 }
    );
    assert_eq!(
        run::next(&View {
            job: Some((Job::Pending, 60_000)),
            ..view(State::Running)
        }),
        Action::Poll { after_ms: 5_000 }
    );
    assert_eq!(
        run::next(&View {
            job: Some((Job::Succeeded, 9)),
            ..view(State::Running)
        }),
        Action::Read
    );
    assert_eq!(
        run::next(&View {
            job: Some((Job::Ended, 9)),
            ..view(State::Running)
        }),
        Action::Retry
    );
    // Tool calls run in order; one interrupted is repeated only when that is safe.
    let call = |name: &str, started: bool, done: bool| Call {
        id: format!("t-{name}"),
        name: name.into(),
        started,
        done,
    };
    let calls = [call("read_file", true, true), call("run", false, false)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Call { index: 1 }
    );
    let calls = [call("grep", true, false)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Call { index: 0 }
    );
    let calls = [call("commit_push", true, false)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Interrupted { index: 0 }
    );
    // Results in: the next turn. A report ends the run, once its calls are answered.
    let calls = [call("run", true, true)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Turn
    );
    let calls = [call("report", true, true)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 3,
            reported: Some(State::Done),
            ..view(State::Running)
        }),
        Action::End(State::Done)
    );
    // Stopping without a tool or a report: nudged twice, then failed; cut off by max_tokens: go on.
    let quiet = View {
        turns: 2,
        stop_reason: Some("end_turn"),
        ..view(State::Running)
    };
    assert_eq!(run::next(&quiet), Action::Nudge);
    assert_eq!(
        run::next(&View {
            nudges: run::MAX_NUDGES,
            ..quiet.clone()
        }),
        Action::End(State::Failed)
    );
    assert_eq!(
        run::next(&View {
            stop_reason: Some("max_tokens"),
            ..quiet
        }),
        Action::Turn
    );
    // Waiting: approvals and answers are looked at often, executors less, the provider rarely.
    let approval = Wait::Approval {
        id: "a1".into(),
        call: "t1".into(),
    };
    assert_eq!(
        run::next(&View {
            waiting: Some(&approval),
            ..view(State::Waiting)
        }),
        Action::Check {
            after_ms: run::CHECK_EVERY_MS
        }
    );
    assert_eq!(
        run::next(&View {
            waiting: Some(&Wait::Executors { since: 0 }),
            ..view(State::Waiting)
        }),
        Action::Check {
            after_ms: run::EXECUTORS_EVERY_MS
        }
    );
    assert_eq!(
        run::next(&View {
            waiting: Some(&Wait::Provider { since: 0 }),
            ..view(State::Waiting)
        }),
        Action::Check {
            after_ms: run::PROVIDER_EVERY_MS
        }
    );
    // States move forward, and never out of an end.
    assert!(
        run::allowed(State::Running, State::Waiting)
            && run::allowed(State::Waiting, State::Running)
    );
    assert!(
        run::allowed(State::Queued, State::Stopped) && !run::allowed(State::Done, State::Running)
    );
    assert!(!run::allowed(State::Running, State::Queued));
    assert_eq!(State::named("over_budget"), Some(State::OverBudget));

    // ---- a failed turn ----
    let models = Role::Executor.models();
    assert_eq!(
        run::on_failure("failed", "anthropic answered 529: overloaded", 1, 0, models),
        Recovery::Again
    );
    assert_eq!(
        run::on_failure("failed", "overloaded_error: Overloaded", 2, 0, models),
        Recovery::Fallback { index: 1 }
    );
    assert_eq!(
        run::on_failure("lost", "", 2, 1, models),
        Recovery::WaitProvider
    );
    assert_eq!(
        run::on_failure(
            "failed",
            "anthropic answered 400: prompt is too long",
            1,
            0,
            models
        ),
        Recovery::Fail
    );
    assert_eq!(
        run::on_failure("cancelled", "cancelled by its sender", 1, 0, models),
        Recovery::Fail
    );
    // A deploy that changes the tool list makes the transcript's thinking signatures stale
    // (planner Zzn8ssZs4JsC, 2026-11-22): strip them once and go on, rather than end the run.
    let stale = "anthropic answered 400: {\"error\":{\"message\":\"messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. The `tools` list differs from the one this block was created with.\"}}";
    assert_eq!(
        run::on_failure("failed", stale, 1, 0, models),
        Recovery::StripThinking
    );
    assert_eq!(
        run::on_failure("failed", stale, 2, 0, models),
        Recovery::Fail
    );
    assert_eq!(task("r1", Role::Planner, 7), "ai-agent/r1/planner/7");

    // ---- budgets ----
    let b = Budget::asked(Role::Executor, &Value::Null).unwrap();
    // ai_agents_extend adds to a live run's limits, within the hard caps, and says what it added.
    let planner = Budget::default_for(Role::Planner);
    let (more, added) = budget::extend(
        Role::Planner,
        &planner,
        &json!({ "cost_usd": 10, "turns": 50, "wall_min": 60, "sandbox_min": 30, "executors": 4 }),
    )
    .unwrap();
    assert_eq!(more.cost_micros, planner.cost_micros + 10_000_000);
    assert_eq!(more.turns, planner.turns + 50);
    assert_eq!(more.wall_ms, planner.wall_ms + 60 * 60_000);
    assert_eq!(more.sandbox_ms, planner.sandbox_ms + 30 * 60_000);
    assert_eq!(more.executors, planner.executors + 4);
    assert_eq!(added["executors"], 4);
    let over = budget::extend(Role::Planner, &planner, &json!({ "cost_usd": 90 })).unwrap_err();
    assert!(
        over.contains("over the cap") && over.contains("left"),
        "{over}"
    );
    assert!(budget::extend(Role::Planner, &planner, &json!({ "turns": 301 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "wall_min": 961 })).is_err());
    assert!(budget::extend(Role::Executor, &b, &json!({ "executors": 1 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({})).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "turns": 0 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "cost_usd": -1 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "budget": 1 })).is_err());
    assert_eq!((b.turns, b.cost_micros), (60, 5_000_000));
    assert_eq!(
        Budget::asked(Role::Planner, &Value::Null)
            .unwrap()
            .executors,
        12
    );
    let b = Budget::asked(Role::Executor, &json!({ "cost_usd": 2.5, "wall_min": 30 })).unwrap();
    assert_eq!((b.cost_micros, b.wall_ms), (2_500_000, 30 * 60_000));
    assert!(
        Budget::asked(Role::Executor, &json!({ "cost_usd": 101 }))
            .unwrap_err()
            .contains("cap")
    );
    assert!(
        Budget::asked(Role::Executor, &json!({ "executors": 1 }))
            .unwrap_err()
            .contains("planner")
    );
    assert!(
        Budget::asked(Role::Executor, &json!({ "tokens": 1 }))
            .unwrap_err()
            .contains("tokens")
    );
    assert!(Budget::asked(Role::Executor, &json!({ "turns": 0 })).is_err());
    let planner = Budget::default_for(Role::Planner);
    let left = Spent::left(
        &planner,
        &Spent {
            cost_micros: 24_000_000,
            ..Spent::default()
        },
        0,
        1_000,
    );
    assert!(
        Budget::default_for(Role::Executor)
            .within(&left)
            .unwrap_err()
            .contains("cost")
    );
    let b = Budget::default_for(Role::Executor);
    let spent = Spent {
        turns: 59,
        cost_micros: 4_900_000,
        sandbox_ms: 0,
        executors: 0,
        ..Spent::default()
    };
    assert_eq!(
        budget::check(
            &b,
            &spent,
            0,
            1,
            Next::Turn {
                estimate_micros: 50_000
            }
        ),
        Ok(())
    );
    assert_eq!(
        budget::check(
            &b,
            &spent,
            0,
            1,
            Next::Turn {
                estimate_micros: 200_000
            }
        ),
        Err(Over::Cost)
    );
    assert_eq!(
        budget::check(&b, &Spent { turns: 60, ..spent }, 0, 1, Next::Tool),
        Ok(())
    );
    assert_eq!(
        budget::check(
            &b,
            &Spent { turns: 60, ..spent },
            0,
            1,
            Next::Turn { estimate_micros: 0 }
        ),
        Err(Over::Turns)
    );
    assert_eq!(
        budget::check(&b, &spent, 0, b.wall_ms, Next::Tool),
        Err(Over::Wall)
    );
    assert_eq!(
        budget::check(
            &b,
            &spent,
            0,
            1,
            Next::Sandbox {
                timeout_ms: b.sandbox_ms + 1
            }
        ),
        Err(Over::Sandbox)
    );
    assert_eq!(
        budget::check(&b, &spent, 0, 1, Next::Spawn { cost_micros: 0 }),
        Err(Over::Executors)
    );
    assert_eq!(Over::Turns.why(&b), "the run used its 60 model turns");
    // Prices: cache reads cheap, writes dear, an unknown model priced as the dearest.
    let usage = json!({ "input_tokens": 1_000_000, "output_tokens": 0, "cache_read_input_tokens": 1_000_000, "cache_creation_input_tokens": 1_000_000 });
    assert_eq!(
        budget::cost_micros("claude-sonnet-5-5", &usage),
        2_000_000 + 100_000 + 2_500_000
    );
    let split = json!({ "cache_creation_input_tokens": 2_000_000, "cache_creation": { "ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 1_000_000 } });
    assert_eq!(
        budget::cost_micros("claude-opus-5-5", &split),
        5_000_000 + 8_000_000
    );
    assert_eq!(
        budget::cost_micros("claude-haiku-5-5", &json!({ "output_tokens": 1 })),
        1,
        "rounded up"
    );
    assert_eq!(
        budget::price("claude-sonnet-5-5-20261101"),
        budget::price("claude-sonnet-5-5")
    );
    assert_eq!(
        budget::price("some-model"),
        budget::price("claude-fable-5-2")
    );
    assert_eq!(
        budget::estimate_micros("claude-opus-5-5", 100_000, 32_000),
        400_000 + 640_000
    );
    assert_eq!(budget::money(2_500_000), "$2.5");

    // ---- tools ----
    assert_eq!(tools::kind(Role::Executor, "run"), Some(Kind::Sandbox));
    assert_eq!(tools::kind(Role::Executor, "ci_submit"), Some(Kind::Pigeon));
    assert_eq!(
        tools::kind(Role::Executor, "land"),
        None,
        "executors never land"
    );
    assert_eq!(tools::kind(Role::Planner, "land"), Some(Kind::Pigeon));
    assert_eq!(tools::kind(Role::Executor, "spawn_executor"), None);
    assert_eq!(
        tools::kind(Role::Planner, "spawn_executor"),
        Some(Kind::Agent)
    );
    assert_eq!(tools::kind(Role::Planner, "deploy_token"), None);
    let pigeon = vec![
        (
            "ci_submit".to_string(),
            "Run a CI job.".to_string(),
            json!({ "type": "object" }),
        ),
        (
            "land".to_string(),
            "Land a branch.".to_string(),
            json!({ "type": "object" }),
        ),
        (
            "source_token".to_string(),
            "A git token.".to_string(),
            json!({ "type": "object" }),
        ),
    ];
    let names = |role| -> Vec<String> {
        tools::definitions(role, &pigeon)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let exec = names(Role::Executor);
    assert!(
        exec.contains(&"ci_submit".into())
            && !exec.contains(&"land".into())
            && !exec.contains(&"source_token".into())
    );
    assert!(
        names(Role::Planner).contains(&"land".into())
            && names(Role::Planner).contains(&"executor_stop".into())
    );
    assert!(
        tools::definitions(Role::Executor, &[])
            .iter()
            .all(|t| t["input_schema"]["type"] == "object")
    );
    let refused =
        |name: &str, input: Value| tools::check(Role::Executor, name, &input).unwrap_err();
    assert!(
        refused(
            "write_file",
            json!({ "path": ".pigeon/policy.toml", "content": "" })
        )
        .contains("only the person")
    );
    assert!(
        refused(
            "edit_file",
            json!({ "path": "./.claude//settings.json", "old": "a", "new": "b" })
        )
        .contains("only the person")
    );
    assert!(refused("read_file", json!({ "path": "../etc/passwd" })).contains(".."));
    assert!(refused("read_file", json!({ "path": "/etc/passwd" })).contains("relative"));
    assert!(refused("list", json!({ "path": ".git/config" })).contains(".git"));
    assert!(
        refused(
            "edit_file",
            json!({ "path": "a.rs", "old": "x", "new": "x" })
        )
        .contains("same")
    );
    assert!(refused("run", json!({ "cmd": "cargo test", "timeout_s": 900 })).contains("840"));
    assert!(refused("report", json!({ "state": "maybe", "summary": "s" })).contains("done"));
    assert!(
        refused("land", json!({ "branch": "claude/x" })).contains("not one of this run's tools")
    );
    assert!(
        tools::check(
            Role::Executor,
            "run",
            &json!({ "cmd": "cargo test -p pigeon-ai-agents" })
        )
        .is_ok()
    );
    assert!(
        tools::check(
            Role::Executor,
            "edit_file",
            &json!({ "path": "src/a.rs", "old": "x", "new": "y" })
        )
        .is_ok()
    );
    assert!(
        tools::idempotent("grep") && !tools::idempotent("commit_push") && !tools::idempotent("run")
    );

    // ---- the turn ----
    let defs = tools::definitions(Role::Planner, &pigeon);
    let messages = [json!({ "role": "user", "content": "the brief" })];
    let spec = Spec {
        role: Role::Planner,
        model: "claude-opus-5-5",
        system: "rules",
        tools: &defs,
    };
    let req = transcript::request(&spec, &messages);
    assert_eq!(
        req["system"][0]["cache_control"]["ttl"], "1h",
        "a planner waits: its prefix is kept an hour"
    );
    assert_eq!(
        req["tools"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(req["tools"][0].get("cache_control"), None);
    assert_eq!(
        (
            req["thinking"]["type"].as_str(),
            req["output_config"]["effort"].as_str()
        ),
        (Some("adaptive"), Some("high"))
    );
    assert_eq!(req["cache_control"]["type"], "ephemeral");
    let spec = Spec {
        role: Role::Mechanical,
        model: "claude-haiku-5-5",
        system: "rules",
        tools: &[],
    };
    let req = transcript::request(&spec, &messages);
    assert!(req.get("thinking").is_none() && req.get("tools").is_none());
    assert_eq!(req["system"][0]["cache_control"].get("ttl"), None);
    // The request a turn sends passes the gateway's own checks (pigeon_models::turn).
    let spec = Spec {
        role: Role::Executor,
        model: "claude-sonnet-5-5",
        system: "rules",
        tools: &defs,
    };
    assert!(
        pigeon_models::turn::turn(
            &transcript::request(&spec, &messages),
            &Value::Null,
            "claude-haiku-5-5",
            pigeon_models::Dialect::Anthropic,
            pigeon_models::ModelIds::Claude
        )
        .is_ok()
    );
    // Tool calls out, results back; long output cut in the middle, at char boundaries.
    let content = json!([{ "type": "text", "text": "x" }, { "type": "tool_use", "id": "t1", "name": "run", "input": { "cmd": "ls" } }, { "type": "tool_use", "name": "nameless" }]);
    assert_eq!(
        transcript::tool_uses(&content),
        vec![("t1".to_string(), "run".to_string(), json!({ "cmd": "ls" }))]
    );
    let msg = transcript::results_message(&[
        ("t1".into(), "ok".into(), false),
        ("t2".into(), "no".into(), true),
    ]);
    assert_eq!(msg["content"][0].get("is_error"), None);
    assert_eq!(msg["content"][1]["is_error"], true);
    let long = "é".repeat(20_000);
    let cut = transcript::cut(&long, Some("/tmp/out/3"));
    assert!(
        cut.len() < transcript::MAX_RESULT_BYTES
            && cut.contains("bytes cut; the whole output is in `/tmp/out/3`")
    );
    assert_eq!(transcript::cut("short", None), "short");
    // Compaction: when the context is large; cut only where no tool result loses its call.
    assert!(transcript::needs_compaction(
        &json!({ "input_tokens": 10, "cache_read_input_tokens": 150_000 })
    ));
    assert!(!transcript::needs_compaction(
        &json!({ "input_tokens": 10 })
    ));
    let mut history = vec![json!({ "role": "user", "content": "brief" })];
    for i in 0..30 {
        history.push(json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": format!("t{i}"), "name": "run", "input": {} }] }));
        history.push(json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": format!("t{i}"), "content": "ok" }] }));
    }
    assert_eq!(
        transcript::cut_point(&history, 20),
        Some(41),
        "a tool loop cuts before an assistant message"
    );
    history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "done?" }] }));
    history.push(
        json!({ "role": "user", "content": "Call `report` to end the run, or keep working." }),
    );
    history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }));
    let at = transcript::cut_point(&history, 4).unwrap();
    assert_eq!(history[at]["role"], "assistant");
    let kept = transcript::compacted("did things", &history[at..]);
    assert_eq!(kept[0]["role"], "user");
    assert_eq!(kept[1]["role"], "assistant");
    assert!(kept[0]["content"].as_str().unwrap().contains("did things"));
    let summary = transcript::summary_request("claude-haiku-5-5", &history[..at]);
    assert!(
        summary["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("[tool run: {}]")
    );
    // ---- the run's git grant (core and the proxies) ----
    use pigeon_core::proxy_caller::within_prefix;
    let prefix = pigeon_capabilities::proxy::run_branch_prefix("r1").unwrap();
    assert_eq!(prefix, "claude/ai-agent-r1");
    assert!(pigeon_capabilities::proxy::run_branch_prefix("../main").is_err());
    assert!(pigeon_capabilities::proxy::run_branch_prefix("").is_err());
    assert!(
        within_prefix(
            [
                "refs/heads/claude/ai-agent-r1",
                "refs/heads/claude/ai-agent-r1-wip"
            ],
            &prefix
        )
        .is_ok()
    );
    assert_eq!(
        within_prefix(
            [
                "refs/heads/claude/ai-agent-r1",
                "refs/heads/main",
                "refs/heads/claude/ai-agent-r2"
            ],
            &prefix
        )
        .unwrap_err(),
        ["refs/heads/main", "refs/heads/claude/ai-agent-r2"]
    );
    assert!(within_prefix(["refs/tags/v1"], &prefix).is_err());
    // A grant is a proxy caller with a prefix; an older answer without one reads as none.
    let old: pigeon_core::proxy_caller::ProxyCaller =
        serde_json::from_value(json!({ "id": "x", "label": "l", "person": "p" })).unwrap();
    assert!(old.branch_prefix.is_none());

    // ---- who a run acts as ----
    let starter = json!({ "user": { "id": "portal", "name": "Portal" }, "acting_for": { "id": "willow", "name": "Willow" } });
    let linked = json!({ "user": { "id": "svc-exec", "name": "Pigeon executor" }, "acting_for": { "id": "willow", "name": "Willow" } });
    let unlinked =
        json!({ "user": { "id": "svc-exec", "name": "Pigeon executor" }, "acting_for": null });
    let other = json!({ "user": { "id": "svc-exec", "name": "Pigeon executor" }, "acting_for": { "id": "mallory" } });
    let as_svc = pigeon_ai_agents::object::chosen_caller(&starter, Some(&linked), "r1");
    assert_eq!(
        (
            as_svc["user"]["id"].as_str(),
            as_svc["acting_for"]["id"].as_str(),
            as_svc["run"].as_str()
        ),
        (Some("svc-exec"), Some("willow"), Some("r1"))
    );
    for svc in [Some(&unlinked), Some(&other), None] {
        let c = pigeon_ai_agents::object::chosen_caller(&starter, svc, "r1");
        assert_eq!(
            (c["user"]["id"].as_str(), c["run"].as_str()),
            (Some("portal"), Some("r1")),
            "falls back to the starter"
        );
    }
    assert_eq!(
        pigeon_ai_agents::object::person_of(&starter),
        Some("willow")
    );
    assert_eq!(
        pigeon_ai_agents::object::person_of(&json!({ "user": { "id": "willow" } })),
        Some("willow")
    );
    assert_eq!(
        pigeon_capabilities::split::service_user_name("ai-agent:executor"),
        Some("Pigeon executor")
    );
    assert_eq!(pigeon_capabilities::split::service_user_name("root"), None);

    // ---- the Worker side's pure parts ----
    use pigeon_ai_agents::{object, prompts, sandbox, service};
    // A passkey wait, read from dispatch's own words.
    let refusal = "`deploy_token` needs the person's passkey for this call: approval `aP3x_9-k` waits on the console's Approvals tab until 14:32 UTC. Once approved, call `deploy_token` again with the same arguments and \"approval\": \"aP3x_9-k\"; `approvals_status` says where it stands.";
    assert_eq!(object::approval_id(refusal).as_deref(), Some("aP3x_9-k"));
    assert_eq!(object::approval_id("ci_submit: no such job"), None);
    assert_eq!(object::role_named("mechanical"), Role::Mechanical);
    // Starting: an executor by default; planners later; a brief is required; base is checked.
    let (role, brief, base, b) = service::start_args(&json!({ "brief": " fix it " })).unwrap();
    assert_eq!(
        (role, brief.as_str(), base.as_str(), b.turns),
        (Role::Executor, "fix it", "main", 60)
    );
    let (role, _, _, b) = service::start_args(&json!({ "brief": "x", "role": "planner" })).unwrap();
    assert_eq!((role, b.executors), (Role::Planner, 12));
    assert!(service::start_args(&json!({ "brief": "x", "role": "boss" })).is_err());
    assert!(
        service::start_args(&json!({}))
            .unwrap_err()
            .contains("brief")
    );
    assert!(service::start_args(&json!({ "brief": "x", "base": "main..evil" })).is_err());
    assert!(service::start_args(&json!({ "brief": "x", "budget": { "cost_usd": 500 } })).is_err());
    assert_eq!(service::branch("r1"), "claude/ai-agent-r1");
    // The policy's session branches cover a run's branch: no new rule.
    let policy = pigeon_core::policy::parse(pigeon_capabilities::policy::TEXT).unwrap();
    let push = policy.evaluate(
        &vars(),
        &request("push_branch", &service::branch("r1"), None),
    );
    assert_eq!(push.rule.as_deref(), Some("session-branches"));
    // Sandbox tools map onto the server's ops; a commit pushes the run's branch only.
    let (op, body) = sandbox::op_for(
        "commit_push",
        &json!({ "message": "m", "branch": "main" }),
        "claude/ai-agent-r1",
    )
    .unwrap();
    assert_eq!(
        (op, body["branch"].as_str()),
        ("commit_push", Some("claude/ai-agent-r1"))
    );
    assert_eq!(
        sandbox::op_for("run", &json!({ "cmd": "ls" }), "b")
            .unwrap()
            .0,
        "exec"
    );
    // A run's command goes as given: image v2's sandboxd runs `bash -c`, which keeps PATH.
    let (_, ran) = sandbox::op_for("run", &json!({ "cmd": "cargo --version" }), "b").unwrap();
    assert_eq!(ran["cmd"], json!("cargo --version"));
    assert!(sandbox::op_for("land", &json!({}), "b").is_none());
    // A planner reads its executors' branches through sandboxd's `fetch`, which holds the grant.
    assert_eq!(
        sandbox::op_for("fetch", &json!({ "ref": "claude/ai-agent-r2" }), "b"),
        Some(("fetch", json!({ "ref": "claude/ai-agent-r2" })))
    );
    assert_eq!(
        sandbox::shown(
            "fetch",
            &json!({ "commit": "abc", "at": "origin/claude/ai-agent-r2" })
        ),
        "fetched abc at `origin/claude/ai-agent-r2`"
    );
    let shown = sandbox::shown(
        "exec",
        &json!({ "exit": 0, "ms": 1200, "bytes": 3, "saved_at": "/tmp/out/1.log", "output": "ok\n", "timed_out": false }),
    );
    assert!(shown.starts_with("exit 0 after 1200 ms") && shown.contains("/tmp/out/1.log"));
    let found = sandbox::shown(
        "grep",
        &json!({ "count": 2, "matches": ["a.rs:1:x", "b.rs:2:y"], "truncated": false }),
    );
    assert_eq!(found, "2 matches\na.rs:1:x\nb.rs:2:y");
    assert_eq!(sandbox::instance("executor"), "standard-4");
    sandbox_prepare_retry();
    // What a run is told: its branch, the rules, and only tools pigeon has.
    let sys = prompts::system(Role::Executor, "claude/ai-agent-r1");
    assert!(
        sys.contains("claude/ai-agent-r1")
            && sys.contains("tests/smoke.rs")
            && sys.contains("never land")
    );
    assert!(
        prompts::first_message(
            &json!({ "brief": "do x", "base": "main", "branch": "claude/ai-agent-r1" })
        )
        .contains("do x")
    );
    let offered = prompts::pigeon_tools();
    assert!(
        tools::PLANNER_PIGEON
            .iter()
            .all(|t| offered.iter().any(|(n, _, _)| n == t)),
        "every listed tool has a schema"
    );
    assert!(prompts::step_line("run", &json!({ "cmd": "a\nb" })).starts_with("run: a b"));
    assert!(
        prompts::step_line("run", &json!({ "cmd": "x".repeat(500) }))
            .chars()
            .count()
            <= 120
    );

    // The capability: its manifest offers exactly its tools, and its section on Instances.
    use pigeon_contract::Service;
    let manifest = service::Agents.manifest();
    let offered: Vec<&str> = manifest.capabilities[0]
        .tools
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(offered, service::TOOLS);
    assert_eq!(manifest.capabilities[0].sections[0].tab, "instances");
    assert_eq!(manifest.worker, "pigeon-ai-agents");
    // The section: what waits for the person first, every value escaped.
    use pigeon_ai_agents::page;
    let html = page::section(&json!({
        "paused": false,
        "runs": [
            { "run": "r1", "role": "executor", "runtime": "gateway", "state": "waiting",
              "waiting": { "for": "answer", "call": "t1", "question": "Use <b>A</b> or B?", "options": ["A", "B"] },
              "step": "waiting for the person's answer", "spent": { "cost": "$0.42", "turns": 7 },
              "branch": "claude/ai-agent-r1", "brief": "fix <script>alert(1)</script>" },
            { "run": "r2", "role": "planner", "runtime": "claude_code", "state": "done",
              "report": "landed", "spent": { "cost": "$0", "turns": 0 }, "brief": "plan x" }
        ]
    }));
    assert!(html.contains("Waiting for you") && html.contains("Use &lt;b&gt;A&lt;/b&gt; or B?"));
    assert!(!html.contains("<script>") && html.contains("&lt;script&gt;"));
    assert!(html.contains("Claude chat") && html.contains("$0.42 · 7 turns"));
    assert!(page::section(&json!({ "paused": true, "runs": [] })).contains("paused"));
    // An unanswered question: after 5 minutes the run goes on, and says what it assumed.
    assert_eq!(run::ANSWER_PATIENCE_MS, 5 * 60_000);
    let told = run::unanswered("Use A or B?", None);
    assert!(told.contains("Use A or B?") && told.contains("Assumed:"));
    let html = page::section(
        &json!({ "runs": [{ "run": "r3", "state": "running", "assumed": [{ "question": "<i>A</i> or B?", "at": 1 }], "spent": {} }] }),
    );
    assert!(
        html.contains("Went on without your answer") && html.contains("&lt;i&gt;A&lt;/i&gt; or B?")
    );
    // A run waiting for an answer keeps its question, as the inbox and the section show it.
    let w: Wait =
        serde_json::from_value(json!({ "for": "answer", "call": "t1", "question": "q" })).unwrap();
    assert!(matches!(w, Wait::Answer { ref question, since: 0, .. } if question == "q"));

    // A planner's executors: how many are live, which ended since its wait began, in words.
    let children = json!([
        { "run": "c1", "state": "done", "branch": "claude/ai-agent-c1", "report": "added x", "ended_at": 200 },
        { "run": "c2", "state": "running", "branch": "claude/ai-agent-c2", "ended_at": null },
        { "run": "c3", "state": "failed", "branch": "claude/ai-agent-c3", "report": "old", "ended_at": 50 }
    ]);
    assert_eq!(object::live_children(&children), 1);
    assert!(object::ended_since(&children, 100) && !object::ended_since(&children, 200));
    let summary = object::children_summary(&children, 100);
    assert!(
        summary.contains("c1 done on `claude/ai-agent-c1`: added x") && !summary.contains("c3")
    );
    assert!(summary.ends_with("1 executor(s) still running; executor_result has each report"));
    assert_eq!(
        tools::kind(Role::Planner, "wait_executors"),
        Some(Kind::Agent)
    );
    assert_eq!(tools::kind(Role::Executor, "wait_executors"), None);

    // Handing a run to the Claude chat: the packet carries its brief, branch and a prompt.
    let packet = object::handoff_packet(
        &json!({ "run": "r9", "role": "executor", "branch": "claude/ai-agent-r9", "step": "run: cargo test", "spent": { "cost": "$1" } }),
        "fix the thing",
        Some("main"),
    );
    let prompt = packet["session_prompt"].as_str().unwrap();
    assert!(
        prompt.contains("fix the thing")
            && prompt.contains("`claude/ai-agent-r9`")
            && prompt.contains("ai_agents_report with run r9")
    );
    assert!(service::TOOLS.contains(&"ai_agents_handoff"));
    // Pause is for every run, and says so: a `run` is refused, not taken as a per-run pause.
    assert!(
        service::pause_args("ai_agents_pause", &json!({ "run": "r1" }))
            .unwrap_err()
            .contains("ai_agents_stop")
    );
    assert!(service::pause_args("ai_agents_pause", &json!({})).is_ok());
    assert_eq!(service::pause_answer(true)["scope"], "every run");
    assert_eq!(service::pause_answer(false)["paused"], false);
    // ai_agents_extend takes its amounts beside `run`; `cost` is `cost_usd`.
    assert_eq!(
        service::extension(&json!({ "run": "r1", "cost": 5, "turns": 20 })),
        json!({ "cost_usd": 5, "turns": 20 })
    );
    // ai_agents_note: a short text, which goes into the run's next turn without breaking the
    // alternation of roles, and reads as the person's note.
    assert!(service::note(&json!({ "run": "r1", "text": "  " })).is_err());
    assert!(service::note(&json!({ "run": "r1", "text": "x".repeat(4001) })).is_err());
    assert_eq!(
        service::note(&json!({ "run": "r1", "text": " work in parallel " })).unwrap(),
        "work in parallel"
    );
    let results = transcript::results_message(&[("t1".into(), "ok".into(), false)]);
    let (merged, replaces) = transcript::with_notes(Some(&results), &["go faster".into()]);
    assert!(replaces);
    assert_eq!(merged["content"][0]["type"], "tool_result");
    assert!(
        merged["content"][1]["text"]
            .as_str()
            .unwrap()
            .ends_with("go faster")
    );
    let brief = json!({ "role": "user", "content": "the brief" });
    let (merged, _) = transcript::with_notes(Some(&brief), &["a".into(), "b".into()]);
    assert_eq!(merged["content"][0]["text"], "the brief");
    assert_eq!(merged["content"].as_array().unwrap().len(), 3);
    let reply = json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }] });
    let (alone, replaces) = transcript::with_notes(Some(&reply), &["n".into()]);
    assert!(!replaces && alone["role"] == "user");
    assert!(
        service::TOOLS.contains(&"ai_agents_extend") && service::TOOLS.contains(&"ai_agents_note")
    );

    // Falling back to another model drops thinking it cannot read.
    let thought = [
        json!({ "role": "assistant", "content": [{ "type": "thinking", "thinking": "t", "signature": "s" }] }),
    ];
    assert_eq!(
        transcript::strip_thinking(&thought)[0]["content"][0]["text"],
        "(thinking removed)"
    );
}

/// An assistant message never ends in a thinking block: the repair drops trailing ones, drops
/// a message left empty, and the provider's refusal is recognised and retried once.
#[test]
fn ai_agents_repairs_trailing_thinking() {
    use pigeon_ai_agents::run::{self, Recovery};
    use pigeon_ai_agents::transcript;
    let think = json!({ "type": "thinking", "thinking": "t", "signature": "s" });
    let text = json!({ "type": "text", "text": "hi" });
    let msgs = [
        json!({ "role": "user", "content": "go" }),
        json!({ "role": "assistant", "content": [text.clone(), think.clone()] }),
        json!({ "role": "assistant", "content": [think.clone()] }),
        json!({ "role": "assistant", "content": [think.clone(), text.clone()] }),
    ];
    let fixed = transcript::repair_transcript(&msgs);
    assert_eq!(fixed.len(), 3);
    assert_eq!(fixed[1]["content"], json!([text.clone()]));
    assert_eq!(fixed[2], msgs[3]);
    let e =
        "answered 400: messages.1: The final block in an assistant message cannot be `thinking`.";
    assert!(run::trailing_thinking(e));
    assert_eq!(
        run::on_failure("failed", e, 1, 0, &["a"]),
        Recovery::RepairTranscript
    );
    assert_eq!(run::on_failure("failed", e, 2, 0, &["a"]), Recovery::Fail);
}

/// The AI gateway refusing its token (401, code 2009), as it did to every run in flight while
/// pigeon-model-gateway deployed (2026-11-22 and -23), is the environment's fault: the turn is
/// sent again after a short wait, a few times, then the run waits for the provider. It never
/// ends the run, and a provider's own 401 is still not retried.
#[test]
fn ai_agents_gateway_unauthorized_waits() {
    use pigeon_ai_agents::run::{self, Recovery};
    let e =
        r#"anthropic answered 401: {"code":2009,"message":"Unauthorized","name":"AiGatewayError"}"#;
    assert!(run::gateway_unauthorized(e));
    assert!(!run::gateway_unauthorized(
        r#"anthropic answered 401: {"type":"error","error":{"type":"authentication_error"}}"#
    ));
    assert!(!run::gateway_unauthorized(
        "anthropic answered 400: code 2009"
    ));
    let models = &["a", "b"];
    assert_eq!(
        run::on_failure("failed", e, 1, 0, models),
        Recovery::WaitGateway { after_ms: 15_000 }
    );
    // After a lost job (a deploy restarted its runner), the count goes on.
    assert_eq!(
        run::on_failure("failed", e, 2, 0, models),
        Recovery::WaitGateway { after_ms: 30_000 }
    );
    assert_eq!(
        run::on_failure("failed", e, 4, 0, models),
        Recovery::WaitGateway { after_ms: 120_000 }
    );
    assert_eq!(
        run::on_failure("failed", e, 5, 0, models),
        Recovery::WaitProvider
    );
    assert_eq!(run::gateway_wait_ms(0), Some(15_000));
    assert_eq!(
        run::on_failure("cancelled", e, 1, 0, models),
        Recovery::Fail
    );
}

/// A run reports before its cost budget runs out: the 90% rule, the report-only request, and
/// what the loop does once the last turn is answered.
#[test]
fn ai_agents_last_turn_reports_before_budget() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::budget::{self, Budget, Spent};
    use pigeon_ai_agents::run::{self, Action, Call, State, View};
    use pigeon_ai_agents::transcript::{self, Spec};

    let b = Budget {
        cost_micros: 1_000_000,
        ..Budget::default_for(Role::Executor)
    };
    let spent = |cost_micros: u64| Spent {
        cost_micros,
        ..Spent::default()
    };
    assert!(!budget::last_turn_due(&b, &spent(899_999), 10_000));
    assert!(budget::last_turn_due(&b, &spent(900_000), 0));
    assert!(budget::last_turn_due(&b, &spent(500_000), 500_001));
    assert!(!budget::last_turn_due(&b, &spent(500_000), 500_000));

    let tools = [
        json!({"name": "run", "description": "d", "input_schema": {"type": "object"}}),
        json!({"name": "report", "description": "d", "input_schema": {"type": "object"}}),
        json!({"name": "grep", "description": "d", "input_schema": {"type": "object"}}),
    ];
    let spec = Spec {
        role: Role::Planner,
        model: "claude-opus-5-5",
        system: "s",
        tools: &tools,
    };
    let messages = [
        json!({"role": "user", "content": "brief"}),
        json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "t", "signature": "s"},
            {"type": "text", "text": "ok"}
        ]}),
        json!({"role": "user", "content": "go on"}),
    ];
    let req = transcript::last_turn_request(&spec, &messages);
    let names: Vec<&str> = req["tools"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(names, ["report"]);
    assert!(
        req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("cache_control").is_none())
    );
    assert_eq!(
        req["tool_choice"],
        json!({"type": "auto"}),
        "a forced tool choice is refused by some models"
    );
    assert!(req.get("thinking").is_none() && req.get("output_config").is_none());
    assert!(!req["messages"].to_string().contains("\"thinking\""));
    assert_eq!(req["max_tokens"], 4096);
    assert_eq!(transcript::LAST_TURN_MAX_TOKENS, 4096);

    let view = View {
        state: State::Running,
        stop_asked: false,
        paused: false,
        job: None,
        calls: &[],
        turns: 5,
        stop_reason: Some("end_turn"),
        reported: None,
        nudges: 0,
        user_last: false,
        last_turn: true,
        waiting: None,
    };
    assert_eq!(run::next(&view), Action::End(State::OverBudget));
    let pending = [Call {
        id: "t".into(),
        name: "report".into(),
        started: false,
        done: false,
    }];
    assert_eq!(
        run::next(&View {
            calls: &pending,
            ..view.clone()
        }),
        Action::Call { index: 0 }
    );
    assert_eq!(
        run::next(&View {
            user_last: true,
            ..view.clone()
        }),
        Action::Turn,
        "a retry sends the last turn again"
    );
}

/// The last turn is also due on the turn and wall limits, and the prompt names the limit.
#[test]
fn ai_agents_last_turn_on_turns_and_wall() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::budget::{self, Budget, Over, Spent};
    use pigeon_ai_agents::prompts;

    let hour = 3_600_000;
    let b = Budget {
        turns: 10,
        cost_micros: 1_000_000,
        wall_ms: 8 * hour,
        ..Budget::default_for(Role::Executor)
    };
    let turns = |turns: u32| Spent {
        turns,
        ..Spent::default()
    };
    let due = |s: &Spent, now: i64| budget::last_turn_over(&b, s, 0, now, 0);
    assert_eq!(due(&turns(8), 0), None);
    assert_eq!(due(&turns(9), 0), Some(Over::Turns));
    let cost = Spent {
        cost_micros: 900_000,
        turns: 9,
        ..Spent::default()
    };
    assert_eq!(due(&cost, 0), Some(Over::Cost), "cost wins");

    // The reserve is 10% of 8 hours: 48 minutes.
    assert_eq!(budget::wall_reserve_ms(8 * hour), 48 * 60_000);
    assert_eq!(due(&turns(0), 8 * hour - 48 * 60_000 - 1), None);
    assert_eq!(due(&turns(0), 8 * hour - 48 * 60_000 + 1), Some(Over::Wall));
    // Short budgets: at least 5 minutes, never more than half.
    assert_eq!(budget::wall_reserve_ms(20 * 60_000), 5 * 60_000);
    assert_eq!(budget::wall_reserve_ms(6 * 60_000), 3 * 60_000);
    assert_eq!(budget::wall_reserve_ms(0), 0);

    // The codes round-trip and zero means no last turn.
    for over in [Over::Cost, Over::Turns, Over::Wall] {
        assert_eq!(
            budget::last_turn_from_code(budget::last_turn_code(over)),
            Some(over)
        );
    }
    assert_eq!(budget::last_turn_from_code(0), None);

    let p = |over, s: &Spent| prompts::last_turn(over, &b, s, 7 * hour + 20 * 60_000);
    assert!(p(Over::Turns, &turns(9)).contains("9 of your 10 model turns"));
    assert!(p(Over::Wall, &turns(0)).contains("of your 8 hours wall time"));
    assert!(p(Over::Cost, &cost).contains("cost budget"));
}

/// The cost estimate: kinds, actual costs (with the legacy planner correction), percentiles,
/// the suggested budget and the nearest-ten choice. Fake past runs only.
#[test]
fn ai_agents_cost_estimate() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::budget::Budget;
    use pigeon_ai_agents::estimate::{self, Past};

    // Kinds: first match wins, case does not matter.
    assert_eq!(estimate::kind("Update the README"), "docs");
    assert_eq!(estimate::kind("Fix the broken build"), "fix");
    assert_eq!(estimate::kind("Add a new tool"), "build");
    assert_eq!(estimate::kind("Run a LIVE CHECK of the fix"), "live check");
    assert_eq!(estimate::kind("Integrate the branches"), "integrate");
    assert_eq!(estimate::kind("Design the cache"), "design");
    assert_eq!(estimate::kind("fix the docs-only plan"), "fix", "fix first");
    // A stray `plan` is not docs; a planner brief about a fix is a fix.
    assert_eq!(
        estimate::kind("Read docs/plans/x.md, the plan. Fix the retry bug."),
        "fix"
    );
    assert_eq!(estimate::kind("Add a tool described in the plan."), "build");
    // Docs need the first sentence or line, or `docs-only`.
    assert_eq!(estimate::kind("Update the docs. Then add a tool."), "docs");
    assert_eq!(estimate::kind("Add a tool.\nThis is docs-only."), "docs");
    assert_eq!(estimate::kind("Add a tool.\nAlso the README."), "build");
    // A given kind wins over keywords, and is shown as given.
    let given = estimate::estimate(Role::Executor, "Fix the bug", Some("design"), &[]);
    assert_eq!((given.kind, given.kind_source), ("design", "given"));
    let classified = estimate::estimate(Role::Executor, "Fix the bug", None, &[]);
    assert_eq!(
        (classified.kind, classified.kind_source),
        ("fix", "classified")
    );
    assert_eq!(classified.to_json()["kind_source"], "classified");
    // Past uses the stored kind, else classifies its brief.
    let stored = json!({ "id": "a", "role": "executor", "state": "done", "parent_run": null,
        "brief": "Fix the bug", "kind": "live check", "budget": {}, "spent": { "turns": 1 },
        "ended_at": 1 });
    let mut unstored = stored.clone();
    unstored["kind"] = Value::Null;
    let past = estimate::actual_costs(&[stored, unstored]);
    assert_eq!((past[0].kind, past[1].kind), ("live check", "fix"));

    // Fake index rows.
    let row = |id: &str, role: &str, state: &str, parent: Value, budget: u64, spent: Value| {
        json!({
            "id": id, "role": role, "state": state, "parent_run": parent,
            "brief": "x".repeat(100),
            "budget": { "cost_micros": budget },
            "spent": spent,
            "ended_at": 1,
        })
    };
    let kids = |p: &str| {
        vec![
            row(
                "c1",
                "executor",
                "done",
                json!(p),
                3_000_000,
                json!({ "turns": 5, "cost_micros": 400_000 }),
            ),
            row(
                "c2",
                "executor",
                "done",
                json!(p),
                3_000_000,
                json!({ "turns": 5, "cost_micros": 600_000 }),
            ),
        ]
    };
    // Legacy planner (no `v`): the children's budgets, 6M, are taken off; then their own costs
    // are added: 7M - 6M + 1M = 2M.
    let mut rows = kids("p");
    rows.push(row(
        "p",
        "planner",
        "done",
        Value::Null,
        25_000_000,
        json!({ "turns": 9, "cost_micros": 7_000_000 }),
    ));
    let past = estimate::actual_costs(&rows);
    let planner = past.iter().find(|p| p.role == Role::Planner).unwrap();
    assert_eq!(planner.cost_micros, 2_000_000);
    assert_eq!(planner.executors, 2);
    assert_eq!(past.len(), 3);
    // `v == 1`: nothing is taken off; the total includes the children: 7M + 1M.
    let mut rows = kids("p");
    rows.push(row(
        "p",
        "planner",
        "done",
        Value::Null,
        25_000_000,
        json!({ "v": 1, "turns": 9, "cost_micros": 7_000_000 }),
    ));
    let past = estimate::actual_costs(&rows);
    let planner = past.iter().find(|p| p.role == Role::Planner).unwrap();
    assert_eq!(planner.cost_micros, 8_000_000);
    // The correction saturates.
    let mut rows = kids("p");
    rows.push(row(
        "p",
        "planner",
        "failed",
        Value::Null,
        25_000_000,
        json!({ "turns": 1, "cost_micros": 100 }),
    ));
    let past = estimate::actual_costs(&rows);
    let planner = past.iter().find(|p| p.role == Role::Planner).unwrap();
    assert_eq!(planner.cost_micros, 1_000_000);
    // Live runs and stopped runs with no turns are not samples.
    let mut live = row("l", "executor", "running", Value::Null, 1, json!({}));
    live["ended_at"] = Value::Null;
    let idle = row(
        "s",
        "executor",
        "stopped",
        Value::Null,
        1,
        json!({ "turns": 0 }),
    );
    let used = row(
        "u",
        "executor",
        "stopped",
        Value::Null,
        1,
        json!({ "turns": 2 }),
    );
    assert_eq!(estimate::actual_costs(&[live, idle, used]).len(), 1);

    // Fewer than 3 samples: the role's default, and a note.
    let samples = |sizes_costs: &[(usize, u64)]| -> Vec<Past> {
        sizes_costs
            .iter()
            .map(|&(brief_bytes, cost_micros)| Past {
                role: Role::Executor,
                brief_bytes,
                kind: "build",
                executors: 0,
                cost_micros,
            })
            .collect()
    };
    let brief = "x".repeat(100);
    let few = estimate::estimate(
        Role::Executor,
        &brief,
        None,
        &samples(&[(100, 1), (100, 2)]),
    );
    assert_eq!(few.samples, 2);
    assert_eq!(
        few.suggested.cost_micros,
        Budget::default_for(Role::Executor).cost_micros
    );
    assert!(
        few.note
            .contains("not enough similar past runs (2); the role's default")
    );
    // Other roles' runs do not count.
    let none = estimate::estimate(Role::Planner, &brief, None, &samples(&[(100, 1); 5]));
    assert_eq!(none.samples, 0);
    assert_eq!(
        none.suggested.executors,
        Some(Budget::default_for(Role::Planner).executors)
    );

    // Ten runs of $1 to $10: median rank 5, low rank 1, high rank 9. 9 * 1.25 = 11.25, up to 11.5.
    let ten: Vec<(usize, u64)> = (1..=10).map(|i| (100, i * 1_000_000)).collect();
    let e = estimate::estimate(Role::Executor, &brief, None, &samples(&ten));
    assert_eq!(
        (e.samples, e.median_micros, e.low_micros, e.high_micros),
        (10, 5_000_000, 1_000_000, 9_000_000)
    );
    assert_eq!(e.suggested.cost_micros, 11_500_000);
    let j = e.to_json();
    assert_eq!(j["median_usd"], 5.0);
    assert_eq!(j["suggested_budget"]["cost_usd"], 11.5);
    assert_eq!(j["kind"], "build");
    // The minimum is $0.50 and the cap is the hard cap.
    let cheap = estimate::estimate(Role::Executor, &brief, None, &samples(&[(100, 10); 3]));
    assert_eq!(cheap.suggested.cost_micros, 500_000);
    let dear = estimate::estimate(
        Role::Executor,
        &brief,
        None,
        &samples(&[(100, 90_000_000); 3]),
    );
    assert_eq!(dear.suggested.cost_micros, Budget::HARD.cost_micros);

    // The nearest ten by size win over farther ones.
    let mut mixed: Vec<(usize, u64)> = vec![(100_000, 50_000_000); 10];
    mixed.extend(vec![(120, 1_000_000); 10]);
    let near = estimate::estimate(Role::Executor, &brief, None, &samples(&mixed));
    assert_eq!(near.samples, 10);
    assert_eq!(near.high_micros, 1_000_000);
}

/// A live run's cost follows the gateway: old spend parses, logs are applied, executors count.
#[test]
fn ai_agents_cost_reconcile() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::budget::{self, Budget, Next, Over, Spent, TurnCost};

    // Spend stored before the new fields still parses.
    let old: Spent =
        serde_json::from_str(r#"{"turns":3,"cost_micros":500,"sandbox_ms":0,"executors":1}"#)
            .unwrap();
    assert_eq!((old.gateway_micros, old.unreconciled, old.v), (0, 0, 0));
    assert_eq!(old.executors_micros, 0);

    // Found logs with a cost are applied; not found and null cost are skipped.
    let rows = |ids: &[(i64, &str, u64)]| -> Vec<TurnCost> {
        ids.iter()
            .map(|(id, log, local)| TurnCost {
                id: *id,
                log_id: log.to_string(),
                local_micros: *local,
            })
            .collect()
    };
    let spent = Spent {
        turns: 4,
        cost_micros: 1_000,
        unreconciled: 4,
        v: 1,
        ..Spent::default()
    };
    let logs = json!({"logs": [
        {"log_id": "a", "found": true, "cost_usd": 0.000_250_4},
        {"log_id": "b", "found": false, "cost_usd": null},
        {"log_id": "c", "found": true, "cost_usd": null},
        {"log_id": "d", "found": true, "cost_usd": 0.0},
    ]});
    let (next, done) = budget::reconcile(
        spent,
        &rows(&[(1, "a", 300), (2, "b", 300), (3, "c", 300), (4, "d", 100)]),
        &logs,
    );
    assert_eq!(done, vec![(1, 250), (4, 0)]);
    assert_eq!(next.cost_micros, 1_000 - 300 + 250 - 100);
    assert_eq!(next.gateway_micros, 250);
    assert_eq!(next.unreconciled, 2);
    // A local cost above the total saturates at zero.
    let (next, _) = budget::reconcile(
        Spent {
            cost_micros: 10,
            unreconciled: 0,
            ..Spent::default()
        },
        &rows(&[(1, "a", 300)]),
        &logs,
    );
    assert_eq!((next.cost_micros, next.unreconciled), (250, 0));
    // No `logs` key changes nothing.
    let (same, none) = budget::reconcile(spent, &rows(&[(1, "a", 1)]), &json!({}));
    assert_eq!((same, none.len()), (spent, 0));

    // A live child holds its spend plus a small reserve; an ended child holds its spend.
    let kids = json!([
        {"ended_at": null, "spent": {"cost": "$1", "cost_usd": 1.0}, "budget": {"cost": "$5", "budget_cost_usd": 5.0}},
        {"ended_at": null, "spent": {"cost": "$7", "cost_usd": 7.0}, "budget": {"cost": "$5", "budget_cost_usd": 5.0}},
        {"ended_at": 9, "spent": {"cost": "$1.25"}, "budget": {"cost": "$5"}},
    ]);
    assert_eq!(
        budget::executors_committed(&kids),
        (1_000_000 + 500_000) + 7_000_000 + 1_250_000
    );
    assert_eq!(budget::executors_committed(&json!([])), 0);
    assert_eq!(budget::executors_committed(&json!(null)), 0);

    // An old planner gets its children's budgets taken off once.
    let old_planner = Spent {
        cost_micros: 6_000_000,
        ..Spent::default()
    };
    let moved = budget::migrate_planner(old_planner, 5_000_000);
    assert_eq!(
        (moved.cost_micros, moved.v),
        (1_000_000, budget::SPENT_VERSION)
    );
    assert_eq!(budget::migrate_planner(moved, 5_000_000), moved);
    let tiny = budget::migrate_planner(Spent::default(), 5_000_000);
    assert_eq!(tiny.cost_micros, 0);

    // Checks count the executors' hold.
    let b = Budget {
        cost_micros: 10_000_000,
        executors: 5,
        ..Budget::default_for(Role::Planner)
    };
    let planner = Spent {
        cost_micros: 1_000_000,
        executors_micros: 8_900_000,
        v: 1,
        ..Spent::default()
    };
    assert_eq!(planner.committed_micros(), 9_900_000);
    let turn = Next::Turn {
        estimate_micros: 200_000,
    };
    assert_eq!(budget::check(&b, &planner, 0, 1, turn), Err(Over::Cost));
    assert_eq!(Spent::left(&b, &planner, 0, 1).cost_micros, 100_000);
    assert!(budget::last_turn_due(&b, &planner, 0));
    let spawn = Next::Spawn {
        cost_micros: 200_000,
    };
    assert_eq!(budget::check(&b, &planner, 0, 1, spawn), Err(Over::Cost));
    let roomy = Spent {
        executors_micros: 2_000_000,
        ..planner
    };
    assert_eq!(budget::check(&b, &roomy, 0, 1, spawn), Ok(()));
}

/// A run's title comes from its brief, and its elapsed time is never negative.
#[test]
fn ai_agents_run_title_and_elapsed() {
    use pigeon_ai_agents::object::{elapsed_ms, run_title};

    assert_eq!(run_title(""), "Untitled run");
    assert_eq!(run_title("\n  \n"), "Untitled run");
    assert_eq!(
        run_title("## Brief: Fix the sample page\nmore"),
        "Fix the sample page"
    );
    assert_eq!(run_title("\n  task: Add a thing  \nx"), "Add a thing");
    assert_eq!(run_title("Brief:"), "Untitled run");
    let long = "word ".repeat(40);
    let t = run_title(&long);
    assert!(t.ends_with('…') && t.chars().count() <= 80, "{t}");
    assert!(t.trim_end_matches('…').ends_with("word"));
    let one = "x".repeat(200);
    assert!(run_title(&one).chars().count() <= 80);

    assert_eq!(elapsed_ms(100, Some(150), Some(400), 999), 250);
    assert_eq!(elapsed_ms(100, None, None, 600), 500);
    assert_eq!(elapsed_ms(100, Some(500), None, 200), 0);
}

/// The sandbox's prepare, retried after a failed try (overnight 2026-11-22, bug 2): the Worker's
/// setup empties what that try left in /work, files or a `.git` git cannot read, and keeps a
/// checkout that is there.
fn sandbox_prepare_retry() {
    use pigeon_ai_agents::sandbox;
    let base = std::env::temp_dir().join(format!("pigeon-smoke-sbprep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let origin = repo("sbprep-origin");
    std::fs::write(origin.join("a.txt"), "a\n").unwrap();
    git(&origin, &["add", "a.txt"], None);
    commit(&origin, "one");
    let sh = |cmd: &str, dir: &Path| {
        let out = Command::new("bash")
            .args(["-c", cmd])
            .current_dir(dir)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let empty = |what: &str| {
        assert_eq!(
            std::fs::read_dir(&work).unwrap().count(),
            0,
            "setup empties /work: {what}"
        );
    };
    // What a failed clone or checkout leaves: the checkout's files, no .git.
    std::fs::write(work.join("a.txt"), "left over\n").unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    sh(sandbox::PREPARE_SETUP, &work);
    empty("files");
    // A .git that git cannot read, as a repack still writing into a removed one leaves.
    std::fs::create_dir_all(work.join(".git/objects/pack")).unwrap();
    std::fs::write(work.join("a.txt"), "left over\n").unwrap();
    sh(sandbox::PREPARE_SETUP, &work);
    empty("a broken .git");
    // A checkout that is there already is kept.
    let url = format!("file://{}", origin.display());
    sh(
        &format!("git clone -q --no-tags -- {url} {}", work.display()),
        &base,
    );
    sh(sandbox::PREPARE_SETUP, &work);
    assert!(work.join("a.txt").exists() && work.join(".git").exists());
    // The first error is the one reported, with the last.
    assert_eq!(
        sandbox::not_ready(Some("git checkout failed: 403"), "git clone failed: exists"),
        "the sandbox did not get ready: git checkout failed: 403 (and on the last try: git clone failed: exists)"
    );
    assert_eq!(
        sandbox::not_ready(Some("x"), "x"),
        "the sandbox did not get ready: x"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// `commit_push` pushes HEAD, so a rebase or cherry-pick leaves the sandbox, and takes `force`
/// for a force-with-lease push of the run's own branch: the branch is always the run's, never
/// one the model names. What the model reads covers sandboxd's answers, and an answer without
/// `committed` stays harmless.
#[test]
fn ai_agents_commit_push_pushes_local_commits_with_lease() {
    use pigeon_ai_agents::{Role, sandbox, tools};
    // Offered with an optional boolean `force`; `message` stays the one required field.
    let defs = tools::definitions(Role::Executor, &[]);
    let cp = defs.iter().find(|d| d["name"] == "commit_push").unwrap();
    assert_eq!(cp["input_schema"]["properties"]["force"]["type"], "boolean");
    assert_eq!(cp["input_schema"]["required"], json!(["message"]));
    assert!(
        cp["description"]
            .as_str()
            .unwrap()
            .contains("local commits")
    );
    // `force` is true, false or absent; nothing else.
    for ok in [
        json!({ "message": "m" }),
        json!({ "message": "m", "force": true }),
        json!({ "message": "m", "force": false }),
    ] {
        assert_eq!(
            tools::check(Role::Executor, "commit_push", &ok),
            Ok(()),
            "{ok}"
        );
    }
    for bad in [
        json!({ "message": "m", "force": "yes" }),
        json!({ "message": "m", "force": 1 }),
        json!({ "force": true }),
    ] {
        assert!(
            tools::check(Role::Planner, "commit_push", &bad).is_err(),
            "{bad}"
        );
    }
    // The server is told the run's branch and whether to force; a branch the model names is
    // ignored, and a missing `force` is a plain push.
    let (op, body) = sandbox::op_for(
        "commit_push",
        &json!({ "message": "m", "force": true, "branch": "main" }),
        "claude/ai-agent-r1",
    )
    .unwrap();
    assert_eq!(op, "commit_push");
    assert_eq!(
        body,
        json!({ "message": "m", "branch": "claude/ai-agent-r1", "force": true })
    );
    let (_, plain) = sandbox::op_for("commit_push", &json!({ "message": "m" }), "b").unwrap();
    assert_eq!(plain["force"], false);
    // What the model reads.
    let shown = |a: Value| sandbox::shown("commit_push", &a);
    let c = "c0ffee";
    assert_eq!(
        shown(
            json!({ "commit": c, "pushed": true, "committed": true, "forced": false, "nothing": false })
        ),
        "pushed c0ffee"
    );
    assert!(
        shown(json!({ "commit": c, "pushed": true, "committed": false, "forced": false, "nothing": false }))
            .contains("local commits")
    );
    assert!(
        shown(json!({ "commit": c, "pushed": true, "committed": false, "forced": true, "nothing": false }))
            .contains("forced, with lease")
    );
    assert_eq!(
        shown(
            json!({ "commit": c, "pushed": false, "committed": false, "forced": false, "nothing": true })
        ),
        "nothing to commit or push; the branch is at c0ffee"
    );
    // An answer without `committed` (an older image) reads the same as v3's, with no
    // claim about what the sandbox cannot do.
    assert_eq!(
        shown(json!({ "commit": c, "pushed": false, "nothing": true })),
        "nothing to commit or push; the branch is at c0ffee"
    );
    assert_eq!(
        shown(json!({ "commit": c, "pushed": true })),
        "pushed c0ffee"
    );
}

#[test]
fn ai_agents_status_shape_options() {
    use pigeon_ai_agents::service::{BriefShape, ReportShape, Shape, shape, shape_args};
    assert_eq!(shape_args(&json!({})).unwrap(), Shape::default());
    let s =
        shape_args(&json!({ "brief": "line", "report": "short", "fields": ["state"] })).unwrap();
    assert_eq!(s.brief, BriefShape::Line);
    assert_eq!(s.report, ReportShape::Short);
    assert_eq!(s.fields, Some(vec!["state".to_string()]));
    assert_eq!(
        shape_args(&json!({ "brief": false })).unwrap().brief,
        BriefShape::Drop
    );
    assert_eq!(
        shape_args(&json!({ "report": false })).unwrap().report,
        ReportShape::Drop
    );
    assert!(shape_args(&json!({ "brief": "x" })).is_err());
    assert!(shape_args(&json!({ "report": true })).is_err());
    assert!(shape_args(&json!({ "fields": "run" })).is_err());
    assert!(shape_args(&json!({ "fields": [1] })).is_err());

    let long = "a".repeat(500);
    let run = json!({
        "run": "r1", "state": "done", "step": "x",
        "brief": format!("{long}\nsecond"), "report": format!("{long}\nmore"),
    });
    assert_eq!(shape(&run, &Shape::default()), run);
    let line = shape(
        &run,
        &shape_args(&json!({ "brief": "line", "report": "short" })).unwrap(),
    );
    assert_eq!(line["brief"].as_str().unwrap().len(), 100);
    assert_eq!(line["report"].as_str().unwrap().len(), 200);
    let dropped = shape(
        &run,
        &shape_args(&json!({ "brief": false, "report": false })).unwrap(),
    );
    assert!(dropped.get("brief").is_none() && dropped.get("report").is_none());
    assert_eq!(dropped["state"], "done");
    let kept = shape(
        &run,
        &shape_args(&json!({ "fields": ["state", "nope"] })).unwrap(),
    );
    assert_eq!(kept, json!({ "run": "r1", "state": "done" }));
}

#[test]
fn ai_agents_event_notes_and_wait_events() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::run::{self, Action, State, View, Wait};
    use pigeon_ai_agents::tools::{self, Kind};
    use pigeon_ai_agents::transcript;

    // The marker round-trips through with_notes as an event, and a plain note stays a note.
    let event = format!("{}landing l1 merged", transcript::EVENT_MARKER);
    let (m, _) = transcript::with_notes(None, &[event, "hello".to_string()]);
    let first = m["content"][0]["text"].as_str().unwrap();
    assert_eq!(first, transcript::event_text("landing l1 merged"));
    assert!(first.starts_with("An event from pigeon (not the person)"));
    assert!(
        m["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("A note from the person")
    );

    // run::next checks Wait::Events as often as Wait::Executors.
    let wait = Wait::Events { since: 1, until: 2 };
    let view = View {
        state: State::Waiting,
        stop_asked: false,
        paused: false,
        job: None,
        calls: &[],
        turns: 1,
        stop_reason: None,
        reported: None,
        nudges: 0,
        user_last: false,
        last_turn: false,
        waiting: Some(&wait),
    };
    assert_eq!(
        run::next(&view),
        Action::Check {
            after_ms: run::EXECUTORS_EVERY_MS
        }
    );
    let s = serde_json::to_value(&wait).unwrap();
    assert_eq!(s, json!({ "for": "events", "since": 1, "until": 2 }));
    assert_eq!(serde_json::from_value::<Wait>(s).unwrap(), wait);
    assert_eq!(
        serde_json::from_value::<Wait>(json!({ "for": "events" })).unwrap(),
        Wait::Events { since: 0, until: 0 }
    );

    // Who is offered what.
    assert_eq!(tools::kind(Role::Planner, "wait_events"), Some(Kind::Agent));
    assert_eq!(tools::kind(Role::Executor, "wait_events"), None);
    for t in ["land_list", "land_watch", "work_status"] {
        assert_eq!(tools::kind(Role::Planner, t), Some(Kind::Pigeon), "{t}");
    }
    assert_eq!(
        tools::kind(Role::Executor, "work_status"),
        Some(Kind::Pigeon)
    );
    assert_eq!(
        tools::kind(Role::Mechanical, "land_list"),
        Some(Kind::Pigeon)
    );
    assert_eq!(tools::kind(Role::Executor, "land_watch"), None);
    assert!(tools::idempotent("work_status") && tools::idempotent("land_list"));
    assert!(!tools::idempotent("land_watch"));

    // `minutes` is 1 to 120.
    let ok = |v: serde_json::Value| tools::check(Role::Planner, "wait_events", &v);
    assert!(ok(json!({})).is_ok());
    assert!(ok(json!({ "minutes": 1 })).is_ok());
    assert!(ok(json!({ "minutes": 120 })).is_ok());
    assert!(ok(json!({ "minutes": 0 })).is_err());
    assert!(ok(json!({ "minutes": 121 })).is_err());
    assert!(ok(json!({ "minutes": "5" })).is_err());
}

/// `may_start` over a table: the live cap, the planner reserve, the per-planner executor cap,
/// and another planner's executors not being blocked.
#[test]
fn ai_agents_may_start_table() {
    use pigeon_ai_agents::Role::{Executor, Mechanical, Planner};
    use pigeon_ai_agents::object::{Limits, may_start};
    let l = Limits::default();
    assert_eq!((l.max_live, l.per_planner, l.reserve), (8, 3, 3));
    // (role, live, own live executors, oldest, may start)
    let table = [
        (Planner, 0, 0, true, true),
        (Planner, 4, 0, true, true),
        // Planners stop at max_live - reserve = 5.
        (Planner, 5, 0, true, false),
        (Executor, 5, 0, true, true),
        (Executor, 7, 0, true, true),
        // The live cap.
        (Executor, 8, 0, true, false),
        (Mechanical, 8, 0, true, false),
        (Planner, 8, 0, true, false),
        // The per-planner cap: 3 live executors of its own planner block it.
        (Executor, 4, 3, true, false),
        (Mechanical, 4, 3, true, false),
        (Executor, 4, 2, true, true),
        // Another planner's executor, with room under its own cap, is not blocked by that.
        (Executor, 4, 0, true, true),
        // Not the oldest allowed: waits whatever the room.
        (Planner, 0, 0, false, false),
        (Executor, 0, 0, false, false),
    ];
    for (role, live, own, oldest, want) in table {
        assert_eq!(
            may_start(role, live, own, &l, oldest),
            want,
            "{role:?} live {live} own {own} oldest {oldest}"
        );
    }
    // A reserve is clamped so that one planner can always start.
    let one = Limits {
        max_live: 1,
        per_planner: 1,
        reserve: 0,
    };
    assert!(may_start(Planner, 0, 0, &one, true));
}

/// The stored caps: defaults, bounds, clamping and the refusal text.
#[test]
fn ai_agents_config_defaults_and_bounds() {
    use pigeon_ai_agents::limits::{StoredLimits, check_config};
    let none = StoredLimits::default();
    let a = none.answer();
    assert_eq!(a["max_live_runs"], 8);
    assert_eq!(a["max_executors_per_planner"], 3);
    assert_eq!(a["executor_reserve"], 3);
    assert_eq!(a["is_default"]["max_live_runs"], true);
    // No arguments change nothing.
    assert_eq!(check_config(&json!(null), &none).unwrap(), none);
    // Setting some marks them stored.
    let s = check_config(
        &json!({ "max_live_runs": 12, "executor_reserve": 11 }),
        &none,
    )
    .unwrap();
    let a = s.answer();
    assert_eq!(
        (a["max_live_runs"].as_u64(), a["executor_reserve"].as_u64()),
        (Some(12), Some(11))
    );
    assert_eq!(a["is_default"]["max_live_runs"], false);
    assert_eq!(a["is_default"]["max_executors_per_planner"], true);
    // Bounds.
    for bad in [
        json!({ "max_live_runs": 0 }),
        json!({ "max_live_runs": 65 }),
        json!({ "max_live_runs": "8" }),
        json!({ "max_executors_per_planner": 0 }),
        json!({ "max_executors_per_planner": 17 }),
        json!({ "executor_reserve": 8 }),
        json!({ "max_live_runs": 4, "executor_reserve": 4 }),
        json!({ "colour": 1 }),
    ] {
        assert!(check_config(&bad, &none).is_err(), "{bad}");
    }
    let e = check_config(&json!({ "max_live_runs": 65 }), &none).unwrap_err();
    assert_eq!(e, "`max_live_runs` must be from 1 to 64, not 65");
    let e = check_config(&json!({ "executor_reserve": 8 }), &none).unwrap_err();
    assert!(
        e.contains("from 0 to 7") && e.contains("max_live_runs"),
        "{e}"
    );
    assert!(
        check_config(
            &json!({ "max_live_runs": 64, "max_executors_per_planner": 16, "executor_reserve": 0 }),
            &none
        )
        .is_ok()
    );
    // A stored reserve too big for a smaller max_live reads clamped, so a planner can start.
    let s = StoredLimits {
        max_live: Some(2),
        reserve: Some(9),
        ..none
    };
    assert_eq!(s.limits().reserve, 1);
}

/// The reasons shown in `ai_agents_status` for a queued run.
#[test]
fn ai_agents_queue_reasons() {
    use pigeon_ai_agents::Role::{Executor, Planner};
    use pigeon_ai_agents::object::{Limits, queue_reason};
    let l = Limits::default();
    let why =
        |role, live, own, paused, oldest| queue_reason(role, live, own, &l, paused, oldest, None);
    assert_eq!(
        why(Executor, 8, 0, false, true).unwrap(),
        "queued: 8 of 8 live runs (max_live_runs)"
    );
    assert_eq!(
        why(Executor, 4, 3, false, true).unwrap(),
        "queued: this planner already has 3 live executors (max_executors_per_planner)"
    );
    assert_eq!(
        why(Planner, 5, 0, false, true).unwrap(),
        "queued: planners leave 3 slots for executors (executor_reserve)"
    );
    assert_eq!(
        why(Planner, 0, 0, true, true).unwrap(),
        "queued: runs are paused"
    );
    assert_eq!(
        why(Executor, 0, 0, false, false).unwrap(),
        "queued: older queued runs go first"
    );
    assert_eq!(why(Executor, 0, 0, false, true), None);
}

/// The token gauge in `queue_reason`: below the ceiling a run starts, at or above it queues
/// with the reason, an unknown gauge never holds it, and the reason comes after the caps.
#[test]
fn ai_agents_token_gauge_ceiling() {
    use pigeon_ai_agents::Role::{Executor, Planner};
    use pigeon_ai_agents::object::{Limits, gauge_of, queue_reason};
    let l = Limits::default();
    let why = |role, live, tokens| queue_reason(role, live, 0, &l, false, true, tokens);
    assert_eq!(why(Executor, 0, Some((299, 300))), None);
    let text = "queued: 300 live Cloudflare tokens, at or above the soft ceiling of 300 (waits until fewer than 300)";
    assert_eq!(why(Executor, 0, Some((300, 300))).unwrap(), text);
    assert_eq!(
        why(Planner, 0, Some((450, 300))).unwrap(),
        "queued: 450 live Cloudflare tokens, at or above the soft ceiling of 300 (waits until fewer than 300)"
    );
    // The gauge could not be read: the run starts.
    assert_eq!(why(Executor, 0, None), None);
    // The live-run cap is named first.
    assert_eq!(
        why(Executor, 8, Some((400, 300))).unwrap(),
        "queued: 8 of 8 live runs (max_live_runs)"
    );
    // The index reads the gauge from a `tokens_status` answer, and from nothing else.
    assert_eq!(
        gauge_of(&json!({ "live": 12, "soft_ceiling": 300, "hard_cap": 500 })),
        Some((12, 300))
    );
    assert_eq!(gauge_of(&json!({ "live": 12 })), None);
    assert_eq!(gauge_of(&Value::Null), None);
}

/// The model gateway refusing the turn itself (a model id it does not take) is permanent: the
/// runner reads the refusal and ends the run `failed` with the gateway's words, not a retry. The
/// gateway being full or unreachable is still retried.
#[test]
fn ai_agents_refused_turn_fails_at_once() {
    use pigeon_ai_agents::run;
    use pigeon_models::{Dialect, ModelIds, turn};
    let e = turn::turn(
        &json!({ "model": "@cf/a/b", "messages": [{ "role": "user", "content": "x" }] }),
        &Value::Null,
        "gpt-6.2-sol",
        Dialect::OpenAi,
        ModelIds::Plain,
    )
    .unwrap_err();
    assert_eq!(run::REFUSED, turn::REFUSED);
    assert!(run::refused_turn(&e));
    assert!(e.contains("plain model id"));
    assert!(!run::refused_turn("the model gateway is full: try later"));
    assert!(!run::refused_turn("anthropic answered 529: overloaded"));
}

#[test]
fn ai_agents_read_only_observability_tools_offered() {
    use pigeon_ai_agents::tools::{self, Kind};
    use pigeon_ai_agents::{Role, prompts};
    let new = ["cost_daily", "mcp_conformance", "observability_errors"];
    assert_eq!(tools::EXECUTOR_PIGEON.len(), 10);
    assert_eq!(tools::PLANNER_PIGEON.len(), 15);
    let offered = prompts::pigeon_tools();
    for t in new {
        assert!(tools::EXECUTOR_PIGEON.contains(&t), "{t}");
        assert!(tools::PLANNER_PIGEON.contains(&t), "{t}");
        assert!(offered.iter().any(|(n, _, _)| n == t), "{t} has a schema");
        assert!(tools::idempotent(t), "{t}");
        for role in [Role::Planner, Role::Executor, Role::Mechanical] {
            assert_eq!(tools::kind(role, t), Some(Kind::Pigeon), "{t}");
        }
    }
    for list in [&tools::EXECUTOR_PIGEON[..], &tools::PLANNER_PIGEON[..]] {
        let mut seen = std::collections::BTreeSet::new();
        assert!(list.iter().all(|t| seen.insert(*t)), "no name twice");
    }
}

/// `model_probe` and `run_spend` are offered to every role, and `run_spend` alone is safe to
/// repeat.
#[test]
fn ai_agents_probe_and_spend_are_in_every_role_list() {
    use pigeon_ai_agents::{Role, tools};
    for role in [Role::Planner, Role::Executor, Role::Mechanical] {
        let names: Vec<String> = tools::definitions(role, &[])
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for n in ["model_probe", "run_spend"] {
            assert!(names.contains(&n.to_string()), "{role:?} lacks {n}");
            assert_eq!(tools::kind(role, n), Some(tools::Kind::Agent));
        }
    }
    assert!(tools::idempotent("run_spend") && !tools::idempotent("model_probe"));
}

/// The inputs of the two tools: bounds, the model rule, unknown keys.
#[test]
fn ai_agents_probe_and_spend_check_their_input() {
    use pigeon_ai_agents::{Role, probe, tools};
    let probe_err = |v: Value| tools::check(Role::Planner, "model_probe", &v).unwrap_err();
    assert!(
        tools::check(
            Role::Executor,
            "model_probe",
            &json!({ "model": "cf-openai/gpt-6.2-sol" })
        )
        .is_ok()
    );
    assert!(
        tools::check(
            Role::Executor,
            "model_probe",
            &json!({ "model": "claude-haiku-5-5", "prompt": "hi", "max_tokens": 64 })
        )
        .is_ok()
    );
    assert!(probe_err(json!({})).contains("`model` is required"));
    assert!(probe_err(json!({ "model": 3 })).contains("string"));
    assert!(probe_err(json!({ "model": "mistral/large" })).contains("mistral"));
    assert!(probe_err(json!({ "model": "anthropic/bad name" })).contains("`model`"));
    assert!(probe_err(json!({ "model": "anthropic", "max_tokens": 65 })).contains("64"));
    assert!(probe_err(json!({ "model": "anthropic", "max_tokens": 0 })).contains("between"));
    assert!(probe_err(json!({ "model": "anthropic", "max_tokens": "8" })).contains("whole number"));
    assert!(probe_err(json!({ "model": "anthropic", "prompt": "x".repeat(501) })).contains("500"));
    assert!(probe_err(json!({ "model": "anthropic", "prompt": "" })).contains("empty"));
    assert!(probe_err(json!({ "model": "anthropic", "run": "r1" })).contains("`run`"));
    assert!(tools::check(Role::Planner, "run_spend", &json!({})).is_ok());
    assert!(
        tools::check(Role::Planner, "run_spend", &json!({ "run": "r1" }))
            .unwrap_err()
            .contains("`run`")
    );
    // Defaults, and the model as the run's `model` field reads it.
    let r = probe::parse(Role::Executor, &json!({ "model": "cf-openai/gpt-6.2-sol" })).unwrap();
    assert_eq!(
        (r.provider.as_str(), r.model.as_str()),
        ("cf-openai", "gpt-6.2-sol")
    );
    assert_eq!(
        (r.prompt.as_str(), r.max_tokens),
        (probe::DEFAULT_PROMPT, 16)
    );
    assert_eq!(probe::task("r1", 3), "ai-agent/r1/probe/3");
}

/// The tenth probe is the last; the eleventh is refused in plain words.
#[test]
fn ai_agents_probe_limit_is_ten() {
    use pigeon_ai_agents::probe;
    assert!(probe::may_send(0).is_ok() && probe::may_send(9).is_ok());
    assert!(probe::may_send(10).unwrap_err().contains("10 probes"));
}

/// A probe's answer from a canned gateway reply and error; its cost counts in the run's spend.
#[test]
fn ai_agents_probe_shapes_the_gateway_answer() {
    use pigeon_ai_agents::budget::{Budget, Spent};
    use pigeon_ai_agents::probe::{self, Outcome};
    let reply = json!({ "model": "claude-haiku-5-5", "text": "ok".repeat(300), "tokens_in": 20, "tokens_out": 2 });
    let s = probe::shape("claude-haiku-5-5", &Outcome::Reply(reply), 123);
    assert_eq!(s.answer["ok"], true);
    assert_eq!(s.answer["status"], 200);
    assert_eq!(s.answer["latency_ms"], 123);
    assert_eq!(s.answer["input_tokens"], 20);
    assert_eq!(s.answer["cost_source"], "estimate");
    assert_eq!(s.answer["text"].as_str().unwrap().chars().count(), 200);
    assert!(!s.from_gateway && s.cost_micros > 0);
    // A gateway cost wins and counts as gateway money.
    let g = probe::shape(
        "claude-haiku-5-5",
        &Outcome::Reply(json!({ "text": "ok", "cost_usd": 0.0004 })),
        5,
    );
    assert_eq!((g.cost_micros, g.from_gateway), (400, true));
    assert_eq!(g.answer["cost_source"], "gateway");
    let spent = probe::charge(&Spent::default(), &g);
    assert_eq!(
        (spent.cost_micros, spent.gateway_micros, spent.unreconciled),
        (400, 400, 0)
    );
    let spent = probe::charge(&Spent::default(), &s);
    assert_eq!(
        (spent.cost_micros, spent.gateway_micros),
        (s.cost_micros, 0)
    );
    // A provider error keeps its text and status, cut to 500 characters.
    let text = format!("Anthropic answered 429: {}", "rate limited ".repeat(100));
    let f = probe::shape("m", &Outcome::Failed(text.clone()), 9);
    assert_eq!(f.answer["ok"], false);
    assert_eq!(f.answer["status"], 429);
    assert_eq!(f.answer["error"].as_str().unwrap(), &text[..500]);
    assert_eq!(f.cost_micros, 0);
    // A refusal with no status keeps its words, whatever they are.
    let n = probe::shape(
        "m",
        &Outcome::Failed("provider `cf-openai` is not ready: no key".into()),
        1,
    );
    assert_eq!(n.answer["status"], "error");
    assert_eq!(
        n.answer["error"],
        "provider `cf-openai` is not ready: no key"
    );
    assert_eq!(
        probe::shape("m", &Outcome::TimedOut, 30_000).answer["status"],
        "timeout"
    );
    // The run's budget decides whether a probe may go.
    let b = Budget::default_for(pigeon_ai_agents::Role::Executor);
    assert!(probe::budget_allows(&b, &Spent::default(), 0, 1_000, 100).is_ok());
    let rich = Spent {
        cost_micros: b.cost_micros,
        ..Spent::default()
    };
    assert!(
        probe::budget_allows(&b, &rich, 0, 1_000, 100)
            .unwrap_err()
            .contains("cost budget")
    );
    assert!(
        probe::budget_allows(&b, &Spent::default(), 0, b.wall_ms + 1, 100)
            .unwrap_err()
            .contains("wall")
    );
}

/// `run_spend` shows this run's numbers and what is left.
#[test]
fn ai_agents_run_spend_answers_from_what_the_run_keeps() {
    use pigeon_ai_agents::budget::{Budget, Spent};
    use pigeon_ai_agents::{Role, probe};
    let b = Budget::default_for(Role::Planner);
    let spent = Spent {
        turns: 3,
        cost_micros: 2_000_000,
        gateway_micros: 2_000_000,
        executors: 2,
        executors_micros: 1_000_000,
        ..Spent::default()
    };
    let a = probe::spend_answer(&b, &spent, 0, 10 * 60_000, 4);
    assert_eq!(a["turns"], 3);
    assert_eq!(a["cost"], "$2");
    assert_eq!(a["cost_source"], "gateway");
    assert_eq!(a["probes_used"], 4);
    assert_eq!(a["left"]["turns"], 197);
    assert_eq!(a["left"]["cost"], "$22");
    assert_eq!(a["left"]["probes"], 6);
    assert_eq!(a["left"]["wall_min"], 8 * 60 - 10);
    assert_eq!(a["left"]["executors"], 10);
    assert_eq!(a["executors_cost"], "$1");
    let est = Spent {
        cost_micros: 5,
        ..Spent::default()
    };
    assert_eq!(
        probe::spend_answer(&b, &est, 0, 0, 0)["cost_source"],
        "estimate"
    );
}

/// A planner's executors hold only a small reserve, so the planner can still take a turn
/// (the incident: cap $15, own $0.32, two live executors with $7.25 budgets).
#[test]
fn ai_agents_executor_reserve_leaves_planner_room() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::budget::{self, Budget, Next, Over, Spent};

    let kids = json!([
        {"ended_at": null, "spent": {"cost_usd": 0.1}, "budget": {"budget_cost_usd": 7.25}},
        {"ended_at": null, "spent": {"cost_usd": 0.2}, "budget": {"budget_cost_usd": 7.25}},
    ]);
    assert_eq!(budget::executors_split(&kids), (300_000, 1_000_000));
    let b = Budget {
        cost_micros: 15_000_000,
        executors: 5,
        ..Budget::default_for(Role::Planner)
    };
    let planner = budget::with_executors(
        Spent {
            cost_micros: 320_000,
            executors: 2,
            v: 1,
            ..Spent::default()
        },
        &kids,
    );
    assert_eq!(planner.executors_micros, 1_300_000);
    let turn = Next::Turn {
        estimate_micros: 200_000,
    };
    assert_eq!(budget::check(&b, &planner, 0, 1, turn), Ok(()));
    assert!(!budget::last_turn_due(&b, &planner, 0));

    // An ended child releases its reserve and keeps only its spend.
    let ended = json!([
        {"ended_at": 5, "spent": {"cost_usd": 0.1}, "budget": {"budget_cost_usd": 7.25}},
    ]);
    assert_eq!(budget::executors_split(&ended), (100_000, 0));
    // A live child near its budget reserves only what is unspent.
    let near = json!([
        {"ended_at": null, "spent": {"cost_usd": 7.0}, "budget": {"budget_cost_usd": 7.25}},
    ]);
    assert_eq!(budget::executors_split(&near), (7_000_000, 250_000));

    // The status JSON shows spent and reserved apart; old rows still parse.
    let mut sj = json!({"cost": "$0.32"});
    pigeon_ai_agents::object::add_planner_cost(&mut sj, &planner);
    assert_eq!(sj["executors_reserved_usd"], 1.0);
    assert_eq!(sj["executors_spent_usd"], 0.3);
    assert_eq!(sj["executors_cost_usd"], 1.3);
    assert_eq!(sj["total_cost"], "$1.62");
    let old: Spent =
        serde_json::from_str(r#"{"turns":1,"cost_micros":2,"sandbox_ms":0,"executors":0}"#)
            .unwrap();
    assert_eq!(old.executors_reserved_micros, 0);
    assert_eq!(old.executors_spent_micros, 0);

    // A spawn needs the planner's remaining cost to cover only the reserve.
    let spawn = Next::Spawn {
        cost_micros: budget::EXECUTOR_RESERVE_MICROS,
    };
    assert_eq!(budget::check(&b, &planner, 0, 1, spawn), Ok(()));
    let broke = Spent {
        cost_micros: 14_000_000,
        executors_micros: 600_000,
        ..planner
    };
    assert_eq!(budget::check(&b, &broke, 0, 1, spawn), Err(Over::Cost));
}
/// An `openai/...` choice is accepted: it names the gateway's BYOK provider `openai`, it takes the
/// gateway's plain model-id rule, it is paid by the person's own key, and no text says Cloudflare
/// credits pay for it. The bare word `openai` picks the tier model, as `anthropic` does.
#[test]
fn ai_agents_openai_choice_routes_to_byok_provider() {
    use pigeon_ai_agents::models::{Choice, Defaults, Slot, defaults_answer, route};
    let c = Choice::parse("openai/gpt-6.2-sol", Slot::Executor).unwrap();
    assert_eq!(
        (c.provider.as_str(), c.model.as_str()),
        ("openai", "gpt-6.2-sol")
    );
    assert_eq!(c.text(), "openai/gpt-6.2-sol");
    // The provider name is the one the model gateway declares.
    let src = include_str!("../../src/model-gateway/src/lib.rs");
    assert!(src.contains("name: \"openai\","));
    assert_eq!(
        Choice::parse("openai", Slot::Planner).unwrap().text(),
        "openai/gpt-6.2-astra"
    );
    assert_eq!(
        Choice::parse("openai", Slot::Summary).unwrap().text(),
        "openai/gpt-6.1-terra"
    );
    // The gateway's id rule decides which ids are taken.
    assert!(Choice::parse("openai/gpt 6", Slot::Executor).is_err());
    assert!(Choice::parse("openai/", Slot::Executor).is_err());
    assert!(Choice::parse("openai/@cf/x/y", Slot::Executor).is_err());
    // It is on the person's own key, not credits.
    assert_eq!(route("openai"), "byok");
    assert_eq!(route("cf-openai"), "credits");
    let raw = json!({ "executor": "openai/gpt-6.2-sol" });
    let a = defaults_answer(&Defaults::from_json(&raw), &[]);
    assert_eq!(a["defaults"]["executor"]["model"], "openai/gpt-6.2-sol");
    assert_eq!(a["defaults"]["executor"]["route"], "byok");
    let shown = a.to_string();
    assert!(
        !shown.contains("OpenAI is paid with Cloudflare credits"),
        "{shown}"
    );
}

/// Both branch tools are offered by role: restack to all, combine to planners only, and
/// neither is safe to repeat.
#[test]
fn ai_agents_branch_tools_are_in_the_right_role_lists() {
    use pigeon_ai_agents::{Role, tools};
    for role in [Role::Planner, Role::Executor, Role::Mechanical] {
        let names: Vec<String> = tools::definitions(role, &[])
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"branch_restack".to_string()));
        let combine = names.contains(&"branch_combine".to_string());
        assert_eq!(combine, role == Role::Planner, "{role:?}");
        assert_eq!(
            tools::kind(role, "branch_combine").is_some(),
            role == Role::Planner
        );
    }
    assert!(!tools::idempotent("branch_restack") && !tools::idempotent("branch_combine"));
}

/// Argument checks, through `tools::check` and the module's own parser.
#[test]
fn ai_agents_branch_tools_check_their_input() {
    use pigeon_ai_agents::{Role, branches, tools};
    assert!(tools::check(Role::Executor, "branch_restack", &json!({})).is_ok());
    assert!(tools::check(Role::Executor, "branch_restack", &json!({"x": 1})).is_err());
    assert!(
        tools::check(
            Role::Executor,
            "branch_combine",
            &json!({"branches": ["a"]})
        )
        .is_err()
    );
    let p = |v: Value| tools::check(Role::Planner, "branch_combine", &v);
    assert!(p(json!({})).is_err());
    assert!(p(json!({"branches": []})).is_err());
    assert!(p(json!({"branches": "claude/ai-agent-r1"})).is_err());
    assert!(p(json!({"branches": [1]})).is_err());
    assert!(p(json!({"branches": ["a b"]})).is_err());
    assert!(p(json!({"branches": ["-x"]})).is_err());
    assert!(p(json!({"branches": ["a/../b"]})).is_err());
    assert!(p(json!({"branches": ["claude/ai-agent-r1"], "x": 1})).is_err());
    let many: Vec<String> = (0..11).map(|i| format!("claude/ai-agent-r{i}")).collect();
    assert!(p(json!({ "branches": many })).is_err());
    // Order is kept and a repeat is dropped.
    let names = branches::parse_combine(
        &json!({"branches": ["claude/ai-agent-b", "claude/ai-agent-a", "claude/ai-agent-b"]}),
    )
    .unwrap();
    assert_eq!(names, ["claude/ai-agent-b", "claude/ai-agent-a"]);
}

/// Ownership: the own branch and the executors' branches pass; main and others are refused.
#[test]
fn ai_agents_branch_combine_ownership() {
    use pigeon_ai_agents::branches::check_ownership;
    let own = "claude/ai-agent-p1";
    let kids = json!([{"run": "c1"}, {"run": "c2"}]);
    let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(check_ownership(own, &kids, &n(&[own, "claude/ai-agent-c2"])).is_ok());
    assert!(check_ownership(own, &kids, &n(&["main"])).is_err());
    assert!(check_ownership(own, &kids, &n(&["origin/main"])).is_err());
    assert!(check_ownership(own, &kids, &n(&["claude/ai-agent-c3"])).is_err());
    assert!(check_ownership(own, &kids, &n(&["claude/ai-agent-c1", "claude/ai-agent-x"])).is_err());
    assert!(check_ownership(own, &json!([]), &n(&["claude/ai-agent-c1"])).is_err());
}

/// Conflict lists and exit codes are read from canned sandbox answers.
#[test]
fn ai_agents_branch_conflict_parse() {
    use pigeon_ai_agents::branches::{Conflict, parse_conflicts, parse_exit};
    let text = "exit 1 after 80 ms; 90 bytes of output, the whole of it in `/tmp/o`\n\
                @@CONFLICT@@ claude/ai-agent-c2\nsrc/a.rs\ndocs/b.md\n@@END@@\nerror: could not apply";
    assert_eq!(parse_exit(text), Some(1));
    assert_eq!(
        parse_conflicts(text),
        Some(Conflict {
            branch: Some("claude/ai-agent-c2".into()),
            paths: vec!["src/a.rs".into(), "docs/b.md".into()],
        })
    );
    let restack = "exit 1 after 5 ms\n@@CONFLICT@@\nx.rs\n@@END@@\n";
    let c = parse_conflicts(restack).unwrap();
    assert_eq!((c.branch, c.paths), (None, vec!["x.rs".to_string()]));
    assert_eq!(parse_conflicts("exit 0 after 5 ms\n@@DONE@@\n"), None);
    assert_eq!(parse_exit("timed out after 5 ms"), None);
}

/// The plan: commit order, one pick per commit, the dirty refusal and the dropping check.
#[test]
fn ai_agents_branch_combine_plan_and_scripts() {
    use pigeon_ai_agents::branches::{
        combine_script, list_script, parse_listing, plan, restack_script,
    };
    let own = "claude/ai-agent-p1";
    let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let c1 = "claude/ai-agent-c1";
    let c2 = "claude/ai-agent-c2";
    let names = n(&[c2, c1]);
    let out = format!("exit 0 after 9 ms\nOWN o1\nB {c2} s1\nB {c2} s2\nB {c1} s2\nB {c1} s3\n");
    let l = parse_listing(&out, &names);
    assert_eq!(l.own, ["o1"]);
    // o1 is in no listed branch, so it would be dropped.
    let err = plan(own, &names, &l).unwrap_err();
    assert!(err.contains("drop"), "{err}");
    // Naming the own branch lifts the refusal.
    let with_own = n(&[own, c2, c1]);
    let l2 = parse_listing(&format!("OWN o1\nB {own} o1\n{}", &out[18..]), &with_own);
    let p = plan(own, &with_own, &l2).unwrap();
    assert_eq!(p[0], (own.to_string(), vec!["o1".to_string()]));
    // Order is the list's; s2 is picked once, for the first branch that has it.
    assert_eq!(p[1].1, ["s1", "s2"]);
    assert_eq!(p[2].1, ["s3"]);
    // A covered own commit passes.
    let covered = parse_listing(&format!("OWN s1\nB {c2} s1\n"), &n(&[c2]));
    assert!(plan(own, &n(&[c2]), &covered).is_ok());
    // A dirty checkout is refused.
    assert!(plan(own, &n(&[c2]), &parse_listing("DIRTY\nB x s\n", &n(&[c2]))).is_err());
    // The scripts: own branch is read from HEAD, picks are in order, nothing touches main.
    let ls = list_script(own, &with_own);
    assert!(ls.contains("origin/main..HEAD") && ls.contains(&format!("origin/main...origin/{c1}")));
    assert!(
        !ls.contains("sed"),
        "a branch name has `/` in it, so no sed"
    );
    assert!(ls.contains("--no-merges") && ls.contains("--reverse"));
    let cs = combine_script(&p);
    assert!(cs.find("cherry-pick o1").unwrap() < cs.find("cherry-pick s1 s2").unwrap());
    assert!(cs.contains("cherry-pick --abort") && cs.contains("reset -q --hard \"$orig\""));
    assert!(!cs.contains("push") && !restack_script().contains("push"));
    assert!(restack_script().contains("git rebase origin/main"));
    assert!(restack_script().contains("git rebase --abort"));
}

/// The list and combine scripts run in a real git repository with branch names that have `/` in
/// them: the listing is read back, and a commit main already holds is left out.
#[test]
fn ai_agents_branch_combine_scripts_run_in_git() {
    use pigeon_ai_agents::branches::{combine_script, list_script, parse_listing, plan};
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("pigeon-combine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sh = |script: &str| -> String {
        let out = Command::new("sh")
            .args(["-c", script])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{script}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    sh(
        "git init -q -b main . && git config user.email t@example.com && git config user.name t \
        && echo a > a && git add a && git commit -qm base \
        && git update-ref refs/remotes/origin/main HEAD",
    );
    // The branch has one commit that main also gets (same change), and one that it does not.
    sh(
        "git checkout -qb work && echo b > b && git add b && git commit -qm same \
        && echo c > c && git add c && git commit -qm only \
        && git update-ref refs/remotes/origin/claude/ai-agent-x HEAD \
        && git checkout -q main && echo b > b && git add b && git commit -qm same-on-main \
        && git update-ref refs/remotes/origin/main HEAD",
    );
    let own = "claude/ai-agent-p";
    let names = vec!["claude/ai-agent-x".to_string()];
    let out = sh(&list_script(own, &names));
    let listing = parse_listing(&out, &names);
    assert_eq!(
        listing.branches[0].1.len(),
        1,
        "only the commit main lacks: {out}"
    );
    let p = plan(own, &names, &listing).unwrap();
    sh(&combine_script(&p));
    assert!(dir.join("c").exists() && dir.join("b").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Builds a tool-loop transcript: the brief, then `pairs` of assistant tool_use and user result.
fn ai_agents_loop_history(pairs: usize) -> Vec<Value> {
    let mut history = vec![json!({ "role": "user", "content": "brief" })];
    for i in 0..pairs {
        history.push(json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": format!("t{i}"), "name": "run", "input": {} }] }));
        history.push(json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": format!("t{i}"), "content": "ok" }] }));
    }
    history
}

/// A compaction of a tool loop keeps every tool_result next to its tool_use, alternates roles and
/// starts with a user message.
#[test]
fn ai_agents_compaction_cuts_a_tool_loop_cleanly() {
    use pigeon_ai_agents::transcript;
    for pairs in [10, 11, 25, 40] {
        let history = ai_agents_loop_history(pairs);
        let at = transcript::cut_point(&history, transcript::KEEP_TAIL).unwrap();
        assert_eq!(history[at]["role"], "assistant");
        let kept = transcript::compacted("summary", &history[at..]);
        assert_eq!(kept[0]["role"], "user");
        let mut uses = std::collections::HashSet::new();
        for (i, m) in kept.iter().enumerate() {
            let want = if i % 2 == 0 { "user" } else { "assistant" };
            assert_eq!(m["role"], want, "roles alternate at {i}");
            for b in m["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("tool_use") => {
                        uses.insert(b["id"].as_str().unwrap().to_string());
                    }
                    Some("tool_result") => assert!(
                        uses.contains(b["tool_use_id"].as_str().unwrap()),
                        "a result lost its tool_use"
                    ),
                    _ => {}
                }
            }
        }
    }
    assert!(transcript::cut_point(&ai_agents_loop_history(3), transcript::KEEP_TAIL).is_none());
    assert_eq!(transcript::COMPACT_AT, 100_000);
}

/// The tools and the system prompt in a request are the same bytes before and after a compaction,
/// so the cached prefix survives.
#[test]
fn ai_agents_compaction_keeps_the_cached_prefix() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::transcript::{self, Spec};
    let tools = vec![
        json!({ "name": "run", "description": "d", "input_schema": { "type": "object" } }),
        json!({ "name": "report", "description": "r", "input_schema": { "type": "object" } }),
    ];
    let spec = Spec {
        role: Role::Planner,
        model: "claude-opus-4-5",
        system: "You are a planner.",
        tools: &tools,
    };
    let history = ai_agents_loop_history(30);
    let at = transcript::cut_point(&history, transcript::KEEP_TAIL).unwrap();
    let after = transcript::compacted("summary", &history[at..]);
    let before = transcript::request(&spec, &history);
    let later = transcript::request(&spec, &after);
    assert_ne!(before["messages"], later["messages"]);
    for key in ["tools", "system", "model"] {
        assert_eq!(
            serde_json::to_vec(&before[key]).unwrap(),
            serde_json::to_vec(&later[key]).unwrap(),
            "{key} changed"
        );
    }
}

/// The 75% warning: cost counts what executors hold, sandbox time counts too, and a zero budget
/// never warns.
#[test]
fn ai_agents_warn_due_at_75_percent() {
    use pigeon_ai_agents::budget::{Budget, Over, Spent, warn_due};
    let budget = Budget {
        cost_micros: 10_000_000,
        sandbox_ms: 60 * 60_000,
        ..Budget::default_for(pigeon_ai_agents::Role::Planner)
    };
    assert_eq!(warn_due(&budget, &Spent::default()), None);
    let own = Spent {
        cost_micros: 7_499_999,
        ..Spent::default()
    };
    assert_eq!(warn_due(&budget, &own), None);
    let own = Spent {
        cost_micros: 7_500_000,
        ..Spent::default()
    };
    assert_eq!(warn_due(&budget, &own), Some(Over::Cost));
    let held = Spent {
        cost_micros: 2_000_000,
        executors_micros: 5_500_000,
        ..Spent::default()
    };
    assert_eq!(warn_due(&budget, &held), Some(Over::Cost));
    let sandbox = Spent {
        sandbox_ms: 45 * 60_000,
        ..Spent::default()
    };
    assert_eq!(warn_due(&budget, &sandbox), Some(Over::Sandbox));
    let zero = Budget {
        cost_micros: 0,
        sandbox_ms: 0,
        ..budget
    };
    assert_eq!(warn_due(&zero, &Spent::default()), None);
}

/// The hand-off section: rows for executors not landed, tips from reports, CI when known.
#[test]
fn ai_agents_handoff_section_from_children() {
    use pigeon_ai_agents::handoff;
    let tip = "0123456789abcdef0123456789abcdef01234567";
    let children = json!([
        { "run": "a", "branch": "claude/ai-agent-a", "state": "done", "ended_at": 5,
          "report": format!("Pushed. Tip {tip}, CI green.") },
        { "run": "b", "branch": "claude/ai-agent-b", "state": "done", "ended_at": 6,
          "report": "landed already" },
        { "run": "c", "branch": "claude/ai-agent-c", "state": "running", "ended_at": null,
          "report": null },
    ]);
    assert_eq!(
        handoff::tip_from_report(&format!("x {tip}.")),
        Some(tip.to_string())
    );
    assert_eq!(handoff::tip_from_report("abc1234"), None);
    let landed = handoff::landed_branches(&json!([
        { "branch": "refs/heads/claude/ai-agent-b", "state": "landed" },
        { "branch": "claude/ai-agent-c", "state": "failed" },
    ]));
    assert!(landed.contains("claude/ai-agent-b") && landed.len() == 1);
    let ci_answer = json!({ "jobs": [
        { "job": "lint", "state": "failed" }, { "job": "test", "state": "passed" } ] });
    assert_eq!(handoff::ci_word(&ci_answer), "passed");
    assert_eq!(handoff::ci_word(&json!({ "jobs": [] })), "none recorded");
    let mut ci = std::collections::HashMap::new();
    ci.insert(tip.to_string(), handoff::ci_word(&ci_answer));
    let text = handoff::section(&children, &landed, &ci).unwrap();
    assert!(text.starts_with("## Unlanded branches"));
    assert!(text.contains(&format!(
        "| `claude/ai-agent-a` | `{tip}` | done | passed |"
    )));
    assert!(text.contains("| `claude/ai-agent-c` | unknown | running (not ended) | unknown |"));
    assert!(!text.contains("ai-agent-b"));
    // Nothing to show: no section.
    assert!(handoff::section(&json!([]), &landed, &ci).is_none());
    // The model's text comes first; a second append adds nothing.
    let report = handoff::append("All done.", &text);
    assert!(report.starts_with("All done.\n\n## Unlanded branches"));
    assert_eq!(handoff::append(&report, &text), report);
    assert_eq!(handoff::append("", &text), text);
}

/// The model of a planner: its own, else its kind's stored model, else the stored planner
/// default, else the built-in. No kind behaves as before.
#[test]
fn ai_agents_kind_model_resolution() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{Choice, Defaults, Kind, Slot, merge, resolve};
    let p = |t: &str| Choice::parse(t, Slot::Planner).unwrap();
    let mut d = Defaults::default();
    let fix = Some(Kind::Fix);
    // Nothing stored: the built-in, with or without a kind.
    assert_eq!(
        resolve(Role::Planner, None, None, fix, &d).text(),
        "anthropic/claude-opus-5-5"
    );
    // A stored planner default applies to every kind.
    d.put(Slot::Planner, p("cf-openai/gpt-6.2-astra"));
    assert_eq!(
        resolve(Role::Planner, None, None, fix, &d).text(),
        "cf-openai/gpt-6.2-astra"
    );
    // The kind's slot beats the planner default; other kinds fall through.
    d.put(Slot::Kind(Kind::Fix), p("cf-anthropic/claude-sonnet-5-5"));
    assert_eq!(
        resolve(Role::Planner, None, None, fix, &d).text(),
        "cf-anthropic/claude-sonnet-5-5"
    );
    assert_eq!(
        resolve(Role::Planner, None, None, Some(Kind::Docs), &d).text(),
        "cf-openai/gpt-6.2-astra"
    );
    // No kind: exactly `merge`.
    assert_eq!(
        resolve(Role::Planner, None, None, None, &d),
        merge(Role::Planner, None, None, &d)
    );
    assert_eq!(
        resolve(Role::Planner, None, None, None, &d).text(),
        "cf-openai/gpt-6.2-astra"
    );
    // The run's own model wins.
    let own = p("cf-openai/gpt-6-luna");
    assert_eq!(
        resolve(Role::Planner, Some(&own), None, fix, &d).text(),
        "cf-openai/gpt-6-luna"
    );
    // A kind never touches another role.
    assert_eq!(
        resolve(Role::Executor, None, None, fix, &d).text(),
        "anthropic/claude-sonnet-5-5"
    );
}

/// `kind` is checked at start; the kind slots are checked like the others and shown unset.
#[test]
fn ai_agents_kind_arg_and_slots() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{Defaults, Kind, Slot, check_set, defaults_answer, start_kind};
    assert_eq!(start_kind(Role::Planner, &json!({})).unwrap(), None);
    for k in ["integrate", "docs", "fix", "design", "build", "live check"] {
        let got = start_kind(Role::Planner, &json!({ "kind": k })).unwrap();
        assert_eq!(got, Some(k));
    }
    assert!(start_kind(Role::Planner, &json!({ "kind": "other" })).is_err());
    assert!(start_kind(Role::Planner, &json!({ "kind": 3 })).is_err());
    assert!(start_kind(Role::Executor, &json!({ "kind": "fix" })).is_err());
    assert!(start_kind(Role::Mechanical, &json!({ "kind": "docs" })).is_err());
    assert_eq!(start_kind(Role::Executor, &json!({})).unwrap(), None);
    // Slots: same checks as the others.
    let set = check_set(&json!({ "planner_docs": "cf-openai" })).unwrap();
    assert_eq!(set[0].0, Slot::Kind(Kind::Docs));
    assert_eq!(set[0].1.text(), "cf-openai/gpt-6.2-astra");
    assert!(check_set(&json!({ "planner_docs": "nope/x" })).is_err());
    assert!(check_set(&json!({ "planner_other": "cf-openai" })).is_err());
    // Shown unset by default; stored and read back once set.
    let a = defaults_answer(&Defaults::default(), &[]);
    for k in ["integrate", "docs", "fix", "design"] {
        let slot = &a["defaults"][format!("planner_{k}")];
        assert_eq!(slot["source"], "unset");
        assert!(slot["model"].is_null());
    }
    let d = Defaults::from_json(&json!({ "planner_fix": "cf-openai/gpt-6.2-astra" }));
    assert_eq!(Defaults::from_json(&d.to_json()), d);
    let a = defaults_answer(&d, &[]);
    assert_eq!(a["defaults"]["planner_fix"]["source"], "stored");
}

/// With nothing stored, every role's resolved model is its built-in default, as before kinds.
#[test]
fn ai_agents_kind_defaults_unchanged() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{Defaults, KINDS, merge, resolve};
    let d = Defaults::default();
    for (role, want) in [
        (Role::Planner, "anthropic/claude-opus-5-5"),
        (Role::Executor, "anthropic/claude-sonnet-5-5"),
        (Role::Mechanical, "anthropic/claude-haiku-5-5"),
    ] {
        assert_eq!(merge(role, None, None, &d).text(), want);
        assert_eq!(resolve(role, None, None, None, &d).text(), want);
        for k in KINDS {
            assert_eq!(resolve(role, None, None, Some(k), &d).text(), want);
        }
    }
    assert_eq!(d.to_json(), json!({}));
}

/// The compare groups carry a planner's kind, and runs of different kinds are separate groups.
#[test]
fn ai_agents_kind_in_compare() {
    use pigeon_ai_agents::compare::groups;
    let row = |kind: Value| {
        json!({ "role": "planner", "model": "anthropic/claude-opus-5-5", "kind": kind,
                "state": "done", "spent": { "cost_micros": 1000000, "turns": 3 },
                "created_at": 0, "started_at": 0, "ended_at": 60000 })
    };
    let g = groups(
        &[row(json!("fix")), row(json!("docs")), row(Value::Null)],
        None,
    );
    assert_eq!(g.len(), 3);
    let kinds: Vec<&Value> = g.iter().map(|x| &x["kind"]).collect();
    assert!(kinds.contains(&&json!("fix")) && kinds.contains(&&Value::Null));
}

/// Agent turns ask a reasoning effort by role on OpenAI models, so GPT-6 tool turns go to
/// /responses; Anthropic requests without thinking stay without effort.
#[test]
fn ai_agents_role_effort_routes_openai_to_responses() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{Defaults, SLOTS, defaults_answer};
    use pigeon_ai_agents::transcript::{self, Spec};
    let tools = [json!({
        "name": "grep", "description": "d",
        "input_schema": { "type": "object", "properties": {} }
    })];
    let messages = [json!({ "role": "user", "content": "go" })];
    for (role, want) in [
        (Role::Planner, "high"),
        (Role::Executor, "medium"),
        (Role::Mechanical, "low"),
    ] {
        let spec = Spec {
            role,
            model: "gpt-6.1-terra",
            system: "rules",
            tools: &tools,
        };
        let req = transcript::request(&spec, &messages);
        assert_eq!(req["output_config"]["effort"], want);
        assert!(pigeon_models::openai::wants_responses(&req));
    }
    // The last turn carries no effort; terra refuses `none`, so it still goes to Responses.
    let report = [json!({
        "name": "report", "description": "d",
        "input_schema": { "type": "object", "properties": {} }
    })];
    let spec = Spec {
        role: Role::Executor,
        model: "gpt-6.1-terra",
        system: "rules",
        tools: &report,
    };
    let last = transcript::last_turn_request(&spec, &messages);
    assert!(pigeon_models::openai::wants_responses(&last));
    // Anthropic: a Claude without thinking carries none; a thinking Claude's mechanical turn too.
    for model in ["claude-haiku-5-5", "claude-sonnet-5-5"] {
        let spec = Spec {
            role: Role::Mechanical,
            model,
            system: "rules",
            tools: &tools,
        };
        assert!(
            transcript::request(&spec, &messages)
                .get("output_config")
                .is_none()
        );
    }
    // The summary asks low on OpenAI only.
    let s = transcript::summary_request("gpt-6.1-terra", &messages);
    assert_eq!(s["output_config"]["effort"], "low");
    assert!(
        transcript::summary_request("claude-haiku-5-5", &messages)
            .get("output_config")
            .is_none()
    );
    // ai_agents_models shows each slot's default effort.
    let a = defaults_answer(&Defaults::default(), &[]);
    for (slot, want) in SLOTS.iter().zip(["high", "medium", "low", "low"]) {
        assert_eq!(a["defaults"][slot.name()]["effort"], want);
    }
}

/// The job record names the OpenAI endpoint a request goes to.
#[test]
fn models_job_endpoint_chat_or_responses() {
    use pigeon_models::{Dialect, endpoint_of};
    let tools = json!([{
        "name": "grep", "description": "d",
        "input_schema": { "type": "object", "properties": {} }
    }]);
    let req = |effort: Option<&str>| {
        let mut r = json!({
            "model": "gpt-6-luna", "max_tokens": 100, "tools": tools,
            "messages": [{ "role": "user", "content": "go" }]
        });
        if let Some(e) = effort {
            r["output_config"] = json!({ "effort": e });
        }
        pigeon_models::openai::request(&r).unwrap()
    };
    assert_eq!(
        endpoint_of(Dialect::OpenAi, &req(Some("medium"))),
        Some("responses")
    );
    assert_eq!(endpoint_of(Dialect::OpenAi, &req(None)), Some("chat"));
    assert_eq!(endpoint_of(Dialect::Anthropic, &json!({})), None);
}

#[test]
fn ai_agents_token_capacity_retries_the_same_turn() {
    use pigeon_ai_agents::run::{Recovery, on_failure};
    let error = pigeon_core::tokens::CAPACITY_ERROR;
    assert_eq!(
        on_failure("failed", error, 1, 0, &["model"]),
        Recovery::WaitTokenCapacity
    );
    assert_eq!(
        on_failure("failed", error, 50, 0, &["model"]),
        Recovery::WaitTokenCapacity
    );
    assert_eq!(
        on_failure("cancelled", error, 1, 0, &["model"]),
        Recovery::Fail
    );
}

/// The last turn never sends a reasoning effort the model refuses: gpt-6.2-sol goes to Responses
/// with effort low, luna stays on chat with `none`, and a Claude request has no effort.
#[test]
fn ai_agents_last_turn_sends_an_accepted_effort() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::transcript::{self, Spec};
    let tools = [json!({
        "name": "report", "description": "d",
        "input_schema": { "type": "object", "properties": {} }
    })];
    let messages = [json!({ "role": "user", "content": "go" })];
    let last = |model: &str| {
        let spec = Spec {
            role: Role::Executor,
            model,
            system: "rules",
            tools: &tools,
        };
        transcript::last_turn_request(&spec, &messages)
    };
    let sol = pigeon_models::openai::request(&last("openai/gpt-6.2-sol")).unwrap();
    assert_eq!(sol["reasoning"], json!({ "effort": "low" }));
    assert!(sol.get("reasoning_effort").is_none() && sol.get("messages").is_none());
    let luna = pigeon_models::openai::request(&last("gpt-6-luna")).unwrap();
    assert_eq!(luna["reasoning_effort"], "none");
    assert!(luna["messages"].is_array());
    let claude = last("claude-opus-5-5");
    assert!(claude.get("output_config").is_none());
}

#[test]
fn ai_agents_preflight_extracts_the_paths_a_brief_names() {
    use pigeon_ai_agents::preflight::paths_in;
    let brief = "Read `CLAUDE.md` and `.claude/decisions/`, then src/pigeon-ios/App.swift:42. \
                 Also see src/ai-agents/src/*, docs/plans/ai-agents.md.";
    assert_eq!(
        paths_in(brief),
        vec![
            "CLAUDE.md",
            ".claude/decisions",
            "src/pigeon-ios/App.swift",
            "src/ai-agents/src",
            "docs/plans/ai-agents.md",
        ]
    );
    // Repeats once, and the cap holds.
    assert_eq!(paths_in("`a/b` a/b `a/b/`"), vec!["a/b"]);
    let many: String = (0..60).map(|i| format!("dir/f{i}.rs ")).collect();
    assert_eq!(paths_in(&many).len(), 40);
}

#[test]
fn ai_agents_preflight_ignores_what_is_not_a_path() {
    use pigeon_ai_agents::preflight::paths_in;
    for brief in [
        "use and/or here, yes/no, read/write",
        "see https://example.com/a/b.html and http://x.dev/y",
        "branch claude/ai-agent-abc, origin/main, refs/heads/x",
        "run /usr/bin/env or ~/notes/a.md",
        "version v1/v2 or 1.2/3",
        "`git log src/x`, `$HOME/a`, {{tpl/x}}, src/**/*.rs, a/$b, `x|y/z`",
        "plain words and `Vec.len` or `main`",
    ] {
        assert!(paths_in(brief).is_empty(), "{brief}: {:?}", paths_in(brief));
    }
}

#[test]
fn ai_agents_preflight_answer_shape() {
    use pigeon_ai_agents::preflight::{command, missing_from, note_for, sentence};
    let paths: Vec<String> = ["CLAUDE.md", "src/core", ".claude/decisions"]
        .map(String::from)
        .to_vec();
    assert!(command(&paths).contains("'.claude/decisions'"));
    let out = "MISSING CLAUDE.md\nMISSING .claude/decisions\n";
    let missing = missing_from(0, out, &paths).unwrap();
    assert_eq!(missing, vec!["CLAUDE.md", ".claude/decisions"]);
    assert_eq!(
        sentence(3, &missing, "main"),
        "preflight: 2 of 3 paths named in the brief do not exist at `main`: CLAUDE.md, .claude/decisions."
    );
    let note = note_for(&missing, "main").unwrap();
    assert!(note.contains("CLAUDE.md") && note.contains("instead of asking"));
    // Nothing missing: no note. A broken check is an error, not "all missing".
    assert!(note_for(&[], "main").is_none());
    assert!(sentence(3, &[], "main").contains("all 3 paths"));
    assert!(sentence(0, &[], "main").contains("no repo paths"));
    assert!(missing_from(3, "NOGIT\n", &paths).is_err());
    assert!(missing_from(1, "", &paths).is_err());
    assert_eq!(missing_from(0, "", &paths).unwrap(), Vec::<String>::new());
}

/// The committed repo map is what `ci/repo-map.sh` writes now.
#[test]
fn ai_agents_repo_map_current() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let out = std::env::temp_dir().join(format!("pigeon-repo-map-{}.md", std::process::id()));
    let run = std::process::Command::new("sh")
        .arg(format!("{root}/ci/repo-map.sh"))
        .arg(&out)
        .output()
        .expect("sh runs");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let fresh = std::fs::read_to_string(&out).expect("map written");
    let _ = std::fs::remove_file(&out);
    assert!(
        fresh == pigeon_ai_agents::prompts::REPO_MAP,
        "src/ai-agents/repo-map.md is stale: run `sh ci/repo-map.sh` and commit it"
    );
}

/// The system blocks, with the map, are the same for two briefs; only the messages differ.
#[test]
fn ai_agents_repo_map_prefix_is_stable() {
    use pigeon_ai_agents::transcript::{self, Spec};
    use pigeon_ai_agents::{Role, prompts};
    let tools =
        vec![json!({ "name": "report", "description": "r", "input_schema": { "type": "object" } })];
    let system = prompts::system(Role::Executor, "claude/x");
    let spec = Spec {
        role: Role::Executor,
        model: "claude-opus-4-5",
        system: &system,
        tools: &tools,
    };
    let ask = |brief: &str| {
        let start = json!({ "brief": brief, "base": "main", "branch": "claude/x" });
        let first = json!({ "role": "user", "content": prompts::first_message(&start) });
        transcript::request(&spec, &[first])
    };
    let (a, b) = (ask("build one thing"), ask("fix another"));
    assert_ne!(a["messages"], b["messages"]);
    for key in ["system", "tools", "model"] {
        assert_eq!(
            serde_json::to_vec(&a[key]).unwrap(),
            serde_json::to_vec(&b[key]).unwrap(),
            "{key} changed"
        );
    }
    let blocks = a["system"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1]["text"], prompts::REPO_MAP);
    assert!(blocks[1]["cache_control"].is_object());
    assert!(!a["messages"].to_string().contains("# Repo map"));
}

/// The map stays short and holds no `{` template text.
#[test]
fn ai_agents_repo_map_is_small_and_plain() {
    let map = pigeon_ai_agents::prompts::REPO_MAP;
    assert!(map.lines().count() < 160, "map too long");
    assert!(!map.contains('{'), "map has template text");
    assert!(map.starts_with("# Repo map"));
}
