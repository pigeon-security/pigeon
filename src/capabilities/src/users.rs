//! Users: standard users, who are people, and service users, who are agents. A service user
//! can be linked to a standard user, to act for them; the person approves the link with a
//! passkey. Both kinds can do the same things for now. Every user has a display name and
//! nothing else about them is kept.
//!
//! Users live in the `Directory`, a Durable Object with SQLite storage. A service user is an
//! Access service token, made a user the first time it calls.

use pigeon_core::Account;
use pigeon_core::access::Session;
use pigeon_core::approval;
use pigeon_core::durable::{self, Db, opt};
use pigeon_core::identity::{self, Expected};
use pigeon_core::mcp::Tool;

use crate::console::initials;
use crate::console::sections::{Section, Tab};
use pigeon_core::webauthn::{CHALLENGE_TTL_MS, random};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::wasm_bindgen;
use worker::{DurableObject, Env, Error, Request, Response, Result, State, durable_object, js_sys};

use crate::{Call, Capability, Context, arg, to_value, unknown};

/// The Directory's binding, and the one object of it.
pub const BINDING: &str = "DIRECTORY";
const OBJECT: &str = "directory";

/// How long an enrolment link and a link request stay usable, in milliseconds.
const ENROL_TTL_MS: i64 = 15 * 60 * 1000;
const REQUEST_TTL_MS: i64 = 15 * 60 * 1000;

/// How long the Access issuer and audience are kept before they are looked up again.
const EXPECTED_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// A user.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct User {
    pub id: String,
    /// `standard` (a person) or `service` (an agent).
    pub kind: String,
    pub name: String,
    pub created_at: i64,
}

/// Who is calling: a user, and the standard user they act for when linked.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Caller {
    pub user: User,
    pub acting_for: Option<User>,
    /// The AI agent run this caller acts on behalf of, set when pigeon-ai-agents dispatches for a
    /// run; a label for audit lines, never used to decide anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// The run id as an audit label: up to 32 ASCII letters and digits, else dropped.
pub use pigeon_core::policy_file::run_label;

/// A passkey, as kept: its credential ID, uncompressed P-256 point (base64url) and counter.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Passkey {
    pub id: String,
    pub user_id: String,
    pub point: String,
    pub sign_count: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// A request for a service user to act for a standard user.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LinkRequest {
    pub id: String,
    pub service_user_id: String,
    pub standard_user_id: String,
    pub expires_at: i64,
}

fn now() -> i64 {
    js_sys::Date::now() as i64
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

// ---- the Directory ----

/// The Directory's schema, one statement per migration, applied in order and never edited.
const MIGRATIONS: [&str; 19] = [
    "CREATE TABLE users (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL)",
    "CREATE TABLE service_ids (access_client_id TEXT PRIMARY KEY, user_id TEXT NOT NULL)",
    "CREATE TABLE passkeys (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, point TEXT NOT NULL, sign_count INTEGER NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER)",
    "CREATE TABLE links (service_user_id TEXT PRIMARY KEY, standard_user_id TEXT NOT NULL, approved_at INTEGER NOT NULL, approved_with TEXT NOT NULL)",
    "CREATE TABLE link_requests (id TEXT PRIMARY KEY, service_user_id TEXT NOT NULL, standard_user_id TEXT NOT NULL, expires_at INTEGER NOT NULL)",
    "CREATE TABLE challenges (id TEXT PRIMARY KEY, purpose TEXT NOT NULL, challenge TEXT NOT NULL, expires_at INTEGER NOT NULL)",
    "CREATE TABLE enrolments (code TEXT PRIMARY KEY, user_id TEXT NOT NULL, expires_at INTEGER NOT NULL)",
    "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER NOT NULL)",
    "CREATE TABLE oauth_clients (id TEXT PRIMARY KEY, name TEXT NOT NULL, redirect_uris TEXT NOT NULL, service_user_id TEXT NOT NULL, created_at INTEGER NOT NULL)",
    "CREATE TABLE oauth_requests (id TEXT PRIMARY KEY, client_id TEXT NOT NULL, redirect_uri TEXT NOT NULL, challenge TEXT NOT NULL, state TEXT, expires_at INTEGER NOT NULL)",
    "CREATE TABLE oauth_codes (hash TEXT PRIMARY KEY, client_id TEXT NOT NULL, redirect_uri TEXT NOT NULL, challenge TEXT NOT NULL, expires_at INTEGER NOT NULL)",
    "CREATE TABLE oauth_tokens (hash TEXT PRIMARY KEY, kind TEXT NOT NULL, client_id TEXT, service_user_id TEXT NOT NULL, expires_at INTEGER NOT NULL, created_at INTEGER NOT NULL)",
    "ALTER TABLE oauth_tokens ADD COLUMN label TEXT",
    "ALTER TABLE users ADD COLUMN avatar TEXT",
    "CREATE TABLE sessions (hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, created_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL)",
    "CREATE TABLE policy_decisions (seq INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, actor TEXT NOT NULL, action TEXT NOT NULL, target TEXT NOT NULL, decision TEXT NOT NULL, rule TEXT, reason TEXT NOT NULL)",
    // Approvals of single actions (`pigeon_core::approval`): the record as JSON, with the
    // columns it is looked up by.
    "CREATE TABLE approvals (id TEXT PRIMARY KEY, caller_id TEXT NOT NULL, digest TEXT NOT NULL, state TEXT NOT NULL, expires_ms INTEGER NOT NULL, record TEXT NOT NULL)",
    // AI agent runs' git grants (`run_grant`): the branch prefix they may push.
    "ALTER TABLE oauth_tokens ADD COLUMN scope TEXT",
    // iOS push registrations (docs/plans/ios.md, phase 6): one device token per app and client.
    "CREATE TABLE apns_devices (id TEXT PRIMARY KEY, app TEXT NOT NULL, person_id TEXT NOT NULL, service_user_id TEXT NOT NULL, token TEXT NOT NULL, environment TEXT NOT NULL, created_at INTEGER NOT NULL, last_ok_at INTEGER, last_error TEXT, UNIQUE(app, token))",
];

/// A day, in milliseconds: how long an ended approval is kept.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How many policy decisions the Directory keeps.
const POLICY_KEPT: i64 = 200;

/// The largest profile picture kept, as a `data:` URL: a 128 px square the browser made.
const AVATAR_MAX: usize = 65_536;

/// Whether `avatar` is a picture the console made: a PNG, JPEG or WebP `data:` URL in base64,
/// no larger than [`AVATAR_MAX`].
pub fn avatar_ok(avatar: &str) -> bool {
    let body = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/webp;base64,",
    ]
    .iter()
    .find_map(|p| avatar.strip_prefix(p));
    avatar.len() <= AVATAR_MAX
        && body.is_some_and(|b| {
            !b.is_empty()
                && b.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'='))
        })
}

/// The users, their passkeys and links: one Durable Object.
#[durable_object]
pub struct Directory {
    db: Db,
}

impl DurableObject for Directory {
    fn new(state: State, _env: Env) -> Self {
        let db = Db::open(&state, &MIGRATIONS).expect("the Directory's migrations apply");
        Self { db }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args)).await
    }
}

impl Directory {
    fn user(&self, id: &str) -> Result<Option<User>> {
        self.db
            .row("SELECT * FROM users WHERE id = ?", vec![id.into()])
    }

    fn create(&self, kind: &str, name: &str) -> Result<User> {
        let user = User {
            id: random()?[..22].to_string(),
            kind: kind.into(),
            name: name.into(),
            created_at: now(),
        };
        self.db.exec(
            "INSERT INTO users (id, kind, name, created_at) VALUES (?, ?, ?, ?)",
            vec![
                user.id.as_str().into(),
                kind.into(),
                name.into(),
                user.created_at.into(),
            ],
        )?;
        Ok(user)
    }

    /// The standard user `service_user_id` acts for.
    fn acting_for(&self, service_user_id: &str) -> Result<Option<User>> {
        self.db.row(
            "SELECT u.* FROM links l JOIN users u ON u.id = l.standard_user_id WHERE l.service_user_id = ?",
            vec![service_user_id.into()],
        )
    }

    /// Approvals matching `query`, which selects `record`.
    fn approvals_where(
        &self,
        query: &str,
        args: Vec<worker::SqlStorageValue>,
    ) -> Result<Vec<approval::Record>> {
        #[derive(Deserialize)]
        struct Row {
            record: String,
        }
        let rows: Vec<Row> = self.db.rows(query, args)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| serde_json::from_str(&r.record).ok())
            .collect())
    }

    /// A setting's value while it is kept.
    fn setting_value(&self, key: &str, t: i64) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct Row {
            value: String,
        }
        let row: Option<Row> = self.db.row(
            "SELECT value FROM settings WHERE key = ? AND expires_at > ?",
            vec![key.into(), t.into()],
        )?;
        Ok(row.map(|r| r.value))
    }

    fn setting_keep(&self, key: &str, value: &str, until: i64) -> Result<()> {
        self.db.exec(
            "INSERT OR REPLACE INTO settings (key, value, expires_at) VALUES (?, ?, ?)",
            vec![key.into(), value.into(), until.into()],
        )?;
        Ok(())
    }

    /// Applies approved instance draft `r`: the record it holds becomes the instance's next
    /// version, unless the instance moved on since the draft was made, which expires it.
    fn apply_draft(&self, r: &mut approval::Record, t: i64) -> Result<()> {
        use crate::instances::{self, record::Instance};
        let base = r.payload["base_version"].as_u64().unwrap_or(0);
        let mut next: Instance = serde_json::from_value(r.payload["record"].clone())?;
        let key = instances::key(&next.id);
        let at = match self.setting_value(&key, t)? {
            Some(text) => serde_json::from_str::<Instance>(&text)?.version,
            None => 0,
        };
        if approval::stale_draft(r, at, base) {
            return Ok(());
        }
        next.version = base + 1;
        let until = t.saturating_add(instances::KEPT_MS);
        self.setting_keep(&key, &serde_json::to_string(&next)?, until)?;
        let mut ids: Vec<String> = match self.setting_value(instances::INDEX, t)? {
            Some(text) => serde_json::from_str(&text)?,
            None => Vec::new(),
        };
        if !ids.contains(&next.id) {
            ids.push(next.id.clone());
            ids.sort();
            self.setting_keep(instances::INDEX, &serde_json::to_string(&ids)?, until)?;
        }
        r.state = approval::State::Used;
        r.used_ms = Some(t);
        Ok(())
    }

    fn approval_get(&self, id: &str) -> Result<Option<approval::Record>> {
        Ok(self
            .approvals_where("SELECT record FROM approvals WHERE id = ?", vec![id.into()])?
            .pop())
    }

    fn approval_put(&self, r: &approval::Record) -> Result<()> {
        self.db.exec(
            "INSERT OR REPLACE INTO approvals (id, caller_id, digest, state, expires_ms, record) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                r.id.as_str().into(),
                r.caller_id.as_str().into(),
                r.digest.as_str().into(),
                r.state.name().into(),
                r.expires_ms.into(),
                serde_json::to_string(r)?.into(),
            ],
        )
    }

    /// Expires the open approvals past their time, answering them so each expiry is recorded
    /// once, and forgets ended ones a day after they expired.
    fn approvals_sweep(&self, now_ms: i64) -> Result<Vec<approval::Record>> {
        let mut stale = self.approvals_where(
            "SELECT record FROM approvals WHERE state IN ('pending', 'approved') AND expires_ms <= ?",
            vec![now_ms.into()],
        )?;
        for r in &mut stale {
            approval::expire(r, now_ms);
            self.approval_put(r)?;
        }
        self.db.exec(
            "DELETE FROM approvals WHERE state NOT IN ('pending', 'approved') AND expires_ms <= ?",
            vec![(now_ms - DAY_MS).into()],
        )?;
        Ok(stale)
    }

    fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        let t = now();
        Ok(match op {
            "users.list" => {
                let users: Vec<User> = self
                    .db
                    .rows("SELECT * FROM users ORDER BY created_at", vec![])?;
                let links: Vec<Value> = self.db.rows(
                    "SELECT service_user_id, standard_user_id, approved_at FROM links",
                    vec![],
                )?;
                #[derive(Deserialize, Serialize)]
                struct Count {
                    user_id: String,
                    passkeys: i64,
                }
                let counts: Vec<Count> = self.db.rows(
                    "SELECT user_id, COUNT(*) AS passkeys FROM passkeys GROUP BY user_id",
                    vec![],
                )?;
                json!({ "users": users, "links": links, "passkeys": counts })
            }
            "users.get" => to_value(self.user(s("id"))?),
            "users.create" => {
                if !matches!(s("kind"), "standard" | "service") {
                    return Err(bad("a user is standard or service"));
                }
                to_value(self.create(s("kind"), s("name"))?)
            }
            "users.remove" => {
                let user = self.user(s("id"))?.ok_or_else(|| bad("no such user"))?;
                let id = user.id.as_str();
                for q in [
                    "DELETE FROM service_ids WHERE user_id = ?",
                    "DELETE FROM passkeys WHERE user_id = ?",
                    "DELETE FROM links WHERE service_user_id = ? OR standard_user_id = ?",
                    "DELETE FROM link_requests WHERE service_user_id = ? OR standard_user_id = ?",
                    "DELETE FROM oauth_tokens WHERE service_user_id = ?",
                    "DELETE FROM oauth_clients WHERE service_user_id = ?",
                    "DELETE FROM sessions WHERE user_id = ?",
                    "DELETE FROM users WHERE id = ?",
                ] {
                    let args = vec![id.into(); q.matches('?').count()];
                    self.db.exec(q, args)?;
                }
                json!({ "removed": user })
            }
            "users.rename" => {
                self.db.exec(
                    "UPDATE users SET name = ? WHERE id = ?",
                    vec![s("name").into(), s("id").into()],
                )?;
                to_value(self.user(s("id"))?)
            }
            "user.avatar_set" => {
                let avatar = s("avatar");
                if !avatar.is_empty() && !avatar_ok(avatar) {
                    return Err(bad(
                        "a picture is a PNG, JPEG or WebP data URL of at most 64 KiB",
                    ));
                }
                self.db.exec(
                    "UPDATE users SET avatar = ? WHERE id = ?",
                    vec![opt(Some(avatar).filter(|a| !a.is_empty())), s("id").into()],
                )?;
                json!({ "ok": true })
            }
            "user.avatar" => {
                #[derive(Deserialize)]
                struct Row {
                    avatar: Option<String>,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT avatar FROM users WHERE id = ?",
                    vec![s("id").into()],
                )?;
                json!(row.and_then(|r| r.avatar))
            }
            "session.create" => {
                self.user(s("user_id"))?
                    .filter(|u| u.kind == "standard")
                    .ok_or_else(|| bad("only a person has a console session"))?;
                self.db.exec(
                    "INSERT INTO sessions (hash, user_id, created_at, last_seen_at) VALUES (?, ?, ?, ?)",
                    vec![s("hash").into(), s("user_id").into(), t.into(), t.into()],
                )?;
                json!({ "created_at": t })
            }
            // A stale session is removed when it is looked up; a fresh one is seen now.
            "session.get" => {
                #[derive(Deserialize)]
                struct Row {
                    user_id: String,
                    created_at: i64,
                    last_seen_at: i64,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT user_id, created_at, last_seen_at FROM sessions WHERE hash = ?",
                    vec![s("hash").into()],
                )?;
                let Some(row) = row else {
                    return Ok(Value::Null);
                };
                let user = self.user(&row.user_id)?.filter(|u| u.kind == "standard");
                if user.is_none()
                    || !crate::console::session::fresh(row.created_at, row.last_seen_at, t)
                {
                    self.db.exec(
                        "DELETE FROM sessions WHERE hash = ?",
                        vec![s("hash").into()],
                    )?;
                    return Ok(Value::Null);
                }
                self.db.exec(
                    "UPDATE sessions SET last_seen_at = ? WHERE hash = ?",
                    vec![t.into(), s("hash").into()],
                )?;
                json!({ "user": user, "created_at": row.created_at, "last_seen_at": t })
            }
            "people.list" => {
                let people: Vec<Value> = self.db.rows(
                    "SELECT id, name, created_at, avatar FROM users WHERE kind = 'standard' ORDER BY created_at",
                    vec![],
                )?;
                let passkeys: Vec<Value> = self.db.rows(
                    "SELECT id, user_id, name, created_at, last_used_at FROM passkeys ORDER BY created_at",
                    vec![],
                )?;
                let apps: Vec<Value> = self.db.rows(
                    "SELECT u.id AS id, u.name AS name, l.standard_user_id AS person, l.approved_at AS linked_at, (SELECT COUNT(*) FROM oauth_tokens o WHERE o.service_user_id = u.id AND o.kind != 'proxy' AND o.expires_at > ?) AS tokens FROM users u LEFT JOIN links l ON l.service_user_id = u.id WHERE u.kind = 'service' ORDER BY u.name",
                    vec![t.into()],
                )?;
                json!({ "people": people, "passkeys": passkeys, "apps": apps })
            }
            // An app's own tokens go and its link goes; proxy credentials a person issued stay.
            "app.disconnect" => {
                let app = self
                    .user(s("id"))?
                    .filter(|u| u.kind == "service")
                    .ok_or_else(|| bad("no such app"))?;
                self.db.exec(
                    "DELETE FROM oauth_tokens WHERE service_user_id = ? AND kind != 'proxy'",
                    vec![s("id").into()],
                )?;
                self.db.exec(
                    "DELETE FROM links WHERE service_user_id = ?",
                    vec![s("id").into()],
                )?;
                self.db.exec(
                    "DELETE FROM apns_devices WHERE service_user_id = ?",
                    vec![s("id").into()],
                )?;
                json!({ "name": app.name })
            }
            // One row per (app, client); a token that moves to another client moves its row.
            "apns.register" => {
                let id = format!("{}:{}", s("app"), s("service_user_id"));
                self.db.exec(
                    "DELETE FROM apns_devices WHERE app = ? AND (service_user_id = ? OR token = ?)",
                    vec![
                        s("app").into(),
                        s("service_user_id").into(),
                        s("token").into(),
                    ],
                )?;
                self.db.exec(
                    "INSERT INTO apns_devices (id, app, person_id, service_user_id, token, environment, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    vec![
                        id.into(),
                        s("app").into(),
                        s("person_id").into(),
                        s("service_user_id").into(),
                        s("token").into(),
                        s("environment").into(),
                        t.into(),
                    ],
                )?;
                json!({ "ok": true })
            }
            "apns.unregister" => {
                self.db.exec(
                    "DELETE FROM apns_devices WHERE app = ? AND service_user_id = ?",
                    vec![s("app").into(), s("service_user_id").into()],
                )?;
                json!({ "ok": true })
            }
            // The rows of an app, for one person when `person_id` is given. Holds tokens: only
            // the Worker reads this, and no tool answers it as is.
            "apns.devices" => {
                let rows: Vec<Value> = if s("person_id").is_empty() {
                    self.db.rows(
                        "SELECT id, app, person_id, service_user_id, token, environment, created_at, last_ok_at, last_error FROM apns_devices WHERE app = ? ORDER BY created_at",
                        vec![s("app").into()],
                    )?
                } else {
                    self.db.rows(
                        "SELECT id, app, person_id, service_user_id, token, environment, created_at, last_ok_at, last_error FROM apns_devices WHERE app = ? AND person_id = ? ORDER BY created_at",
                        vec![s("app").into(), s("person_id").into()],
                    )?
                };
                json!(rows)
            }
            "session.remove" => {
                self.db.exec(
                    "DELETE FROM sessions WHERE hash = ?",
                    vec![s("hash").into()],
                )?;
                json!({ "ok": true })
            }
            "users.standard_count" => {
                #[derive(Deserialize)]
                struct N {
                    n: i64,
                }
                let n: Option<N> = self.db.row(
                    "SELECT COUNT(*) AS n FROM users WHERE kind = 'standard'",
                    vec![],
                )?;
                json!(n.map_or(0, |n| n.n))
            }
            "service.find" => {
                #[derive(Deserialize)]
                struct Row {
                    user_id: String,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT user_id FROM service_ids WHERE access_client_id = ?",
                    vec![s("access_client_id").into()],
                )?;
                match row
                    .and_then(|r| self.user(&r.user_id).transpose())
                    .transpose()?
                {
                    Some(user) => to_value(Caller {
                        acting_for: self.acting_for(&user.id)?,
                        run: None,
                        user,
                    }),
                    None => Value::Null,
                }
            }
            "service.add" => {
                let user = self.create("service", s("name"))?;
                self.db.exec(
                    "INSERT INTO service_ids (access_client_id, user_id) VALUES (?, ?)",
                    vec![s("access_client_id").into(), user.id.as_str().into()],
                )?;
                to_value(Caller {
                    user,
                    acting_for: None,
                    run: None,
                })
            }
            "enrol.create" => {
                let code = random()?;
                self.db.exec(
                    "INSERT INTO enrolments (code, user_id, expires_at) VALUES (?, ?, ?)",
                    vec![
                        code.as_str().into(),
                        s("user_id").into(),
                        (t + ENROL_TTL_MS).into(),
                    ],
                )?;
                json!(code)
            }
            "enrol.peek" | "enrol.take" => {
                #[derive(Deserialize)]
                struct Row {
                    user_id: String,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT user_id FROM enrolments WHERE code = ? AND expires_at > ?",
                    vec![s("code").into(), t.into()],
                )?;
                let row = row.ok_or_else(|| bad("this enrolment link is used or has expired"))?;
                if op == "enrol.take" {
                    self.db.exec(
                        "DELETE FROM enrolments WHERE code = ?",
                        vec![s("code").into()],
                    )?;
                }
                to_value(self.user(&row.user_id)?)
            }
            "challenge.create" => {
                let (id, challenge) = (random()?, random()?);
                self.db.exec(
                    "INSERT INTO challenges (id, purpose, challenge, expires_at) VALUES (?, ?, ?, ?)",
                    vec![
                        id.as_str().into(),
                        s("purpose").into(),
                        challenge.as_str().into(),
                        (t + CHALLENGE_TTL_MS).into(),
                    ],
                )?;
                json!({ "id": id, "challenge": challenge })
            }
            "challenge.take" => {
                #[derive(Deserialize)]
                struct Row {
                    challenge: String,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT challenge FROM challenges WHERE id = ? AND purpose = ? AND expires_at > ?",
                    vec![s("id").into(), s("purpose").into(), t.into()],
                )?;
                self.db.exec(
                    "DELETE FROM challenges WHERE id = ? OR expires_at <= ?",
                    vec![s("id").into(), t.into()],
                )?;
                json!(
                    row.ok_or_else(|| bad("this challenge is used or has expired"))?
                        .challenge
                )
            }
            "passkey.add" => {
                self.db.exec(
                    "INSERT INTO passkeys (id, user_id, point, sign_count, name, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                    vec![
                        s("id").into(),
                        s("user_id").into(),
                        s("point").into(),
                        a["sign_count"].as_i64().unwrap_or(0).into(),
                        s("name").into(),
                        t.into(),
                    ],
                )?;
                Value::Null
            }
            "passkey.get" => to_value(
                self.db
                    .row::<Passkey>("SELECT * FROM passkeys WHERE id = ?", vec![s("id").into()])?,
            ),
            "passkey.touch" => {
                self.db.exec(
                    "UPDATE passkeys SET sign_count = ?, last_used_at = ? WHERE id = ?",
                    vec![
                        a["sign_count"].as_i64().unwrap_or(0).into(),
                        t.into(),
                        s("id").into(),
                    ],
                )?;
                Value::Null
            }
            "passkey.list" => {
                let user = a["user_id"].as_str();
                let rows: Vec<Passkey> = self.db.rows(
                    "SELECT * FROM passkeys WHERE ? IS NULL OR user_id = ? ORDER BY created_at",
                    vec![opt(user), opt(user)],
                )?;
                to_value(rows)
            }
            "passkey.remove" => {
                let key: Passkey = self
                    .db
                    .row("SELECT * FROM passkeys WHERE id = ?", vec![s("id").into()])?
                    .ok_or_else(|| bad("no such passkey"))?;
                let theirs: Vec<Passkey> = self.db.rows(
                    "SELECT * FROM passkeys WHERE user_id = ?",
                    vec![key.user_id.as_str().into()],
                )?;
                if theirs.len() <= 1 {
                    return Err(bad("this is the user's last passkey; add another first"));
                }
                self.db
                    .exec("DELETE FROM passkeys WHERE id = ?", vec![s("id").into()])?;
                json!({ "removed": key.id })
            }
            "link.request" => {
                let id = random()?;
                self.db.exec(
                    "INSERT INTO link_requests (id, service_user_id, standard_user_id, expires_at) VALUES (?, ?, ?, ?)",
                    vec![
                        id.as_str().into(),
                        s("service_user_id").into(),
                        s("standard_user_id").into(),
                        (t + REQUEST_TTL_MS).into(),
                    ],
                )?;
                json!(id)
            }
            "link.request_get" => {
                let r: LinkRequest = self
                    .db
                    .row(
                        "SELECT * FROM link_requests WHERE id = ? AND expires_at > ?",
                        vec![s("id").into(), t.into()],
                    )?
                    .ok_or_else(|| bad("this link request is used or has expired"))?;
                json!({
                    "request": r,
                    "service": self.user(&r.service_user_id)?,
                    "standard": self.user(&r.standard_user_id)?,
                })
            }
            "link.approve" => {
                let r: LinkRequest = self
                    .db
                    .row(
                        "SELECT * FROM link_requests WHERE id = ? AND expires_at > ?",
                        vec![s("id").into(), t.into()],
                    )?
                    .ok_or_else(|| bad("this link request is used or has expired"))?;
                self.db.exec(
                    "INSERT OR REPLACE INTO links (service_user_id, standard_user_id, approved_at, approved_with) VALUES (?, ?, ?, ?)",
                    vec![
                        r.service_user_id.as_str().into(),
                        r.standard_user_id.as_str().into(),
                        t.into(),
                        s("passkey_id").into(),
                    ],
                )?;
                self.db.exec(
                    "DELETE FROM link_requests WHERE id = ?",
                    vec![s("id").into()],
                )?;
                to_value(r)
            }
            "link.remove" => {
                self.db.exec(
                    "DELETE FROM links WHERE service_user_id = ?",
                    vec![s("service_user_id").into()],
                )?;
                Value::Null
            }
            "policy.log" => {
                self.db.exec(
                    "INSERT INTO policy_decisions (at_ms, actor, action, target, decision, rule, reason) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    vec![
                        t.into(),
                        s("actor").into(),
                        s("action").into(),
                        s("target").into(),
                        s("decision").into(),
                        opt(a["rule"].as_str()),
                        s("reason").into(),
                    ],
                )?;
                self.db.exec(
                    "DELETE FROM policy_decisions WHERE seq <= (SELECT MAX(seq) FROM policy_decisions) - ?",
                    vec![POLICY_KEPT.into()],
                )?;
                Value::Null
            }
            "policy.recent" => {
                #[derive(Serialize, Deserialize)]
                struct Decision {
                    at_ms: i64,
                    actor: String,
                    action: String,
                    target: String,
                    decision: String,
                    rule: Option<String>,
                    reason: String,
                }
                let rows: Vec<Decision> = self.db.rows(
                    "SELECT at_ms, actor, action, target, decision, rule, reason FROM policy_decisions ORDER BY seq DESC LIMIT ?",
                    vec![a["limit"].as_i64().unwrap_or(10).clamp(1, POLICY_KEPT).into()],
                )?;
                to_value(rows)
            }
            "setting.get" => {
                #[derive(Deserialize)]
                struct Row {
                    value: String,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT value FROM settings WHERE key = ? AND expires_at > ?",
                    vec![s("key").into(), t.into()],
                )?;
                row.map_or(Value::Null, |r| json!(r.value))
            }
            "setting.put" => {
                self.db.exec(
                    "INSERT OR REPLACE INTO settings (key, value, expires_at) VALUES (?, ?, ?)",
                    vec![
                        s("key").into(),
                        s("value").into(),
                        (t + a["ttl_ms"].as_i64().unwrap_or(EXPECTED_TTL_MS)).into(),
                    ],
                )?;
                Value::Null
            }
            "link.set" => {
                self.db.exec(
                    "INSERT OR REPLACE INTO links (service_user_id, standard_user_id, approved_at, approved_with) VALUES (?, ?, ?, ?)",
                    vec![
                        s("service_user_id").into(),
                        s("standard_user_id").into(),
                        t.into(),
                        s("passkey_id").into(),
                    ],
                )?;
                Value::Null
            }
            "caller.of" => match self.user(s("user_id"))? {
                Some(user) => to_value(Caller {
                    acting_for: self.acting_for(&user.id)?,
                    run: None,
                    user,
                }),
                None => Value::Null,
            },
            "oauth.client_add" => {
                let user = self.create("service", s("name"))?;
                let id = random()?[..22].to_string();
                self.db.exec(
                    "INSERT INTO oauth_clients (id, name, redirect_uris, service_user_id, created_at) VALUES (?, ?, ?, ?, ?)",
                    vec![
                        id.as_str().into(),
                        s("name").into(),
                        a["redirect_uris"].to_string().into(),
                        user.id.as_str().into(),
                        t.into(),
                    ],
                )?;
                json!({ "id": id, "service_user_id": user.id })
            }
            "oauth.client_get" => {
                let row: Option<Value> = self.db.row(
                    "SELECT * FROM oauth_clients WHERE id = ?",
                    vec![s("id").into()],
                )?;
                row.unwrap_or(Value::Null)
            }
            "oauth.clients" => {
                let rows: Vec<Value> = self.db.rows(
                    "SELECT c.id, c.name, c.redirect_uris, c.service_user_id, c.created_at, l.standard_user_id FROM oauth_clients c LEFT JOIN links l ON l.service_user_id = c.service_user_id ORDER BY c.created_at",
                    vec![],
                )?;
                to_value(rows)
            }
            "oauth.client_remove" => {
                #[derive(Deserialize)]
                struct Row {
                    service_user_id: String,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT service_user_id FROM oauth_clients WHERE id = ?",
                    vec![s("id").into()],
                )?;
                let row = row.ok_or_else(|| bad("no such OAuth client"))?;
                for q in [
                    "DELETE FROM oauth_tokens WHERE service_user_id = ?",
                    "DELETE FROM links WHERE service_user_id = ?",
                ] {
                    self.db.exec(q, vec![row.service_user_id.as_str().into()])?;
                }
                self.db.exec(
                    "DELETE FROM oauth_clients WHERE id = ?",
                    vec![s("id").into()],
                )?;
                json!({ "removed": s("id"), "service_user_id": row.service_user_id })
            }
            "oauth.request_add" => {
                let id = random()?;
                self.db.exec(
                    "INSERT INTO oauth_requests (id, client_id, redirect_uri, challenge, state, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
                    vec![
                        id.as_str().into(),
                        s("client_id").into(),
                        s("redirect_uri").into(),
                        s("challenge").into(),
                        opt(a["state"].as_str()),
                        (t + REQUEST_TTL_MS).into(),
                    ],
                )?;
                json!(id)
            }
            "oauth.request_get" => {
                let row: Option<Value> = self.db.row(
                    "SELECT * FROM oauth_requests WHERE id = ? AND expires_at > ?",
                    vec![s("id").into(), t.into()],
                )?;
                row.ok_or_else(|| bad("this sign-in request is used or has expired"))?
            }
            "oauth.request_remove" => {
                self.db.exec(
                    "DELETE FROM oauth_requests WHERE id = ?",
                    vec![s("id").into()],
                )?;
                Value::Null
            }
            "oauth.code_add" => {
                self.db.exec(
                    "INSERT INTO oauth_codes (hash, client_id, redirect_uri, challenge, expires_at) VALUES (?, ?, ?, ?, ?)",
                    vec![
                        s("hash").into(),
                        s("client_id").into(),
                        s("redirect_uri").into(),
                        s("challenge").into(),
                        (t + a["ttl_ms"].as_i64().unwrap_or(120_000)).into(),
                    ],
                )?;
                Value::Null
            }
            "oauth.code_take" => {
                let row: Option<Value> = self.db.row(
                    "SELECT * FROM oauth_codes WHERE hash = ? AND expires_at > ?",
                    vec![s("hash").into(), t.into()],
                )?;
                self.db.exec(
                    "DELETE FROM oauth_codes WHERE hash = ? OR expires_at <= ?",
                    vec![s("hash").into(), t.into()],
                )?;
                row.ok_or_else(|| bad("invalid_grant: the code is used or has expired"))?
            }
            "oauth.token_add" => {
                self.db.exec(
                    "INSERT INTO oauth_tokens (hash, kind, client_id, service_user_id, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                    vec![
                        s("hash").into(),
                        s("kind").into(),
                        opt(a["client_id"].as_str()),
                        s("service_user_id").into(),
                        (t + a["ttl_ms"].as_i64().unwrap_or(3_600_000)).into(),
                        t.into(),
                    ],
                )?;
                Value::Null
            }
            "oauth.token_get" => {
                let row: Option<Value> = self.db.row(
                    "SELECT * FROM oauth_tokens WHERE hash = ? AND kind = ? AND expires_at > ?",
                    vec![s("hash").into(), s("kind").into(), t.into()],
                )?;
                row.unwrap_or(Value::Null)
            }
            "oauth.token_remove" => {
                self.db.exec(
                    "DELETE FROM oauth_tokens WHERE hash = ?",
                    vec![s("hash").into()],
                )?;
                Value::Null
            }
            "oauth.tokens" => {
                let rows: Vec<Value> = self.db.rows(
                    "SELECT kind, client_id, service_user_id, expires_at, created_at FROM oauth_tokens WHERE expires_at > ? ORDER BY created_at",
                    vec![t.into()],
                )?;
                to_value(rows)
            }
            // Proxy credentials: kind 'proxy', held by the person who issued them, known
            // outside the Directory only by the first 12 characters of their hash.
            "proxy.token_add" => {
                let ttl_ms = a["ttl_ms"]
                    .as_i64()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| bad("ttl_ms must be a positive whole number"))?;
                self.db.exec(
                    "INSERT INTO oauth_tokens (hash, kind, client_id, service_user_id, label, expires_at, created_at) VALUES (?, 'proxy', NULL, ?, ?, ?, ?)",
                    vec![
                        s("hash").into(),
                        s("user_id").into(),
                        s("label").into(),
                        t.saturating_add(ttl_ms).into(),
                        t.into(),
                    ],
                )?;
                Value::Null
            }
            "proxy.token_get" => {
                let row: Option<Value> = self.db.row(
                    "SELECT substr(o.hash, 1, 12) AS id, o.label AS label, u.name AS person, CASE WHEN o.kind = 'run_grant' THEN o.scope END AS branch_prefix FROM oauth_tokens o JOIN users u ON u.id = o.service_user_id WHERE o.hash = ? AND o.kind IN ('proxy', 'run_grant') AND o.expires_at > ?",
                    vec![s("hash").into(), t.into()],
                )?;
                row.unwrap_or(Value::Null)
            }
            // An AI agent run's git grant (`docs/plans/ai-agents.md`): kind 'run_grant', held by
            // the person the run acts for, pushing only the branches under `scope`.
            "run_grant.add" => {
                let ttl_ms = a["ttl_ms"]
                    .as_i64()
                    .filter(|v| *v > 0 && *v <= 24 * 3_600_000)
                    .ok_or_else(|| bad("ttl_ms must be positive and at most a day"))?;
                let scope = s("scope");
                if !scope.starts_with("claude/ai-agent-") {
                    return Err(bad(
                        "a run grant's scope is an AI agent run's branch prefix",
                    ));
                }
                self.db.exec(
                    "INSERT INTO oauth_tokens (hash, kind, client_id, service_user_id, label, expires_at, created_at, scope) VALUES (?, 'run_grant', NULL, ?, ?, ?, ?, ?)",
                    vec![
                        s("hash").into(),
                        s("user_id").into(),
                        s("label").into(),
                        t.saturating_add(ttl_ms).into(),
                        t.into(),
                        scope.into(),
                    ],
                )?;
                // Expired grants go as new ones come.
                self.db.exec(
                    "DELETE FROM oauth_tokens WHERE kind = 'run_grant' AND expires_at <= ?",
                    vec![t.into()],
                )?;
                Value::Null
            }
            // Revokes a run's git grants for a person: scope equality, never a prefix match.
            // Answers how many went.
            "run_grant.revoke" => {
                #[derive(Deserialize)]
                struct Count {
                    n: i64,
                }
                let found: Option<Count> = self.db.row(
                    "SELECT COUNT(*) AS n FROM oauth_tokens WHERE kind = 'run_grant' AND scope = ? AND service_user_id = ?",
                    vec![s("scope").into(), s("user_id").into()],
                )?;
                self.db.exec(
                    "DELETE FROM oauth_tokens WHERE kind = 'run_grant' AND scope = ? AND service_user_id = ?",
                    vec![s("scope").into(), s("user_id").into()],
                )?;
                json!(found.map_or(0, |c| c.n))
            }
            // The live ones, and with `expired_within_ms` also those expired that recently.
            "proxy.tokens" => {
                let since = t - a["expired_within_ms"].as_i64().unwrap_or(0).max(0);
                let rows: Vec<Value> = self.db.rows(
                    "SELECT substr(o.hash, 1, 12) AS id, o.label AS label, u.name AS person, o.created_at AS created_at, o.expires_at AS expires_at FROM oauth_tokens o LEFT JOIN users u ON u.id = o.service_user_id WHERE o.kind = 'proxy' AND o.expires_at > ? ORDER BY o.created_at DESC",
                    vec![since.into()],
                )?;
                to_value(rows)
            }
            "proxy.token_remove" => {
                #[derive(Deserialize)]
                struct Count {
                    n: i64,
                }
                let id = crate::proxy::id(s("id")).map_err(|e| bad(&e))?;
                let count: Option<Count> = self.db.row(
                    "SELECT COUNT(*) AS n FROM oauth_tokens WHERE substr(hash, 1, 12) = ? AND kind = 'proxy'",
                    vec![id.into()],
                )?;
                match count.map_or(0, |c| c.n) {
                    0 => return Err(bad("no such proxy credential")),
                    1 => {}
                    _ => return Err(bad("this id names more than one credential")),
                }
                self.db.exec(
                    "DELETE FROM oauth_tokens WHERE substr(hash, 1, 12) = ? AND kind = 'proxy'",
                    vec![id.into()],
                )?;
                Value::Null
            }
            "approval.create" => {
                let expired = self.approvals_sweep(t)?;
                let digest = s("digest");
                let caller = s("caller_id");
                let pending: Vec<approval::Record> = self.approvals_where(
                    "SELECT record FROM approvals WHERE caller_id = ? AND state = 'pending'",
                    vec![caller.into()],
                )?;
                if let Some(r) = pending.iter().find(|r| r.digest == digest) {
                    return Ok(json!({ "record": r, "existing": true, "expired": expired }));
                }
                if pending.len() >= approval::MAX_PENDING_PER_CALLER {
                    return Err(bad(&format!(
                        "{} approvals already wait for this caller; ask again once one is decided or expired",
                        pending.len()
                    )));
                }
                let mut r = approval::new(
                    random()?[..22].to_string(),
                    approval::Kind::named(s("kind")),
                    s("subject").into(),
                    digest.into(),
                    a["summary"].clone(),
                    a["payload"].clone(),
                    caller.into(),
                    a["approver_id"].as_str().map(str::to_string),
                    t,
                );
                // A plan the person reads before deciding may ask to wait longer than its kind's
                // lifetime, up to a linked plan's 30 minutes, never more.
                if let Some(ms) = a["pending_ms"].as_i64() {
                    let ms = ms.clamp(60_000, approval::LINKED_PLAN_TTL_MS);
                    r.expires_ms = t.saturating_add(ms);
                }
                self.approval_put(&r)?;
                json!({ "record": r, "existing": false, "expired": expired })
            }
            "approval.list" => {
                let expired = self.approvals_sweep(t)?;
                let open: Vec<approval::Record> = self.approvals_where(
                    "SELECT record FROM approvals WHERE state IN ('pending', 'approved') ORDER BY expires_ms",
                    vec![],
                )?;
                json!({ "open": open, "expired": expired })
            }
            "approval.get" => to_value(self.approval_get(s("id"))?),
            "approval.decide" | "approval.take" => {
                let id = s("id");
                let mut r = self
                    .approval_get(id)?
                    .ok_or_else(|| bad(&approval::Refusal::NotFound(id.into()).to_string()))?;
                let done = if op == "approval.decide" {
                    approval::decide(
                        &mut r,
                        a["approve"].as_bool().unwrap_or(false),
                        s("digest"),
                        s("person_id"),
                        t,
                    )
                } else {
                    approval::take(&mut r, s("digest"), s("caller_id"), t)
                };
                // An approved instance draft is applied here, in the same call, so nothing can
                // change the record between the check and the write.
                if op == "approval.decide"
                    && done.is_ok()
                    && r.state == approval::State::Approved
                    && r.kind.applies_on_approve()
                {
                    self.apply_draft(&mut r, t)?;
                }
                // A refusal may still have changed the record, such as expiring it.
                self.approval_put(&r)?;
                match done {
                    Ok(()) => json!({ "record": r }),
                    Err(why) => json!({ "record": r, "refused": why.to_string() }),
                }
            }
            other => return Err(bad(&format!("the Directory has no operation `{other}`"))),
        })
    }
}

/// Calls the Directory.
pub async fn directory(env: &Env, op: &str, args: Value) -> Result<Value> {
    durable::call(env, BINDING, OBJECT, op, &args).await
}

fn parse<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T> {
    Ok(serde_json::from_value(v)?)
}

// ---- who is calling ----

/// The caller of `req`, from the Access token Access signed it with: a service token is a
/// service user, made one the first time it calls. An error says why there is none.
pub async fn caller(env: &Env, account: &Account, host: &str, req: &Request) -> Result<Caller> {
    if req.headers().get(identity::HEADER)?.is_none() {
        return Err(bad("the request carries no Access token"));
    }
    let expected = expected(env, account, host).await?;
    let claims = identity::caller(req, &expected)
        .await?
        .ok_or_else(|| bad("the request carries no Access token"))?;
    let client_id = claims
        .common_name
        .ok_or_else(|| bad("the Access token names no service token"))?;
    let found = directory(
        env,
        "service.find",
        json!({ "access_client_id": client_id }),
    )
    .await?;
    if !found.is_null() {
        return parse(found);
    }
    let session = Session::open(account).await?;
    let name = session.service_token_name(&client_id).await;
    let name = session
        .finish(name)
        .await?
        .unwrap_or_else(|| client_id.clone());
    let added = directory(
        env,
        "service.add",
        json!({ "access_client_id": client_id, "name": name }),
    )
    .await?;
    parse(added)
}

/// The issuer and audience the host's Access tokens carry, kept in the Directory for a day.
async fn expected(env: &Env, account: &Account, host: &str) -> Result<Expected> {
    let key = format!("access.expected.{host}");
    let kept = directory(env, "setting.get", json!({ "key": key })).await?;
    if let Some(kept) = kept.as_str() {
        let v: Value = serde_json::from_str(kept)?;
        return Ok(Expected {
            team_domain: v["team_domain"].as_str().unwrap_or_default().into(),
            aud: v["aud"].as_str().unwrap_or_default().into(),
        });
    }
    let session = Session::open(account).await?;
    let found = async {
        let team_domain = session.team_domain().await?;
        let aud = session
            .app_aud(host)
            .await?
            .ok_or_else(|| bad(&format!("no Access application on {host}")))?;
        Ok((team_domain, aud))
    }
    .await;
    let (team_domain, aud) = session.finish(found).await?;
    let value = json!({ "team_domain": team_domain, "aud": aud }).to_string();
    directory(env, "setting.put", json!({ "key": key, "value": value })).await?;
    Ok(Expected { team_domain, aud })
}

// ---- the capability ----

/// Users, on the hostname whose Access tokens say who calls and whose `/auth` pages take
/// passkeys.
pub struct Users {
    pub host: String,
}

impl Users {
    fn enrol_url(&self, code: &str) -> String {
        format!("https://{}/auth/enrol?code={code}", self.host)
    }

    async fn standard_with_link(&self, env: &Env, name: &str) -> Result<Value> {
        let user = directory(
            env,
            "users.create",
            json!({ "kind": "standard", "name": name }),
        )
        .await?;
        let id = user["id"].as_str().unwrap_or_default();
        let code = directory(env, "enrol.create", json!({ "user_id": id })).await?;
        Ok(json!({
            "user": user,
            "enrol": self.enrol_url(code.as_str().unwrap_or_default()),
            "expires_in_minutes": ENROL_TTL_MS / 60_000,
        }))
    }

    async fn run(&self, cx: &Context<'_>, tool: &str, args: &Value) -> Result<Value> {
        let env = cx.env;
        let service = || {
            cx.caller
                .filter(|c| c.user.kind == "service")
                .ok_or_else(|| bad("only a service user can do this"))
        };
        Ok(match tool {
            "users_whoami" => match cx.caller {
                Some(c) => to_value(c),
                None => json!({ "caller": null, "why": cx.caller_note }),
            },
            "users_list" => directory(env, "users.list", json!({})).await?,
            "users_bootstrap" => {
                let n = directory(env, "users.standard_count", json!({})).await?;
                if n.as_i64().unwrap_or(0) > 0 {
                    return Err(bad("a standard user exists; invite with users_invite"));
                }
                self.standard_with_link(env, arg(args, "name")?).await?
            }
            "users_invite" => self.standard_with_link(env, arg(args, "name")?).await?,
            "users_enrol" => {
                let user: Option<User> = parse(
                    directory(env, "users.get", json!({ "id": arg(args, "user_id")? })).await?,
                )?;
                let user = user
                    .filter(|u| u.kind == "standard")
                    .ok_or_else(|| bad("no such standard user"))?;
                let code = directory(env, "enrol.create", json!({ "user_id": user.id })).await?;
                json!({ "user": user, "enrol": self.enrol_url(code.as_str().unwrap_or_default()) })
            }
            "users_link" => {
                let me = service()?;
                let standard = match args["standard_user_id"].as_str() {
                    Some(id) => id.to_string(),
                    None => {
                        let all: Vec<User> =
                            parse(directory(env, "users.list", json!({})).await?["users"].clone())?;
                        let people: Vec<User> =
                            all.into_iter().filter(|u| u.kind == "standard").collect();
                        match people.as_slice() {
                            [one] => one.id.clone(),
                            _ => return Err(bad("name the standard user: standard_user_id")),
                        }
                    }
                };
                let id = directory(
                    env,
                    "link.request",
                    json!({ "service_user_id": me.user.id, "standard_user_id": standard }),
                )
                .await?;
                json!({
                    "approve": format!("https://{}/auth/link?request={}", self.host, id.as_str().unwrap_or_default()),
                    "expires_in_minutes": REQUEST_TTL_MS / 60_000,
                })
            }
            "users_unlink" => {
                let id = match args["service_user_id"].as_str() {
                    Some(id) => id.to_string(),
                    None => service()?.user.id.clone(),
                };
                directory(env, "link.remove", json!({ "service_user_id": id })).await?;
                json!({ "unlinked": id })
            }
            "users_remove" => {
                let id = arg(args, "id")?;
                if cx.caller.is_some_and(|c| c.user.id == id) {
                    return Err(bad("a user cannot remove themselves"));
                }
                directory(env, "users.remove", json!({ "id": id })).await?
            }
            "users_rename" => {
                directory(
                    env,
                    "users.rename",
                    json!({ "id": arg(args, "id")?, "name": arg(args, "name")? }),
                )
                .await?
            }
            other => return Err(unknown(other)),
        })
    }
}

impl Capability for Users {
    fn name(&self) -> &'static str {
        "users"
    }

    fn sections(&self) -> Vec<Section> {
        vec![
            Section {
                tab: Tab::Access,
                id: "people",
                title: "People",
                description: "Who can sign in, with their passkeys. An invite is a one-time link to register a passkey.",
                order: 2,
                cost: None,
            },
            Section {
                tab: Tab::Access,
                id: "delegation",
                title: "Delegation",
                description: "Service accounts that act for a person. Agents ask your policy before acting, as their account.",
                order: 3,
                cost: None,
            },
        ]
    }

    /// Each person with their picture, passkeys and the apps acting for them; and apps acting
    /// for no one. People and Delegation show the same data, each its own part.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let all = directory(cx.env, "people.list", json!({})).await?;
            let list = |k: &str| all[k].as_array().cloned().unwrap_or_default();
            let (passkeys, apps) = (list("passkeys"), list("apps"));
            let people: Vec<Value> = list("people")
                .into_iter()
                .map(|p| {
                    let id = p["id"].clone();
                    json!({
                        "id": id,
                        "name": p["name"],
                        "initials": initials(p["name"].as_str().unwrap_or_default()),
                        "avatar": p["avatar"],
                        "passkeys": passkeys.iter().filter(|k| k["user_id"] == id).collect::<Vec<_>>(),
                        "apps": apps.iter().filter(|a| a["person"] == id).collect::<Vec<_>>(),
                    })
                })
                .collect();
            let unlinked: Vec<&Value> = apps.iter().filter(|a| a["person"].is_null()).collect();
            Ok(json!({ "people": people, "unlinked": unlinked }))
        })
    }

    fn tools(&self) -> Vec<Tool> {
        let one =
            |name: &'static str, description: &'static str, field: &str, required: bool| Tool {
                name,
                description,
                input_schema: json!({
                    "type": "object",
                    "properties": { field: { "type": "string" } },
                    "required": if required { vec![field] } else { vec![] },
                }),
            };
        vec![
            Tool::bare(
                "users_whoami",
                "Who is calling: the user, and the standard user they act for when linked.",
            ),
            Tool::bare(
                "users_list",
                "Every user, standard and service, with links and passkey counts.",
            ),
            one(
                "users_bootstrap",
                "Create the first standard user, only while there is none, and answer a \
                 single-use link where they register a passkey.",
                "name",
                true,
            ),
            one(
                "users_invite",
                "Create a standard user and answer a single-use link where they register a \
                 passkey.",
                "name",
                true,
            ),
            one(
                "users_enrol",
                "Answer a single-use link where a standard user registers another passkey.",
                "user_id",
                true,
            ),
            one(
                "users_link",
                "Ask for the calling service user to act for a standard user (the only one, \
                 when there is one). Answers a link the person opens to approve with a passkey.",
                "standard_user_id",
                false,
            ),
            one(
                "users_unlink",
                "End a service user's link (the caller's, by default).",
                "service_user_id",
                false,
            ),
            one(
                "users_remove",
                "Remove a user and everything that is theirs: links, passkeys, tokens and OAuth \
                 client. A caller cannot remove themselves.",
                "id",
                true,
            ),
            Tool {
                name: "users_rename",
                description: "Change a user's display name.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" }, "name": { "type": "string" } },
                    "required": ["id", "name"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(self.run(cx, tool, args))
    }
}
