//! `site_deploy`: an instance's site, deployed from its GitHub repository at a commit. What it
//! must not hold itself comes from core, as the caller it was called for: the instance's
//! approved record, an installation token for the repository, and the linked account's token.
//! Every call is signed with instance context ([`crate::context`] refused any other before this
//! module runs), and gets each through core's `sites_scope` callback ([`SitesScopeRequest`]),
//! which answers only for that instance and, for credentials, only while the record still
//! hashes to the binding the deploy read. There is no other path: the legacy platform callbacks
//! (core's `dispatch` `instances_list` and `credential` kinds `linked` and `github`) are retired.
//! Only a record whose `publishing` profile is enabled deploys.
//! The repository is read as one tarball ([`pigeon_core::tarball`]), sorted and rendered by the
//! template its `site.toml` names ([`crate::templates`]: `blog` or `landing`), and written to the profile's site bucket
//! as an immutable release in the layout the site Worker serves: the blobs it lacks, then
//! `deploys/{release}.json` and `release-meta/{release}.json`, then `live`. It only adds objects
//! and moves `live`, so the previous release stays whole until the last write. The release,
//! rollback and listing code is [`pigeon_core::site_publish`]'s, shared with core's home
//! publishing.

use pigeon_contract::{Answer, Signed, SitesScopeRequest, WorkerContext};
use pigeon_core::r2::{self, Bucket};
use pigeon_core::site_publish::{Owner, R2};
use pigeon_core::versions::{Requirement, Version};
use pigeon_core::{Account, site_publish, tarball};
use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::queued::Receipt;
use crate::templates::{self, Template};

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
    /// The requirement on `landing`, if set.
    pub landing: Option<String>,
}

/// Reads [`Pins`] from an approved record, as core's `sites_scope` answers it. Pure.
pub fn pins(record: &Value) -> Pins {
    let get = |k: &str| record["pins"][k].as_str().map(str::to_string);
    Pins {
        sites: get("sites"),
        blog: get("blog"),
        landing: get("landing"),
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
    template_requirement(Template::Blog, pin)
}

/// Parses a template's pin into a requirement. Pure.
pub fn template_requirement(
    template: Template,
    pin: Option<&str>,
) -> std::result::Result<Option<Requirement>, String> {
    pin.map(|p| {
        Requirement::parse(p).map_err(|e| format!("the {} pin `{p}`: {e}", template.component()))
    })
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
    let store = R2 {
        account: &account,
        bucket: &target.bucket,
    };
    let owner = Owner {
        instance,
        base_url: &target.base_url,
        site: &target.site,
    };
    let provenance = site_publish::staged(&store, release, &owner).await?;
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
    site_publish::activate(&store, release).await?;
    Ok(
        json!({"instance": instance, "release": release, "commit": provenance["commit"], "deployed": true}),
    )
}

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
    let store = R2 {
        account: &account,
        bucket: &target.bucket,
    };
    site_publish::releases(&store, instance, &target.site, limit).await
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
    let landing_pin =
        template_requirement(Template::Landing, pins.landing.as_deref()).map_err(bad)?;
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
    let s = templates::split(archive.files).map_err(bad)?;
    if !s.has_site_toml {
        return Err(bad("the repository has no .pigeon/site.toml"));
    }
    let parts = templates::Parts {
        base_url: t.base_url.clone(),
        commit: commit.chars().take(7).collect(),
        commit_year: year,
        ..s.parts
    };
    let pinned = |template: Template, v: &str| match template {
        Template::Blog => allows(blog_pin.as_ref(), v),
        Template::Landing => allows(landing_pin.as_ref(), v),
    };
    let out = match templates::render(&parts, &pinned) {
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
    let store = R2 {
        account: &account,
        bucket: &t.bucket,
    };
    let mut files: Vec<(String, Vec<u8>, String)> = out
        .files
        .into_iter()
        .map(|(path, (bytes, ctype))| (path, bytes, ctype.to_string()))
        .collect();
    for (path, bytes) in s.statics {
        let ctype = r2::content_type(&path).to_string();
        files.push((path, bytes, ctype));
    }
    let provenance = json!({
        "commit": commit,
        "instance": instance,
        "site": t.site,
        "template": out.template,
        "template_version": out.version,
        "template_digest": out.digest,
        "renderer_digest": out.renderer_digest,
        "base_url": t.base_url,
    });
    // Every deploy is an immutable release; `target` refused a record without a profile.
    let staged = site_publish::stage(&store, &provenance, files, &mut |stage| match stage
        .strip_prefix("blobs=")
    {
        Some(n) => worker::console_log!("site_deploy staging blobs={n}"),
        None => publish_stage(&mut started, stage),
    })
    .await?;
    let (files, uploaded, release) = (staged.files, staged.uploaded, staged.release);
    ensure_current(env, key, caller, instance, record).await?;
    site_publish::activate(&store, &release).await?;
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
