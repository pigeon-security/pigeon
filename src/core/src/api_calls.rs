//! Counts this isolate's Cloudflare API calls by method, path template and status class, so the
//! account's API budget can be read per path (docs/plans/token-minting.md, "The account's API
//! budget").
//!
//! [`record`] is called by [`crate::cloudflare`] for every call. A path is kept only as its
//! template ([`template`]): the query string is cut off and every id or name is `{id}`, so no
//! token, account id, repository name or query value is kept. Counts live in this isolate's
//! memory until the hosting Worker [`drain`]s them into its store (core: the token ledger's
//! Durable Object, read by the `cloudflare_calls` tool).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How often an isolate sends its counts on, in milliseconds.
pub const FLUSH_EVERY_MS: f64 = 60_000.0;

/// The most templates one isolate keeps; past it, calls are counted under [`OVERFLOW`].
const MAX_KEYS: usize = 256;

/// The template counted once [`MAX_KEYS`] are kept.
pub const OVERFLOW: &str = "(other)";

/// The longest template kept, in bytes.
const MAX_TEMPLATE: usize = 160;

/// The status classes, in the order the tool shows them. `429` is a rate-limit refusal:
/// HTTP 429, or Cloudflare's code 10429 (or 971, "please wait") in any status; `error` is a call
/// that got no answer at all.
pub const CLASSES: [&str; 6] = ["2xx", "4xx", "429", "5xx", "other", "error"];

/// The collections whose next path segment is an id or a name.
const COLLECTIONS: [&str; 40] = [
    "accounts",
    "zones",
    "tokens",
    "scripts",
    "services",
    "environments",
    "buckets",
    "gateways",
    "stores",
    "secrets",
    "apps",
    "namespaces",
    "repos",
    "dns_records",
    "custom_hostnames",
    "policies",
    "domains",
    "queues",
    "consumers",
    "database",
    "databases",
    "subscriptions",
    "deployments",
    "versions",
    "registries",
    "images",
    "applications",
    "keys",
    "identity_providers",
    "groups",
    "routes",
    "certificates",
    "portals",
    "servers",
    "destinations",
    "objects",
    "members",
    "logs",
    "datasets",
    "rules",
];

/// Words that follow a collection but are not an id: `/tokens/verify`, `/tokens/permission_groups`.
const LITERALS: [&str; 8] = [
    "verify",
    "permission_groups",
    "permissions",
    "preview",
    "search",
    "bulk",
    "export",
    "import",
];

/// What an object key, which may hold slashes, becomes: the whole rest of the path.
const TAIL: &str = "/objects/{id}";

/// `path` as a template: no query string or fragment, and each id or name `{id}`. A segment is
/// an id when it follows a collection (`/accounts/{id}`, `/tokens/{id}`), or when it is not a
/// lowercase word (`_` and `-` allowed, digits only in a word of three letters at most, such as
/// `r2`): hex, longer digits, dots, `@` and upper case all make an id.
pub fn template(path: &str) -> String {
    let bare = path.split(['?', '#']).next().unwrap_or_default();
    let mut out = String::new();
    let mut after_collection = false;
    for segment in bare.split('/').filter(|s| !s.is_empty()) {
        if out.ends_with(TAIL) {
            // An object key may hold slashes: the rest of the path is that one key.
            break;
        }
        // Product words may hold a digit when short (`r2`, `d1`); an id is longer.
        let digits = segment.bytes().any(|b| b.is_ascii_digit());
        let longest = if digits { 3 } else { 40 };
        let word = segment.len() <= longest
            && segment.starts_with(|c: char| c.is_ascii_lowercase())
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        let id = !word || (after_collection && !LITERALS.contains(&segment));
        out.push('/');
        out.push_str(if id { "{id}" } else { segment });
        after_collection = !id && COLLECTIONS.contains(&segment);
    }
    if out.is_empty() {
        out.push('/');
    }
    if out.len() > MAX_TEMPLATE {
        let mut cut = MAX_TEMPLATE;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// The class of an answer with HTTP `status` (`None`: no answer) and Cloudflare error `codes`.
pub fn class(status: Option<u16>, codes: &[i64]) -> &'static str {
    let Some(status) = status else {
        return "error";
    };
    if status == 429 || codes.iter().any(|c| matches!(c, 10429 | 971)) {
        return "429";
    }
    match status {
        200..=299 => "2xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

/// The error codes in a v4 answer's `errors`.
pub fn codes(answer: &Value) -> Vec<i64> {
    answer["errors"]
        .as_array()
        .map(|errors| errors.iter().filter_map(|e| e["code"].as_i64()).collect())
        .unwrap_or_default()
}

/// Calls counted for one method, template and class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Count {
    pub method: String,
    pub path: String,
    pub class: String,
    pub n: u64,
}

type Key = (String, String, &'static str);

std::thread_local! {
    /// This isolate's counts since its last drain.
    static COUNTS: RefCell<HashMap<Key, u64>> = RefCell::new(HashMap::new());
    /// When this isolate last drained, in milliseconds since the epoch; 0 before the first.
    static LAST_FLUSH: Cell<f64> = const { Cell::new(0.0) };
}

/// Counts one call of `method` on `path` answered `status` with error `codes`.
pub fn record(method: &str, path: &str, status: Option<u16>, codes: &[i64]) {
    add(method, &template(path), class(status, codes), 1);
}

fn add(method: &str, path: &str, class: &'static str, n: u64) {
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        let mut key = (method.to_string(), path.to_string(), class);
        if !c.contains_key(&key) && c.len() >= MAX_KEYS {
            key.1 = OVERFLOW.to_string();
        }
        *c.entry(key).or_insert(0) += n;
    });
}

/// Takes this isolate's counts, leaving none, and notes `now_ms` as the last flush.
pub fn drain(now_ms: f64) -> Vec<Count> {
    LAST_FLUSH.with(|l| l.set(now_ms));
    let taken = COUNTS.with(|c| std::mem::take(&mut *c.borrow_mut()));
    let mut out: Vec<Count> = taken
        .into_iter()
        .map(|((method, path, class), n)| Count {
            method,
            path,
            class: class.to_string(),
            n,
        })
        .collect();
    out.sort_by(|a, b| (&a.path, &a.method, &a.class).cmp(&(&b.path, &b.method, &b.class)));
    out
}

/// Puts back counts that could not be stored, for the next flush.
pub fn restore(counts: Vec<Count>) {
    for c in counts {
        let class = CLASSES
            .iter()
            .find(|k| **k == c.class)
            .copied()
            .unwrap_or("other");
        add(&c.method, &c.path, class, c.n);
    }
}

/// Whether this isolate has counts and has not flushed for [`FLUSH_EVERY_MS`].
pub fn due(now_ms: f64) -> bool {
    let waiting = COUNTS.with(|c| !c.borrow().is_empty());
    waiting && now_ms - LAST_FLUSH.with(Cell::get) >= FLUSH_EVERY_MS
}

/// The `cloudflare_calls` answer for `rows` (summed per method, template and class) over the
/// last `hours`: totals by class, and each path with its total and classes, most called first.
pub fn view(rows: &[Count], hours: u64, limit: usize) -> Value {
    let mut by_class: BTreeMap<&str, u64> = CLASSES.iter().map(|c| (*c, 0)).collect();
    let mut paths: BTreeMap<(&str, &str), BTreeMap<&str, u64>> = BTreeMap::new();
    let mut total = 0;
    for r in rows {
        total += r.n;
        let class = CLASSES
            .iter()
            .find(|k| **k == r.class)
            .copied()
            .unwrap_or("other");
        *by_class.entry(class).or_insert(0) += r.n;
        *paths
            .entry((r.method.as_str(), r.path.as_str()))
            .or_default()
            .entry(class)
            .or_insert(0) += r.n;
    }
    let mut listed: Vec<(u64, Value)> = paths
        .into_iter()
        .map(|((method, path), classes)| {
            let n: u64 = classes.values().sum();
            let mut row = json!({ "method": method, "path": path, "total": n });
            for class in CLASSES {
                row[class] = json!(classes.get(class).copied().unwrap_or(0));
            }
            (n, row)
        })
        .collect();
    listed.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["path"].as_str().cmp(&b.1["path"].as_str()))
    });
    let shown = listed.len().min(limit);
    json!({
        "hours": hours,
        "total": total,
        "by_class": by_class,
        "paths": listed.into_iter().take(limit).map(|(_, v)| v).collect::<Vec<_>>(),
        "paths_shown": shown,
    })
}
