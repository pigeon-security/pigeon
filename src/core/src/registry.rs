//! Cloudflare's container registry and the container applications that run from it, read only:
//! which applications exist and the image each runs, and which repositories and tags the
//! account's registry (`registry.cloudflare.com`) holds. The registry is read with pull-only
//! credentials living five minutes, made with a token minted for the call and revoked after;
//! neither ever leaves this block. The calls are the ones wrangler 4.166.0's
//! `containers images list` makes.

use serde::Serialize;
use serde_json::{Value, json};
use worker::{Fetch, Headers, Method, Request, RequestInit, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// What reading the container applications takes.
pub const READ_PERMISSIONS: [&str; 1] = ["Workers Containers Read"];

/// What making registry credentials takes, even pull-only ones.
pub const CREDENTIALS_PERMISSIONS: [&str; 1] = ["Workers Containers Write"];

/// The account's registry.
pub const HOST: &str = "registry.cloudflare.com";

/// The most catalog pages read, so a large registry cannot run a call long.
const MAX_PAGES: usize = 20;

/// A container application, as listed: never its secrets or environment.
#[derive(Serialize, Debug, PartialEq)]
pub struct Application {
    pub id: Option<String>,
    pub name: Option<String>,
    pub image: Option<String>,
    pub instance_type: Option<String>,
    pub instances: Option<u64>,
    pub max_instances: Option<u64>,
    pub scheduling_policy: Option<String>,
    /// The Durable Object namespace it serves, under the `durable_object` policy.
    pub namespace: Option<String>,
}

/// `v`, one application as the API lists it, with only what [`Application`] keeps.
pub fn application(v: &Value) -> Application {
    let s = |v: &Value| v.as_str().map(String::from);
    Application {
        id: s(&v["id"]),
        name: s(&v["name"]),
        image: s(&v["configuration"]["image"]),
        instance_type: s(&v["configuration"]["instance_type"]),
        instances: v["instances"].as_u64(),
        max_instances: v["max_instances"].as_u64(),
        scheduling_policy: s(&v["scheduling_policy"]),
        namespace: s(&v["durable_objects"]["namespace_id"]),
    }
}

/// The account's container applications.
pub async fn applications(account: &Account) -> Result<Vec<Application>> {
    let token = Scoped::open(
        account,
        "containers-read",
        vec![Policy::new(Scope::Account, &READ_PERMISSIONS)],
    )
    .await?;
    let path = format!("/accounts/{}/containers/applications", account.id);
    let listed = call(token.value(), Method::Get, &path, None).await;
    let listed = token.finish(listed).await?;
    Ok(listed
        .as_array()
        .into_iter()
        .flatten()
        .map(application)
        .collect())
}

/// A repository in the registry and its tags.
#[derive(Serialize, Debug, PartialEq)]
pub struct Repository {
    pub name: String,
    pub tags: Vec<String>,
}

/// The cursor of the catalog's next page, from its `Link` header: the `last` parameter of the
/// link whose `rel` is `next`.
pub fn next_cursor(link: &str) -> Option<String> {
    for part in link.split(',') {
        let (target, params) = part.split_once(';').unwrap_or((part, ""));
        let next = params.split(';').any(|p| {
            let p = p.trim();
            p.strip_prefix("rel=")
                .map(|r| r.trim_matches('"').split_whitespace().any(|r| r == "next"))
                .unwrap_or(false)
        });
        if !next {
            continue;
        }
        let target = target.trim().trim_start_matches('<').trim_end_matches('>');
        let query = target.split_once('?')?.1;
        return query
            .split('&')
            .find_map(|kv| kv.strip_prefix("last="))
            .filter(|c| !c.is_empty())
            .map(String::from);
    }
    None
}

/// `page`, one catalog answer (`{"repositories": {name: [tags]}}`), added to `into`.
pub fn add_page(into: &mut Vec<Repository>, page: &Value) {
    for (name, tags) in page["repositories"].as_object().into_iter().flatten() {
        let tags: Vec<String> = tags
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(String::from))
            .collect();
        match into.iter_mut().find(|r| &r.name == name) {
            Some(r) => {
                for t in tags {
                    if !r.tags.contains(&t) {
                        r.tags.push(t);
                    }
                }
            }
            None => into.push(Repository {
                name: name.clone(),
                tags,
            }),
        }
    }
}

/// The repositories in the account's registry, with their tags, sorted by name.
pub async fn repositories(account: &Account) -> Result<Vec<Repository>> {
    let token = Scoped::open(
        account,
        "containers-registry",
        vec![Policy::new(Scope::Account, &CREDENTIALS_PERMISSIONS)],
    )
    .await?;
    let path = format!(
        "/accounts/{}/containers/registries/{HOST}/credentials",
        account.id
    );
    let body = json!({ "expiration_minutes": 5, "permissions": ["pull"] });
    let made = call(token.value(), Method::Post, &path, Some(&body)).await;
    let made = token.finish(made).await?;
    let password = made["password"]
        .as_str()
        .ok_or_else(|| worker::Error::RustError("the registry gave no credentials".into()))?;
    let basic = format!(
        "Basic {}",
        crate::git::base64(format!("v1:{password}").as_bytes())
    );
    let first = format!("https://{HOST}/v2/_catalog?tags=true");
    let mut out = Vec::new();
    let mut url = first.clone();
    let mut seen = Vec::new();
    for _ in 0..MAX_PAGES {
        let headers = Headers::new();
        headers.set("Authorization", &basic)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Get).with_headers(headers);
        let mut res = Fetch::Request(Request::new_with_init(&url, &init)?)
            .send()
            .await?;
        let status = res.status_code();
        if !(200..300).contains(&status) {
            // The status only: the answer could echo the request.
            return Err(worker::Error::RustError(format!(
                "the registry's catalog answered {status}"
            )));
        }
        let link = res.headers().get("Link")?;
        let page: Value = res.json().await?;
        add_page(&mut out, &page);
        let Some(cursor) = link.as_deref().and_then(next_cursor) else {
            break;
        };
        if seen.contains(&cursor) {
            break;
        }
        url = format!("{first}&last={cursor}");
        seen.push(cursor);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_application_keeps_only_what_is_listed() {
        let v = json!({
            "id": "a1", "name": "pigeon-platform-cirunner", "instances": 0, "max_instances": 4,
            "scheduling_policy": "durable_object",
            "configuration": { "image": "registry.cloudflare.com/x/pigeon-ci-base@sha256:ab",
                               "instance_type": "standard-3",
                               "environment_variables": [{ "name": "K", "value": "secret" }],
                               "secrets": [{ "name": "S" }] },
            "durable_objects": { "namespace_id": "ns1" }
        });
        let a = application(&v);
        assert_eq!(a.name.as_deref(), Some("pigeon-platform-cirunner"));
        assert_eq!(a.namespace.as_deref(), Some("ns1"));
        assert_eq!(a.max_instances, Some(4));
        let text = serde_json::to_string(&a).unwrap();
        assert!(!text.contains("secret") && !text.contains("environment"));
        assert_eq!(application(&json!({})).name, None);
    }

    #[test]
    fn the_next_page_is_the_link_marked_next() {
        assert_eq!(
            next_cursor(r#"</v2/_catalog?tags=true&last=x%2Fb&n=100>; rel="next""#).as_deref(),
            Some("x%2Fb")
        );
        assert_eq!(
            next_cursor(r#"</v2/_catalog?last=a>; rel="prev", </v2/_catalog?last=b>; rel=next"#)
                .as_deref(),
            Some("b")
        );
        assert_eq!(next_cursor(r#"</v2/_catalog?last=a>; rel="prev""#), None);
        assert_eq!(next_cursor(r#"</v2/_catalog>; rel="next""#), None);
        assert_eq!(next_cursor(""), None);
    }

    #[test]
    fn pages_merge_by_repository() {
        let mut out = Vec::new();
        add_page(
            &mut out,
            &json!({ "repositories": { "a": ["v1"], "b": [] } }),
        );
        add_page(&mut out, &json!({ "repositories": { "a": ["v1", "v2"] } }));
        add_page(&mut out, &json!({ "nothing": 1 }));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tags, ["v1", "v2"]);
    }
}
