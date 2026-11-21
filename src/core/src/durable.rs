//! Durable Objects with SQLite storage, made easy to use: an object opens a [`Db`] with its
//! migrations and answers JSON operations with [`serve`]; a Worker reaches it with [`call`].
//!
//! An object is declared in the hosting Worker's `wrangler.toml`: a binding, and a migration
//! naming its class in `new_sqlite_classes`.

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use worker::{
    Env, Error, Method, Request, RequestInit, Response, Result, SqlStorage, SqlStorageValue, State,
};

/// A SQLite database inside a Durable Object.
pub struct Db {
    sql: SqlStorage,
}

impl Db {
    /// The object's database, with `migrations` applied in order: each runs once, ever, and
    /// the number applied is kept in the database itself.
    pub fn open(state: &State, migrations: &[&str]) -> Result<Self> {
        let db = Self {
            sql: state.storage().sql(),
        };
        db.sql.exec(
            "CREATE TABLE IF NOT EXISTS _migrations (version INTEGER NOT NULL)",
            None,
        )?;
        #[derive(serde::Deserialize)]
        struct Applied {
            version: i64,
        }
        let applied = db
            .row::<Applied>("SELECT version FROM _migrations LIMIT 1", vec![])?
            .map_or(0, |a| a.version);
        for (i, m) in migrations.iter().enumerate() {
            let version = i as i64 + 1;
            if version <= applied {
                continue;
            }
            db.sql.exec(m, None)?;
            db.sql.exec("DELETE FROM _migrations", None)?;
            db.sql.exec(
                "INSERT INTO _migrations (version) VALUES (?)",
                vec![version.into()],
            )?;
        }
        Ok(db)
    }

    /// Runs `query` with `args` bound to its `?`s.
    pub fn exec(&self, query: &str, args: Vec<SqlStorageValue>) -> Result<()> {
        self.sql.exec(query, args)?;
        Ok(())
    }

    /// The rows `query` answers, each read into `T` by column name.
    pub fn rows<T: DeserializeOwned>(
        &self,
        query: &str,
        args: Vec<SqlStorageValue>,
    ) -> Result<Vec<T>> {
        self.sql.exec(query, args)?.to_array()
    }

    /// The first row `query` answers, if any.
    pub fn row<T: DeserializeOwned>(
        &self,
        query: &str,
        args: Vec<SqlStorageValue>,
    ) -> Result<Option<T>> {
        Ok(self.rows(query, args)?.into_iter().next())
    }
}

/// `v` as a SQL value: `None` is NULL.
pub fn opt(v: Option<&str>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, Into::into)
}

/// Inside an object's `fetch`: answers the operation a [`call`] sent, by its name and JSON
/// arguments; an error goes back as the caller's error.
pub async fn serve(
    mut req: Request,
    handle: impl AsyncFnOnce(&str, Value) -> Result<Value>,
) -> Result<Response> {
    let op = req.path().trim_start_matches('/').to_string();
    let args: Value = req.json().await.unwrap_or(Value::Null);
    match handle(&op, args).await {
        Ok(v) => Response::from_json(&json!({ "ok": v })),
        Err(e) => Response::from_json(&json!({ "error": e.to_string() })),
    }
}

/// Calls operation `op` with `args` on object `name` of namespace `binding`.
pub async fn call(env: &Env, binding: &str, name: &str, op: &str, args: &Value) -> Result<Value> {
    let stub = env.durable_object(binding)?.get_by_name(name)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_body(Some(args.to_string().into()));
    let req = Request::new_with_init(&format!("https://durable/{op}"), &init)?;
    let mut res = stub.fetch_with_request(req).await?;
    let v: Value = res.json().await?;
    match v.get("error").and_then(Value::as_str) {
        Some(e) => Err(Error::RustError(format!("{binding}/{name} {op}: {e}"))),
        None => Ok(v["ok"].clone()),
    }
}
