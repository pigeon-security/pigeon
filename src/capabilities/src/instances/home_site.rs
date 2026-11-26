//! The home instance's publishing (docs/plans/platform-self-hosting.md, L2–L3): `site_deploy`,
//! `site_releases` and `site_rollback` for the record with `account: home`, served by core rather
//! than pigeon-sites, because the bucket is written with the `instance_home` token and only core
//! may hold it ([`super::home`]).
//!
//! The source is the record's Artifacts repository, read at a resolved revision under the
//! site's `build` directory (the content root; the repository root when unset) within
//! [`LIMITS`] (`pigeon_core::pack::read_tree`); no GitHub App and no linked account. The site is
//! rendered by pigeon-sites, which carries the templates and declares their versions, with the
//! `landing@1` template alone ([`BUILD_TOOL`], a pure call holding no credential), and staged
//! into the site's bucket as an immutable release with the release code linked instances use
//! ([`pigeon_core::site_publish`]): the blobs it lacks, the manifest, the provenance, then
//! `live`. Every bucket read and write runs inside [`super::home::with_token`]: one token minted
//! for the call and revoked after it, whether it succeeded or not.
//!
//! Nothing platform-owned is touched: no Worker is uploaded and no domain attached. Who serves
//! the release on the apex is phase S1's.

use pigeon_core::Account;
use pigeon_core::pack::TreeLimits;
use pigeon_core::r2::{Bucket, ObjectWrite};
use pigeon_core::site_publish::{self, Owner, Staged, Store};
use serde_json::{Value, json};
use worker::{Error, Result};

use super::home::{self, Minter};
use super::record::{AccountRef, Instance, LANDING_TEMPLATE, RepoRef};

/// How much of the content root a deploy reads: at most 256 files, 8 MiB in all, 8 directories
/// deep, and 512-byte paths. Anything past it is refused and nothing is written.
pub const LIMITS: TreeLimits = TreeLimits {
    max_files: 256,
    max_bytes: 8 * 1024 * 1024,
    max_depth: 8,
    max_path: 512,
};

/// The publishing tools core serves itself for the home instance.
pub const TOOLS: [&str; 3] = ["site_deploy", "site_releases", "site_rollback"];

/// Whether core serves `tool` of remote capability `capability` itself, for an instance that is
/// (`home`) or is not the home instance: only the publishing tools, and only for home. Every
/// other call, every linked instance's among them, goes to its Worker as before. Pure.
pub fn routed(capability: &str, tool: &str, home: bool) -> bool {
    home && capability == super::publishing::CAPABILITY && TOOLS.contains(&tool)
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// What the home instance publishes, from its approved record.
#[derive(Clone, Debug)]
pub struct Target {
    /// The site the `publishing` profile names.
    pub site: String,
    pub bucket: Bucket,
    /// Where the site is canonically served, such as `https://pigeon.dev`.
    pub base_url: String,
    /// The Artifacts repository.
    pub namespace: String,
    pub repo: String,
    /// The content root in the repository (`sites[].build`); `None` is the repository root.
    pub root: Option<String>,
}

/// Reads [`Target`] from the home instance's approved record: Artifacts, an enabled
/// `publishing` profile naming a public `landing@1` site with a hostname. Pure.
pub fn target(record: &Instance, platform_zone: &str) -> std::result::Result<Target, String> {
    let id = &record.id;
    if record.account != AccountRef::Home {
        return Err(format!("instance {id} is not the home instance"));
    }
    let Some(RepoRef::Artifacts { namespace, repo }) = &record.repo else {
        return Err(format!(
            "the home instance {id} publishes from Artifacts, and its repository is not there"
        ));
    };
    let value = serde_json::to_value(record).map_err(|e| e.to_string())?;
    let site = site_publish::site(&value)?;
    let name = site["name"]
        .as_str()
        .ok_or_else(|| format!("instance {id}'s site has no name"))?;
    let named = record
        .sites
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("instance {id} has no site {name}"))?;
    if !named.landing() {
        return Err(format!(
            "the home instance publishes only {LANDING_TEMPLATE}, and site {name} does not name it"
        ));
    }
    if named.auth.is_some() {
        return Err(format!(
            "site {name} of the home instance is protected; the home instance publishes public sites only"
        ));
    }
    let base_url = site_publish::base_url(site, platform_zone)
        .ok_or_else(|| format!("instance {id}'s site has no hostname to be served at"))?;
    Ok(Target {
        site: name.to_string(),
        bucket: Bucket {
            name: named.bucket.clone(),
            jurisdiction: named.jurisdiction.clone(),
        },
        base_url,
        namespace: namespace.clone(),
        repo: repo.clone(),
        root: named.build.clone(),
    })
}

/// A rendered release, ready to stage.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    /// Site path, bytes and content type: the template's files, then `static/`'s.
    pub files: Vec<(String, Vec<u8>, String)>,
    pub template: String,
    pub version: String,
    pub template_digest: String,
    pub renderer_digest: String,
}

/// What pigeon-sites made of the content.
#[derive(Clone, Debug, PartialEq)]
pub enum Build {
    Ready(Built),
    /// What is wrong with the content, for the person to fix. Nothing is written.
    Problems(Vec<String>),
}

/// The private call pigeon-sites renders a home release for, never in its manifest.
pub const BUILD_TOOL: &str = "site_build";

/// The arguments of [`BUILD_TOOL`]: the content root's `files` (paths relative to it) at
/// `commit`, and the record's `landing` pin. Pure.
pub fn build_request(
    instance: &str,
    files: &[(String, Vec<u8>)],
    commit: &str,
    landing_pin: Option<&str>,
) -> Value {
    json!({
        "instance": instance,
        "commit": commit,
        "pin": landing_pin,
        "files": files
            .iter()
            .map(|(p, b)| json!([p, pigeon_core::webauthn::b64url(b)]))
            .collect::<Vec<_>>(),
    })
}

/// Reads pigeon-sites' answer to [`BUILD_TOOL`]. Only `landing@1` is ever published, and
/// every path must be a site path as the release manifest takes it. Pure.
pub fn built_from(answer: &Value) -> std::result::Result<Build, String> {
    if answer["built"] == false {
        let problems = answer["problems"]
            .as_array()
            .ok_or("pigeon-sites named no problems")?
            .iter()
            .map(|p| p.as_str().unwrap_or("?").to_string())
            .collect();
        return Ok(Build::Problems(problems));
    }
    if answer["built"] != true {
        return Err("pigeon-sites did not answer a build".into());
    }
    let text = |k: &str| {
        answer[k]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("pigeon-sites' build has no {k}"))
    };
    let template = text("template")?;
    if template != LANDING_TEMPLATE {
        return Ok(Build::Problems(vec![format!(
            ".pigeon/site.toml: the home instance publishes only {LANDING_TEMPLATE}, not {template}"
        )]));
    }
    let mut files = Vec::new();
    for f in answer["files"]
        .as_array()
        .ok_or("pigeon-sites' build has no files")?
    {
        let (Some(path), Some(body), Some(ctype)) = (f[0].as_str(), f[1].as_str(), f[2].as_str())
        else {
            return Err("pigeon-sites' build has a malformed file".into());
        };
        if path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(format!("pigeon-sites' build names a bad path `{path}`"));
        }
        let bytes = pigeon_core::webauthn::unb64url(body).map_err(|e| e.to_string())?;
        files.push((path.to_string(), bytes, ctype.to_string()));
    }
    Ok(Build::Ready(Built {
        files,
        template,
        version: text("template_version")?,
        template_digest: text("template_digest")?,
        renderer_digest: text("renderer_digest")?,
    }))
}

/// A release's provenance, in the shape linked releases have, so one listing and one
/// rollback read both. Pure.
pub fn provenance(instance: &str, target: &Target, commit: &str, built: &Built) -> Value {
    json!({
        "commit": commit,
        "instance": instance,
        "site": target.site,
        "template": built.template,
        "template_version": built.version,
        "template_digest": built.template_digest,
        "renderer_digest": built.renderer_digest,
        "base_url": target.base_url,
    })
}

/// How the work reaches the home token: what mints it, what it may hold, and the zone and
/// account it is scoped to.
pub struct Token<'a, M> {
    pub minter: &'a M,
    pub permissions: Option<&'a [&'a str]>,
    pub zone_id: &'a str,
    pub account_id: &'a str,
    pub secrets_store: &'a str,
}

impl<M: Minter> Token<'_, M> {
    /// Runs `work` on the store `open` makes from the account holding a freshly minted
    /// `instance_home` token, then revokes the token, whether `work` succeeded or not.
    async fn with<S, O, F, Fut, T>(&self, open: O, work: F) -> Result<T>
    where
        S: Store,
        O: FnOnce(Account) -> S,
        F: FnOnce(S) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        home::with_token(
            self.minter,
            self.permissions,
            self.zone_id,
            self.account_id,
            self.secrets_store,
            |account| work(open(account)),
        )
        .await
    }
}

/// Stages `files` as the release `provenance` describes and makes it live, in the store `open`
/// makes with the home token; `still_current` is asked, after staging and before `live` moves,
/// whether the approved record is still the one the deploy read.
pub async fn release<M, S, O, C, CF>(
    token: &Token<'_, M>,
    open: O,
    provenance: &Value,
    files: Vec<(String, Vec<u8>, String)>,
    still_current: C,
) -> Result<Staged>
where
    M: Minter,
    S: Store,
    O: FnOnce(Account) -> S,
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = Result<()>>,
{
    token
        .with(open, |store| async move {
            let staged = site_publish::stage(&store, provenance, files, &mut |_| {}).await?;
            still_current().await?;
            site_publish::activate(&store, &staged.release).await?;
            Ok(staged)
        })
        .await
}

/// Makes staged release `release` of `owner` live again, in the store `open` makes with the
/// home token: its provenance, once checked.
pub async fn rollback<M, S, O, C, CF>(
    token: &Token<'_, M>,
    open: O,
    release: &str,
    owner: &Owner<'_>,
    still_current: C,
) -> Result<Value>
where
    M: Minter,
    S: Store,
    O: FnOnce(Account) -> S,
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = Result<()>>,
{
    if !site_publish::is_release_id(release) {
        return Err(bad("expected a site release ID"));
    }
    token
        .with(open, |store| async move {
            let provenance = match site_publish::staged(&store, release, owner).await {
                // R1: a release staged before the rename names the old id.
                Err(e) => match super::rename::renamed_from(owner.instance) {
                    Some(old) => {
                        let before = Owner {
                            instance: old,
                            ..*owner
                        };
                        site_publish::staged(&store, release, &before)
                            .await
                            .map_err(|_| e)?
                    }
                    None => return Err(e),
                },
                Ok(p) => p,
            };
            still_current().await?;
            site_publish::activate(&store, release).await?;
            Ok(provenance)
        })
        .await
}

/// The releases of `site` in the store `open` makes with the home token. Reads only.
pub async fn releases<M, S, O>(
    token: &Token<'_, M>,
    open: O,
    instance: &str,
    site: &str,
    limit: usize,
) -> Result<Value>
where
    M: Minter,
    S: Store,
    O: FnOnce(Account) -> S,
{
    token
        .with(open, |store| async move {
            site_publish::releases(&store, instance, site, limit).await
        })
        .await
}

/// The site bucket, with the account holding the home token.
struct HomeBucket {
    account: Account,
    bucket: Bucket,
}

impl HomeBucket {
    fn r2(&self) -> site_publish::R2<'_> {
        site_publish::R2 {
            account: &self.account,
            bucket: &self.bucket,
        }
    }
}

impl Store for HomeBucket {
    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        self.r2().list(prefix).await
    }
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.r2().get(key).await
    }
    async fn put(&self, key: &str, content_type: &str, body: Vec<u8>) -> Result<()> {
        self.r2().put(key, content_type, body).await
    }
    async fn put_objects(&self, objects: Vec<ObjectWrite>) -> Result<()> {
        self.r2().put_objects(objects).await
    }
}

/// Whether `rev` is a commit or branch as `site_deploy` takes it. Pure.
fn revision_ok(rev: &str) -> bool {
    !rev.is_empty()
        && rev
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
}

/// Serves `tool` (one of [`TOOLS`]) for the home instance `id`, after the caller's policy gate
/// (and passkey, for deploy and rollback) passed exactly as for a linked instance. `zone` is the
/// platform zone.
pub async fn call(
    cx: &crate::Context<'_>,
    zone: &str,
    tool: &str,
    id: &str,
    args: &Value,
) -> Result<Value> {
    let read = || async {
        crate::instances::one(cx.env, id)
            .await?
            .ok_or_else(|| bad("unknown publishing instance"))
    };
    let record = read().await?;
    let t = target(&record, zone).map_err(bad)?;
    let still_current = || async {
        if read().await? != record {
            return Err(bad(
                "the approved publishing record changed; retry with the current configuration",
            ));
        }
        Ok(())
    };
    let zone_id = pigeon_core::zones::active(cx.account, zone)
        .await
        .map(|z| z.id)
        .unwrap_or_default();
    let token = Token {
        minter: cx.account,
        permissions: crate::split::purpose_permissions(home::PURPOSE),
        zone_id: &zone_id,
        account_id: &cx.account.id,
        secrets_store: &cx.account.secrets_store,
    };
    let bucket = t.bucket.clone();
    let open = move |account| HomeBucket { account, bucket };
    match tool {
        "site_releases" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            releases(&token, open, id, &t.site, limit).await
        }
        "site_rollback" => {
            let release = crate::arg(args, "release")?;
            let owner = Owner {
                instance: id,
                base_url: &t.base_url,
                site: &t.site,
            };
            let provenance = rollback(&token, open, release, &owner, still_current).await?;
            Ok(
                json!({"instance": id, "release": release, "commit": provenance["commit"], "deployed": true}),
            )
        }
        "site_deploy" => {
            let rev = args["rev"].as_str().unwrap_or("main");
            if !revision_ok(rev) {
                return Err(bad("`rev` must be a commit or branch"));
            }
            let (commit, files) = pigeon_core::artifacts::read_tree(
                cx.account,
                &t.namespace,
                &t.repo,
                rev,
                t.root.as_deref(),
                &LIMITS,
            )
            .await?;
            let request = build_request(
                id,
                &files,
                &commit,
                record.pins.get("landing").map(String::as_str),
            );
            drop(files);
            let caller = serde_json::to_value(cx.caller).unwrap_or(Value::Null);
            let answer = crate::remote::call_owned_instance(
                cx.env.clone(),
                &crate::github::SITES_BINDING,
                super::publishing::CAPABILITY,
                BUILD_TOOL,
                request,
                caller,
                &record,
            )
            .await?;
            let built = match built_from(&answer).map_err(bad)? {
                Build::Ready(built) => built,
                Build::Problems(problems) => {
                    return Ok(
                        json!({ "commit": commit, "deployed": false, "problems": problems }),
                    );
                }
            };
            let provenance = provenance(id, &t, &commit, &built);
            let staged = release(&token, open, &provenance, built.files, still_current).await?;
            Ok(json!({
                "instance": id,
                "commit": commit,
                "deployed": true,
                "files": staged.files,
                "uploaded": staged.uploaded,
                "release": staged.release,
                "read_with": "Artifacts",
                "template": built.template,
                "template_version": built.version,
                "template_digest": built.template_digest,
                "pins": json!(record.pins),
            }))
        }
        other => Err(bad(format!("no home publishing tool `{other}`"))),
    }
}
