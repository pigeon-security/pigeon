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
            pigeon_models::Dialect::Anthropic
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
        None,
        "no clean place to cut"
    );
    history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "done?" }] }));
    history.push(
        json!({ "role": "user", "content": "Call `report` to end the run, or keep working." }),
    );
    history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }));
    let at = transcript::cut_point(&history, 4).unwrap();
    assert_eq!(history[at]["role"], "user");
    let kept = transcript::compacted("did things", &history[at..]);
    assert_eq!(kept[0]["role"], "user");
    assert_eq!(kept[1]["role"], "assistant");
    assert!(
        kept[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("did things")
    );
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
    // Live slots (docs/plans/ai-agents.md, decision 4): one ceiling over every role, no per-role
    // planner cap, executors always keep a reserve, and each role starts oldest first.
    {
        use pigeon_ai_agents::Role;
        let planner_cap = object::MAX_LIVE - object::EXECUTOR_RESERVE;
        assert!(planner_cap > 2 && object::MAX_LIVE >= 32);
        // Not the oldest of its role: waits, whatever the room.
        assert!(!object::may_start(Role::Planner, 0, false));
        assert!(!object::may_start(Role::Executor, 0, false));
        // The index's admission, as `slot` runs it: queued runs, in created order, each taking a
        // slot when it is the oldest queued of its role and `may_start` lets it.
        let admit = |queue: &mut Vec<(Role, u32)>, live: &mut Vec<(Role, u32)>| {
            loop {
                let next = queue.iter().position(|(role, _)| {
                    let oldest = queue
                        .iter()
                        .find(|(r, _)| (*r == Role::Planner) == (*role == Role::Planner))
                        .map(|(_, n)| *n);
                    let me = queue.iter().find(|(r, _)| r == role).map(|(_, n)| *n);
                    object::may_start(*role, live.len() as u32, oldest == me)
                });
                match next {
                    Some(i) => live.push(queue.remove(i)),
                    None => break,
                }
            }
        };
        // Five planners queued at once all start: no more waiting behind two.
        let mut live = vec![];
        let mut queue: Vec<(Role, u32)> = (0..5).map(|n| (Role::Planner, n)).collect();
        admit(&mut queue, &mut live);
        assert_eq!(live.len(), 5);
        assert!(queue.is_empty());
        // Many more planners: they fill up to the ceiling less the reserve, oldest first.
        let mut queue: Vec<(Role, u32)> = (5..40).map(|n| (Role::Planner, n)).collect();
        admit(&mut queue, &mut live);
        assert_eq!(live.len() as u32, planner_cap);
        assert_eq!(
            live.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            (0..planner_cap).collect::<Vec<_>>()
        );
        assert_eq!(queue.first(), Some(&(Role::Planner, planner_cap)));
        // Their executors still start, in the reserve, oldest first, up to the ceiling.
        queue.extend((100..120).map(|n| {
            (
                if n % 2 == 0 {
                    Role::Executor
                } else {
                    Role::Mechanical
                },
                n,
            )
        }));
        admit(&mut queue, &mut live);
        assert_eq!(live.len() as u32, object::MAX_LIVE);
        let started: Vec<u32> = live[planner_cap as usize..]
            .iter()
            .map(|(_, n)| *n)
            .collect();
        assert_eq!(
            started,
            (100..100 + object::EXECUTOR_RESERVE).collect::<Vec<_>>()
        );
        // At the ceiling nothing starts.
        assert!(!object::may_start(Role::Executor, object::MAX_LIVE, true));
        assert!(!object::may_start(Role::Planner, planner_cap, true));
        // A run ends: the oldest queued executor takes its place, not a planner (no room for one
        // while the reserve is in use).
        live.retain(|(_, n)| *n != 3);
        admit(&mut queue, &mut live);
        assert_eq!(
            live.last(),
            Some(&(Role::Executor, 100 + object::EXECUTOR_RESERVE))
        );
        assert_eq!(queue.first(), Some(&(Role::Planner, planner_cap)));
    }
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
    assert_eq!(estimate::kind("add a smoke scenario"), "test");
    assert_eq!(estimate::kind("Rename the module"), "mechanical");
    assert_eq!(estimate::kind("Add a new tool"), "build");
    assert_eq!(
        estimate::kind("fix the docs-only plan"),
        "docs",
        "docs first"
    );

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
    let few = estimate::estimate(Role::Executor, &brief, &samples(&[(100, 1), (100, 2)]));
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
    let none = estimate::estimate(Role::Planner, &brief, &samples(&[(100, 1); 5]));
    assert_eq!(none.samples, 0);
    assert_eq!(
        none.suggested.executors,
        Some(Budget::default_for(Role::Planner).executors)
    );

    // Ten runs of $1 to $10: median rank 5, low rank 1, high rank 9. 9 * 1.25 = 11.25, up to 11.5.
    let ten: Vec<(usize, u64)> = (1..=10).map(|i| (100, i * 1_000_000)).collect();
    let e = estimate::estimate(Role::Executor, &brief, &samples(&ten));
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
    let cheap = estimate::estimate(Role::Executor, &brief, &samples(&[(100, 10); 3]));
    assert_eq!(cheap.suggested.cost_micros, 500_000);
    let dear = estimate::estimate(Role::Executor, &brief, &samples(&[(100, 90_000_000); 3]));
    assert_eq!(dear.suggested.cost_micros, Budget::HARD.cost_micros);

    // The nearest ten by size win over farther ones.
    let mut mixed: Vec<(usize, u64)> = vec![(100_000, 50_000_000); 10];
    mixed.extend(vec![(120, 1_000_000); 10]);
    let near = estimate::estimate(Role::Executor, &brief, &samples(&mixed));
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

    // A live child holds the larger of budget and spend; an ended child holds its spend.
    let kids = json!([
        {"ended_at": null, "spent": {"cost": "$1", "cost_usd": 1.0}, "budget": {"cost": "$5", "budget_cost_usd": 5.0}},
        {"ended_at": null, "spent": {"cost": "$7", "cost_usd": 7.0}, "budget": {"cost": "$5", "budget_cost_usd": 5.0}},
        {"ended_at": 9, "spent": {"cost": "$1.25"}, "budget": {"cost": "$5"}},
    ]);
    assert_eq!(
        budget::executors_committed(&kids),
        5_000_000 + 7_000_000 + 1_250_000
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
