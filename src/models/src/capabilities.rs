//! What each model takes: the one table every OpenAI and Anthropic request is shaped by.
//!
//! A row is found by the model's family, its name up to a `-` boundary. A name the table does
//! not know gets the safe row. Pure: a name in, a row out.

/// Which OpenAI endpoint a model is called on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Chat Completions.
    Chat,
    /// The Responses API. No row uses it yet; the request builders only read it.
    Responses,
}

/// A `tool_choice` kind, as Anthropic names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The model decides.
    Auto,
    /// The model must call some tool.
    Any,
    /// The model must call the named tool.
    Tool,
    /// The model may not call a tool.
    None,
}

impl Choice {
    /// The kind a request's `tool_choice.type` names, if it is one of the four.
    pub fn named(kind: &str) -> Option<Choice> {
        match kind {
            "auto" => Some(Choice::Auto),
            "any" => Some(Choice::Any),
            "tool" => Some(Choice::Tool),
            "none" => Some(Choice::None),
            _ => None,
        }
    }
}

/// What one model takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether it takes function tools.
    pub tools: bool,
    /// Whether it takes a reasoning effort at all.
    pub effort: bool,
    /// Whether Chat Completions takes function tools and a reasoning effort in one request. When
    /// not, the effort is sent as `none` beside tools.
    pub effort_with_tools: bool,
    /// The endpoint it is called on.
    pub endpoint: Endpoint,
    /// Whether it takes `tool_choice` `auto`.
    pub auto: bool,
    /// Whether it takes `tool_choice` `any`.
    pub any: bool,
    /// Whether it takes `tool_choice` `tool`.
    pub tool: bool,
    /// Whether it takes `tool_choice` `none`.
    pub none: bool,
    /// The most output tokens a request may ask for.
    pub max_output: u64,
}

impl Capabilities {
    /// Whether the model takes a `tool_choice` of this kind.
    pub fn takes(&self, choice: Choice) -> bool {
        match choice {
            Choice::Auto => self.auto,
            Choice::Any => self.any,
            Choice::Tool => self.tool,
            Choice::None => self.none,
        }
    }
}

/// The row for a model the table does not know: Chat Completions, tools without a reasoning
/// effort, `tool_choice` `auto` and `none` only.
pub const SAFE: Capabilities = Capabilities {
    tools: true,
    effort: false,
    effort_with_tools: false,
    endpoint: Endpoint::Chat,
    auto: true,
    any: false,
    tool: false,
    none: true,
    max_output: 16_384,
};

/// A Claude row that takes every `tool_choice` kind.
const CLAUDE_ALL: Capabilities = Capabilities {
    effort: true,
    effort_with_tools: true,
    any: true,
    tool: true,
    max_output: 128_000,
    ..SAFE
};

/// A Claude row that takes `tool_choice` `auto` and `none` only. Anthropic's tool use guide says
/// `any` and `tool` return a 400 for these models.
const CLAUDE_AUTO: Capabilities = Capabilities {
    any: false,
    tool: false,
    ..CLAUDE_ALL
};

/// An OpenAI row on Chat Completions that takes every `tool_choice` kind and an effort beside
/// tools. Every GPT-6 model lists 128,000 as its largest output.
const OPENAI_ALL: Capabilities = Capabilities {
    effort: true,
    effort_with_tools: true,
    any: true,
    tool: true,
    max_output: 128_000,
    ..SAFE
};

/// The table: a family's name, and its row. The first family that matches is used, so a more
/// specific name goes before a shorter one. Checked against Anthropic's models overview and tool
/// use guide and OpenAI's model pages on 2026-11-24. A Claude model not listed gets [`SAFE`],
/// because the docs do not say whether it takes forced tool use.
const TABLE: &[(&str, Capabilities)] = &[
    // The 5.5 line, Fable and Mythos: the guide lists these as refusing `any` and `tool`.
    ("claude-opus-5-5", CLAUDE_AUTO),
    ("claude-sonnet-5-5", CLAUDE_AUTO),
    ("claude-fable-5", CLAUDE_AUTO),
    ("claude-mythos-5", CLAUDE_AUTO),
    // Opus 5 and Haiku 5.5 take forced tool use (the call then skips thinking).
    ("claude-opus-5", CLAUDE_ALL),
    ("claude-haiku-5-5", CLAUDE_ALL),
    // Sonnet 5 is not named in the guide either way: the safe kinds, the 5.x output size.
    ("claude-sonnet-5", CLAUDE_AUTO),
    // The 4.5 line: 64,000 output tokens.
    (
        "claude-opus-4-5",
        Capabilities {
            max_output: 64_000,
            ..CLAUDE_ALL
        },
    ),
    (
        "claude-sonnet-4-5",
        Capabilities {
            max_output: 64_000,
            ..CLAUDE_ALL
        },
    ),
    (
        "claude-haiku-4-5",
        Capabilities {
            max_output: 64_000,
            ..CLAUDE_ALL
        },
    ),
    // OpenAI's page for gpt-6-luna: "Chat Completions supports function calling only with
    // reasoning_effort set to none." Its 400 says the same.
    (
        "gpt-6-luna",
        Capabilities {
            effort_with_tools: false,
            ..OPENAI_ALL
        },
    ),
    ("gpt-6.2-astra", OPENAI_ALL),
    ("gpt-6.2-sol", OPENAI_ALL),
    ("gpt-6.1-terra", OPENAI_ALL),
];

/// The row for `model`. A `provider/` prefix and a dated suffix (`gpt-6-luna-2026-…`) are read as
/// the model. An unknown name gets [`SAFE`].
pub fn of(model: &str) -> Capabilities {
    let name = model.rsplit('/').next().unwrap_or(model);
    TABLE
        .iter()
        .find(|(family, _)| {
            name == *family
                || name
                    .strip_prefix(family)
                    .is_some_and(|rest| rest.starts_with('-'))
        })
        .map_or(SAFE, |(_, row)| *row)
}
