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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
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
pub const BINDING: &str = tokens::LEDGER_BINDING;

/// The name of the one ledger object.
pub const OBJECT: &str = tokens::LEDGER_OBJECT;

/// A token with less than this left (milliseconds) is not handed out again. Five minutes, so a
/// taker that keeps one token for a job's steps still has time to finish with it.
pub const MIN_LEFT_MS: i64 = 300_000;

const MIGRATIONS: [&str; 2] = [
    "CREATE TABLE tokens (
        id TEXT PRIMARY KEY,
        key TEXT NOT NULL,
        name TEXT NOT NULL,
        purpose TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        caller TEXT NOT NULL,
        released INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX tokens_key ON tokens (key);
    CREATE INDEX tokens_caller ON tokens (caller)",
    // The gauge and the soft ceiling: one key and value per row (`count`, `at_ms`, `ceiling`).
    // `created_at` is when the ledger minted a row; rows from before are NULL.
    "ALTER TABLE tokens ADD COLUMN created_at INTEGER;
    CREATE TABLE gauge (k TEXT PRIMARY KEY, v INTEGER NOT NULL)",
];

/// Cloudflare's cap on live API tokens in an account.
pub const HARD_CAP: u32 = 500;

/// The soft ceiling until someone sets one: new agent runs queue at or above it.
pub const DEFAULT_SOFT_CEILING: u32 = tokens::DEFAULT_SOFT_CEILING;

/// The soft ceilings that may be set.
pub const CEILING_RANGE: std::ops::RangeInclusive<u32> = 10..=450;

/// A stored gauge older than this (milliseconds) is refreshed when it is read.
pub const GAUGE_MAX_AGE_MS: i64 = 120_000;
const ACCOUNT_CACHE_MS: i64 = 15_000;
const PENDING_PREFIX: &str = "pending:";
const MINT_FLIGHT: &str = "account mint";

/// Checks a soft ceiling against [`CEILING_RANGE`]. The error is a plain sentence. Pure.
pub fn check_ceiling(v: i64) -> std::result::Result<u32, String> {
    u32::try_from(v)
        .ok()
        .filter(|v| CEILING_RANGE.contains(v))
        .ok_or_else(|| {
            format!(
                "soft_ceiling must be from {} to {} (Cloudflare's hard cap is {HARD_CAP}), not {v}",
                CEILING_RANGE.start(),
                CEILING_RANGE.end()
            )
        })
}

/// The last count of live pigeon tokens from the account's list, and when it was taken.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepGauge {
    /// Unexpired pigeon-minted tokens the list showed.
    pub count: u32,
    /// When the list was taken, in milliseconds since the epoch.
    pub at_ms: i64,
}

#[derive(Clone, Copy)]
struct AccountCount {
    listed: u32,
    reserved: u32,
    pigeon_live: u32,
    at_ms: i64,
}

impl AccountCount {
    fn fresh(self, now_ms: i64) -> bool {
        (0..ACCOUNT_CACHE_MS).contains(&(now_ms - self.at_ms))
    }
}

/// The gauge as it is read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gauge {
    /// Live pigeon-minted tokens, as best known.
    pub live: u32,
    /// New agent runs queue at or above this.
    pub soft_ceiling: u32,
    /// Cloudflare's cap, [`HARD_CAP`].
    pub hard_cap: u32,
    /// When the count it is based on was taken, in milliseconds since the epoch.
    pub as_of_ms: i64,
    /// Where the count came from: `sweep`, `refreshed`, `stored (refresh failed)` or
    /// `ledger rows (refresh failed)`.
    pub source: String,
    /// All records in the account token listing, including tokens outside Pigeon.
    pub account_total: Option<u32>,
    pub account_total_as_of_ms: Option<i64>,
    pub last_sweep_succeeded_at_ms: Option<i64>,
}

/// The live count: the last list's count plus the rows the ledger minted after it, and never
/// less than the ledger's unexpired rows, including pending and released rows. With no list yet, the rows
/// alone. Pure.
pub fn live_now(last: Option<&SweepGauge>, rows: &[Row], now_ms: i64) -> u32 {
    let floor = rows.iter().filter(|r| r.expires_at > now_ms).count() as u32;
    match last {
        Some(g) => {
            let since = rows
                .iter()
                .filter(|r| r.expires_at > now_ms && r.created_at.is_some_and(|c| c >= g.at_ms))
                .count() as u32;
            (g.count + since).max(floor)
        }
        None => floor,
    }
}

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
    /// When the ledger minted it, in milliseconds since the epoch; `None` for a row from before
    /// the gauge existed.
    pub created_at: Option<i64>,
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
    /// Keeps the setting `key` (`count`, `at_ms` or `ceiling`) as `value`. Nothing by default.
    fn save_setting(&self, _key: &str, _value: i64) -> Result<()> {
        Ok(())
    }
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

/// Whether `e` says the token is already gone.
fn gone(e: &Error) -> bool {
    let t = e.to_string().to_lowercase();
    t.contains(": 404:")
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
    restart_orphans: HashSet<String>,
    /// Lease key to (token id, value). Memory only.
    values: RefCell<HashMap<String, (String, String)>>,
    flights: Flights,
    gauge: RefCell<Option<SweepGauge>>,
    ceiling: Cell<u32>,
    account_total: Cell<Option<AccountCount>>,
    last_sweep: Cell<Option<i64>>,
    store: S,
}

impl<S: Store> Ledger<S> {
    /// A ledger over `store` that starts with `rows` and no values: each row is an orphan until
    /// it is replaced.
    pub fn new(store: S, rows: Vec<Row>) -> Self {
        Self {
            restart_orphans: rows
                .iter()
                .filter(|row| !row.key.is_empty())
                .map(|row| row.id.clone())
                .collect(),
            rows: RefCell::new(rows),
            values: RefCell::default(),
            flights: RefCell::default(),
            gauge: RefCell::new(None),
            ceiling: Cell::new(DEFAULT_SOFT_CEILING),
            account_total: Cell::new(None),
            last_sweep: Cell::new(None),
            store,
        }
    }

    /// Starts with the stored gauge and soft ceiling, as read back after a restart.
    pub fn with_settings(self, gauge: Option<SweepGauge>, ceiling: Option<u32>) -> Self {
        *self.gauge.borrow_mut() = gauge;
        self.ceiling.set(ceiling.unwrap_or(DEFAULT_SOFT_CEILING));
        self
    }

    /// Stores the count the sweep's list showed, and when.
    pub fn gauge_set(&self, count: u32, at_ms: i64) -> Result<()> {
        self.store.save_setting("count", i64::from(count))?;
        self.store.save_setting("at_ms", at_ms)?;
        *self.gauge.borrow_mut() = Some(SweepGauge { count, at_ms });
        Ok(())
    }

    /// Sets the soft ceiling; a value outside [`CEILING_RANGE`] is refused.
    pub fn ceiling_set(&self, value: i64) -> Result<u32> {
        let v = check_ceiling(value).map_err(Error::RustError)?;
        self.store.save_setting("ceiling", i64::from(v))?;
        self.ceiling.set(v);
        Ok(v)
    }

    /// The gauge. A stored count older than [`GAUGE_MAX_AGE_MS`] (or none) is refreshed first
    /// by listing through `api`; when that fails, the stored count is used and `source` says so.
    pub async fn gauge(&self, api: &impl TokenApi, now_ms: i64) -> Result<Gauge> {
        let _slot = self.mint_slot().await;
        let stored = *self.gauge.borrow();
        let stale = stored.is_none_or(|g| now_ms - g.at_ms > GAUGE_MAX_AGE_MS);
        let mut source = "sweep";
        if stale {
            match self.refresh_total(api, now_ms).await {
                Ok(_) => {
                    source = "refreshed";
                }
                Err(_) if stored.is_some() => source = "stored (refresh failed)",
                Err(_) => source = "ledger rows (refresh failed)",
            }
        }
        if self
            .account_total
            .get()
            .is_none_or(|snapshot| !snapshot.fresh(now_ms))
            && !stale
            && self.account_total(api, now_ms).await.is_ok()
        {
            source = "refreshed";
        }
        let last = *self.gauge.borrow();
        let account = self
            .account_total
            .get()
            .filter(|snapshot| snapshot.fresh(now_ms));
        Ok(Gauge {
            live: account.map_or_else(
                || live_now(last.as_ref(), &self.rows.borrow(), now_ms),
                |snapshot| snapshot.pigeon_live,
            ),
            soft_ceiling: self.ceiling.get(),
            hard_cap: HARD_CAP,
            as_of_ms: last.map_or(now_ms, |g| g.at_ms),
            source: source.to_string(),
            account_total: account.map(|snapshot| snapshot.listed),
            account_total_as_of_ms: account.map(|snapshot| snapshot.at_ms),
            last_sweep_succeeded_at_ms: self.last_sweep.get(),
        })
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

    async fn refresh_total(&self, api: &impl TokenApi, now_ms: i64) -> Result<u32> {
        let listed = tokens::list_all(api).await?;
        let names: HashSet<&str> = listed.iter().map(|t| t.name.as_str()).collect();
        let unseen = self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.expires_at > now_ms && !names.contains(r.name.as_str()))
            .count();
        let live = listed
            .iter()
            .filter(|token| tokens::live_pigeon(token, now_ms))
            .count() as u32;
        let snapshot = AccountCount {
            listed: listed.len() as u32,
            reserved: unseen as u32,
            pigeon_live: live + unseen as u32,
            at_ms: now_ms,
        };
        self.gauge_set(live, now_ms)?;
        self.account_total.set(Some(snapshot));
        Ok(snapshot.listed + snapshot.reserved)
    }

    async fn account_total(&self, api: &impl TokenApi, now_ms: i64) -> Result<u32> {
        match self.account_total.get() {
            Some(snapshot) if snapshot.fresh(now_ms) => Ok(snapshot.listed + snapshot.reserved),
            _ => self.refresh_total(api, now_ms).await,
        }
    }

    /// Serialize new leases across keys so concurrent requests cannot spend the same capacity.
    async fn mint_slot(&self) -> Flight<'_> {
        while self.flights.borrow().contains_key(MINT_FLIGHT) {
            Idle {
                flights: &self.flights,
                key: MINT_FLIGHT,
            }
            .await;
        }
        self.flights
            .borrow_mut()
            .insert(MINT_FLIGHT.into(), Vec::new());
        Flight {
            flights: &self.flights,
            key: MINT_FLIGHT.into(),
        }
    }

    /// Persist the intended name before POST. If the response is lost or saving the resulting
    /// ID fails, the sweep can recover the token by its unique name.
    async fn create(
        &self,
        api: &impl TokenApi,
        req: &LeaseRequest<'_>,
        now_ms: i64,
        mut body: Value,
    ) -> Result<Lease> {
        let random = pigeon_core::webauthn::random()?;
        let nonce = &random[..22];
        let original = body["name"].as_str().unwrap_or_default();
        let timestamp = original
            .get(original.len().saturating_sub(20)..)
            .ok_or_else(|| Error::RustError("invalid token mint name".into()))?;
        let name = tokens::name(&format!("{}-{nonce}", req.purpose), timestamp);
        body["name"] = json!(&name);
        let pending = Row {
            id: format!("{PENDING_PREFIX}{nonce}"),
            key: req.key.into(),
            name,
            purpose: req.purpose.into(),
            expires_at: tokens::parse_ms(body["expires_on"].as_str().unwrap_or_default())
                .unwrap_or(now_ms + (req.ttl_secs as i64) * 1000),
            caller: req.caller.into(),
            released: false,
            created_at: Some(now_ms),
        };
        self.store.save(&pending)?;
        self.rows.borrow_mut().push(pending.clone());
        // Invalidate on an ambiguous failure or interruption. A later list sees the result
        // even when no token ID made it back to us.
        let observed = self.account_total.take();
        let minted =
            match api.create(&body).await {
                Ok(minted) => minted,
                Err(e) if tokens::quota_error(&e) => {
                    self.forget(&pending.id)?;
                    return Err(tokens::capacity_error());
                }
                Err(_) => return Err(Error::RustError(
                    "Cloudflare token creation failed; the sweep will reconcile its reservation"
                        .into(),
                )),
            };
        let row = Row {
            id: minted.id.clone(),
            ..pending.clone()
        };
        if let Err(error) = self.store.save(&row) {
            if api.delete(&minted.id).await.is_ok() {
                self.forget(&pending.id)?;
            }
            return Err(error);
        }
        self.rows.borrow_mut().push(row.clone());
        if let Err(error) = self.forget(&pending.id) {
            self.revoke_row(api, &row).await?;
            return Err(error);
        }
        self.account_total.set(observed.map(|mut snapshot| {
            snapshot.reserved += 1;
            snapshot.pigeon_live += 1;
            snapshot
        }));
        if !req.key.is_empty() {
            self.values
                .borrow_mut()
                .insert(req.key.into(), (minted.id.clone(), minted.value.clone()));
        }
        Ok(Lease {
            value: minted.value,
            id: minted.id,
            expires_at: pending.expires_at,
        })
    }

    /// Mints a token for `req`, revokes the orphans of its key and records the new one.
    async fn fresh(
        &self,
        api: &impl TokenApi,
        req: &LeaseRequest<'_>,
        now_ms: i64,
        body: &impl AsyncFn() -> Result<Value>,
    ) -> Result<Lease> {
        let _slot = self.mint_slot().await;
        let in_use = self.values.borrow().get(req.key).map(|(id, _)| id.clone());
        let older: Vec<Row> = self
            .rows()
            .into_iter()
            .filter(|r| {
                r.key == req.key
                    && !r.id.starts_with(PENDING_PREFIX)
                    && (r.released
                        || r.expires_at <= now_ms
                        || self.restart_orphans.contains(&r.id))
                    && Some(&r.id) != in_use.as_ref()
            })
            .collect();
        for old in &older {
            self.revoke_row(api, old).await?;
        }
        let body = body().await.map_err(|_| {
            Error::RustError("could not prepare Cloudflare token permissions".into())
        })?;
        self.mint_locked(api, req, now_ms, body).await
    }

    async fn mint_locked(
        &self,
        api: &impl TokenApi,
        req: &LeaseRequest<'_>,
        now_ms: i64,
        body: Value,
    ) -> Result<Lease> {
        for attempt in 0..2 {
            let total = self
                .account_total(api, now_ms)
                .await
                .map_err(|_| tokens::capacity_error())?;
            if total < self.ceiling.get() {
                match self.create(api, req, now_ms, body.clone()).await {
                    Err(e) if attempt == 0 && e.to_string().contains(tokens::CAPACITY_ERROR) => {}
                    result => return result,
                }
            } else if attempt != 0 {
                break;
            }
            self.sweep_locked(api, now_ms)
                .await
                .map_err(|_| tokens::capacity_error())?;
        }
        Err(tokens::capacity_error())
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
            Ok(()) => {
                self.account_total.set(None);
                self.forget(&row.id)
            }
            Err(e) if gone(&e) => {
                self.account_total.set(None);
                self.forget(&row.id)
            }
            Err(_) => self.mark_released(&row.id),
        }
    }

    /// Drops the row of `id`.
    fn forget(&self, id: &str) -> Result<()> {
        self.store.remove(id)?;
        self.rows.borrow_mut().retain(|r| r.id != id);
        Ok(())
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
        let _slot = self.mint_slot().await;
        let mine: Vec<Row> = self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.caller == caller && !r.id.starts_with(PENDING_PREFIX))
            .cloned()
            .collect();
        for row in &mine {
            self.mark_released(&row.id)?;
            self.revoke_row(api, row).await?;
        }
        Ok(mine.len())
    }

    /// Reconciles abandoned reservations, released and expired tokens, and shared leases
    /// recovered after a restart. It only deletes tokens the ledger can identify as its own.
    pub async fn reap(&self, api: &impl TokenApi, now_ms: i64) -> Result<Reaped> {
        let _slot = self.mint_slot().await;
        self.sweep_locked(api, now_ms).await
    }

    /// Mints a single-use token with the same account capacity and durable cleanup as leases.
    pub async fn mint_transient(
        &self,
        api: &impl TokenApi,
        purpose: &str,
        ttl_secs: u64,
        now_ms: i64,
        body: Value,
    ) -> Result<Lease> {
        let _slot = self.mint_slot().await;
        self.mint_locked(
            api,
            &LeaseRequest {
                key: "",
                caller: "",
                purpose,
                ttl_secs,
            },
            now_ms,
            body,
        )
        .await
    }

    /// Revokes a token issued to an individual call. Failed deletions are left for the sweep.
    pub async fn revoke_transient(&self, api: &impl TokenApi, id: &str) -> Result<()> {
        let _slot = self.mint_slot().await;
        if let Some(row) = self
            .rows()
            .into_iter()
            .find(|row| row.id == id && row.key.is_empty())
        {
            self.mark_released(id)?;
            self.revoke_row(api, &row).await?;
            if self.rows.borrow().iter().any(|row| row.id == id) {
                return Err(Error::RustError(
                    "Cloudflare token revocation failed; the sweep will retry".into(),
                ));
            }
        }
        Ok(())
    }

    async fn sweep_locked(&self, api: &impl TokenApi, now_ms: i64) -> Result<Reaped> {
        self.account_total.set(None);
        let listed = tokens::list_all(api).await?;
        let mut out = Reaped::default();
        for row in self.rows() {
            if row.id.starts_with(PENDING_PREFIX) {
                if let Some(token) = listed.iter().find(|token| token.name == row.name) {
                    // A saved lease with this ID can still be in use.
                    if !self.rows.borrow().iter().any(|known| known.id == token.id) {
                        match api.delete(&token.id).await {
                            Ok(()) => out.deleted += 1,
                            Err(e) if gone(&e) => {}
                            Err(_) => {
                                out.failed += 1;
                                continue;
                            }
                        }
                    }
                } else if row.expires_at > now_ms {
                    // An ambiguous POST can still be in flight on Cloudflare's side.
                    continue;
                }
                self.forget(&row.id)?;
                out.forgotten += 1;
                continue;
            }
            if row.released || row.expires_at <= now_ms || self.restart_orphans.contains(&row.id) {
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
            }
        }
        self.refresh_total(api, now_ms).await?;
        if out.failed == 0 {
            self.store.save_setting("last_sweep_ms", now_ms)?;
            self.last_sweep.set(Some(now_ms));
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
    created_at: Option<i64>,
}

#[derive(Deserialize)]
struct DbSetting {
    k: String,
    v: i64,
}

impl Store for DbStore {
    fn save(&self, r: &Row) -> Result<()> {
        self.0.exec(
            "INSERT OR REPLACE INTO tokens \
             (id, key, name, purpose, expires_at, caller, released, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                r.id.as_str().into(),
                r.key.as_str().into(),
                r.name.as_str().into(),
                r.purpose.as_str().into(),
                r.expires_at.into(),
                r.caller.as_str().into(),
                i64::from(r.released).into(),
                r.created_at
                    .map_or(worker::SqlStorageValue::Null, Into::into),
            ],
        )
    }

    fn save_setting(&self, key: &str, value: i64) -> Result<()> {
        self.0.exec(
            "INSERT OR REPLACE INTO gauge (k, v) VALUES (?, ?)",
            vec![key.into(), value.into()],
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
                created_at: r.created_at,
            })
            .collect();
        let settings = db.rows::<DbSetting>("SELECT k, v FROM gauge", vec![])?;
        let get = |k: &str| settings.iter().find(|s| s.k == k).map(|s| s.v);
        let gauge = get("count")
            .zip(get("at_ms"))
            .map(|(count, at_ms)| SweepGauge {
                count: count.max(0) as u32,
                at_ms,
            });
        let ceiling = get("ceiling").and_then(|v| check_ceiling(v).ok());
        let ledger = Ledger::new(DbStore(db), rows).with_settings(gauge, ceiling);
        ledger.last_sweep.set(get("last_sweep_ms"));
        Ok(Self {
            env,
            parent: parent_binding.to_string(),
            ledger,
        })
    }

    /// Dispatches token lifecycle and status operations.
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
            "mint" | "revoke" => {
                let parent = match a["candidate"].as_str() {
                    Some(candidate) => Parent {
                        token: candidate.to_string(),
                        account: self.env.var("CF_ACCOUNT_ID")?.to_string(),
                    },
                    None => self.parent().await?,
                };
                let api = tokens::CloudflareTokens {
                    parent: &parent.token,
                    account: &parent.account,
                };
                if op == "revoke" {
                    self.ledger.revoke_transient(&api, &s("id")).await?;
                    return Ok(json!({ "ok": true }));
                }
                let body = a["body"].clone();
                let expires_on = body["expires_on"].as_str().unwrap_or_default().to_string();
                let token = self
                    .ledger
                    .mint_transient(
                        &api,
                        &s("purpose"),
                        a["ttl_secs"].as_u64().unwrap_or(tokens::DEFAULT_TTL_SECS),
                        now_ms,
                        body,
                    )
                    .await?;
                Ok(serde_json::to_value(tokens::Minted {
                    id: token.id,
                    value: token.value,
                    expires_on,
                })?)
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
            "ceiling_set" => {
                let v = a["soft_ceiling"].as_i64().unwrap_or(-1);
                Ok(json!({ "soft_ceiling": self.ledger.ceiling_set(v)? }))
            }
            "gauge" => {
                let parent = self.parent().await?;
                let api = tokens::CloudflareTokens {
                    parent: &parent.token,
                    account: &parent.account,
                };
                Ok(serde_json::to_value(
                    self.ledger.gauge(&api, now_ms).await?,
                )?)
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

/// Stores the sweep's count of live pigeon tokens, taken at `at_ms`.
pub async fn gauge(env: &Env) -> Result<Gauge> {
    let v = durable::call(env, BINDING, OBJECT, "gauge", &json!({})).await?;
    Ok(serde_json::from_value(v)?)
}

/// Sets the soft ceiling (10 to 450) and answers it.
pub async fn ceiling_set(env: &Env, soft_ceiling: i64) -> Result<u32> {
    let args = json!({ "soft_ceiling": soft_ceiling });
    let v = durable::call(env, BINDING, OBJECT, "ceiling_set", &args).await?;
    Ok(v["soft_ceiling"].as_u64().unwrap_or(0) as u32)
}

/// The sweeper's call: deletes released tokens and forgets expired rows.
pub async fn reap(env: &Env) -> Result<Reaped> {
    let v = durable::call(env, BINDING, OBJECT, "reap", &json!({})).await?;
    Ok(serde_json::from_value(v)?)
}
