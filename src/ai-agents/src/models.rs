//! Which model a run uses: choices (`provider/model`), tiers per provider, the stored defaults,
//! the fallback chain and the order in which a run's choice is merged. Pure: no I/O, no clock.

use serde_json::{Map, Value, json};

use crate::Role;

/// The providers a run may use.
pub const PROVIDERS: [&str; 5] = [
    "anthropic",
    "cf-anthropic",
    "openai",
    "cf-openai",
    "workers-ai",
];

/// Who pays for a provider: `byok` (the person's own key) or `credits` (Cloudflare credits).
pub fn route(provider: &str) -> &'static str {
    if provider.starts_with("cf-") || provider == "workers-ai" {
        "credits"
    } else {
        "byok"
    }
}

/// The text shown when a bare `workers-ai` names a slot that has no Workers AI model.
const WORKERS_AI_NO_SMALL: &str = "Workers AI has no small model: name one (workers-ai/@cf/…) or choose another provider for this slot";

/// The longest model id, in characters.
pub const MAX_MODEL_LEN: usize = 100;

/// How capable (and costly) a model is within its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// The strongest model: a planner's.
    Top,
    /// The everyday model: an executor's.
    Middle,
    /// The cheap model: mechanical runs and summaries.
    Small,
}

/// A place a model is chosen for: a role, or the compaction summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// A planner run.
    Planner,
    /// An executor run.
    Executor,
    /// A mechanical run.
    Mechanical,
    /// The summary that compacts a long context.
    Summary,
    /// A planner run of one kind: its own stored model, beside the planner's.
    Kind(Kind),
}

/// What a planner run is for, as a hint given at its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Combining and landing branches.
    Integrate,
    /// Writing or changing docs.
    Docs,
    /// Fixing a bug.
    Fix,
    /// Designing a change.
    Design,
}

/// Every kind, in the order shown.
pub const KINDS: [Kind; 4] = [Kind::Integrate, Kind::Docs, Kind::Fix, Kind::Design];

impl Kind {
    /// Its name, as given to `ai_agents_start` and shown.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Integrate => "integrate",
            Kind::Docs => "docs",
            Kind::Fix => "fix",
            Kind::Design => "design",
        }
    }

    /// The name of its model slot in `ai_agents_models`.
    pub fn slot_name(self) -> &'static str {
        match self {
            Kind::Integrate => "planner_integrate",
            Kind::Docs => "planner_docs",
            Kind::Fix => "planner_fix",
            Kind::Design => "planner_design",
        }
    }

    /// Reads a kind name; `None` for any other text.
    pub fn parse(text: &str) -> Option<Kind> {
        KINDS.into_iter().find(|k| k.name() == text)
    }
}

/// Every slot a model can be stored for: the four base slots, then one per planner kind.
pub fn all_slots() -> Vec<Slot> {
    SLOTS
        .into_iter()
        .chain(KINDS.into_iter().map(Slot::Kind))
        .collect()
}

/// Checks the `kind` of `ai_agents_start` for a run of `role`. Only a planner may give one;
/// any other role is refused, not ignored.
pub fn start_kind(role: Role, args: &Value) -> Result<Option<Kind>, String> {
    let text = match &args["kind"] {
        Value::Null => return Ok(None),
        Value::String(s) => s.trim(),
        _ => return Err("`kind` must be one of: integrate, docs, fix, design".into()),
    };
    if role != Role::Planner {
        return Err("`kind` is for a planner: it hints what the planner is for".into());
    }
    Kind::parse(text)
        .map(Some)
        .ok_or_else(|| format!("`{text}` is not a kind: use integrate, docs, fix or design"))
}

/// Every slot, in the order shown.
pub const SLOTS: [Slot; 4] = [
    Slot::Planner,
    Slot::Executor,
    Slot::Mechanical,
    Slot::Summary,
];

impl Slot {
    /// The slot a role's model is chosen in.
    pub fn of(role: Role) -> Slot {
        match role {
            Role::Planner => Slot::Planner,
            Role::Executor => Slot::Executor,
            Role::Mechanical => Slot::Mechanical,
        }
    }

    /// Its name, as stored and shown.
    pub fn name(self) -> &'static str {
        match self {
            Slot::Planner => "planner",
            Slot::Executor => "executor",
            Slot::Mechanical => "mechanical",
            Slot::Summary => "summary",
            Slot::Kind(k) => k.slot_name(),
        }
    }

    /// The tier its model belongs to: planner top, executor middle, mechanical and summary small.
    pub fn tier(self) -> Tier {
        match self {
            Slot::Planner | Slot::Kind(_) => Tier::Top,
            Slot::Executor => Tier::Middle,
            Slot::Mechanical | Slot::Summary => Tier::Small,
        }
    }
}

/// The model `provider` offers in `tier`, if it is a known provider.
pub fn tier_model(provider: &str, tier: Tier) -> Option<&'static str> {
    match (provider, tier) {
        ("anthropic" | "cf-anthropic", Tier::Top) => Some("claude-opus-5-5"),
        ("anthropic" | "cf-anthropic", Tier::Middle) => Some("claude-sonnet-5-5"),
        ("anthropic" | "cf-anthropic", Tier::Small) => Some("claude-haiku-5-5"),
        ("openai" | "cf-openai", Tier::Top) => Some("gpt-6.2-astra"),
        ("openai" | "cf-openai", Tier::Middle) => Some("gpt-6.2-sol"),
        ("openai" | "cf-openai", Tier::Small) => Some("gpt-6.1-terra"),
        ("workers-ai", Tier::Top) => Some("@cf/zai-org/glm-5.3"),
        ("workers-ai", Tier::Middle) => Some("@cf/moonshotai/kimi-k2.7-code"),
        _ => None,
    }
}

/// Models that are known but sit in no tier.
const EXTRA_KNOWN: [(&str, &str); 2] = [
    ("cf-openai", "gpt-6-luna"),
    ("workers-ai", "@cf/deepseek-ai/deepseek-v4-pro-0813"),
];

/// Whether `model` is one this code knows for `provider` (a tier model or a listed extra).
pub fn known(provider: &str, model: &str) -> bool {
    [Tier::Top, Tier::Middle, Tier::Small]
        .iter()
        .any(|t| tier_model(provider, *t) == Some(model))
        || EXTRA_KNOWN.contains(&(provider, model))
}

/// One model of one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// The provider: `anthropic`, `cf-anthropic`, `openai`, `cf-openai` or `workers-ai`.
    pub provider: String,
    /// The model id the provider knows.
    pub model: String,
}

/// Checks a model id: letters, digits and `. - _ :`, at most 100 characters. A Workers AI id
/// (`@cf/vendor/name`) may also hold `@` and `/`.
fn check_model(provider: &str, model: &str) -> Result<(), String> {
    // `openai` takes the model gateway's own id rule, so there is one rule for it.
    if provider == "openai" {
        return pigeon_models::ModelIds::Plain.check(model);
    }
    let extra = if provider == "workers-ai" {
        ".-_:@/"
    } else {
        ".-_:"
    };
    if model.is_empty() {
        return Err("the model id is empty".into());
    }
    if model.chars().count() > MAX_MODEL_LEN {
        return Err(format!(
            "a model id is at most {MAX_MODEL_LEN} characters long"
        ));
    }
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    {
        return Err(format!(
            "`{model}` is not a model id: use letters, digits and `. - _ :`"
        ));
    }
    Ok(())
}

impl Choice {
    /// Reads `text` as a choice for `slot`: `provider/model`, a bare `claude-…` name (Anthropic),
    /// or a bare provider name (that provider's model for the slot's tier).
    pub fn parse(text: &str, slot: Slot) -> Result<Choice, String> {
        let text = text.trim();
        if let Some((provider, model)) = text.split_once('/') {
            if !PROVIDERS.contains(&provider) {
                return Err(format!(
                    "unknown provider `{provider}`: use `anthropic`, `cf-anthropic`, `openai`, `cf-openai` or `workers-ai`"
                ));
            }
            check_model(provider, model)?;
            if provider == "workers-ai" {
                if !model.starts_with("@cf/") {
                    return Err(format!(
                        "`{model}` is not a Workers AI model: `workers-ai` models start with `@cf/`"
                    ));
                }
            } else if !matches!(provider, "openai" | "cf-openai") && !model.starts_with("claude-") {
                return Err(format!(
                    "`{model}` is not a Claude model: `{provider}` models start with `claude-`"
                ));
            }
            return Ok(Choice {
                provider: provider.into(),
                model: model.into(),
            });
        }
        if let Some(model) = tier_model(text, slot.tier()) {
            return Ok(Choice {
                provider: text.into(),
                model: model.into(),
            });
        }
        if text.starts_with("claude-") {
            check_model("anthropic", text)?;
            return Ok(Choice {
                provider: "anthropic".into(),
                model: text.into(),
            });
        }
        if text == "workers-ai" {
            return Err(WORKERS_AI_NO_SMALL.into());
        }
        Err(format!(
            "`{text}` is not a model choice: use `provider/model` (e.g. cf-openai/gpt-6.2-sol), \
             a `claude-…` name, or `anthropic`, `cf-anthropic`, `openai`, `cf-openai` or `workers-ai`"
        ))
    }

    /// Reads a stored `provider/model` string; no shorthand.
    pub fn stored(text: &str) -> Option<Choice> {
        let (provider, model) = text.split_once('/')?;
        (PROVIDERS.contains(&provider) && check_model(provider, model).is_ok()).then(|| Choice {
            provider: provider.into(),
            model: model.into(),
        })
    }

    /// Its text, `provider/model`.
    pub fn text(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// The built-in default for `slot`: what runs used before models were a choice.
pub fn builtin(slot: Slot) -> Choice {
    let model = tier_model("anthropic", slot.tier()).unwrap_or("claude-sonnet-5-5");
    Choice {
        provider: "anthropic".into(),
        model: model.into(),
    }
}

/// The fallback of `choice` in a run of `role`: the role's built-in fallback tier of the same
/// provider. None for a model this code does not know, or when it is the fallback itself.
pub fn fallback(choice: &Choice, role: Role) -> Option<Choice> {
    if !known(&choice.provider, &choice.model) {
        return None;
    }
    let tier = match role {
        Role::Planner | Role::Mechanical => Tier::Middle,
        Role::Executor => Tier::Top,
    };
    let model = tier_model(&choice.provider, tier)?;
    (model != choice.model).then(|| Choice {
        provider: choice.provider.clone(),
        model: model.into(),
    })
}

/// A run's models, first choice first: the chosen model, then its fallback if it has one.
pub fn chain(choice: &Choice, role: Role) -> Vec<Choice> {
    let mut out = vec![choice.clone()];
    out.extend(fallback(choice, role));
    out
}

/// The chain of a run row with a stored `model` (`provider/model`); with none stored or one that
/// cannot be read, the role's built-in default, as before models were a choice.
pub fn chain_of(stored: Option<&str>, role: Role) -> Vec<Choice> {
    let choice = stored
        .and_then(Choice::stored)
        .unwrap_or_else(|| builtin(Slot::of(role)));
    chain(&choice, role)
}

/// The stored defaults: the choices set, each optional.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defaults {
    /// A planner's model.
    pub planner: Option<Choice>,
    /// An executor's model.
    pub executor: Option<Choice>,
    /// A mechanical run's model.
    pub mechanical: Option<Choice>,
    /// The compaction summary's model.
    pub summary: Option<Choice>,
    /// A planner's model for each kind, in `KINDS` order.
    pub kinds: [Option<Choice>; 4],
}

/// The place of `kind` in `KINDS`.
fn kind_index(kind: Kind) -> usize {
    KINDS.iter().position(|k| *k == kind).unwrap_or(0)
}

impl Defaults {
    /// The stored choice for `slot`, if any.
    pub fn stored_for(&self, slot: Slot) -> Option<&Choice> {
        match slot {
            Slot::Planner => self.planner.as_ref(),
            Slot::Executor => self.executor.as_ref(),
            Slot::Mechanical => self.mechanical.as_ref(),
            Slot::Summary => self.summary.as_ref(),
            Slot::Kind(k) => self.kinds[kind_index(k)].as_ref(),
        }
    }

    /// Sets the stored choice for `slot`.
    pub fn put(&mut self, slot: Slot, choice: Choice) {
        let place = match slot {
            Slot::Planner => &mut self.planner,
            Slot::Executor => &mut self.executor,
            Slot::Mechanical => &mut self.mechanical,
            Slot::Summary => &mut self.summary,
            Slot::Kind(k) => &mut self.kinds[kind_index(k)],
        };
        *place = Some(choice);
    }

    /// The choice in effect for `slot`: the stored one, else the built-in.
    pub fn effective(&self, slot: Slot) -> Choice {
        self.stored_for(slot)
            .cloned()
            .unwrap_or_else(|| builtin(slot))
    }

    /// Reads the stored JSON; an entry that cannot be read is left out.
    pub fn from_json(v: &Value) -> Defaults {
        let get = |k: &str| v[k].as_str().and_then(Choice::stored);
        Defaults {
            planner: get("planner"),
            executor: get("executor"),
            mechanical: get("mechanical"),
            summary: get("summary"),
            kinds: KINDS.map(|k| get(k.slot_name())),
        }
    }

    /// The JSON to store: each choice set, as `provider/model`.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        for slot in all_slots() {
            if let Some(c) = self.stored_for(slot) {
                m.insert(slot.name().into(), json!(c.text()));
            }
        }
        Value::Object(m)
    }
}

/// Checks the `set` argument of `ai_agents_models`: an object whose keys are slots, each a choice.
pub fn check_set(set: &Value) -> Result<Vec<(Slot, Choice)>, String> {
    let Some(obj) = set.as_object() else {
        return Err("`set` must be an object like {executor: \"cf-openai/gpt-6.2-sol\"}".into());
    };
    let mut out = Vec::new();
    for (k, v) in obj {
        let slot = all_slots()
            .into_iter()
            .find(|s| s.name() == k)
            .ok_or_else(|| {
                format!(
                    "`set.{k}` is not a slot: planner, executor, mechanical, summary, \
                     planner_integrate, planner_docs, planner_fix or planner_design"
                )
            })?;
        let text = v
            .as_str()
            .ok_or_else(|| format!("`set.{k}` must be a model choice string"))?;
        out.push((
            slot,
            Choice::parse(text, slot).map_err(|e| format!("`set.{k}`: {e}"))?,
        ));
    }
    Ok(out)
}

/// Notes for stored default entries that cannot be read (such as an old `mistral/…`): the built-in
/// default applies to that slot instead.
pub fn ignored_notes(raw: &Value) -> Vec<String> {
    all_slots()
        .iter()
        .filter_map(|slot| {
            let text = raw[slot.name()].as_str()?;
            Choice::stored(text).is_none().then(|| {
                format!(
                    "stored {} default `{text}` is ignored; the built-in default {} applies",
                    slot.name(),
                    builtin(*slot).text()
                )
            })
        })
        .collect()
}

/// What `ai_agents_models` answers: the effective defaults (each `stored` or `built_in`, with its
/// `route`), the known tiers per provider, notes on ignored entries, and an example call.
pub fn defaults_answer(d: &Defaults, notes: &[String]) -> Value {
    let mut eff = Map::new();
    for slot in SLOTS {
        let stored = d.stored_for(slot).is_some();
        let choice = d.effective(slot);
        eff.insert(
            slot.name().into(),
            json!({
                "model": choice.text(),
                "route": route(&choice.provider),
                "source": if stored { "stored" } else { "built_in" },
            }),
        );
    }
    // A kind slot has no built-in value: it is unset until a person sets it.
    for k in KINDS {
        let slot = Slot::Kind(k);
        eff.insert(
            slot.name().into(),
            match d.stored_for(slot) {
                Some(c) => json!({
                    "model": c.text(),
                    "route": route(&c.provider),
                    "source": "stored",
                }),
                None => json!({ "model": null, "route": null, "source": "unset" }),
            },
        );
    }
    let mut tiers = Map::new();
    for p in PROVIDERS {
        let mut t = Map::new();
        for (name, tier) in [
            ("top", Tier::Top),
            ("middle", Tier::Middle),
            ("small", Tier::Small),
        ] {
            t.insert(name.into(), json!(tier_model(p, tier)));
        }
        let extra: Vec<&str> = EXTRA_KNOWN
            .iter()
            .filter(|(q, _)| *q == p)
            .map(|(_, m)| *m)
            .collect();
        t.insert("other".into(), json!(extra));
        tiers.insert(p.into(), Value::Object(t));
    }
    json!({
        "defaults": eff,
        "tiers": tiers,
        "notes": notes,
        "example": {
            "tool": "ai_agents_models",
            "args": { "set": { "executor": "cf-openai/gpt-6.2-sol", "summary": "cf-openai" } },
        },
        "note": "a bare provider name means its model for the slot's tier; planner is top, \
                 executor middle, mechanical and summary small. `route` says who pays: `byok` is \
                 your own key, `credits` is Cloudflare credits",
    })
}

/// The models a planner's executors use unless a spawn says otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildModels {
    /// For executors.
    pub executor: Option<Choice>,
    /// For mechanical runs.
    pub mechanical: Option<Choice>,
}

impl ChildModels {
    /// Checks the `models` argument of `ai_agents_start`: `{executor?, mechanical?}`.
    pub fn parse(v: &Value) -> Result<ChildModels, String> {
        let mut out = ChildModels::default();
        let Some(obj) = v.as_object() else {
            return Err("`models` must be an object like {executor: \"anthropic\"}".into());
        };
        for (k, val) in obj {
            let slot = match k.as_str() {
                "executor" => Slot::Executor,
                "mechanical" => Slot::Mechanical,
                other => {
                    return Err(format!(
                        "`models.{other}` is not a kind: executor or mechanical"
                    ));
                }
            };
            let text = val
                .as_str()
                .ok_or_else(|| format!("`models.{k}` must be a model choice string"))?;
            let c = Choice::parse(text, slot).map_err(|e| format!("`models.{k}`: {e}"))?;
            match slot {
                Slot::Executor => out.executor = Some(c),
                _ => out.mechanical = Some(c),
            }
        }
        Ok(out)
    }

    /// The choice for an executor of `role`, if any.
    pub fn for_role(&self, role: Role) -> Option<&Choice> {
        match role {
            Role::Executor => self.executor.as_ref(),
            Role::Mechanical => self.mechanical.as_ref(),
            Role::Planner => None,
        }
    }

    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self.executor.is_none() && self.mechanical.is_none()
    }

    /// Reads the stored JSON; entries that cannot be read are left out.
    pub fn from_json(v: &Value) -> ChildModels {
        ChildModels {
            executor: v["executor"].as_str().and_then(Choice::stored),
            mechanical: v["mechanical"].as_str().and_then(Choice::stored),
        }
    }

    /// The JSON to store and show.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        if let Some(c) = &self.executor {
            m.insert("executor".into(), json!(c.text()));
        }
        if let Some(c) = &self.mechanical {
            m.insert("mechanical".into(), json!(c.text()));
        }
        Value::Object(m)
    }
}

/// The model of a run: its own choice; else, for an executor spawned by a planner, the planner's
/// `models` for its kind; else the stored default; else the built-in default.
pub fn merge(
    role: Role,
    own: Option<&Choice>,
    parent: Option<&ChildModels>,
    defaults: &Defaults,
) -> Choice {
    resolve(role, own, parent, None, defaults)
}

/// The model of a run, as `merge`, with a planner's `kind`: the run's own choice; else the stored
/// model for its kind; else the stored planner default; else the built-in default. With no kind,
/// or for a role that is not a planner, the kind is not looked at.
pub fn resolve(
    role: Role,
    own: Option<&Choice>,
    parent: Option<&ChildModels>,
    kind: Option<Kind>,
    defaults: &Defaults,
) -> Choice {
    let by_kind = || {
        kind.filter(|_| role == Role::Planner)
            .and_then(|k| defaults.stored_for(Slot::Kind(k)))
    };
    own.or_else(|| parent.and_then(|p| p.for_role(role)))
        .or_else(by_kind)
        .cloned()
        .unwrap_or_else(|| defaults.effective(Slot::of(role)))
}

/// Checks the `model` and `models` of `ai_agents_start` for a run of `role`. A planner may give
/// `models`; others may not.
pub fn start_models(role: Role, args: &Value) -> Result<(Option<Choice>, ChildModels), String> {
    let own = match &args["model"] {
        Value::Null => None,
        Value::String(s) => {
            Some(Choice::parse(s, Slot::of(role)).map_err(|e| format!("`model`: {e}"))?)
        }
        _ => return Err("`model` must be a string like cf-openai/gpt-6.2-sol".into()),
    };
    let children = match &args["models"] {
        Value::Null => ChildModels::default(),
        v if role == Role::Planner => ChildModels::parse(v)?,
        _ => return Err("`models` is for a planner: it says what its executors use".into()),
    };
    Ok((own, children))
}

/// Checks the `model` a planner's `spawn_executor` is given, for a child of `role`.
pub fn spawn_model(role: Role, input: &Value) -> Result<Option<Choice>, String> {
    match &input["model"] {
        Value::Null => Ok(None),
        Value::String(s) => Choice::parse(s, Slot::of(role))
            .map(Some)
            .map_err(|e| format!("`model`: {e}")),
        _ => Err("`model` must be a string like cf-openai/gpt-6.2-sol".into()),
    }
}
