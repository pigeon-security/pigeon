//! The token ledger: one shared Cloudflare API token per (caller, permission set), minted and
//! revoked only by core (docs/plans/token-minting.md, decisions 3 and 6).
//!
//! The decisions are in [`Ledger`], a state machine with a clock argument and a
//! [`TokenApi`] seam, so the smoke suite runs it without a Durable Object. [`TokenLedger`] is the
//! thin object around it: one named instance, `ledger`, in the platform Worker.
//!
//! A token's value is held in the machine's memory only. Storage keeps the id, key hash, name,
//! purpose, expiry, caller and a `released` flag. A row with no value in memory (the object
//! restarted) is an orphan: it is revoked by id and replaced at the next lease.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll, Waker};

use pigeon_core::durable::{self, Db};
use pigeon_core::secrets_store;
use pigeon_core::tokens::{self, MintRequest, Policy, Scope, TokenApi};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result, State as ObjectState};

/// The ledger's Durable Object binding in the platform Worker.
pub const BINDING: &str = "TOKEN_LEDGER";

/// The name of the one ledger object.
pub const OBJECT: &str = "ledger";

/// A token with less than this left (milliseconds) is not handed out again. Five minutes, so a
/// taker that keeps one token for a job's steps still has time to finish with it.
pub const MIN_LEFT_MS: i64 = 300_000;

const MIGRATIONS: [&str; 1] = ["CREATE TABLE tokens (
        id TEXT PRIMARY KEY,
        key TEXT NOT NULL,
        name TEXT NOT NULL,
        purpose TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        caller TEXT NOT NULL,
        released INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX tokens_key ON tokens (key);
    CREATE INDEX tokens_caller ON tokens (caller)"];

/// The hash that names a caller: its user id and the id of the person it acts for (or none).
/// Stable, and different for any different pair. Pure.
pub fn caller_hash(user: &str, acting_for: Option<&str>) -> String {
    pigeon_core::oauth::hash(&format!(
        "caller\u{1f}{user}\u{1f}{}",
        acting_for.unwrap_or("")
    ))
}

/// The lease key of `caller` (from [`caller_hash`]) and a permission set: the sorted,
/// de-duplicated permissions, hashed with the caller. Pure.
pub fn lease_key(caller: &str, permissions: &[&str]) -> String {
    let mut set: Vec<&str> = permissions.to_vec();
    set.sort_unstable();
    set.dedup();
    pigeon_core::oauth::hash(&format!("lease\u{1f}{caller}\u{1f}{}", set.join("\u{1e}")))
}

/// One token the ledger knows. It never holds the token's value.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The token's id.
    pub id: String,
    /// The lease key hash.
    pub key: String,
    /// The token's name.
    pub name: String,
    /// The purpose in the name.
    pub purpose: String,
    /// When it expires, in milliseconds since the epoch.
    pub expires_at: i64,
    /// The caller hash it was leased to.
    pub caller: String,
    /// Set when it must be revoked: released, or a revoke that failed. The sweeper deletes it.
    pub released: bool,
}

/// A token handed to a caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    /// The token's value, a secret.
    pub value: String,
    /// The token's id.
    pub id: String,
    /// When it expires, in milliseconds since the epoch.
    pub expires_at: i64,
}

/// What a lease asks for.
pub struct LeaseRequest<'a> {
    /// The lease key hash, from [`lease_key`].
    pub key: &'a str,
    /// The caller hash, from [`caller_hash`].
    pub caller: &'a str,
    /// The purpose that names the token.
    pub purpose: &'a str,
    /// How long a new token lives, in seconds; used when Cloudflare's answer has no expiry.
    pub ttl_secs: u64,
}

/// Where the machine keeps its rows, so a restart finds them.
pub trait Store {
    /// Writes `row`, replacing the row of the same id.
    fn save(&self, row: &Row) -> Result<()>;
    /// Deletes the row of `id`.
    fn remove(&self, id: &str) -> Result<()>;
}

/// No storage: for tests.
impl Store for () {
    fn save(&self, _: &Row) -> Result<()> {
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// What one [`Ledger::reap`] did.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reaped {
    /// Released tokens deleted from Cloudflare.
    pub deleted: usize,
    /// Rows forgotten: those deleted, and those whose expiry passed.
    pub forgotten: usize,
    /// Deletes that failed; the rows stay for the next sweep.
    pub failed: usize,
}

/// Whether `e` is Cloudflare's token quota error (code 1001).
fn quota(e: &Error) -> bool {
    e.to_string().contains("1001")
}

/// Whether `e` says the token is already gone.
fn gone(e: &Error) -> bool {
    let t = e.to_string().to_lowercase();
    t.contains("not found") || t.contains("404")
}

/// The machine's single-flight markers: a key in the map is being minted, and the wakers are the
/// leases waiting for it.
type Flights = RefCell<HashMap<String, Vec<Waker>>>;

/// Marks a key as being minted until dropped, then wakes its waiters.
struct Flight<'a> {
    flights: &'a Flights,
    key: String,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        let wakers = self.flights.borrow_mut().remove(&self.key);
        for w in wakers.unwrap_or_default() {
            w.wake();
        }
    }
}

/// Ready when `key` is no longer being minted.
struct Idle<'a> {
    flights: &'a Flights,
    key: &'a str,
}

impl Future for Idle<'_> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        match self.flights.borrow_mut().get_mut(self.key) {
            Some(waiting) => {
                waiting.push(cx.waker().clone());
                Poll::Pending
            }
            None => Poll::Ready(()),
        }
    }
}

/// The ledger's decisions: rows, the values held in memory, and the single-flight markers.
pub struct Ledger<S: Store> {
    rows: RefCell<Vec<Row>>,
    /// Lease key to (token id, value). Memory only.
    values: RefCell<HashMap<String, (String, String)>>,
    flights: Flights,
    store: S,
}

impl<S: Store> Ledger<S> {
    /// A ledger over `store` that starts with `rows` and no values: each row is an orphan until
    /// it is replaced.
    pub fn new(store: S, rows: Vec<Row>) -> Self {
        Self {
            rows: RefCell::new(rows),
            values: RefCell::default(),
            flights: RefCell::default(),
            store,
        }
    }

    /// A copy of the rows.
    pub fn rows(&self) -> Vec<Row> {
        self.rows.borrow().clone()
    }

    /// The held token for `key` with at least [`MIN_LEFT_MS`] left, if any.
    fn cached(&self, key: &str, now_ms: i64) -> Option<Lease> {
        let values = self.values.borrow();
        let (id, value) = values.get(key)?;
        let rows = self.rows.borrow();
        let row = rows
            .iter()
            .find(|r| r.id == *id && !r.released && r.expires_at - now_ms >= MIN_LEFT_MS)?;
        Some(Lease {
            value: value.clone(),
            id: row.id.clone(),
            expires_at: row.expires_at,
        })
    }

    /// The token for `req`: the held one while it has [`MIN_LEFT_MS`] left, else a new one.
    /// `body` builds the JSON that creates the token (see [`tokens::mint_body`]). Leases for one
    /// key at once mint once: the others wait for it. A quota error (code 1001) runs
    /// [`tokens::sweep`] once and mints once more; the second error is returned.
    pub async fn lease(
        &self,
        api: &impl TokenApi,
        req: &LeaseRequest<'_>,
        now_ms: i64,
        body: impl AsyncFn() -> Result<Value>,
    ) -> Result<Lease> {
        loop {
            if let Some(held) = self.cached(req.key, now_ms) {
                return Ok(held);
            }
            if self.flights.borrow().contains_key(req.key) {
                Idle {
                    flights: &self.flights,
                    key: req.key,
                }
                .await;
                continue;
            }
            self.flights
                .borrow_mut()
                .insert(req.key.to_string(), Vec::new());
            let _flight = Flight {
                flights: &self.flights,
                key: req.key.to_string(),
            };
            return self.fresh(api, req, now_ms, &body).await;
        }
    }

    /// Creates through `api`, with one sweep and one retry on the quota error. Answers the
    /// token and the name it was given.
    async fn create(
        &self,
        api: &impl TokenApi,
        now_ms: i64,
        body: &impl AsyncFn() -> Result<Value>,
    ) -> Result<(tokens::Minted, String)> {
        let mut tried = false;
        loop {
            let body = body().await?;
            let name = body["name"].as_str().unwrap_or_default().to_string();
            match api.create(&body).await {
                Err(e) if quota(&e) && !tried => {
                    tried = true;
                    // A failed sweep is no reason not to try again.
                    let _ = tokens::sweep(api, now_ms).await;
                }
                other => return other.map(|m| (m, name)),
            }
        }
    }

    /// Mints a token for `req`, revokes the orphans of its key and records the new one.
    async fn fresh(
        &self,
        api: &impl TokenApi,
        req: &LeaseRequest<'_>,
        now_ms: i64,
        body: &impl AsyncFn() -> Result<Value>,
    ) -> Result<Lease> {
        let (minted, name) = self.create(api, now_ms, body).await?;
        let expires_at =
            tokens::parse_ms(&minted.expires_on).unwrap_or(now_ms + (req.ttl_secs as i64) * 1000);
        // The token handed out last for this key is left to expire: a taker may still be using
        // it, and it has at most `MIN_LEFT_MS` to live. Only orphans (rows with no value held,
        // after a restart) are revoked now.
        let in_use = self.values.borrow().get(req.key).map(|(id, _)| id.clone());
        let older: Vec<Row> = self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.key == req.key && Some(&r.id) != in_use.as_ref())
            .cloned()
            .collect();
        for old in &older {
            self.revoke_row(api, old).await?;
        }
        let row = Row {
            id: minted.id.clone(),
            key: req.key.to_string(),
            name,
            purpose: req.purpose.to_string(),
            expires_at,
            caller: req.caller.to_string(),
            released: false,
        };
        self.store.save(&row)?;
        self.rows.borrow_mut().push(row);
        self.values.borrow_mut().insert(
            req.key.to_string(),
            (minted.id.clone(), minted.value.clone()),
        );
        Ok(Lease {
            value: minted.value,
            id: minted.id,
            expires_at,
        })
    }

    /// Revokes `row`'s token and forgets the row. A token already gone counts as revoked. Any
    /// other failure keeps the row, marked released, for the sweeper.
    async fn revoke_row(&self, api: &impl TokenApi, row: &Row) -> Result<()> {
        {
            let mut values = self.values.borrow_mut();
            if values.get(&row.key).is_some_and(|(id, _)| *id == row.id) {
                values.remove(&row.key);
            }
        }
        match api.delete(&row.id).await {
            Ok(()) => self.forget(&row.id),
            Err(e) if gone(&e) => self.forget(&row.id),
            Err(_) => self.mark_released(&row.id),
        }
    }

    /// Drops the row of `id`.
    fn forget(&self, id: &str) -> Result<()> {
        self.rows.borrow_mut().retain(|r| r.id != id);
        self.store.remove(id)
    }

    /// Marks the row of `id` released.
    fn mark_released(&self, id: &str) -> Result<()> {
        let row = {
            let mut rows = self.rows.borrow_mut();
            rows.iter_mut().find(|r| r.id == id).map(|r| {
                r.released = true;
                r.clone()
            })
        };
        match row {
            Some(row) => self.store.save(&row),
            None => Ok(()),
        }
    }

    /// Whether `id` is a leased token that is live: in the ledger and not released.
    pub fn holds(&self, id: &str) -> bool {
        self.rows.borrow().iter().any(|r| r.id == id && !r.released)
    }

    /// Revokes every token leased to `caller` (all permission sets), drops their values and
    /// answers how many. A revoke that fails leaves the row released, for the sweeper.
    pub async fn release(&self, api: &impl TokenApi, caller: &str) -> Result<usize> {
        let mine: Vec<Row> = self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.caller == caller)
            .cloned()
            .collect();
        for row in &mine {
            self.mark_released(&row.id)?;
            self.revoke_row(api, row).await?;
        }
        Ok(mine.len())
    }

    /// The sweeper's part: deletes the tokens of released rows (only names that
    /// [`tokens::minted`] accepts) and forgets them, and forgets rows whose expiry passed. A
    /// failed delete keeps its row.
    pub async fn reap(&self, api: &impl TokenApi, now_ms: i64) -> Result<Reaped> {
        let mut out = Reaped::default();
        for row in self.rows() {
            if row.released {
                if tokens::minted(&row.name).is_none() {
                    self.forget(&row.id)?;
                    out.forgotten += 1;
                    continue;
                }
                match api.delete(&row.id).await {
                    Ok(()) => out.deleted += 1,
                    Err(e) if gone(&e) => {}
                    Err(_) => {
                        out.failed += 1;
                        continue;
                    }
                }
                self.forget(&row.id)?;
                out.forgotten += 1;
            } else if row.expires_at <= now_ms {
                self.forget(&row.id)?;
                out.forgotten += 1;
            }
        }
        let rows = self.rows.borrow();
        self.values
            .borrow_mut()
            .retain(|k, (id, _)| rows.iter().any(|r| r.key == *k && r.id == *id));
        Ok(out)
    }
}

/// The ledger's storage: one table in the object's SQLite.
struct DbStore(Db);

#[derive(Deserialize)]
struct DbRow {
    id: String,
    key: String,
    name: String,
    purpose: String,
    expires_at: i64,
    caller: String,
    released: i64,
}

impl Store for DbStore {
    fn save(&self, r: &Row) -> Result<()> {
        self.0.exec(
            "INSERT OR REPLACE INTO tokens (id, key, name, purpose, expires_at, caller, released) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            vec![
                r.id.as_str().into(),
                r.key.as_str().into(),
                r.name.as_str().into(),
                r.purpose.as_str().into(),
                r.expires_at.into(),
                r.caller.as_str().into(),
                i64::from(r.released).into(),
            ],
        )
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.0
            .exec("DELETE FROM tokens WHERE id = ?", vec![id.into()])
    }
}

/// The token ledger's Durable Object logic; the platform Worker's class wraps it.
pub struct TokenLedger {
    env: Env,
    parent: String,
    ledger: Ledger<DbStore>,
}

/// The parent token and the account it acts on.
struct Parent {
    token: String,
    account: String,
}

impl TokenLedger {
    /// The ledger over `state`. `parent_binding` is the Secrets Store binding of the parent token.
    pub fn new(state: ObjectState, env: Env, parent_binding: &str) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        let rows = db
            .rows::<DbRow>("SELECT * FROM tokens", vec![])?
            .into_iter()
            .map(|r| Row {
                id: r.id,
                key: r.key,
                name: r.name,
                purpose: r.purpose,
                expires_at: r.expires_at,
                caller: r.caller,
                released: r.released != 0,
            })
            .collect();
        Ok(Self {
            env,
            parent: parent_binding.to_string(),
            ledger: Ledger::new(DbStore(db), rows),
        })
    }

    /// Answers one op: `lease`, `release`, `holds` or `reap`.
    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    async fn parent(&self) -> Result<Parent> {
        let token = secrets_store::read(&self.env.secret_store(&self.parent)?)
            .await?
            .ok_or_else(|| Error::RustError("the parent token is not set".into()))?;
        Ok(Parent {
            token,
            account: self.env.var("CF_ACCOUNT_ID")?.to_string(),
        })
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        let s = |k: &str| a[k].as_str().unwrap_or_default().to_string();
        let now_ms = worker::Date::now().as_millis() as i64;
        match op {
            "holds" => Ok(json!(self.ledger.holds(&s("id")))),
            "lease" => {
                let parent = self.parent().await?;
                let api = tokens::CloudflareTokens {
                    parent: &parent.token,
                    account: &parent.account,
                };
                let ttl_secs = a["ttl_secs"].as_u64().unwrap_or(900);
                let permissions: Vec<String> =
                    serde_json::from_value(a["permissions"].clone()).unwrap_or_default();
                let perms: Vec<&str> = permissions.iter().map(String::as_str).collect();
                let purpose = s("purpose");
                let request = MintRequest {
                    purpose: purpose.clone(),
                    policies: vec![Policy::new(Scope::Account, &perms)],
                    ttl_secs,
                };
                let (key, caller) = (s("key"), s("caller"));
                let lease = self
                    .ledger
                    .lease(
                        &api,
                        &LeaseRequest {
                            key: &key,
                            caller: &caller,
                            purpose: &purpose,
                            ttl_secs,
                        },
                        now_ms,
                        async || tokens::mint_body(&parent.token, &parent.account, &request).await,
                    )
                    .await?;
                Ok(serde_json::to_value(lease)?)
            }
            "release" => {
                let parent = self.parent().await?;
                let api = tokens::CloudflareTokens {
                    parent: &parent.token,
                    account: &parent.account,
                };
                let n = self.ledger.release(&api, &s("caller")).await?;
                Ok(json!({ "released": n }))
            }
            "reap" => {
                let parent = self.parent().await?;
                let api = tokens::CloudflareTokens {
                    parent: &parent.token,
                    account: &parent.account,
                };
                Ok(serde_json::to_value(self.ledger.reap(&api, now_ms).await?)?)
            }
            other => Err(Error::RustError(format!("the ledger has no op `{other}`"))),
        }
    }
}

/// Asks the ledger for the shared token of (`caller`, `permissions`): see [`Ledger::lease`].
pub async fn lease(
    env: &Env,
    caller: &str,
    purpose: &str,
    permissions: &[&str],
    ttl_secs: u64,
) -> Result<Lease> {
    let args = json!({
        "key": lease_key(caller, permissions),
        "caller": caller,
        "purpose": purpose,
        "permissions": permissions,
        "ttl_secs": ttl_secs,
    });
    let v = durable::call(env, BINDING, OBJECT, "lease", &args).await?;
    Ok(serde_json::from_value(v)?)
}

/// Whether token `id` is a live leased token.
pub async fn holds(env: &Env, id: &str) -> Result<bool> {
    let v = durable::call(env, BINDING, OBJECT, "holds", &json!({ "id": id })).await?;
    Ok(v.as_bool().unwrap_or(false))
}

/// Revokes every token leased to `caller`; answers how many.
pub async fn release(env: &Env, caller: &str) -> Result<usize> {
    let v = durable::call(
        env,
        BINDING,
        OBJECT,
        "release",
        &json!({ "caller": caller }),
    )
    .await?;
    Ok(v["released"].as_u64().unwrap_or(0) as usize)
}

/// The sweeper's call: deletes released tokens and forgets expired rows.
pub async fn reap(env: &Env) -> Result<Reaped> {
    let v = durable::call(env, BINDING, OBJECT, "reap", &json!({})).await?;
    Ok(serde_json::from_value(v)?)
}
