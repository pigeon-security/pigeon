//! `site_deploy`: an instance's site, deployed from its GitHub repository at a commit. What it
//! must not hold itself comes from core, as the caller it was called for: the instance's
//! approved record, an installation token for the repository, and the linked account's token.
//! Every call is signed with instance context ([`crate::context`] refused any other before this
//! module runs), and gets each through core's `sites_scope` callback ([`SitesScopeRequest`]),
//! which answers only for that instance and, for credentials, only while the record still
//! hashes to the binding the deploy read. There is no other path: the legacy platform callbacks
//! (core's `dispatch` `instances_list` and `credential` kinds `linked` and `github`) are retired.
//! Only a record whose `publishing` profile is enabled deploys.
//! The repository is read as one tarball ([`pigeon_core::tarball`]), sorted
//! ([`pigeon_blog::repo`]), rendered ([`pigeon_blog`]), and written to the profile's site bucket
//! as an immutable release in the layout the site Worker serves: the blobs it lacks, then
//! `deploys/{release}.json` and `release-meta/{release}.json`, then `live`. It only adds objects
//! and moves `live`, so the previous release stays whole until the last write.

use std::collections::BTreeSet;

use pigeon_contract::{Answer, Signed, SitesScopeRequest, WorkerContext};
use pigeon_core::r2::{self, Bucket, Manifest};
use pigeon_core::versions::{Requirement, Version};
use pigeon_core::{Account, site_publish, tarball};
use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::queued::Receipt;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// A scoped Sites call may only request authority for its authenticated instance.
async fn scoped_core(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    request: &SitesScopeRequest,
) -> Result<Value> {
    let instance = request.instance();
    caller
        .require_instance(instance)
        .map_err(|why| bad(why.to_string()))?;
    let context = WorkerContext::new("pigeon-platform", Some(instance));
    let args = serde_json::to_value(request)?;
    let nonce = pigeon_core::webauthn::random()?;
    let platform = env.service("PLATFORM")?;
    match pigeon_contract::ask_with_context(
        &platform,
        "https://core",
        "sites_scope",
        &args,
        (key, &caller.caller),
        &nonce,
        &context,
    )
    .await?
    {
        Answer::Ok(value) => Ok(value),
        Answer::Error(failure) => Err(bad(format!("core's sites_scope: {}", failure.message))),
    }
}

/// What a deploy needs from an instance's record.
pub struct Target {
    pub repo: String,
    pub bucket: Bucket,
    pub base_url: String,
    pub site: String,
    pub account_id: String,
    pub protected: bool,
}

/// Reads [`Target`] from an approved record, as core's `sites_scope` answers it: the site its `publishing` profile names. Pure.
pub fn target(record: &Value, platform_zone: &str) -> std::result::Result<Target, String> {
    let id = record["id"].as_str().unwrap_or("?");
    if record["repo"]["kind"] != "github" {
        return Err(format!("instance {id}'s repository is not on GitHub"));
    }
    let repo = record["repo"]["full_name"]
        .as_str()
        .ok_or_else(|| format!("instance {id} names no repository"))?
        .to_string();
    let site = site_publish::site(record)?;
    let account_id = record["account"]["account_id"]
        .as_str()
        .filter(|_| record["account"]["kind"] == "linked")
        .ok_or_else(|| format!("instance {id} has no linked account"))?;
    let bucket = site["bucket"]
        .as_str()
        .ok_or_else(|| format!("instance {id} has no site"))?;
    let base_url = site_publish::base_url(site, platform_zone)
        .ok_or_else(|| format!("instance {id}'s site has no hostname to be served at"))?;
    Ok(Target {
        repo,
        account_id: account_id.into(),
        protected: site["auth"] == "passkey",
        bucket: Bucket {
            name: bucket.to_string(),
            jurisdiction: site["jurisdiction"].as_str().map(str::to_string),
        },
        base_url,
        site: site["name"]
            .as_str()
            .ok_or_else(|| format!("instance {id}'s site has no name"))?
            .into(),
    })
}

/// The pins a record carries that this Worker acts on.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pins {
    /// The requirement on `sites`, if set.
    pub sites: Option<String>,
    /// The requirement on `blog`, if set.
    pub blog: Option<String>,
}

/// Reads [`Pins`] from an approved record, as core's `sites_scope` answers it. Pure.
pub fn pins(record: &Value) -> Pins {
    let get = |k: &str| record["pins"][k].as_str().map(str::to_string);
    Pins {
        sites: get("sites"),
        blog: get("blog"),
    }
}

/// Refuses when the `sites` pin does not allow the running version. Pure.
pub fn check_sites_pin(pin: Option<&str>, running: &str) -> std::result::Result<(), String> {
    let Some(pin) = pin else {
        return Ok(());
    };
    let req = Requirement::parse(pin).map_err(|e| format!("the sites pin `{pin}`: {e}"))?;
    let version = Version::parse(running).map_err(|e| format!("running sites {running}: {e}"))?;
    if req.matches(&version) {
        Ok(())
    } else {
        Err(format!(
            "the instance pins sites to `{pin}`, but the running sites is {running}; plan the move with capabilities_upgrade_plan"
        ))
    }
}

/// Parses the `blog` pin into a requirement. Pure.
pub fn blog_requirement(pin: Option<&str>) -> std::result::Result<Option<Requirement>, String> {
    pin.map(|p| Requirement::parse(p).map_err(|e| format!("the blog pin `{p}`: {e}")))
        .transpose()
}

/// Whether the requirement, when there is one, allows the template version `v`. Pure.
pub fn allows(req: Option<&Requirement>, v: &str) -> bool {
    req.is_none_or(|r| Version::parse(v).is_ok_and(|v| r.matches(&v)))
}

async fn approved_record(env: &Env, key: &[u8], caller: &Signed, instance: &str) -> Result<Value> {
    let record = scoped_core(
        env,
        key,
        caller,
        &SitesScopeRequest::Record {
            instance: instance.to_owned(),
        },
    )
    .await?;
    if record["id"].as_str() != Some(instance) {
        return Err(bad("core returned a different approved publishing record"));
    }
    Ok(record)
}

async fn ensure_current(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    expected: &Value,
) -> Result<()> {
    if approved_record(env, key, caller, instance).await? != *expected {
        return Err(bad(
            "the approved publishing record changed; retry with the current configuration",
        ));
    }
    Ok(())
}

async fn linked_account(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    target: &Target,
    record: &Value,
) -> Result<Account> {
    let linked = scoped_core(
        env,
        key,
        caller,
        &SitesScopeRequest::Linked {
            instance: instance.to_owned(),
            binding: pigeon_core::oauth::hash(&record.to_string()),
        },
    )
    .await?;
    let account = Account {
        id: linked["account_id"]
            .as_str()
            .ok_or_else(|| bad("core's linked credential has no account"))?
            .to_string(),
        parent: String::new(),
        secrets_store: String::new(),
        bearer: Some(
            linked["token"]
                .as_str()
                .ok_or_else(|| bad("core's linked credential has no token"))?
                .to_string(),
        ),
        token_ledger: None,
    };
    if account.id != target.account_id {
        return Err(bad("linked credential belongs to a different account"));
    }
    Ok(account)
}

/// Reactivates a previously staged release within the same approved publishing target.
pub async fn rollback(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    release: &str,
) -> Result<Value> {
    if !site_publish::is_release_id(release) {
        return Err(bad("expected a site release ID"));
    }
    let record = approved_record(env, key, caller, instance).await?;
    let target = target(&record, &env.var("ZONE")?.to_string()).map_err(bad)?;
    let account = linked_account(env, key, caller, instance, &target, &record).await?;
    let provenance = r2::get(
        &account,
        &target.bucket,
        &site_publish::release_key(release),
    )
    .await?
    .ok_or_else(|| bad("unknown site release"))?;
    let provenance: Value = serde_json::from_slice(&provenance)?;
    if provenance["instance"] != instance
        || provenance["base_url"] != target.base_url
        || provenance["site"] != target.site
    {
        return Err(bad("release belongs to a different publishing target"));
    }
    let manifest = r2::get(&account, &target.bucket, &r2::manifest_key(release))
        .await?
        .ok_or_else(|| bad("site release is missing its manifest"))?;
    let manifest: Manifest = serde_json::from_slice(&manifest)?;
    if site_publish::release_id(&provenance, &manifest) != release {
        return Err(bad("site release manifest does not match its identity"));
    }
    ensure_current(env, key, caller, instance, &record).await?;
    if target.protected {
        crate::public::require_protected_origin(
            env,
            key,
            &target.base_url,
            instance,
            &target.site,
            &target.bucket.name,
        )
        .await?;
        r2::require_private_bucket(&account, &target.bucket).await?;
    }
    r2::put(
        &account,
        &target.bucket,
        r2::LIVE,
        "text/plain",
        release.as_bytes().to_vec(),
    )
    .await?;
    Ok(
        json!({"instance": instance, "release": release, "commit": provenance["commit"], "deployed": true}),
    )
}

/// The most releases [`releases`] answers.
pub const MAX_RELEASES: usize = 50;

/// The immutable releases of an instance's site and the one `live` points at: each release's
/// provenance (commit, template, base URL), live first, then by id. Reads only.
pub async fn releases(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    limit: usize,
) -> Result<Value> {
    let record = approved_record(env, key, caller, instance).await?;
    let target = target(&record, &env.var("ZONE")?.to_string()).map_err(bad)?;
    let account = linked_account(env, key, caller, instance, &target, &record).await?;
    let live = r2::get(&account, &target.bucket, r2::LIVE)
        .await?
        .map(|b| String::from_utf8_lossy(&b).trim().to_string());
    let mut ids: Vec<String> = r2::list(&account, &target.bucket, "release-meta/")
        .await?
        .iter()
        .filter_map(|k| {
            k.strip_prefix("release-meta/")?
                .strip_suffix(".json")
                .filter(|id| site_publish::is_release_id(id))
                .map(str::to_string)
        })
        .collect();
    let total = ids.len();
    ids.sort_by_key(|id| (Some(id) != live.as_ref(), id.clone()));
    ids.truncate(limit.clamp(1, MAX_RELEASES));
    let mut out = Vec::new();
    for id in ids {
        let provenance: Value =
            match r2::get(&account, &target.bucket, &site_publish::release_key(&id)).await? {
                Some(b) => serde_json::from_slice(&b).unwrap_or(Value::Null),
                None => Value::Null,
            };
        out.push(json!({
            "release": id,
            "live": Some(&id) == live.as_ref(),
            "commit": provenance["commit"],
            "template": provenance["template"],
            "template_version": provenance["template_version"],
            "base_url": provenance["base_url"],
        }));
    }
    Ok(
        json!({ "instance": instance, "site": target.site, "live": live, "total": total, "releases": out }),
    )
}

fn publish_stage(started: &mut u64, stage: &str) {
    let now = worker::Date::now().as_millis();
    worker::console_log!(
        "site_deploy stage={stage} elapsed_ms={}",
        now.saturating_sub(*started)
    );
    *started = now;
}

/// Deploys `instance` at `rev`: the public, manual `site_deploy`.
pub async fn deploy(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    rev: &str,
) -> Result<Value> {
    publish(env, key, caller, instance, rev, None).await
}

/// Deploys a push-started publish from core's private receipt: the approved record must still
/// have the queued digest, and GitHub's archive must be the queued commit.
pub async fn deploy_queued(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    receipt: &Receipt<'_>,
) -> Result<Value> {
    let (instance, rev) = (receipt.instance(), receipt.revision());
    publish(env, key, caller, instance, rev, Some(receipt)).await
}

async fn publish(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    rev: &str,
    receipt: Option<&Receipt<'_>>,
) -> Result<Value> {
    let mut started = worker::Date::now().as_millis();
    let record = approved_record(env, key, caller, instance).await?;
    let record = &record;
    if let Some(receipt) = receipt {
        receipt.check_record(record).map_err(bad)?;
    }
    let pins = pins(record);
    check_sites_pin(
        pins.sites.as_deref(),
        &crate::versions::sites_component().version,
    )
    .map_err(bad)?;
    let blog_pin = blog_requirement(pins.blog.as_deref()).map_err(bad)?;
    let t = target(record, &env.var("ZONE")?.to_string()).map_err(bad)?;

    let credential = scoped_core(
        env,
        key,
        caller,
        &SitesScopeRequest::Github {
            instance: instance.to_owned(),
            binding: pigeon_core::oauth::hash(&record.to_string()),
        },
    )
    .await?;
    let gh = if credential.is_null() {
        // Core explicitly resolved an approved record with no GitHub installation.
        None
    } else {
        let token = credential["token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| bad("core returned a malformed GitHub credential"))?;
        Some(token.to_string())
    };
    publish_stage(&mut started, "source_grant");
    let gh_token = gh.as_deref();
    let gz = pigeon_core::repository::github_archive(&t.repo, rev, gh_token).await?;
    publish_stage(&mut started, "archive_fetch");
    let archive = tarball::read(&gz).map_err(bad)?;
    drop(gz);
    let commit = archive
        .commit
        .clone()
        .ok_or_else(|| bad("GitHub's archive does not name its commit"))?;
    if let Some(receipt) = receipt {
        receipt.check_commit(&commit).map_err(bad)?;
    }
    let year = tarball::year_of(archive.mtime);
    let s = pigeon_blog::repo::split(archive.files).map_err(bad)?;
    let input = pigeon_blog::Input {
        site_toml: s
            .site_toml
            .ok_or_else(|| bad("the repository has no .pigeon/site.toml"))?,
        posts: s.posts,
        pages: s.pages,
        statics: s.statics.iter().map(|(p, _)| p.clone()).collect(),
        base_url: t.base_url.clone(),
        commit: commit.chars().take(7).collect(),
        commit_year: year,
    };
    let out = match pigeon_blog::render_pinned(&input, &|v| allows(blog_pin.as_ref(), v)) {
        Ok(o) => o,
        Err(problems) => {
            return Ok(json!({
                "commit": commit, "deployed": false,
                "problems": problems.iter().map(ToString::to_string).collect::<Vec<_>>(),
            }));
        }
    };

    publish_stage(&mut started, "render");
    ensure_current(env, key, caller, instance, record).await?;
    let account = linked_account(env, key, caller, instance, &t, record).await?;
    if t.protected {
        crate::public::require_protected_origin(
            env,
            key,
            &t.base_url,
            instance,
            &t.site,
            &t.bucket.name,
        )
        .await?;
        r2::require_private_bucket(&account, &t.bucket).await?;
    }
    publish_stage(&mut started, "storage_preflight");
    let mut present: BTreeSet<String> = r2::list(&account, &t.bucket, "blobs/")
        .await?
        .iter()
        .filter_map(|k| r2::blob_of_key(k).map(str::to_string))
        .collect();

    let mut manifest = Manifest::new();
    let mut fresh = Vec::new();
    let mut add = |path: String, bytes: Vec<u8>, ctype: String| {
        let pair = [(path, bytes)];
        let (p, mut e) = r2::manifest_of(&pair)
            .into_iter()
            .next()
            .expect("one file gives one entry");
        let [(_, bytes)] = pair;
        e.content_type = ctype.clone();
        if present.insert(e.blob.clone()) {
            fresh.push(r2::ObjectWrite {
                key: r2::blob_key(&e.blob),
                content_type: ctype,
                body: bytes,
            });
        }
        manifest.insert(p, e);
    };
    for (path, (bytes, ctype)) in out.files {
        add(path, bytes, ctype.to_string());
    }
    for (path, bytes) in s.statics {
        let ctype = r2::content_type(&path).to_string();
        add(path, bytes, ctype);
    }
    let uploaded = fresh.len();
    publish_stage(&mut started, "storage_plan");
    worker::console_log!("site_deploy staging blobs={uploaded}");
    r2::put_objects(&account, &t.bucket, fresh).await?;
    publish_stage(&mut started, "blobs_staged");
    let files = manifest.len();
    let provenance = json!({
        "commit": commit,
        "instance": instance,
        "site": t.site,
        "template": out.template,
        "template_version": out.version,
        "template_digest": out.digest,
        "renderer_digest": pigeon_blog::renderer_digest(),
        "base_url": t.base_url,
    });
    // Every deploy is an immutable release; `target` refused a record without a profile.
    let release = site_publish::release_id(&provenance, &manifest);
    r2::put(
        &account,
        &t.bucket,
        &r2::manifest_key(&release),
        "application/json",
        serde_json::to_vec(&manifest).map_err(|e| bad(e.to_string()))?,
    )
    .await?;
    r2::put(
        &account,
        &t.bucket,
        &site_publish::release_key(&release),
        "application/json",
        serde_json::to_vec(&provenance)?,
    )
    .await?;
    publish_stage(&mut started, "manifest_staged");
    ensure_current(env, key, caller, instance, record).await?;
    r2::put(
        &account,
        &t.bucket,
        r2::LIVE,
        "text/plain",
        release.as_bytes().to_vec(),
    )
    .await?;
    publish_stage(&mut started, "activated");
    Ok(json!({
        "instance": instance,
        "commit": commit,
        "deployed": true,
        "files": files,
        "uploaded": uploaded,
        "release": release,
        "read_with": if gh_token.is_some() { "the GitHub App" } else { "codeload (public)" },
        "template": out.template,
        "template_version": out.version,
        "template_digest": out.digest,
        "pins": record.get("pins").cloned().unwrap_or_else(|| json!({})),
    }))
}
