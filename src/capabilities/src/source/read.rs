//! Instance-bound reads for the Source capability.

use pigeon_core::{artifacts, git, repository};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::Context;
use crate::instances::record::{AccountRef, Instance, RepoRef, Toggle};

/// Check both the opt-in and the repository binding before any provider operation.
pub fn approved_repo(instance: &Instance) -> std::result::Result<&RepoRef, String> {
    if !instance.source.is_some_and(Toggle::enabled) {
        return Err("source reads are disabled for this instance".into());
    }
    let repo = instance
        .repo
        .as_ref()
        .ok_or("source reads require an approved repository")?;
    if matches!(repo, RepoRef::Artifacts { .. })
        && matches!(&instance.account, AccountRef::Linked { account_id: None })
    {
        return Err("source reads require an approved linked account for Artifacts".into());
    }
    Ok(repo)
}

pub const RESOLVE: &str = "source_resolve_revision";
pub const FILE: &str = "source_read_file";

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

pub async fn run(cx: &Context<'_>, tool: &str, args: &Value) -> Result<Value> {
    let mut allowed = vec!["instance", "revision"];
    if tool == FILE {
        allowed.push("path");
    }
    if !args
        .as_object()
        .is_some_and(|fields| fields.keys().all(|name| allowed.contains(&name.as_str())))
    {
        return Err(bad(
            "source tools accept only instance-bound revision and file arguments",
        ));
    }
    let instance = crate::arg(args, "instance")?;
    let revision = match args.get("revision") {
        None => "HEAD",
        Some(_) => crate::arg(args, "revision")?,
    };
    repository::revision(revision).map_err(bad)?;
    let path = if tool == FILE {
        Some(repository::file_path(crate::arg(args, "path")?).map_err(bad)?)
    } else {
        None
    };
    let record = crate::instances::one(cx.env, instance)
        .await?
        .ok_or_else(|| bad("unknown source instance"))?;
    let (commit, file) = match approved_repo(&record).map_err(bad)? {
        RepoRef::Github {
            full_name,
            installation,
        } => {
            let token = match installation {
                Some(id) => Some(
                    crate::github::installation_token_for(
                        cx.env,
                        full_name,
                        *id,
                        &[("contents", "read")],
                    )
                    .await?
                    .0,
                ),
                None => None,
            };
            let commit = repository::github_resolve(full_name, revision, token.as_deref()).await?;
            let file = match path {
                Some(path) => {
                    repository::github_read_file(full_name, &commit, path, token.as_deref()).await?
                }
                None => None,
            };
            (commit, file)
        }
        RepoRef::Artifacts { namespace, repo } => {
            let linked;
            let account = match &record.account {
                AccountRef::Home => cx.account,
                AccountRef::Linked { account_id } => {
                    let host = cx.env.var("PLATFORM_HOST")?.to_string();
                    linked = crate::linked::account_for(cx.env, &host, instance).await?;
                    if account_id.as_deref() != Some(linked.id.as_str()) {
                        return Err(bad(
                            "linked account does not match the approved repository account",
                        ));
                    }
                    &linked
                }
            };
            artifacts::read_revision(account, namespace, repo, revision, path).await?
        }
    };
    let mut result = json!({"instance": instance, "revision": revision, "commit": commit});
    if let Some(path) = path {
        let file = file.ok_or_else(|| bad("regular file not found in the resolved commit"))?;
        if file.len() > repository::INLINE_MAX {
            return Err(bad("repository file exceeds the inline limit"));
        }
        result["path"] = json!(path);
        result["size"] = json!(file.len());
        let (encoding, content) = match String::from_utf8(file) {
            Ok(text) => ("utf-8", text),
            Err(e) => ("base64", git::base64(e.as_bytes())),
        };
        result["encoding"] = json!(encoding);
        result["content"] = json!(content);
    }
    Ok(result)
}

/// Read-only tools available with an approved repository binding.
pub fn instance_tools(
    instance: &Instance,
) -> serde_json::Result<&'static [crate::instances::tools::Registration]> {
    use crate::instances::tools::Registration;
    const TOOLS: &[Registration] = &[
        Registration {
            name: RESOLVE,
            description: None,
            arguments: &["revision"],
        },
        Registration {
            name: FILE,
            description: None,
            arguments: &["revision", "path"],
        },
    ];
    Ok(if approved_repo(instance).is_ok() {
        TOOLS
    } else {
        &[]
    })
}
