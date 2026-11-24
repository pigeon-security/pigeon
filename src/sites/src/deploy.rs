//! `site_deploy`: an instance's site, deployed from its GitHub repository at a commit. What it
//! must not hold itself comes from core, as the caller it was called for: the instance's
//! record (core's `instances_list`), an installation token for the repository (`github`) and the
//! linked account's token (`linked`). The repository is read as one tarball
//! ([`pigeon_core::tarball`]), sorted ([`pigeon_blog::repo`]), rendered ([`pigeon_blog`]), and
//! written to the record's bucket in the layout the site Worker serves: the blobs it lacks, then
//! `deploys/{release}.json` and release metadata, then `live`. It only adds objects and moves `live`, so the previous
//! deploy stays whole until the last write.

use std::collections::BTreeSet;

use pigeon_contract::{Answer, Signed};
use pigeon_core::r2::{self, Bucket, Manifest};
use pigeon_core::versions::{Requirement, Version};
use pigeon_core::{Account, site_publish, tarball};
use serde_json::{Value, json};
use worker::{Env, Error, Fetch, Headers, Method, Request, RequestInit, Result};

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// Asks core for `op` as the caller this call came with.
async fn core(env: &Env, key: &[u8], caller: &Signed, op: &str, body: &Value) -> Result<Value> {
    let nonce = pigeon_core::webauthn::random()?;
    let platform = env.service("PLATFORM")?;
    match pigeon_contract::ask(
        &platform,
        "https://core",
        op,
        body,
        Some((key, &caller.caller)),
        &nonce,
    )
    .await?
    {
        Answer::Ok(v) => Ok(v),
        Answer::Error(f) => Err(bad(format!("core's {op}: {}", f.message))),
    }
}

/// What a deploy needs from an instance's record.
pub struct Target {
    pub repo: String,
    pub bucket: Bucket,
    pub base_url: String,
    pub site: String,
    pub account_id: String,
}

/// Reads [`Target`] from a record as core's `instances_list` answers it. Pure.
pub fn target(record: &Value) -> std::result::Result<Target, String> {
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
    let host = site["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["kind"] == "domain" || t["kind"] == "zone")
        .and_then(|t| t["hostname"].as_str())
        .ok_or_else(|| format!("instance {id}'s site has no domain to be served at"))?;
    Ok(Target {
        repo,
        account_id: account_id.into(),
        bucket: Bucket {
            name: bucket.to_string(),
            jurisdiction: site["jurisdiction"].as_str().map(str::to_string),
        },
        base_url: format!("https://{host}"),
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

/// Reads [`Pins`] from a record as core's `instances_list` answers it. Pure.
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

/// The repository's tarball at `rev`: through the API with the App's token when there is one,
/// else from codeload, which serves public repositories.
async fn tarball_of(repo: &str, rev: &str, token: Option<&str>) -> Result<Vec<u8>> {
    let headers = Headers::new();
    headers.set("User-Agent", "pigeon")?;
    let url = match token {
        Some(t) => {
            headers.set("Authorization", &format!("Bearer {t}"))?;
            headers.set("Accept", "application/vnd.github+json")?;
            format!("https://api.github.com/repos/{repo}/tarball/{rev}")
        }
        None => format!("https://codeload.github.com/{repo}/tar.gz/{rev}"),
    };
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let mut res = Fetch::Request(Request::new_with_init(&url, &init)?)
        .send()
        .await?;
    if res.status_code() != 200 {
        return Err(bad(format!(
            "GitHub answered {} for {repo} at {rev}",
            res.status_code()
        )));
    }
    res.bytes().await
}

async fn approved_record(env: &Env, key: &[u8], caller: &Signed, instance: &str) -> Result<Value> {
    let records = core(
        env,
        key,
        caller,
        "dispatch",
        &json!({ "tool": "instances_list" }),
    )
    .await?;
    records
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["id"] == instance)
        .cloned()
        .ok_or_else(|| bad(format!("there is no instance {instance}")))
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
) -> Result<Account> {
    let linked = core(
        env,
        key,
        caller,
        "credential",
        &json!({"kind": "linked", "instance": instance}),
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
    let target = target(&record).map_err(bad)?;
    let account = linked_account(env, key, caller, instance, &target).await?;
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

/// Deploys `instance` at `rev`.
pub async fn deploy(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    rev: &str,
) -> Result<Value> {
    let record = approved_record(env, key, caller, instance).await?;
    let record = &record;
    let pins = pins(record);
    check_sites_pin(
        pins.sites.as_deref(),
        &crate::versions::sites_component().version,
    )
    .map_err(bad)?;
    let blog_pin = blog_requirement(pins.blog.as_deref()).map_err(bad)?;
    let t = target(record).map_err(bad)?;

    let ask = |kind: &str| json!({ "kind": kind, "instance": instance });
    let gh = core(env, key, caller, "credential", &ask("github"))
        .await
        .ok();
    let gh_token = gh.as_ref().and_then(|c| c["token"].as_str());
    let gz = tarball_of(&t.repo, rev, gh_token).await?;
    let archive = tarball::read(&gz).map_err(bad)?;
    drop(gz);
    let commit = archive
        .commit
        .clone()
        .ok_or_else(|| bad("GitHub's archive does not name its commit"))?;
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

    ensure_current(env, key, caller, instance, record).await?;
    let account = linked_account(env, key, caller, instance, &t).await?;
    let present: BTreeSet<String> = r2::list(&account, &t.bucket, "blobs/")
        .await?
        .iter()
        .filter_map(|k| r2::blob_of_key(k).map(str::to_string))
        .collect();

    let mut manifest = Manifest::new();
    let mut fresh: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut add = |path: String, bytes: Vec<u8>, ctype: String| {
        let pair = [(path, bytes)];
        let (p, mut e) = r2::manifest_of(&pair)
            .into_iter()
            .next()
            .expect("one file gives one entry");
        let [(_, bytes)] = pair;
        e.content_type = ctype.clone();
        let new = !present.contains(&e.blob) && !manifest.values().any(|x| x.blob == e.blob);
        if new {
            fresh.push((e.blob.clone(), ctype, bytes));
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
    for (blob, ctype, bytes) in fresh {
        r2::put(&account, &t.bucket, &r2::blob_key(&blob), &ctype, bytes).await?;
    }
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
    // A legacy instance keeps its existing serving contract until the owner opts in.
    let release =
        (!record["publishing"].is_null()).then(|| site_publish::release_id(&provenance, &manifest));
    let pointer = release.as_deref().unwrap_or(&commit);
    r2::put(
        &account,
        &t.bucket,
        &r2::manifest_key(pointer),
        "application/json",
        serde_json::to_vec(&manifest).map_err(|e| bad(e.to_string()))?,
    )
    .await?;
    if let Some(release) = &release {
        r2::put(
            &account,
            &t.bucket,
            &site_publish::release_key(release),
            "application/json",
            serde_json::to_vec(&provenance)?,
        )
        .await?;
    }
    ensure_current(env, key, caller, instance, record).await?;
    r2::put(
        &account,
        &t.bucket,
        r2::LIVE,
        "text/plain",
        pointer.as_bytes().to_vec(),
    )
    .await?;
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
