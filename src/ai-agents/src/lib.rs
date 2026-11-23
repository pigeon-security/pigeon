//! AI agents: planner and executor runs inside pigeon (`docs/plans/ai-agents.md`). A run is an AI agent
//! loop: it asks a model for its next turn through pigeon's model gateway, runs the tools the
//! turn calls (in its sandbox, through pigeon's dispatch, or itself), and goes on until it
//! reports, is stopped or runs out of budget.
//!
//! The loop's logic is pure (no I/O, no clock), so the smoke suite tests it natively:
//!
//! - [`run`]: a run's states and the next action, from what the run knows.
//! - [`budget`]: budgets per run, what is spent, and model prices.
//! - [`transcript`]: the next turn's request, tool results, compaction.
//! - [`tools`]: the tools each role is offered, and each call's checks.
//! - [`prompts`]: what a run is told.
//! - [`estimate`]: a run's cost, estimated from similar past runs.
//! - [`models`]: which model a run uses (a choice per run, stored defaults, fallback chains).
//! - [`compare`]: ended runs grouped by role and model.
//!
//! The Worker side carries out what [`run::next`] decides:
//!
//! - [`object`]: the runs' Durable Object (the index, and one object per run).
//! - [`sandbox`]: a run's sandbox, a container with `pigeon-sandboxd` in it.
//! - [`service`]: the `agents_*` tools behind the contract, as the `pigeon-ai-agents` Worker
//!   serves them.

use serde::{Deserialize, Serialize};

pub mod budget;
pub mod compare;
pub mod estimate;
pub mod models;
pub mod object;
pub mod page;
pub mod prompts;
pub mod run;
pub mod sandbox;
pub mod service;
pub mod tools;
pub mod transcript;

/// The versions the ai-agents Worker declares: the `ai-agents` capability.
pub fn versions() -> Vec<pigeon_core::versions::Component> {
    vec![pigeon_core::versions::Component::began(
        service::NAME,
        pigeon_core::versions::Kind::Capability,
    )]
}

/// What an AI agent run is for.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Plans a piece of work, spawns executors and reviews what they push.
    Planner,
    /// Builds a well-specified piece of code in its sandbox and pushes it.
    Executor,
    /// An executor for mechanical changes (moves, renames, docs), on a cheaper model.
    Mechanical,
}

impl Role {
    /// Its name, as kept and shown.
    pub fn name(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Executor => "executor",
            Role::Mechanical => "mechanical",
        }
    }

    /// The built-in Anthropic models of this role, first choice first. A run's own chain comes
    /// from [`models::chain_of`]; this is what it equals when no model was chosen.
    pub fn models(self) -> &'static [&'static str] {
        match self {
            Role::Planner => &["claude-opus-5-5", "claude-sonnet-5-5"],
            Role::Executor => &["claude-sonnet-5-5", "claude-opus-5-5"],
            Role::Mechanical => &["claude-haiku-5-5", "claude-sonnet-5-5"],
        }
    }
}

/// The model gateway's task label for turn `turn` of run `run`: cost per run is
/// `models_usage {task_prefix: "ai-agent/{run}/"}`.
pub fn task(run: &str, role: Role, turn: u32) -> String {
    let t = format!("ai-agent/{run}/{}/{turn}", role.name());
    t.chars().take(100).collect()
}
