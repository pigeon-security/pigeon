//! Policy for what an actor may do.
//!
//! This module parses `.pigeon/policy.toml` and evaluates one request
//! against it. A request names an actor, an action, a target, and a context.
//! The answer gives a decision, the rule that made it, a reason, and any
//! approval the request still needs.
//!
//! Evaluation is deterministic. The first matching rule wins. When no rule
//! matches, the answer is deny with the reason `not in policy`.
//!
//! The module is pure. It does no I/O and builds for wasm32 and native.

use serde::{Deserialize, Serialize};

/// The kind of action a request asks to perform.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Send a message to a session.
    MessageSession,
    /// Start a new session.
    StartSession,
    /// Archive a session.
    ArchiveSession,
    /// Hand off a branch to another party.
    HandoffBranch,
    /// Land a branch.
    Land,
    /// Deploy a build.
    Deploy,
    /// Push a branch.
    PushBranch,
    /// Rewrite the history of a branch.
    RewriteBranch,
    /// Delete a branch.
    DeleteBranch,
    /// Move the main branch.
    MoveMain,
    /// Change the policy file.
    ChangePolicy,
    /// Change a gate.
    ChangeGate,
    /// Add a test.
    AddTest,
    /// Call one of the platform's tools; the target is the tool's name.
    CallTool,
}

impl Kind {
    /// Every kind, in declaration order.
    pub const ALL: [Kind; 14] = [
        Kind::MessageSession,
        Kind::StartSession,
        Kind::ArchiveSession,
        Kind::HandoffBranch,
        Kind::Land,
        Kind::Deploy,
        Kind::PushBranch,
        Kind::RewriteBranch,
        Kind::DeleteBranch,
        Kind::MoveMain,
        Kind::ChangePolicy,
        Kind::ChangeGate,
        Kind::AddTest,
        Kind::CallTool,
    ];

    /// The snake_case name of this kind, as written in the policy file.
    pub fn name(self) -> &'static str {
        match self {
            Kind::MessageSession => "message_session",
            Kind::StartSession => "start_session",
            Kind::ArchiveSession => "archive_session",
            Kind::HandoffBranch => "handoff_branch",
            Kind::Land => "land",
            Kind::Deploy => "deploy",
            Kind::PushBranch => "push_branch",
            Kind::RewriteBranch => "rewrite_branch",
            Kind::DeleteBranch => "delete_branch",
            Kind::MoveMain => "move_main",
            Kind::ChangePolicy => "change_policy",
            Kind::ChangeGate => "change_gate",
            Kind::AddTest => "add_test",
            Kind::CallTool => "call_tool",
        }
    }

    /// The kind with the given snake_case name, if there is one.
    pub fn named(s: &str) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.name() == s)
    }
}

/// Whether a request is allowed or denied.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// The request may go ahead.
    Allow,
    /// The request may not go ahead.
    Deny,
}

/// A kind of approval a rule can ask for.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    /// Approval from the person's delegate.
    Delegate,
    /// Approval from the person's passkey.
    Passkey,
}

impl Approval {
    /// Whether approval `self` meets what `needed` asks: Passkey meets both; Delegate meets
    /// Delegate.
    pub fn meets(self, needed: Approval) -> bool {
        self == Approval::Passkey || needed == Approval::Delegate
    }

    /// The lowercase name of this approval, as written in the policy file.
    pub fn name(self) -> &'static str {
        match self {
            Approval::Delegate => "delegate",
            Approval::Passkey => "passkey",
        }
    }

    fn named(s: &str) -> Option<Approval> {
        match s {
            "delegate" => Some(Approval::Delegate),
            "passkey" => Some(Approval::Passkey),
            _ => None,
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            Approval::Passkey => "the person's passkey approval",
            Approval::Delegate => "the approval of the person's delegate",
        }
    }
}

/// One validated rule from the policy file.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The rule id. Lowercase ascii letters, digits, and hyphens.
    pub id: String,
    /// The actions this rule covers. Never empty.
    pub actions: Vec<Kind>,
    /// Target patterns. Empty means every target.
    pub targets: Vec<String>,
    /// Target patterns this rule never covers.
    pub except: Vec<String>,
    /// The decision this rule makes.
    pub decision: Decision,
    /// The approval an allow rule asks for, if any.
    pub approval: Option<Approval>,
    /// Why the rule exists.
    pub reason: String,
}

/// A validated policy.
#[derive(Debug, Clone, Serialize)]
pub struct Policy {
    /// The file format version. Always 1.
    pub version: u32,
    /// The rules, in file order.
    pub rules: Vec<Rule>,
}

/// What the platform fills the placeholders with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vars {
    /// What `@session/` expands to, for example `claude/`. Ends with `/`.
    pub session_prefix: String,
    /// What `@main` expands to, for example `main`.
    pub main: String,
}

/// Extra facts about a request.
#[derive(Deserialize, Serialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    /// The approval the request carries, if any.
    #[serde(default)]
    pub approval: Option<Approval>,
}

/// One request to evaluate.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Who asks. Used for the audit line, not for matching.
    pub actor: String,
    /// The action asked for.
    pub action: Kind,
    /// The target of the action, such as a branch or a path.
    #[serde(default)]
    pub target: String,
    /// Extra facts about the request.
    #[serde(default)]
    pub context: Context,
}

/// The result of evaluating one request.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Whether the request is allowed.
    pub decision: Decision,
    /// The id of the rule that decided, or none when no rule matched.
    pub rule: Option<String>,
    /// Why the decision was made.
    pub reason: String,
    /// The approval the matching rule asks for and the request lacked.
    pub needs: Option<Approval>,
}

/// The reason given when no rule matches.
pub const NO_MATCH: &str = "not in policy";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    #[serde(default)]
    version: Option<i64>,
    #[serde(default)]
    rule: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    actions: Vec<String>,
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    except: Vec<String>,
    decision: String,
    #[serde(default)]
    approval: Option<String>,
    reason: String,
}

/// Parse and validate the text of `.pigeon/policy.toml`.
///
/// Any problem refuses the whole file. The error names the rule, key, or value.
pub fn parse(text: &str) -> Result<Policy, String> {
    let raw: RawPolicy = toml::from_str(text).map_err(|e| format!("policy.toml: {e}"))?;

    match raw.version {
        None => return Err("policy.toml: `version` is missing; it must be 1".into()),
        Some(1) => {}
        Some(v) => {
            return Err(format!(
                "policy.toml: `version` is {v}; only version 1 is supported"
            ));
        }
    }

    if raw.rule.is_empty() {
        return Err("policy.toml: no [[rule]] entries".into());
    }

    let mut rules: Vec<Rule> = Vec::with_capacity(raw.rule.len());
    for (index, r) in raw.rule.into_iter().enumerate() {
        let rule = validate_rule(index, r)?;
        if rules.iter().any(|x| x.id == rule.id) {
            return Err(format!(
                "policy.toml: rule id `{}` is used more than once",
                rule.id
            ));
        }
        rules.push(rule);
    }

    Ok(Policy { version: 1, rules })
}

fn validate_rule(index: usize, r: RawRule) -> Result<Rule, String> {
    if r.id.is_empty() {
        return Err(format!(
            "policy.toml: rule #{} has an empty `id`",
            index + 1
        ));
    }
    if !r
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "policy.toml: rule id `{}` must use only lowercase ascii letters, digits, and hyphens",
            r.id
        ));
    }
    let id = r.id;

    if r.actions.is_empty() {
        return Err(format!("policy.toml: rule `{id}` has empty `actions`"));
    }
    let mut actions: Vec<Kind> = Vec::with_capacity(r.actions.len());
    for a in &r.actions {
        let kind = Kind::named(a)
            .ok_or_else(|| format!("policy.toml: rule `{id}` has unknown action `{a}`"))?;
        if actions.contains(&kind) {
            return Err(format!(
                "policy.toml: rule `{id}` lists action `{a}` more than once"
            ));
        }
        actions.push(kind);
    }

    let decision = match r.decision.as_str() {
        "allow" => Decision::Allow,
        "deny" => Decision::Deny,
        other => {
            return Err(format!(
                "policy.toml: rule `{id}` has unknown decision `{other}`; use \"allow\" or \"deny\""
            ));
        }
    };

    let approval = match &r.approval {
        None => None,
        Some(s) => Some(Approval::named(s).ok_or_else(|| {
            format!(
                "policy.toml: rule `{id}` has unknown approval `{s}`; use \"passkey\" or \"delegate\""
            )
        })?),
    };

    if approval.is_some() && decision == Decision::Deny {
        return Err(format!(
            "policy.toml: rule `{id}` sets `approval` on a deny rule"
        ));
    }

    if r.reason.trim().is_empty() {
        return Err(format!("policy.toml: rule `{id}` has an empty `reason`"));
    }

    for p in &r.targets {
        check_pattern(&id, "targets", p)?;
    }
    for p in &r.except {
        check_pattern(&id, "except", p)?;
    }

    Ok(Rule {
        id,
        actions,
        targets: r.targets,
        except: r.except,
        decision,
        approval,
        reason: r.reason,
    })
}

fn check_pattern(id: &str, key: &str, p: &str) -> Result<(), String> {
    if p.is_empty() {
        return Err(format!(
            "policy.toml: rule `{id}` has an empty pattern in `{key}`"
        ));
    }
    if p.chars().any(char::is_whitespace) {
        return Err(format!(
            "policy.toml: rule `{id}` pattern `{p}` in `{key}` contains whitespace"
        ));
    }
    if p.contains("..") {
        return Err(format!(
            "policy.toml: rule `{id}` pattern `{p}` in `{key}` contains `..`"
        ));
    }
    if let Some(i) = p.find('*')
        && i != p.len() - 1
    {
        return Err(format!(
            "policy.toml: rule `{id}` pattern `{p}` in `{key}` has `*` before its end"
        ));
    }
    if p.contains('@') {
        let ok = p == "@main"
            || p.strip_prefix("@session/")
                .is_some_and(|rest| !rest.contains('@'));
        if !ok {
            return Err(format!(
                "policy.toml: rule `{id}` pattern `{p}` in `{key}` uses `@` other than a leading `@session/` or exactly `@main`"
            ));
        }
    }
    Ok(())
}

fn expand(pattern: &str, vars: &Vars) -> String {
    if pattern == "@main" {
        vars.main.clone()
    } else if let Some(rest) = pattern.strip_prefix("@session/") {
        format!("{}{}", vars.session_prefix, rest)
    } else {
        pattern.to_string()
    }
}

/// Whether `pattern` (placeholders expanded with `vars`) matches `target`.
pub fn matches(pattern: &str, vars: &Vars, target: &str) -> bool {
    let expanded = expand(pattern, vars);
    match expanded.strip_suffix('*') {
        Some(prefix) => target.starts_with(prefix),
        None => expanded == target,
    }
}

fn rule_matches(rule: &Rule, vars: &Vars, req: &Request) -> bool {
    if !rule.actions.contains(&req.action) {
        return false;
    }
    if !rule.targets.is_empty() && !rule.targets.iter().any(|p| matches(p, vars, &req.target)) {
        return false;
    }
    !rule.except.iter().any(|p| matches(p, vars, &req.target))
}

impl Policy {
    /// Evaluate one request. The first matching rule decides.
    ///
    /// When no rule matches, the answer is deny with reason [`NO_MATCH`].
    pub fn evaluate(&self, vars: &Vars, req: &Request) -> Answer {
        for rule in &self.rules {
            if !rule_matches(rule, vars, req) {
                continue;
            }
            let id = Some(rule.id.clone());
            return match (rule.decision, rule.approval) {
                (Decision::Deny, _) => Answer {
                    decision: Decision::Deny,
                    rule: id,
                    reason: rule.reason.clone(),
                    needs: None,
                },
                (Decision::Allow, None) => Answer {
                    decision: Decision::Allow,
                    rule: id,
                    reason: rule.reason.clone(),
                    needs: None,
                },
                (Decision::Allow, Some(needed)) => {
                    let met = req.context.approval.is_some_and(|have| have.meets(needed));
                    if met {
                        Answer {
                            decision: Decision::Allow,
                            rule: id,
                            reason: rule.reason.clone(),
                            needs: None,
                        }
                    } else {
                        Answer {
                            decision: Decision::Deny,
                            rule: id,
                            reason: format!("{}; it needs {}", rule.reason, needed.phrase()),
                            needs: Some(needed),
                        }
                    }
                }
            };
        }
        Answer {
            decision: Decision::Deny,
            rule: None,
            reason: NO_MATCH.into(),
            needs: None,
        }
    }

    /// The rule with the given id, if there is one.
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }
}
