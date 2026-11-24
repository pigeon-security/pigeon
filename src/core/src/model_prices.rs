//! One table of model prices, shared by the AI agents' budget and the model gateway's cache
//! rows. The numbers are Anthropic's pricing and prompt-caching pages, as read for
//! `docs/plans/ai-agents.md` on 2026-11-22: input and output per model; cache reads cost 0.05x
//! the input price, 5-minute cache writes 1.25x and 1-hour writes 2x.
//!
//! The table only serves a fast local estimate. The AI Gateway's per-request cost (its log's
//! `cost`, which already includes cache reads and writes) is the truth. Pure: values in, values
//! out.
//!
//! OpenAI's prices are from OpenAI's pricing page, as given by the person on 2026-11-24: input,
//! cached input and output, and a dearer long-context price once a request's input passes 272,000
//! tokens. OpenAI does not bill cache writes apart, so a write costs the input price.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai_gateway::canonical_model;

/// Tokens per price unit: prices are micro-dollars per million tokens.
const PER_MILLION: u128 = 1_000_000;

/// A model's prices, in micro-dollars per million tokens.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    /// Fresh input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the cache.
    pub cache_read: u64,
    /// Input tokens written to the 5-minute cache.
    pub cache_write_5m: u64,
    /// Input tokens written to the 1-hour cache.
    pub cache_write_1h: u64,
}

/// The price of `claude-opus-5-5`.
const OPUS: Price = Price {
    input: 4_000_000,
    output: 20_000_000,
    cache_read: 200_000,
    cache_write_5m: 5_000_000,
    cache_write_1h: 8_000_000,
};

/// The price of `claude-sonnet-5-5`.
const SONNET: Price = Price {
    input: 2_000_000,
    output: 10_000_000,
    cache_read: 100_000,
    cache_write_5m: 2_500_000,
    cache_write_1h: 4_000_000,
};

/// The price of `claude-haiku-5-5`.
const HAIKU: Price = Price {
    input: 100_000,
    output: 500_000,
    cache_read: 10_000,
    cache_write_5m: 125_000,
    cache_write_1h: 200_000,
};

/// The price of `claude-fable-5-2`. It is also the fallback, being the dearest.
const FABLE: Price = Price {
    input: 8_000_000,
    output: 40_000_000,
    cache_read: 800_000,
    cache_write_5m: 10_000_000,
    cache_write_1h: 16_000_000,
};

/// An OpenAI price: cache writes cost the input price, as OpenAI bills no cache write apart.
const fn openai(input: u64, cache_read: u64, output: u64) -> Price {
    Price {
        input,
        output,
        cache_read,
        cache_write_5m: input,
        cache_write_1h: input,
    }
}

/// The input tokens above which OpenAI bills a request at its long-context price.
const LONG_CONTEXT: u64 = 272_000;

/// The price of `gpt-6.2-astra`, short context.
const ASTRA: Price = openai(8_000_000, 400_000, 40_000_000);
/// The price of `gpt-6.2-astra`, long context.
const ASTRA_LONG: Price = openai(16_000_000, 800_000, 60_000_000);
/// The price of `gpt-6.2-sol`, short context.
const SOL: Price = openai(2_000_000, 100_000, 10_000_000);
/// The price of `gpt-6.2-sol`, long context.
const SOL_LONG: Price = openai(4_000_000, 200_000, 15_000_000);
/// The price of `gpt-6.1-terra`, short context.
const TERRA: Price = openai(1_000_000, 50_000, 5_000_000);
/// The price of `gpt-6.1-terra`, long context.
const TERRA_LONG: Price = openai(2_000_000, 100_000, 7_500_000);
/// The price of `gpt-6-luna`, short context.
const LUNA: Price = openai(100_000, 10_000, 500_000);
/// The price of `gpt-6-luna`, long context.
const LUNA_LONG: Price = openai(200_000, 20_000, 750_000);

/// The price of Workers AI `@cf/zai-org/glm-5.3` (Cloudflare's model catalog, 2026-11-25).
const GLM: Price = openai(1_400_000, 260_000, 4_400_000);
/// The price of Workers AI `@cf/moonshotai/kimi-k2.7-code`.
const KIMI: Price = openai(950_000, 190_000, 4_000_000);
/// The price of Workers AI `@cf/deepseek-ai/deepseek-v4-pro-0813`.
const DEEPSEEK: Price = openai(1_320_000, 44_000, 3_960_000);

/// True when `model` is `name` or is `name` followed by a dash and a suffix.
fn is_model(model: &str, name: &str) -> bool {
    match model.strip_prefix(name) {
        Some(rest) => rest.is_empty() || rest.starts_with('-'),
        None => false,
    }
}

/// A model's short price and, when it has one, its long-context price.
fn lookup(model: &str) -> (Price, Option<Price>) {
    let model = canonical_model(model);
    if is_model(&model, "claude-opus-5-5") {
        (OPUS, None)
    } else if is_model(&model, "claude-sonnet-5-5") {
        (SONNET, None)
    } else if is_model(&model, "claude-haiku-5-5") {
        (HAIKU, None)
    } else if is_model(&model, "gpt-6.2-astra") {
        (ASTRA, Some(ASTRA_LONG))
    } else if is_model(&model, "gpt-6.2-sol") {
        (SOL, Some(SOL_LONG))
    } else if is_model(&model, "gpt-6.1-terra") {
        (TERRA, Some(TERRA_LONG))
    } else if is_model(&model, "gpt-6-luna") {
        (LUNA, Some(LUNA_LONG))
    } else if is_model(&model, "glm-5.3") {
        (GLM, None)
    } else if is_model(&model, "kimi-k2.7-code") {
        (KIMI, None)
    } else if is_model(&model, "deepseek-v4-pro-0813") {
        (DEEPSEEK, None)
    } else {
        (FABLE, None)
    }
}

/// Looks up the price for a model, at short context.
///
/// The name is read as [`canonical_model`] reads it, so `anthropic/claude-opus-5.5` matches. A
/// dated name such as `claude-sonnet-5-5-20261101` matches by prefix. An unknown model gets the
/// dearest price, so it is never under-counted.
pub fn price(model: &str) -> Price {
    lookup(model).0
}

/// The price for a request of `model` whose input (fresh, cached and written) is `input` tokens:
/// the long-context price when the model has one and `input` passes 272,000 tokens.
pub fn price_at(model: &str, input: u64) -> Price {
    match lookup(model) {
        (_, Some(long)) if input > LONG_CONTEXT => long,
        (short, _) => short,
    }
}

/// Reads a token count from a usage object. Missing, null or odd values are 0.
fn tokens(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Turns a sum of token-times-price into whole micro-dollars, rounding up.
fn round_up_micros(total: u128) -> u64 {
    let rounded = total.saturating_add(PER_MILLION - 1) / PER_MILLION;
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

/// Works out what a finished turn cost, in micro-dollars, rounded up.
///
/// If `cache_creation` gives a 5-minute and 1-hour split, it is used. Otherwise all
/// cache-creation tokens are priced as 5-minute writes.
pub fn cost_micros(model: &str, usage: &Value) -> u64 {
    let (write_5m, write_1h) = match usage.get("cache_creation") {
        Some(split) if split.is_object() => (
            tokens(split, "ephemeral_5m_input_tokens"),
            tokens(split, "ephemeral_1h_input_tokens"),
        ),
        _ => (tokens(usage, "cache_creation_input_tokens"), 0),
    };
    let input = tokens(usage, "input_tokens")
        .saturating_add(tokens(usage, "cache_read_input_tokens"))
        .saturating_add(write_5m)
        .saturating_add(write_1h);
    let p = price_at(model, input);
    let parts: [(u64, u64); 5] = [
        (tokens(usage, "input_tokens"), p.input),
        (tokens(usage, "output_tokens"), p.output),
        (tokens(usage, "cache_read_input_tokens"), p.cache_read),
        (write_5m, p.cache_write_5m),
        (write_1h, p.cache_write_1h),
    ];

    let total = parts.iter().fold(0u128, |sum, (count, rate)| {
        sum.saturating_add(u128::from(*count) * u128::from(*rate))
    });
    round_up_micros(total)
}

/// An upper estimate of a turn's cost before it runs, in micro-dollars.
///
/// All input is priced as fresh input. Then `max_tokens` is priced as output. The result is
/// rounded up.
pub fn estimate_micros(model: &str, input_tokens: u64, max_tokens: u64) -> u64 {
    let p = price_at(model, input_tokens);
    let total = (u128::from(input_tokens) * u128::from(p.input))
        .saturating_add(u128::from(max_tokens) * u128::from(p.output));
    round_up_micros(total)
}
