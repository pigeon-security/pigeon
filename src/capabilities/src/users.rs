//! Users: standard users, who are people, and service users, who are agents. A service user
//! can be linked to a standard user, to act for them; the person approves the link with a
//! passkey. A person is the platform's admin or a member ("platform roles", below); a service
//! user acting for a person has that person's role. Every user has a display name and nothing
//! else about them is kept.
//!
//! Users live in the `Directory`, a Durable Object with SQLite storage. A service user is an
//! Access service token, made a user the first time it calls.

use pigeon_core::Account;
use pigeon_core::access::Session;
use pigeon_core::approval;
use pigeon_core::durable::{self, Db, opt};
use pigeon_core::identity::{self, Expected};
use pigeon_core::mcp::Tool;

use crate::console::sections::Section;
use crate::console::write::Write;
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
const MIGRATIONS: [&str; 23] = [
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
    "ALTER TABLE users ADD COLUMN site_scope TEXT",
    "ALTER TABLE enrolments ADD COLUMN site_scope TEXT",
    // Platform roles (docs/plans/capability-scoping.md, "Platform admin"): `admin` or null, a
    // member. The first person on the platform is its first admin.
    ROLE_COLUMN,
    FIRST_ADMIN,
];

/// The users' platform role: `admin`, or null for a member.
#[doc(hidden)]
pub const ROLE_COLUMN: &str = "ALTER TABLE users ADD COLUMN role TEXT";

/// The platform's first person, the oldest standard user, becomes its first admin. Run once,
/// when the role column is added; a platform with no one yet gets its first admin from
/// `users_bootstrap`.
#[doc(hidden)]
pub const FIRST_ADMIN: &str = "UPDATE users SET role = 'admin' WHERE id = (SELECT id FROM users WHERE kind = 'standard' ORDER BY created_at, id LIMIT 1)";

/// A day, in milliseconds: how long an ended approval is kept.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

// These reserved setting keys are ordered between the two prefix bounds.
const DELIVERY_PREFIX: &str = "github:delivery:";
const DELIVERY_END: &str = "github:delivery;";
const DELIVERY_TTL_MS: i64 = 7 * DAY_MS;
const DELIVERY_LIMIT: i64 = 10_000;

#[doc(hidden)]
pub const PRUNE_DELIVERIES: &str =
    "DELETE FROM settings WHERE key >= ? AND key < ? AND expires_at <= ?";

// SQLite materializes the SELECT before replacing any rows. The fingerprint and
// both publication rows are therefore committed together or all rolled back.
#[doc(hidden)]
pub const RECORD_DELIVERY: &str = "
WITH delivery AS MATERIALIZED (
    SELECT ? AS fingerprint, ? AS replay_until, ? AS publish_until,
           ? AS push_key, ? AS push, ? AS deploy_key, ? AS pending
    WHERE NOT EXISTS (SELECT 1 FROM settings WHERE key = ? AND expires_at > ?)
      AND (SELECT count(*) FROM settings WHERE key >= ? AND key < ? AND expires_at > ?) < ?
)
INSERT OR REPLACE INTO settings (key, value, expires_at)
    SELECT fingerprint, '', replay_until FROM delivery
    UNION ALL SELECT push_key, push, publish_until FROM delivery
    UNION ALL SELECT deploy_key, pending, publish_until FROM delivery WHERE pending IS NOT NULL
RETURNING key";

/// How many policy decisions the Directory keeps.
const POLICY_KEPT: i64 = 200;

/// Site visitor list, excluding other hostnames and all previous bindings of this hostname.
#[doc(hidden)]
pub const SITE_VISITORS_SQL: &str = "SELECT id, name,
    EXISTS(SELECT 1 FROM passkeys WHERE user_id = users.id) AS enrolled
    FROM users WHERE kind = ? AND site_scope = ? ORDER BY created_at, id";

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

    fn create(&self, kind: &str, name: &str, site_scope: Option<&str>) -> Result<User> {
        let user = User {
            id: random()?[..22].to_string(),
            kind: kind.into(),
            name: name.into(),
            created_at: now(),
        };
        self.db.exec(
            "INSERT INTO users (id, kind, name, created_at, site_scope) VALUES (?, ?, ?, ?, ?)",
            vec![
                user.id.as_str().into(),
                kind.into(),
                name.into(),
                user.created_at.into(),
                site_scope.map_or(worker::SqlStorageValue::Null, Into::into),
            ],
        )?;
        Ok(user)
    }

    /// The ids of the platform's admins, oldest first.
    fn admins(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Row {
            id: String,
        }
        let rows: Vec<Row> = self.db.rows(
            "SELECT id FROM users WHERE kind = 'standard' AND role = ? ORDER BY created_at, id",
            vec![ADMIN.into()],
        )?;
        Ok(rows.into_iter().map(|r| r.id).collect())
    }

    /// How many people (standard users) there are.
    fn standard_count(&self) -> Result<i64> {
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let n: Option<N> = self.db.row(
            "SELECT COUNT(*) AS n FROM users WHERE kind = 'standard'",
            vec![],
        )?;
        Ok(n.map_or(0, |n| n.n))
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
            Some(text) => match instances::rename::Stored::parse(&text)? {
                instances::rename::Stored::Record(r) => r.version,
                // R1: a renamed id's key holds its tombstone, which no draft replaces.
                instances::rename::Stored::Tombstone(to) => {
                    r.state = approval::State::Expired;
                    r.note = Some(format!(
                        "instance {} was renamed to {}",
                        next.id, to.renamed_to
                    ));
                    return Ok(());
                }
            },
            None => 0,
        };
        if approval::stale_draft(r, at, base) {
            return Ok(());
        }
        let mut ids: Vec<String> = match self.setting_value(instances::INDEX, t)? {
            Some(text) => serde_json::from_str(&text)?,
            None => Vec::new(),
        };
        // Checked again against what is stored now: one home instance, and an account kind
        // that never changes.
        let mut stored = Vec::new();
        for id in &ids {
            if let Some(text) = self.setting_value(&instances::key(id), t)? {
                stored.push(serde_json::from_str::<Instance>(&text)?);
            }
        }
        let problems = instances::record::approval_problems(&next, &stored);
        if !problems.is_empty() {
            r.state = approval::State::Expired;
            r.note = Some(problems.join(" "));
            return Ok(());
        }
        next.version = base + 1;
        let until = t.saturating_add(instances::KEPT_MS);
        self.setting_keep(&key, &serde_json::to_string(&next)?, until)?;
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
            // The raw rows `people` turns into the `users_people` answer; no key material.
            "users.people" => {
                let users: Vec<User> = self
                    .db
                    .rows("SELECT * FROM users ORDER BY created_at", vec![])?;
                let links: Vec<Value> = self.db.rows(
                    "SELECT service_user_id, standard_user_id FROM links",
                    vec![],
                )?;
                let passkeys: Vec<Value> = self.db.rows(
                    "SELECT id, user_id, name, created_at, last_used_at FROM passkeys ORDER BY created_at",
                    vec![],
                )?;
                let clients: Vec<Value> = self
                    .db
                    .rows("SELECT service_user_id FROM oauth_clients", vec![])?;
                json!({ "users": users, "links": links, "passkeys": passkeys, "clients": clients, "admins": self.admins()? })
            }
            "users.get" => to_value(self.user(s("id"))?),
            "users.site_visitors" => {
                let visitors: Vec<Value> = self.db.rows(
                    SITE_VISITORS_SQL,
                    vec![s("kind").into(), s("site_scope").into()],
                )?;
                to_value(visitors)
            }
            "users.site_scope" => {
                #[derive(Deserialize)]
                struct SiteScope {
                    site_scope: Option<String>,
                }
                let row: Option<SiteScope> = self.db.row(
                    "SELECT site_scope FROM users WHERE id = ?",
                    vec![s("id").into()],
                )?;
                to_value(row.and_then(|row| row.site_scope))
            }
            "users.create" => {
                let site_host = s("kind")
                    .strip_prefix("site:")
                    .is_some_and(crate::instances::record::dns_name);
                if !site_host && !matches!(s("kind"), "standard" | "service") {
                    return Err(bad(
                        "a user is standard, service, or site:{host} for a valid hostname",
                    ));
                }
                let scope = a["site_scope"].as_str();
                if site_host != scope.is_some() {
                    return Err(bad("only site users need an approved site binding"));
                }
                // Only the platform's first person is made an admin as they are created; every
                // later admin is made by an admin (`users.set_admin`).
                let admin = a["admin"].as_bool() == Some(true);
                if admin && (s("kind") != "standard" || self.standard_count()? > 0) {
                    return Err(bad(
                        "only the platform's first person is an admin from the start",
                    ));
                }
                let user = self.create(s("kind"), s("name"), scope)?;
                if admin {
                    self.db.exec(
                        "UPDATE users SET role = ? WHERE id = ?",
                        vec![ADMIN.into(), user.id.as_str().into()],
                    )?;
                }
                to_value(user)
            }
            "users.remove" => {
                let user = self.user(s("id"))?.ok_or_else(|| bad("no such user"))?;
                let id = user.id.as_str();
                let admins = self.admins()?;
                if admins.len() <= 1 && admins.iter().any(|a| a == id) {
                    return Err(bad(LAST_ADMIN));
                }
                for q in [
                    "DELETE FROM service_ids WHERE user_id = ?",
                    "DELETE FROM passkeys WHERE user_id = ?",
                    "DELETE FROM links WHERE service_user_id = ? OR standard_user_id = ?",
                    "DELETE FROM link_requests WHERE service_user_id = ? OR standard_user_id = ?",
                    "DELETE FROM oauth_tokens WHERE service_user_id = ?",
                    "DELETE FROM oauth_clients WHERE service_user_id = ?",
                    "DELETE FROM sessions WHERE user_id = ?",
                    "DELETE FROM enrolments WHERE user_id = ?",
                    "DELETE FROM users WHERE id = ?",
                ] {
                    let args = vec![id.into(); q.matches('?').count()];
                    self.db.exec(q, args)?;
                }
                json!({ "removed": user })
            }
            "users.admins" => to_value(self.admins()?),
            // Makes a person an admin, or a member; the last admin stays one.
            "users.set_admin" => {
                let user = self
                    .user(s("id"))?
                    .filter(|u| u.kind == "standard")
                    .ok_or_else(|| bad("no such person"))?;
                let make = a["admin"]
                    .as_bool()
                    .ok_or_else(|| bad("admin is true or false"))?;
                let admins = self.admins()?;
                role_change(&admins, &user.id, make)?;
                self.db.exec(
                    "UPDATE users SET role = ? WHERE id = ?",
                    vec![
                        if make {
                            ADMIN.into()
                        } else {
                            worker::SqlStorageValue::Null
                        },
                        user.id.as_str().into(),
                    ],
                )?;
                json!({ "id": user.id, "name": user.name, "admin": make })
            }
            "users.rename" => {
                self.db.exec(
                    "UPDATE users SET name = ? WHERE id = ?",
                    vec![s("name").into(), s("id").into()],
                )?;
                to_value(self.user(s("id"))?)
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
                    "SELECT id, name, created_at, avatar, role FROM users WHERE kind = 'standard' ORDER BY created_at",
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
            // A push went out: record the time and clear the last error. Answers no token.
            "apns.ok" => {
                self.db.exec(
                    "UPDATE apns_devices SET last_ok_at = ?, last_error = NULL WHERE id = ?",
                    vec![t.into(), s("id").into()],
                )?;
                json!({ "ok": true })
            }
            // Drops one row by id: APNs said its token is wrong or gone.
            "apns.forget" => {
                self.db.exec(
                    "DELETE FROM apns_devices WHERE id = ?",
                    vec![s("id").into()],
                )?;
                json!({ "ok": true })
            }
            // Records APNs's reason text on a row. Never a token.
            "apns.mark" => {
                self.db.exec(
                    "UPDATE apns_devices SET last_error = ? WHERE id = ?",
                    vec![s("error").into(), s("id").into()],
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
            "users.standard_count" => json!(self.standard_count()?),
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
                let user = self.create("service", s("name"), None)?;
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
                    "INSERT INTO enrolments (code, user_id, expires_at, site_scope) VALUES (?, ?, ?, ?)",
                    vec![
                        code.as_str().into(),
                        s("user_id").into(),
                        (t + ENROL_TTL_MS).into(),
                        a["site_scope"].as_str().map_or(worker::SqlStorageValue::Null, Into::into),
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
                    "SELECT user_id FROM enrolments WHERE code = ? AND expires_at > ? AND site_scope IS ?",
                    vec![s("code").into(), t.into(), a["site_scope"].as_str().map_or(worker::SqlStorageValue::Null, Into::into)],
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
                    purpose: String,
                    challenge: String,
                    expires_at: i64,
                }
                let row: Option<Row> = self.db.row(
                    "SELECT purpose, challenge, expires_at FROM challenges WHERE id = ?",
                    vec![s("id").into()],
                )?;
                // Gone once asked for, answered or not: each challenge is used once.
                self.db.exec(
                    "DELETE FROM challenges WHERE id = ? OR expires_at <= ?",
                    vec![s("id").into(), t.into()],
                )?;
                let mut stored = row.map(|r| crate::console::write::Stored {
                    purpose: r.purpose,
                    challenge: r.challenge,
                    expires_ms: r.expires_at,
                });
                json!(crate::console::write::redeem(&mut stored, s("purpose"), t).map_err(bad)?)
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
            // Whose a passkey is and how many they have, to decide who may remove it.
            "passkey.owner" => {
                let key: Passkey = self
                    .db
                    .row("SELECT * FROM passkeys WHERE id = ?", vec![s("id").into()])?
                    .ok_or_else(|| bad("no such passkey"))?;
                let theirs: Vec<Passkey> = self.db.rows(
                    "SELECT * FROM passkeys WHERE user_id = ?",
                    vec![key.user_id.as_str().into()],
                )?;
                json!({ "id": key.id, "user_id": key.user_id, "keys": theirs.len() })
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
                    return Err(bad(LAST_PASSKEY));
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
            "github.delivery" => {
                let instance = s("instance");
                let fingerprint = s("fingerprint");
                let push = a["push"]
                    .as_str()
                    .ok_or_else(|| bad("expected GitHub push"))?;
                if instance.is_empty()
                    || fingerprint.len() != 64
                    || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(bad("expected an instance and GitHub delivery fingerprint"));
                }
                let key = format!("{DELIVERY_PREFIX}{fingerprint}");
                self.db.exec(
                    PRUNE_DELIVERIES,
                    vec![DELIVERY_PREFIX.into(), DELIVERY_END.into(), t.into()],
                )?;
                let rows: Vec<Value> = self.db.rows(
                    RECORD_DELIVERY,
                    vec![
                        key.as_str().into(),
                        (t + DELIVERY_TTL_MS).into(),
                        (t + crate::github::PUSH_KEPT_MS).into(),
                        crate::github::push_key(instance).as_str().into(),
                        push.into(),
                        crate::github::deploy_key(instance).as_str().into(),
                        opt(a["pending"].as_str()),
                        key.as_str().into(),
                        t.into(),
                        DELIVERY_PREFIX.into(),
                        DELIVERY_END.into(),
                        t.into(),
                        DELIVERY_LIMIT.into(),
                    ],
                )?;
                json!(if !rows.is_empty() {
                    "recorded"
                } else if self.setting_value(&key, t)?.is_some() {
                    "duplicate"
                } else {
                    "full"
                })
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
            "setting.replace" => {
                let key = s("key");
                let previous = a["previous"]
                    .as_str()
                    .ok_or_else(|| bad("expected previous setting"))?;
                let value = a["value"]
                    .as_str()
                    .ok_or_else(|| bad("expected new setting"))?;
                let matches = self.setting_value(key, t)?.as_deref() == Some(previous);
                if matches {
                    self.setting_keep(
                        key,
                        value,
                        t + a["ttl_ms"].as_i64().unwrap_or(EXPECTED_TTL_MS),
                    )?;
                }
                json!(matches)
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
                let user = self.create("service", s("name"), None)?;
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
            // Expires those of approvals `ids` still open, with `note`: R1's rename, whose old
            // id their digests name. Answers the ids it expired.
            "approval.expire" => {
                let mut expired = Vec::new();
                for id in a["ids"].as_array().into_iter().flatten() {
                    let Some(mut r) = self.approval_get(id.as_str().unwrap_or_default())? else {
                        continue;
                    };
                    if matches!(
                        r.state,
                        approval::State::Pending | approval::State::Approved
                    ) {
                        r.state = approval::State::Expired;
                        r.note = Some(s("note").to_string());
                        self.approval_put(&r)?;
                        expired.push(r.id);
                    }
                }
                json!({ "expired": expired })
            }
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

/// The normalized display name accepted by the console invitation forms.
pub(crate) fn invite_name(name: &str) -> std::result::Result<String, String> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > 64 || name.chars().any(char::is_control) {
        Err("a name is 1 to 64 characters, none of them control characters".into())
    } else {
        Ok(name.to_string())
    }
}

// ---- platform roles ----
//
// A person is the platform's `admin` or a member (docs/plans/capability-scoping.md, "Platform
// admin"). The role is platform-level, like identity: it lives here in `users`, which only home
// serves. Only an admin invites, removes or disconnects for others, removes another person's
// passkey, or makes someone an admin; the last admin stays one. The rules are the pure
// functions below, asked by the MCP tools and the console's writes alike (at `begin` and again
// at `finish`), with the role read from the Directory each time, never from a carried caller.

/// The admin role, as the Directory keeps it.
pub const ADMIN: &str = "admin";

/// Why the last admin cannot go or be made a member.
pub const LAST_ADMIN: &str =
    "this is the platform's last admin; make another person an admin first";

/// Why a person's last passkey cannot be removed.
pub const LAST_PASSKEY: &str = "this is the user's last passkey; add another first";

/// The person `caller` acts as: themselves when a person, else the person a service user acts
/// for. A service user acting for no one is no person.
pub fn person_of(caller: &Caller) -> Option<&User> {
    if caller.user.kind == "standard" {
        Some(&caller.user)
    } else {
        caller.acting_for.as_ref().filter(|u| u.kind == "standard")
    }
}

/// Whether a person who is an admin (`admin`) may do `what`, which only an admin does.
pub fn admin_only(admin: bool, what: &str) -> std::result::Result<(), String> {
    if admin {
        Ok(())
    } else {
        Err(format!("only an admin can {what}"))
    }
}

/// Whether `person` (an admin or not) may remove a passkey of `owner`, who has `owner_keys`
/// passkeys: their own, or anyone's as an admin; never anyone's last.
pub fn passkey_removal(
    person: &str,
    admin: bool,
    owner: &str,
    owner_keys: usize,
) -> std::result::Result<(), String> {
    if person != owner {
        admin_only(admin, "remove another person's passkey")?;
    }
    if owner_keys <= 1 {
        return Err(LAST_PASSKEY.into());
    }
    Ok(())
}

/// Whether `target` may be made an admin (`make`) or a member, given today's `admins`: the last
/// admin stays one. Who asks is checked by [`admin_only`].
pub fn role_change(admins: &[String], target: &str, make: bool) -> std::result::Result<(), String> {
    if !make && admins.len() <= 1 && admins.iter().any(|a| a == target) {
        return Err(LAST_ADMIN.into());
    }
    Ok(())
}

/// Whether the person `id` is an admin, read from the Directory now.
pub async fn is_admin(env: &Env, id: &str) -> Result<bool> {
    let admins: Vec<String> = parse(directory(env, "users.admins", json!({})).await?)?;
    Ok(admins.iter().any(|a| a == id))
}

/// The person `cx`'s caller acts as, or why there is none.
pub fn acting_person<'a>(cx: &'a Context<'a>) -> Result<&'a User> {
    cx.caller
        .and_then(person_of)
        .ok_or_else(|| bad("this needs a person: a person, or an app or agent acting for one"))
}

/// Refuses `what` unless `person` is an admin now.
pub async fn require_admin(env: &Env, person: &User, what: &str) -> Result<()> {
    admin_only(is_admin(env, &person.id).await?, what).map_err(|e| bad(&e))
}

/// Refuses removing the passkey `key` unless `person` may ([`passkey_removal`]).
pub async fn check_passkey_removal(env: &Env, person: &User, key: &str) -> Result<()> {
    let k = directory(env, "passkey.owner", json!({ "id": key })).await?;
    let owner = k["user_id"].as_str().unwrap_or_default();
    let keys = k["keys"].as_u64().unwrap_or(0) as usize;
    let admin = person.id != owner && is_admin(env, &person.id).await?;
    passkey_removal(&person.id, admin, owner, keys).map_err(|e| bad(&e))
}

/// Refuses making `target` an admin (`make`) or a member unless `person` is an admin and the
/// change keeps one ([`role_change`]).
pub async fn check_role_change(env: &Env, person: &User, target: &str, make: bool) -> Result<()> {
    let admins: Vec<String> = parse(directory(env, "users.admins", json!({})).await?)?;
    admin_only(
        admins.iter().any(|a| a == &person.id),
        "make a person an admin or a member",
    )
    .and_then(|()| role_change(&admins, target, make))
    .map_err(|e| bad(&e))
}

/// Users, on the hostname whose Access tokens say who calls and whose `/auth` pages take
/// passkeys.
pub struct Users {
    pub host: String,
}

/// Who a site enrolment is for, which decides the allowlist check it needs.
enum Visitor<'a> {
    /// A standard user: the site's allowlist is not asked.
    Standard,
    /// A site user about to be created.
    New,
    /// An existing site user, admitted as sign-in would admit them.
    Existing(&'a User),
}

impl Users {
    /// The enrolment link: on `site` when given (so the passkey is made for that host), else
    /// on the platform's own host.
    fn enrol_url(&self, code: &str, site: Option<&str>) -> String {
        match site {
            Some(site) => format!("https://{site}/_pigeon/enrol?code={code}"),
            None => format!("https://{}/auth/enrol?code={code}", self.host),
        }
    }

    /// The approved binding of `host`, when given, checked against the site's allowlist for
    /// the visitor about to be enrolled: a new site user needs the allowlist to admit new site
    /// visitors (the rule the console enforces); an existing user needs what sign-in needs.
    async fn site_scope(
        &self,
        env: &Env,
        host: Option<&str>,
        visitor: Visitor<'_>,
    ) -> Result<Option<String>> {
        let Some(host) = host else {
            return Ok(None);
        };
        let (_, zone) = self
            .host
            .split_once('.')
            .ok_or_else(|| bad("platform host has no zone"))?;
        let (scope, config) = crate::site_passkeys::approved(env, zone, host).await?;
        match visitor {
            Visitor::Standard => {}
            Visitor::New => crate::site_passkeys::admit_new_visitors(&config)?,
            Visitor::Existing(user) => {
                if !pigeon_core::site_auth::allowed(&config, host, &user.id, &user.kind) {
                    return Err(bad("the site's allowlist does not admit this user"));
                }
            }
        }
        Ok(Some(scope))
    }

    /// Creates a user, a standard one or a site user when `site` is given, and answers them
    /// with a single-use enrolment link.
    async fn standard_with_link(&self, env: &Env, name: &str, site: Option<&str>) -> Result<Value> {
        let scope = self.site_scope(env, site, Visitor::New).await?;
        self.enrol_new_user(env, name, site, scope.as_deref()).await
    }

    /// Issues an enrollment for a caller-validated site binding, or a platform enrollment.
    pub(crate) async fn enrol_new_user(
        &self,
        env: &Env,
        name: &str,
        site: Option<&str>,
        scope: Option<&str>,
    ) -> Result<Value> {
        self.new_user(env, name, site, scope, false).await
    }

    /// As [`Users::enrol_new_user`]; `admin` only for the platform's first person.
    async fn new_user(
        &self,
        env: &Env,
        name: &str,
        site: Option<&str>,
        scope: Option<&str>,
        admin: bool,
    ) -> Result<Value> {
        let kind = match site {
            Some(host) => format!("site:{host}"),
            None => "standard".to_string(),
        };
        let user = directory(
            env,
            "users.create",
            json!({ "kind": kind, "name": name, "site_scope": scope, "admin": admin }),
        )
        .await?;
        let id = user["id"].as_str().unwrap_or_default();
        let code = directory(
            env,
            "enrol.create",
            json!({ "user_id": id, "site_scope": scope }),
        )
        .await?;
        Ok(json!({
            "user": user,
            "enrol": self.enrol_url(code.as_str().unwrap_or_default(), site),
            "expires_in_minutes": ENROL_TTL_MS / 60_000,
        }))
    }

    /// Refuses a console write of `users` that its person may not make, asked at `begin` and
    /// again at `finish`, with the role read now: the same rules as the MCP tools.
    async fn write_rule(&self, env: &Env, w: &Write<'_>) -> Result<()> {
        let person = w.person;
        match w.action.id {
            "passkey-remove" => check_passkey_removal(env, person, w.text()).await,
            "app-disconnect" => require_admin(env, person, "disconnect an app").await,
            "invite" => require_admin(env, person, "invite a person").await,
            "person-admin" => {
                let make = w.change["admin"].as_bool().unwrap_or(false);
                check_role_change(env, person, w.text(), make).await
            }
            other => Err(bad(&format!("users makes no write `{other}`"))),
        }
    }

    /// Makes a console write of `users`, approved by its person's passkey.
    async fn write(&self, env: &Env, w: &Write<'_>) -> Result<Value> {
        self.write_rule(env, w).await?;
        let target = w.text();
        Ok(match w.action.id {
            "passkey-remove" => {
                directory(env, "passkey.remove", json!({ "id": target })).await?;
                json!({ "message": "The passkey is removed." })
            }
            "app-disconnect" => {
                let gone = directory(env, "app.disconnect", json!({ "id": target })).await?;
                json!({ "message": format!(
                    "{} is disconnected: its tokens are revoked and it acts for no one.",
                    gone["name"].as_str().unwrap_or("The app")
                ) })
            }
            "invite" => {
                let made = self.enrol_new_user(env, target, None, None).await?;
                let link = made["enrol"].as_str().unwrap_or_default().to_string();
                json!({
                    "message": format!("{target} is added. Send them this link to register a passkey: it works once, within 15 minutes."),
                    "link": link,
                    // The new console's ShowOnce (P3).
                    "reveal": {
                        "value": link,
                        "note": format!("Send this link to {target}. They register their own passkey from it, within 15 minutes."),
                        "kind": "invite",
                    },
                })
            }
            "person-admin" => {
                let make = w.change["admin"].as_bool().unwrap_or(false);
                let done = directory(
                    env,
                    "users.set_admin",
                    json!({ "id": target, "admin": make }),
                )
                .await?;
                let name = done["name"].as_str().unwrap_or("The person");
                json!({ "message": if make {
                    format!("{name} is now an admin.")
                } else {
                    format!("{name} is now a member.")
                } })
            }
            other => return Err(bad(&format!("users makes no write `{other}`"))),
        })
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
                Some(c) => {
                    let mut out = to_value(c);
                    let person = if c.user.kind == "standard" {
                        Some(&c.user)
                    } else {
                        c.acting_for.as_ref().filter(|u| u.kind == "standard")
                    };
                    out["initials"] = person.map_or(Value::Null, |u| json!(initials(&u.name)));
                    out["picture"] = Value::Null;
                    out
                }
                None => json!({ "caller": null, "why": cx.caller_note }),
            },
            "users_list" => directory(env, "users.list", json!({})).await?,
            "users_people" => {
                let raw = directory(env, "users.people", json!({})).await?;
                let users: Vec<User> = serde_json::from_value(raw["users"].clone())?;
                let rows = |k: &str| raw[k].as_array().cloned().unwrap_or_default();
                let clients: Vec<String> = rows("clients")
                    .iter()
                    .filter_map(|c| c["service_user_id"].as_str().map(str::to_string))
                    .collect();
                let admins: Vec<String> = parse(raw["admins"].clone())?;
                people(&users, &rows("links"), &rows("passkeys"), &clients, &admins)
            }
            // The platform's first person is its first admin.
            "users_bootstrap" => {
                let n = directory(env, "users.standard_count", json!({})).await?;
                if n.as_i64().unwrap_or(0) > 0 {
                    return Err(bad("a standard user exists; invite with users_invite"));
                }
                self.new_user(env, arg(args, "name")?, None, None, true)
                    .await?
            }
            "users_invite" => {
                require_admin(env, acting_person(cx)?, "invite a person").await?;
                let site = args["site"].as_str();
                self.standard_with_link(env, arg(args, "name")?, site)
                    .await?
            }
            "users_enrol" => {
                let site = args["site"].as_str();
                let user: Option<User> = parse(
                    directory(env, "users.get", json!({ "id": arg(args, "user_id")? })).await?,
                )?;
                let user = user
                    .filter(|u| {
                        u.kind == "standard"
                            || site.is_some_and(|host| u.kind == format!("site:{host}"))
                    })
                    .ok_or_else(|| bad("no such standard user, or user of that site"))?;
                // A link to register a passkey is a way in: for anyone but oneself, an admin's.
                let me = acting_person(cx)?;
                if user.id != me.id {
                    require_admin(env, me, "add a passkey for another person").await?;
                }
                let visitor = if user.kind == "standard" {
                    Visitor::Standard
                } else {
                    Visitor::Existing(&user)
                };
                let scope = self.site_scope(env, site, visitor).await?;
                if user.kind != "standard"
                    && directory(env, "users.site_scope", json!({ "id": user.id })).await?
                        != to_value(&scope)
                {
                    return Err(bad(
                        "user belongs to a previous site binding; invite them again",
                    ));
                }
                let code = directory(
                    env,
                    "enrol.create",
                    json!({ "user_id": user.id, "site_scope": scope }),
                )
                .await?;
                json!({ "user": user, "enrol": self.enrol_url(code.as_str().unwrap_or_default(), site) })
            }
            "users_link" => {
                let me = service()?;
                let standard = match args["standard_user_id"].as_str() {
                    Some(id) => {
                        let person: Option<User> =
                            parse(directory(env, "users.get", json!({ "id": id })).await?)?;
                        person
                            .filter(|u| u.kind == "standard")
                            .ok_or_else(|| bad("no such standard user"))?
                            .id
                    }
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
                require_admin(env, acting_person(cx)?, "remove a user").await?;
                directory(env, "users.remove", json!({ "id": id })).await?
            }
            "users_admin" => {
                let id = arg(args, "id")?;
                let make = args["admin"]
                    .as_bool()
                    .ok_or_else(|| bad("admin is true or false"))?;
                check_role_change(env, acting_person(cx)?, id, make).await?;
                directory(env, "users.set_admin", json!({ "id": id, "admin": make })).await?
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

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "users",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn sections(&self) -> Vec<Section> {
        vec![
            Section {
                area: "access",
                id: "people",
                title: "People",
                description: "Who can sign in, with their passkeys. An invite is a one-time link to register a passkey.",
                order: 30,
            },
            Section {
                area: "access",
                id: "apps",
                title: "Apps",
                description: "Apps that act for a person, and apps that act for no one. Disconnecting one asks for your passkey.",
                order: 40,
            },
        ]
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
            Tool::bare(
                "users_people",
                "The people who can sign in, with their passkeys and the apps and services acting for them.",
            ),
            one(
                "users_bootstrap",
                "Create the first standard user, only while there is none, and answer a \
                 single-use link where they register a passkey.",
                "name",
                true,
            ),
            Tool {
                name: "users_invite",
                description: "Create a standard user (an admin only) and answer a single-use \
                 link where they register a passkey. With `site` (a host such as notes.pigeon.dev), create a \
                 site user for that host instead, with a link on that host.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" }, "site": { "type": "string" } },
                    "required": ["name"]
                }),
            },
            Tool {
                name: "users_enrol",
                description: "Answer a single-use link where a standard user registers another \
                 passkey. With `site`, the link is on that host, for a site user of it.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "user_id": { "type": "string" }, "site": { "type": "string" } },
                    "required": ["user_id"]
                }),
            },
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
                 client. Only an admin can; a caller cannot remove themselves, and the last \
                 admin stays.",
                "id",
                true,
            ),
            Tool {
                name: "users_admin",
                description: "Make a person the platform's admin (`admin: true`) or a member \
                 (`admin: false`). Only an admin can; the last admin stays one.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" }, "admin": { "type": "boolean" } },
                    "required": ["id", "admin"]
                }),
            },
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

    fn write_check<'a>(&'a self, cx: &'a Context<'a>, w: &'a Write<'a>) -> Call<'a> {
        Box::pin(async move {
            self.write_rule(cx.env, w).await?;
            Ok(Value::Null)
        })
    }

    fn write_make<'a>(&'a self, cx: &'a Context<'a>, w: &'a Write<'a>) -> Call<'a> {
        Box::pin(self.write(cx.env, w))
    }
}

/// One or two uppercase letters for `name`: the first letters of its first and last words.
pub fn initials(name: &str) -> String {
    crate::console::initials(name)
}

/// The `users_people` answer from the Directory's raw rows: `users` (all kinds), `links`
/// (`service_user_id`, `standard_user_id`), `passkeys` (id, user_id, name, created_at,
/// last_used_at only), `clients` (the service user ids that have an OAuth client) and
/// `admins` (the admins' ids). Only
/// standard users are listed, oldest first; service users linked to no one are counted.
pub fn people(
    users: &[User],
    links: &[Value],
    passkeys: &[Value],
    clients: &[String],
    admins: &[String],
) -> Value {
    let linked_to = |service: &str| {
        links
            .iter()
            .find(|l| l["service_user_id"] == service)
            .and_then(|l| l["standard_user_id"].as_str())
    };
    let mut standard: Vec<&User> = users.iter().filter(|u| u.kind == "standard").collect();
    standard.sort_by_key(|u| u.created_at);
    let list: Vec<Value> = standard
        .iter()
        .map(|p| {
            let keys: Vec<Value> = passkeys
                .iter()
                .filter(|k| k["user_id"] == p.id.as_str())
                .map(|k| {
                    json!({
                        "id": k["id"], "name": k["name"],
                        "created_at": k["created_at"], "last_used_at": k["last_used_at"],
                    })
                })
                .collect();
            let acting: Vec<Value> = users
                .iter()
                .filter(|u| u.kind == "service" && linked_to(&u.id) == Some(p.id.as_str()))
                .map(|u| {
                    let kind = if clients.contains(&u.id) {
                        "app"
                    } else {
                        "service"
                    };
                    json!({ "id": u.id, "name": u.name, "kind": kind, "added_ms": u.created_at })
                })
                .collect();
            json!({
                "id": p.id, "name": p.name, "initials": initials(&p.name), "picture": null,
                "role": if admins.contains(&p.id) { "Admin" } else { "Person" },
                "admin": admins.contains(&p.id), "added_ms": p.created_at,
                "passkeys": keys, "acting_for_me": acting,
            })
        })
        .collect();
    let unlinked = users
        .iter()
        .filter(|u| u.kind == "service" && linked_to(&u.id).is_none())
        .count();
    json!({ "people": list, "unlinked": unlinked })
}
