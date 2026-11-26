//! Read-only access to S3-compatible object storage (Scaleway, DigitalOcean Spaces, Backblaze
//! B2) from accounts declared in code. It is off until an account is declared: with none, every
//! tool only says how to set one up. Keys live in the Secrets Store, one secret per account, and
//! are never answered by any tool.

pub mod scope;

use pigeon_core::mcp::Tool;
use pigeon_core::s3::{self, Endpoint, Keys};
use pigeon_core::secrets_store;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use crate::{Call, Capability, Context, Slot, arg, to_value, unknown};

use crate::instances::{
    record::Instance,
    tools::{Enabled, Registration, View},
};

const INSTANCE_TOOLS: &[Registration] = &[
    Registration {
        name: "storage_list_objects",
        description: Some(
            "List objects in the instance’s approved storage location, or with `source: \"data\"` in its data store (`blobs/` and `derived/`). Supply a relative prefix or the continuation token from the preceding page.",
        ),
        arguments: &["prefix", "delimiter", "max_keys", "token", "source"],
    },
    Registration {
        name: "storage_get_object",
        description: Some(
            "Read a small object from the instance’s approved storage location, or with `source: \"data\"` from its data store. Supply a relative key.",
        ),
        arguments: &["key", "source"],
    },
];

const INSTANCE_VIEWS: &[View] = &[View {
    area: "storage",
    id: "buckets",
    title: "Buckets",
    order: 10,
    since: None,
}];

/// Storage declares the subset of its tools, and its console view, on enabled instance profiles:
/// an S3 location (`storage`), the instance's data store (`[capabilities.data]`), or both.
pub fn instance_tools(instance: &Instance) -> serde_json::Result<Enabled> {
    Ok(Enabled::when(
        s3_enabled(instance) || crate::data_store::enabled(instance),
        INSTANCE_TOOLS,
        INSTANCE_VIEWS,
    ))
}

fn s3_enabled(instance: &Instance) -> bool {
    instance
        .storage
        .as_ref()
        .is_some_and(|profile| profile.enabled())
}

/// Where a scoped call reads: the approved S3 location, or the instance's data store
/// (docs/plans/email.md, "Storage reuse").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    S3,
    Data,
}

/// The source a scoped call names (`source`: `s3` or `data`); without one, the S3 location when
/// the record has one, else the data store.
pub fn source_of(instance: &Instance, args: &Value) -> std::result::Result<Source, String> {
    match args.get("source").map(|v| v.as_str()) {
        None => Ok(if s3_enabled(instance) {
            Source::S3
        } else {
            Source::Data
        }),
        Some(Some("s3")) => Ok(Source::S3),
        Some(Some("data")) => Ok(Source::Data),
        _ => Err("`source` is `s3` or `data`".into()),
    }
}

/// The instance a scoped storage call names, once its arguments are only that instance and
/// relative object arguments: a raw `account` or `bucket` never reaches the provider.
pub fn scoped_instance<'a>(tool: &str, args: &'a Value) -> Result<&'a str> {
    let instance = arg(args, "instance")?;
    let allowed: &[&str] = match tool {
        "storage_list_objects" => &[
            "instance",
            "prefix",
            "delimiter",
            "max_keys",
            "token",
            "source",
        ],
        "storage_get_object" => &["instance", "key", "source"],
        _ => return Err(unknown(tool)),
    };
    if !args
        .as_object()
        .is_some_and(|fields| fields.keys().all(|field| allowed.contains(&field.as_str())))
    {
        return Err(Error::RustError(
            "instance storage accepts only relative object arguments".into(),
        ));
    }
    Ok(instance)
}

/// How to set an account up, for the person.
pub const SETUP: &str = "No storage account is set up. To set one up: create a least-privilege \
key in the provider's console (read-only for DigitalOcean). Store it on the app's Access page \
(Credentials) as secret `STORAGE_<PROVIDER>_<NAME>` in the form `ACCESS_KEY_ID:SECRET_ACCESS_KEY`. \
Bind it in src/platform/wrangler.toml. Declare the account in `storage_accounts()` in \
src/platform/src/settings.rs. See docs/plans/storage-providers.md.";

/// The largest object `storage_get_object` answers inline, in bytes.
pub const INLINE_MAX: u64 = 1024 * 1024;

/// An object storage provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Scaleway,
    DigitalOcean,
    Backblaze,
}

impl Provider {
    /// The short lowercase name used in ids and binding names.
    pub fn slug(&self) -> &'static str {
        match self {
            Provider::Scaleway => "scaleway",
            Provider::DigitalOcean => "digitalocean",
            Provider::Backblaze => "backblaze",
        }
    }

    /// Every provider, for declaring their connectors.
    pub const ALL: [Provider; 3] = [
        Provider::Scaleway,
        Provider::DigitalOcean,
        Provider::Backblaze,
    ];
}

/// What an account may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    ReadWrite,
}

/// One storage account, declared in code.
#[derive(Debug, Clone)]
pub struct Account {
    pub provider: Provider,
    /// Such as `personal`.
    pub name: String,
    /// The S3 endpoint, such as `https://s3.fr-par.scw.cloud`, with no trailing slash.
    pub endpoint: String,
    /// The region requests are signed for.
    pub region: String,
    pub mode: Mode,
    /// Empty: any bucket the key can see. Otherwise only these.
    pub buckets: Vec<String>,
}

impl Account {
    /// A Scaleway account in `region`, such as `fr-par`.
    pub fn scaleway(name: &str, region: &str) -> Self {
        Self {
            provider: Provider::Scaleway,
            name: name.into(),
            endpoint: format!("https://s3.{region}.scw.cloud"),
            region: region.into(),
            mode: Mode::ReadWrite,
            buckets: Vec::new(),
        }
    }

    /// A DigitalOcean Spaces account in `region`, such as `nyc3`. Read-only.
    pub fn digitalocean(name: &str, region: &str) -> Self {
        Self {
            provider: Provider::DigitalOcean,
            name: name.into(),
            endpoint: format!("https://{region}.digitaloceanspaces.com"),
            region: region.into(),
            mode: Mode::ReadOnly,
            buckets: Vec::new(),
        }
    }

    /// A Backblaze B2 account in `region`, such as `us-west-004`.
    pub fn backblaze(name: &str, region: &str) -> Self {
        Self {
            provider: Provider::Backblaze,
            name: name.into(),
            endpoint: format!("https://s3.{region}.backblazeb2.com"),
            region: region.into(),
            mode: Mode::ReadWrite,
            buckets: Vec::new(),
        }
    }

    /// The id tools take, such as `scaleway:personal`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.provider.slug(), self.name)
    }

    /// The Secrets Store binding that holds its key, such as `STORAGE_SCALEWAY_PERSONAL`.
    pub fn binding(&self) -> String {
        format!("STORAGE_{}_{}", self.provider.slug(), self.name)
            .to_uppercase()
            .replace('-', "_")
    }

    fn s3_endpoint(&self) -> Endpoint {
        Endpoint {
            base: self.endpoint.clone(),
            region: self.region.clone(),
        }
    }
}

fn slug_chars(s: &str, extra: &[char]) -> bool {
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || extra.contains(&c))
}

fn valid_bucket(bucket: &str) -> bool {
    (3..=63).contains(&bucket.len()) && slug_chars(bucket, &['.'])
}

/// Checks the declared accounts: names, ids, regions, endpoints, buckets, and that DigitalOcean
/// accounts are read-only.
pub fn validate(accounts: &[Account]) -> Result<()> {
    let bad = |a: &Account, why: &str| {
        Err(Error::RustError(format!(
            "storage account `{}`: {why}",
            a.id()
        )))
    };
    let mut seen: Vec<String> = Vec::new();
    for a in accounts {
        let n = &a.name;
        if n.is_empty()
            || n.len() > 32
            || !slug_chars(n, &[])
            || n.starts_with('-')
            || n.ends_with('-')
        {
            return bad(
                a,
                "the name must be 1 to 32 of a-z, 0-9 and `-`, not at either end",
            );
        }
        if seen.contains(&a.id()) {
            return bad(a, "the id is used twice");
        }
        seen.push(a.id());
        if a.region.is_empty() || !slug_chars(&a.region, &[]) {
            return bad(a, "the region must be non-empty, of a-z, 0-9 and `-`");
        }
        if !a.endpoint.starts_with("https://") || a.endpoint.ends_with('/') {
            return bad(
                a,
                "the endpoint must start with https:// and have no trailing `/`",
            );
        }
        if a.provider == Provider::DigitalOcean && a.mode != Mode::ReadOnly {
            return bad(a, "DigitalOcean accounts must be read-only");
        }
        for b in &a.buckets {
            if !valid_bucket(b) {
                return bad(a, "a bucket name must be 3 to 63 of a-z, 0-9, `.` and `-`");
            }
        }
    }
    Ok(())
}

/// The account with id `id`. An unknown id names the known ones, or gives [`SETUP`].
pub fn find<'a>(accounts: &'a [Account], id: &str) -> Result<&'a Account> {
    if accounts.is_empty() {
        return Err(Error::RustError(SETUP.into()));
    }
    accounts.iter().find(|a| a.id() == id).ok_or_else(|| {
        let known: Vec<String> = accounts.iter().map(Account::id).collect();
        Error::RustError(format!(
            "no storage account `{id}`; known: {}",
            known.join(", ")
        ))
    })
}

/// Whether `account` may be used on `bucket`.
pub fn bucket_allowed(account: &Account, bucket: &str) -> bool {
    account.buckets.is_empty() || account.buckets.iter().any(|b| b == bucket)
}

/// The storage capability: read-only tools over the declared accounts.
pub struct Storage {
    pub accounts: Vec<Account>,
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> Tool {
    Tool {
        name,
        description,
        input_schema: json!({ "type": "object", "properties": properties, "required": required }),
    }
}

impl Storage {
    /// The account for `args`, after checking the accounts and the bucket (if the tool has one).
    fn account(&self, args: &Value) -> Result<&Account> {
        validate(&self.accounts)?;
        let account = find(&self.accounts, arg(args, "account")?)?;
        // A bucket goes into the request path as it is, so it must be a plain bucket name.
        if let Some(bucket) = args["bucket"].as_str()
            && !valid_bucket(bucket)
        {
            return Err(Error::RustError(format!(
                "`{bucket}` is not a bucket name: 3 to 63 of a-z, 0-9, `.` and `-`"
            )));
        }
        if let Some(bucket) = args["bucket"].as_str()
            && !bucket_allowed(account, bucket)
        {
            return Err(Error::RustError(format!(
                "bucket `{bucket}` is not allowed for `{}`",
                account.id()
            )));
        }
        Ok(account)
    }

    async fn keys(&self, cx: &Context<'_>, account: &Account) -> Result<Keys> {
        let binding = account.binding();
        let unbound = || {
            Error::RustError(format!(
                "account `{}` is declared but its secret `{binding}` is not bound yet. {SETUP}",
                account.id()
            ))
        };
        let store = cx.env.secret_store(&binding).map_err(|_| unbound())?;
        let value = secrets_store::read(&store)
            .await
            .map_err(|_| unbound())?
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(unbound)?;
        s3::parse_keys(&value)
    }

    async fn accounts_tool(&self, cx: &Context<'_>) -> Result<Value> {
        validate(&self.accounts)?;
        if self.accounts.is_empty() {
            return Ok(json!({ "accounts": [], "setup": SETUP }));
        }
        let mut out = Vec::new();
        for a in &self.accounts {
            let binding = a.binding();
            let configured = match cx.env.secret_store(&binding) {
                Ok(store) => secrets_store::read(&store)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|v| !v.trim().is_empty()),
                Err(_) => false,
            };
            out.push(json!({
                "id": a.id(),
                "provider": a.provider.slug(),
                "mode": match a.mode { Mode::ReadOnly => "read-only", Mode::ReadWrite => "read-write" },
                "endpoint": a.endpoint,
                "region": a.region,
                "buckets": a.buckets,
                "binding": binding,
                "configured": configured,
            }));
        }
        Ok(json!({ "accounts": out }))
    }

    async fn list_buckets(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let account = self.account(args)?;
        let keys = self.keys(cx, account).await?;
        let mut res = s3::send(Method::Get, &account.s3_endpoint(), "/", &[], &[], &keys).await?;
        let mut buckets = s3::parse_list_buckets(&res.text().await?)?;
        if !account.buckets.is_empty() {
            buckets.retain(|b| account.buckets.contains(&b.name));
        }
        Ok(json!({ "buckets": buckets }))
    }

    async fn scoped(&self, cx: &Context<'_>, tool: &str, args: &Value) -> Result<Value> {
        let bad = Error::RustError;
        let instance = scoped_instance(tool, args)?;

        let record = crate::instances::one(cx.env, instance)
            .await?
            .ok_or_else(|| bad("unknown storage instance".into()))?;
        if source_of(&record, args).map_err(bad)? == Source::Data {
            return data_source(cx, &record, tool, args).await;
        }
        let binding = record
            .storage
            .as_ref()
            .ok_or_else(|| bad("object storage is disabled for this instance".into()))?
            .scope()
            .map_err(bad)?;
        validate(&self.accounts)?;
        let account = find(&self.accounts, binding.store)?;
        if !bucket_allowed(account, binding.bucket) {
            return Err(bad(
                "the approved bucket is unavailable in the declared storage account".into(),
            ));
        }
        let keys = self.keys(cx, account).await?;
        let endpoint = account.s3_endpoint();
        if tool == "storage_get_object" {
            let key = binding.key(arg(args, "key")?).map_err(bad)?;
            let path = s3::object_path(binding.bucket, &key);
            return read_scoped_inline(&endpoint, &path, &keys).await;
        }

        let optional = |name: &str| {
            args.get(name)
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| bad(format!("expected a string for `{name}`")))
                })
                .transpose()
        };
        let prefix = binding
            .listing_prefix(optional("prefix")?.unwrap_or_default())
            .map_err(bad)?;
        let delimiter = optional("delimiter")?.filter(|delimiter| !delimiter.is_empty());
        let page = binding.page(
            instance,
            &account.endpoint,
            &account.region,
            &prefix,
            delimiter,
        );
        let token = optional("token")?
            .map(|token| {
                page.decode(keys.secret_access_key.as_bytes(), token)
                    .map_err(bad)
            })
            .transpose()?;
        let max = args["max_keys"].as_u64().unwrap_or(100).clamp(1, 1000) as u32;
        let path = s3::object_path(binding.bucket, "");
        let query = s3::list_objects_query(Some(&prefix), delimiter, max, token.as_deref());
        let mut res = s3::send(Method::Get, &endpoint, &path, &query, &[], &keys).await?;
        let mut listing = s3::parse_list_objects(&res.text().await?)?;
        binding.filter_listing(&prefix, &mut listing);
        listing.next = listing
            .next
            .map(|token| page.encode(keys.secret_access_key.as_bytes(), &token));
        Ok(to_value(listing))
    }

    async fn list_objects(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let account = self.account(args)?;
        let bucket = arg(args, "bucket")?;
        let keys = self.keys(cx, account).await?;
        let max = args["max_keys"].as_u64().unwrap_or(100).min(1000) as u32;
        let query = s3::list_objects_query(
            args["prefix"].as_str(),
            args["delimiter"].as_str(),
            max,
            args["token"].as_str(),
        );
        let path = s3::object_path(bucket, "");
        let mut res = s3::send(
            Method::Get,
            &account.s3_endpoint(),
            &path,
            &query,
            &[],
            &keys,
        )
        .await?;
        Ok(to_value(s3::parse_list_objects(&res.text().await?)?))
    }

    async fn head(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let account = self.account(args)?;
        let path = s3::object_path(arg(args, "bucket")?, arg(args, "key")?);
        let keys = self.keys(cx, account).await?;
        let res = s3::send(Method::Head, &account.s3_endpoint(), &path, &[], &[], &keys).await?;
        head_fields(&res)
    }

    async fn get(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let account = self.account(args)?;
        let path = s3::object_path(arg(args, "bucket")?, arg(args, "key")?);
        let keys = self.keys(cx, account).await?;
        let endpoint = account.s3_endpoint();
        if args["presign"].as_bool().unwrap_or(false) {
            let expires = args["expires_s"].as_u64().unwrap_or(300);
            let expires = u32::try_from(expires)
                .map_err(|_| Error::RustError("`expires_s` must be 1 to 3600".into()))?;
            let date = s3::amz_date(worker::js_sys::Date::now());
            let url = s3::presign_get(&endpoint, &path, &keys, &date, expires)?;
            return Ok(json!({ "url": url, "expires_s": expires }));
        }
        let head = s3::send(Method::Head, &endpoint, &path, &[], &[], &keys).await?;
        let fields = head_fields(&head)?;
        let size = fields["size"].as_u64().unwrap_or(0);
        if size > INLINE_MAX {
            return Err(Error::RustError(format!(
                "the object is {size} bytes, over the {INLINE_MAX} byte inline limit: ask again \
                 with `presign: true`"
            )));
        }
        let mut res = s3::send(Method::Get, &endpoint, &path, &[], &[], &keys).await?;
        let bytes = res.bytes().await?;
        if bytes.len() as u64 > INLINE_MAX {
            return Err(Error::RustError(
                "the object is over the inline limit: ask again with `presign: true`".into(),
            ));
        }
        Ok(inline_fields(&fields, size, bytes))
    }
}

/// A scoped call on the instance's data store, through its data Worker's signed ops: keys are
/// the bucket's own (`blobs/…`, `derived/…`), and nothing else may be listed or read.
async fn data_source(
    cx: &Context<'_>,
    record: &Instance,
    tool: &str,
    args: &Value,
) -> Result<Value> {
    use pigeon_ingest::datastore::DataCall;
    let text = |k: &str| {
        args[k]
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let call = match tool {
        "storage_get_object" => DataCall::Object {
            key: arg(args, "key")?.to_string(),
        },
        _ => DataCall::Objects {
            prefix: text("prefix").unwrap_or_default(),
            cursor: text("token"),
            limit: args["max_keys"].as_u64().map(|n| n.clamp(1, 1000) as u32),
        },
    };
    let mut out = crate::data_store::call(cx.env, record, call).await?;
    out["source"] = json!("data");
    out["store"] = json!(crate::data_store::store_name(record));
    Ok(out)
}

/// Reads a scoped object inline, without creating a reusable link or reading past the limit.
async fn read_scoped_inline(endpoint: &Endpoint, path: &str, keys: &Keys) -> Result<Value> {
    let limit_error = || {
        Error::RustError(format!(
            "the object exceeds the {INLINE_MAX} byte inline limit"
        ))
    };
    let head = s3::send(Method::Head, endpoint, path, &[], &[], keys).await?;
    let fields = head_fields(&head)?;
    if fields["size"].as_u64().unwrap_or(0) > INLINE_MAX {
        return Err(limit_error());
    }
    let mut res = s3::send(Method::Get, endpoint, path, &[], &[], keys).await?;
    // The object may have grown since HEAD. Bound the read as bytes arrive.
    use futures_util::StreamExt;
    let mut stream = res.stream()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if (bytes.len() + chunk.len()) as u64 > INLINE_MAX {
            return Err(limit_error());
        }
        bytes.extend(chunk);
    }
    Ok(inline_fields(&fields, bytes.len() as u64, bytes))
}

fn inline_fields(fields: &Value, size: u64, bytes: Vec<u8>) -> Value {
    let (encoding, body) = match String::from_utf8(bytes) {
        Ok(text) => ("utf-8", text),
        Err(e) => ("base64", pigeon_core::git::base64(e.as_bytes())),
    };
    json!({
        "content_type": fields["content_type"],
        "size": size,
        "encoding": encoding,
        "body": body,
    })
}

/// Size, type, ETag and modified time from a HEAD response's headers.
fn head_fields(res: &worker::Response) -> Result<Value> {
    let h = res.headers();
    let size = h
        .get("content-length")?
        .and_then(|s| s.trim().parse::<u64>().ok());
    Ok(json!({
        "size": size,
        "content_type": h.get("content-type")?,
        "etag": h.get("etag")?.map(|e| e.replace('"', "")),
        "last_modified": h.get("last-modified")?,
    }))
}

impl Capability for Storage {
    fn name(&self) -> &'static str {
        "storage"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        use pigeon_core::versions::{Component, Kind};
        let mut out = vec![Component::began("storage", Kind::Capability)];
        for p in Provider::ALL {
            out.push(Component::began(
                &format!("storage.{}", p.slug()),
                Kind::Connector,
            ));
        }
        out
    }

    fn tools(&self) -> Vec<Tool> {
        let account =
            json!({ "type": "string", "description": "An account id, such as scaleway:personal." });
        let bucket = json!({ "type": "string" });
        let key = json!({ "type": "string" });
        vec![
            Tool::bare(
                "storage_accounts",
                "List the declared object storage accounts and whether each has its key set.",
            ),
            tool(
                "storage_list_buckets",
                "List the buckets an object storage account can see.",
                json!({ "account": account }),
                &["account"],
            ),
            tool(
                "storage_list_objects",
                "List objects in a bucket, one page. Pass `next` back as `token` for the next page.",
                json!({
                    "account": account, "bucket": bucket,
                    "prefix": { "type": "string" }, "delimiter": { "type": "string" },
                    "max_keys": { "type": "integer", "description": "Default 100, at most 1000." },
                    "token": { "type": "string" },
                }),
                &["account", "bucket"],
            ),
            tool(
                "storage_head_object",
                "Answer an object's size, content type, ETag and modified time.",
                json!({ "account": account, "bucket": bucket, "key": key }),
                &["account", "bucket", "key"],
            ),
            tool(
                "storage_get_object",
                "Read an object of at most 1 MiB inline, or with `presign` answer a temporary link.",
                json!({
                    "account": account, "bucket": bucket, "key": key,
                    "presign": { "type": "boolean" },
                    "expires_s": { "type": "integer", "description": "Default 300, at most 3600." },
                }),
                &["account", "bucket", "key"],
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if args.get("instance").is_some() {
                return self.scoped(cx, tool, args).await;
            }
            match tool {
                "storage_accounts" => self.accounts_tool(cx).await,
                "storage_list_buckets" => self.list_buckets(cx, args).await,
                "storage_list_objects" => self.list_objects(cx, args).await,
                "storage_head_object" => self.head(cx, args).await,
                "storage_get_object" => self.get(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        self.accounts
            .iter()
            .map(|a| Slot {
                name: a.binding(),
                purpose: format!(
                    "Reads {} object storage ({}) as `{}`. The key as \
                     `ACCESS_KEY_ID:SECRET_ACCESS_KEY`{}.",
                    a.provider.slug(),
                    a.endpoint,
                    a.id(),
                    if a.mode == Mode::ReadOnly {
                        "; make it a read-only key"
                    } else {
                        ""
                    }
                ),
                scopes: &[secrets_store::WORKERS],
            })
            .collect()
    }

    /// Parses `value` as keys, then asks the provider for one bucket's listing, or the bucket
    /// list, with them.
    fn check_secret<'a>(&'a self, _cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move {
            let account = self
                .accounts
                .iter()
                .find(|a| a.binding() == name)
                .ok_or_else(|| Error::RustError(format!("storage has no secret `{name}`")))?;
            let keys = s3::parse_keys(value)?;
            let endpoint = account.s3_endpoint();
            let refused =
                |e: Error| Error::RustError(format!("the provider refused this key: {e}"));
            if let Some(bucket) = account.buckets.first() {
                let query = s3::list_objects_query(None, None, 1, None);
                let path = s3::object_path(bucket, "");
                s3::send(Method::Get, &endpoint, &path, &query, &[], &keys)
                    .await
                    .map_err(refused)?;
                Ok(Value::String(format!("the key can list bucket `{bucket}`")))
            } else {
                let mut res = s3::send(Method::Get, &endpoint, "/", &[], &[], &keys)
                    .await
                    .map_err(refused)?;
                let n = s3::parse_list_buckets(&res.text().await?)?.len();
                Ok(Value::String(format!("the key can see {n} buckets")))
            }
        })
    }
}
