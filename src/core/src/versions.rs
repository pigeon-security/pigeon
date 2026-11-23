//! Versions of pigeon's parts, and instances' pins on them (docs/plans/versioning.md).
//!
//! A [`Component`] is anything with a version: a capability, a connector, a template, or code
//! pigeon installs into an instance's account. Each declares its version and changelog in code
//! beside it; a breaking entry names the migration it needs. An instance pins components with
//! [`Requirement`]s in Cargo's grammar. Everything here is pure: it parses, checks and plans, and
//! never reads or writes anything.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The summary of the first entry every component that existed when versions began carries.
pub const BEGAN: &str = "The state when versions began (docs/plans/versioning.md).";

/// A semver version, `MAJOR.MINOR.PATCH`, with no pre-release or build parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// The version `major.minor.patch`.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses `MAJOR.MINOR.PATCH`: three numbers, no leading zeros, nothing else.
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.trim().split('.').collect();
        if parts.len() != 3 {
            return Err(format!(
                "`{s}` is not a version: write MAJOR.MINOR.PATCH, such as 1.2.3"
            ));
        }
        let n = |p: &str| {
            number(p).ok_or_else(|| format!("`{s}` is not a version: `{p}` is not a number"))
        };
        Ok(Self::new(n(parts[0])?, n(parts[1])?, n(parts[2])?))
    }

    /// Whether `newer` can replace this version without a migration: the same MAJOR, or below
    /// 1.0.0 the same MINOR, or below 0.1.0 the same PATCH (Cargo's rule).
    pub fn compatible(&self, newer: &Version) -> bool {
        match (self.major, self.minor) {
            (0, 0) => newer.major == 0 && newer.minor == 0 && newer.patch == self.patch,
            (0, m) => newer.major == 0 && newer.minor == m,
            (m, _) => newer.major == m,
        }
    }
}

/// A decimal number with no sign and no leading zero (other than `0` itself).
fn number(p: &str) -> Option<u64> {
    if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0'))
    {
        return None;
    }
    p.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Self::parse(s)
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// How a [`Requirement`] compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `=1.2.3`: exactly that version.
    Exact,
    /// `^1.2`, or bare `1.2`: compatible with it (the same left-most non-zero part).
    Caret,
    /// `~1.2`: at least it, below the next MINOR (or MAJOR when only MAJOR is given).
    Tilde,
    /// `*`: any version.
    Any,
}

/// What an instance pins a component to: Cargo's grammar for one comparator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    pub op: Op,
    pub major: u64,
    pub minor: Option<u64>,
    pub patch: Option<u64>,
}

impl Requirement {
    /// Parses `=1.2.3`, `^1.2`, `~1.2`, `*`, or a bare `1.2.3`, `2.0` or `3` (as with `^`).
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        if t == "*" {
            return Ok(Self {
                op: Op::Any,
                major: 0,
                minor: None,
                patch: None,
            });
        }
        let (op, rest) = match t.chars().next() {
            Some('=') => (Op::Exact, &t[1..]),
            Some('^') => (Op::Caret, &t[1..]),
            Some('~') => (Op::Tilde, &t[1..]),
            _ => (Op::Caret, t),
        };
        let bad = || {
            format!(
                "`{s}` is not a requirement: write =1.2.3 (exactly), ^1.2 (compatible), ~1.2 \
                 (patches only), * (any), or a bare 1.2.3, 2.0 or 3 (as with ^)"
            )
        };
        let parts: Vec<&str> = rest.trim().split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return Err(bad());
        }
        let nums: Vec<u64> = parts
            .iter()
            .map(|p| number(p))
            .collect::<Option<_>>()
            .ok_or_else(bad)?;
        if op == Op::Exact && nums.len() != 3 {
            return Err(format!(
                "`{s}`: an exact pin names the whole version, such as =1.2.3"
            ));
        }
        Ok(Self {
            op,
            major: nums[0],
            minor: nums.get(1).copied(),
            patch: nums.get(2).copied(),
        })
    }

    /// The lowest version it allows.
    pub fn floor(&self) -> Version {
        Version::new(self.major, self.minor.unwrap_or(0), self.patch.unwrap_or(0))
    }

    /// Whether `v` satisfies it.
    pub fn matches(&self, v: &Version) -> bool {
        let floor = self.floor();
        match self.op {
            Op::Any => true,
            Op::Exact => *v == floor,
            Op::Tilde => {
                *v >= floor && v.major == self.major && self.minor.is_none_or(|m| v.minor == m)
            }
            Op::Caret => {
                if *v < floor || v.major != self.major {
                    return false;
                }
                match (self.major, self.minor, self.patch) {
                    (0, None, _) => true,
                    (0, Some(0), None) => v.minor == 0,
                    (0, Some(0), Some(p)) => v.minor == 0 && v.patch == p,
                    (0, Some(m), _) => v.minor == m,
                    _ => true,
                }
            }
        }
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.op {
            Op::Any => return f.write_str("*"),
            Op::Exact => "=",
            Op::Caret => "^",
            Op::Tilde => "~",
        };
        write!(f, "{op}{}", self.major)?;
        if let Some(m) = self.minor {
            write!(f, ".{m}")?;
        }
        if let Some(p) = self.patch {
            write!(f, ".{p}")?;
        }
        Ok(())
    }
}

/// What kind of part a component is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A capability: its tools and behaviour, such as `instances` or `sites`.
    #[default]
    Capability,
    /// A connector a capability carries to another service, such as `ci.buildkite`.
    Connector,
    /// A template a consumer's content is rendered with, such as `blog`.
    Template,
    /// Code pigeon installs into an instance's own account, such as `instances.site_worker`.
    Installed,
}

/// One changelog entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Entry {
    /// The version it made.
    pub version: String,
    /// Whether an instance, a consumer repository or a caller must change something for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub breaking: bool,
    /// What changed, in a line.
    pub summary: String,
    /// For a breaking entry: the work an instance must do to move past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<String>,
}

/// A part of pigeon with a version, as it is declared beside its code.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Component {
    /// Unique across pigeon: a capability's name, `owner.part` for a part it carries, or a
    /// template's name.
    pub name: String,
    pub kind: Kind,
    /// The newest version this build serves.
    pub version: String,
    /// Every version this build can serve at once, newest first; empty means only `version`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serves: Vec<String>,
    /// Newest first; the first entry is `version`.
    #[serde(default)]
    pub changelog: Vec<Entry>,
}

impl Component {
    /// A component at `version`, with no changelog yet.
    pub fn new(name: &str, kind: Kind, version: &str) -> Self {
        Self {
            name: name.into(),
            kind,
            version: version.into(),
            serves: Vec::new(),
            changelog: Vec::new(),
        }
    }

    /// A component that existed when versions began: 1.0.0, with the one entry [`BEGAN`].
    /// Add newer entries with [`Component::entry`] and [`Component::breaking`], newest last.
    pub fn began(name: &str, kind: Kind) -> Self {
        Self::new(name, kind, "1.0.0").entry("1.0.0", BEGAN)
    }

    /// Adds a non-breaking entry as the newest and makes it the version.
    pub fn entry(mut self, version: &str, summary: &str) -> Self {
        self.push(version, false, summary, None);
        self
    }

    /// Adds a breaking entry as the newest, with the migration it needs, and makes it the
    /// version.
    pub fn breaking(mut self, version: &str, summary: &str, migration: &str) -> Self {
        self.push(version, true, summary, Some(migration.into()));
        self
    }

    /// Sets every version this build can serve at once, newest first.
    pub fn serving(mut self, versions: &[&str]) -> Self {
        self.serves = versions.iter().map(|v| (*v).to_string()).collect();
        self
    }

    fn push(&mut self, version: &str, breaking: bool, summary: &str, migration: Option<String>) {
        self.version = version.into();
        self.changelog.insert(
            0,
            Entry {
                version: version.into(),
                breaking,
                summary: summary.into(),
                migration,
            },
        );
    }

    /// The newest version it serves, if `version` parses.
    pub fn current(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }

    /// Every version it serves, newest first; versions that do not parse are left out.
    pub fn served(&self) -> Vec<Version> {
        let mut out: Vec<Version> = if self.serves.is_empty() {
            self.current().into_iter().collect()
        } else {
            self.serves
                .iter()
                .filter_map(|v| Version::parse(v).ok())
                .collect()
        };
        out.sort_by(|a, b| b.cmp(a));
        out.dedup();
        out
    }

    /// The newest version it serves that `requirement` allows.
    pub fn select(&self, requirement: &Requirement) -> Option<Version> {
        self.served().into_iter().find(|v| requirement.matches(v))
    }

    /// The breaking entries above what `requirement` allows, up to and including `to`, oldest
    /// first: the work an instance pinned so must do to move to `to`.
    pub fn work(&self, requirement: &Requirement, to: &Version) -> Vec<Entry> {
        let floor = requirement.floor();
        let mut out: Vec<Entry> = self
            .changelog
            .iter()
            .filter(|e| e.breaking)
            .filter(|e| {
                Version::parse(&e.version)
                    .is_ok_and(|v| v > floor && v <= *to && !requirement.matches(&v))
            })
            .cloned()
            .collect();
        out.reverse();
        out
    }

    /// What is wrong with the declaration: the rules in docs/plans/versioning.md, decision 1.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let name = &self.name;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
        {
            out.push(format!(
                "`{name}`: a component's name is lowercase letters, digits, `-`, `_` and `.`"
            ));
        }
        let current = match Version::parse(&self.version) {
            Ok(v) => Some(v),
            Err(e) => {
                out.push(format!("{name}: {e}"));
                None
            }
        };
        match self.changelog.first() {
            None => out.push(format!("{name}: the changelog is empty")),
            Some(first) if first.version != self.version => out.push(format!(
                "{name}: the newest changelog entry is {}, but the version is {}",
                first.version, self.version
            )),
            Some(_) => {}
        }
        // Oldest first, to compare each entry with the one before it.
        let mut before: Option<Version> = None;
        for e in self.changelog.iter().rev() {
            let v = match Version::parse(&e.version) {
                Ok(v) => v,
                Err(err) => {
                    out.push(format!("{name}: changelog: {err}"));
                    continue;
                }
            };
            if e.summary.trim().is_empty() {
                out.push(format!("{name} {v}: the entry has no summary"));
            }
            if e.breaking && e.migration.as_deref().is_none_or(|m| m.trim().is_empty()) {
                out.push(format!("{name} {v}: a breaking entry names its migration"));
            }
            if let Some(b) = before {
                if v <= b {
                    out.push(format!(
                        "{name}: the changelog is not newest first: {v} comes after {b}"
                    ));
                } else if e.breaking && b.compatible(&v) {
                    out.push(format!(
                        "{name} {v}: a breaking entry moves MAJOR (MINOR below 1.0.0) from {b}"
                    ));
                } else if !e.breaking && !b.compatible(&v) {
                    out.push(format!(
                        "{name} {v}: moving from {b} breaks compatibility, so the entry is marked breaking with its migration"
                    ));
                }
            }
            before = Some(v);
        }
        for s in &self.serves {
            if let Err(e) = Version::parse(s) {
                out.push(format!("{name}: serves: {e}"));
            }
        }
        if let Some(c) = current
            && !self.serves.is_empty()
            && !self.served().contains(&c)
        {
            out.push(format!("{name}: serves does not list the version {c}"));
        }
        out
    }
}

/// The first component named `name`: a name declared twice (core and a capability Worker both
/// hosting `ci`) is the same code, so the first is taken.
pub fn find<'a>(carried: &'a [Component], name: &str) -> Option<&'a Component> {
    carried.iter().find(|c| c.name == name)
}

/// What is wrong with an instance's pins: names nothing pigeon declares, or a requirement that
/// does not parse. A pin no served version satisfies is not a problem: it holds the instance.
pub fn pin_problems(pins: &BTreeMap<String, String>, carried: &[Component]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, req) in pins {
        if find(carried, name).is_none() {
            let mut known: Vec<&str> = carried.iter().map(|c| c.name.as_str()).collect();
            known.sort_unstable();
            known.dedup();
            out.push(format!(
                "pins.{name}: pigeon declares no component `{name}`; it declares {}",
                known.join(", ")
            ));
        }
        if let Err(e) = Requirement::parse(req) {
            out.push(format!("pins.{name}: {e}"));
        }
    }
    out
}

/// How a pin stands against what pigeon serves.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PinState {
    /// A served version satisfies it: `selected`.
    Ok,
    /// No served version satisfies it: operations for the instance on this component refuse
    /// until the pin moves (or pigeon serves an old version again).
    Held,
    /// It names nothing pigeon declares.
    Unknown,
    /// Its requirement does not parse.
    Invalid,
}

/// One pin, checked.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct PinStatus {
    pub component: String,
    pub requirement: String,
    pub state: PinState,
    /// The newest served version it allows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// The newest version pigeon serves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// For a held pin: the breaking entries in the way, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub work: Vec<Entry>,
    /// One line for a person.
    pub note: String,
}

/// Each pin checked against what pigeon serves, in name order.
pub fn pin_status(pins: &BTreeMap<String, String>, carried: &[Component]) -> Vec<PinStatus> {
    pins.iter()
        .map(|(name, req)| {
            let mut s = PinStatus {
                component: name.clone(),
                requirement: req.clone(),
                state: PinState::Ok,
                selected: None,
                current: None,
                work: Vec::new(),
                note: String::new(),
            };
            let Some(c) = find(carried, name) else {
                s.state = PinState::Unknown;
                s.note = format!("pigeon declares no component `{name}`");
                return s;
            };
            s.current = Some(c.version.clone());
            let r = match Requirement::parse(req) {
                Ok(r) => r,
                Err(e) => {
                    s.state = PinState::Invalid;
                    s.note = e;
                    return s;
                }
            };
            match c.select(&r) {
                Some(v) => {
                    s.selected = Some(v.to_string());
                    s.note = format!("{name} {req} selects {v}");
                }
                None => {
                    s.state = PinState::Held;
                    if let Some(cur) = c.current() {
                        s.work = c.work(&r, &cur);
                    }
                    s.note = format!(
                        "{name} is pinned to {req}, and pigeon serves no such version (newest {}): \
                         operations on it for this instance refuse until the pin moves",
                        c.version
                    );
                }
            }
            s
        })
        .collect()
}

/// What moving one pin up to the newest served version takes.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct UpgradeStep {
    pub component: String,
    /// The pin as it is.
    pub pin: String,
    /// The newest served version the pin allows now; none when it is held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The newest version pigeon serves.
    pub to: String,
    /// The requirement to propose instead: `^` and `to`.
    pub suggested: String,
    /// The breaking entries on the way, oldest first, each with its migration; empty means the
    /// move needs no work.
    pub work: Vec<Entry>,
}

/// For each pin that does not already allow the newest served version, what moving it there
/// takes, in name order. Pins that name nothing or do not parse are left out: [`pin_status`]
/// reports them.
pub fn upgrade(pins: &BTreeMap<String, String>, carried: &[Component]) -> Vec<UpgradeStep> {
    let mut out = Vec::new();
    for (name, req) in pins {
        let (Some(c), Ok(r)) = (find(carried, name), Requirement::parse(req)) else {
            continue;
        };
        let Some(to) = c.current() else {
            continue;
        };
        if r.matches(&to) || r.floor() > to {
            continue;
        }
        out.push(UpgradeStep {
            component: name.clone(),
            pin: req.clone(),
            from: c.select(&r).map(|v| v.to_string()),
            to: to.to_string(),
            suggested: format!("^{to}"),
            work: c.work(&r, &to),
        });
    }
    out
}
