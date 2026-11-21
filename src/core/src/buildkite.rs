//! Buildkite's REST API, as the CI gateway uses it: create a build on a pipeline, read one,
//! cancel one, and check what an API token may do. The token goes in the `Authorization`
//! header only; no error repeats it.

use serde_json::Value;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

const API: &str = "https://api.buildkite.com/v2";

/// A pipeline, reached with an API token.
pub struct Pipeline<'a> {
    pub token: &'a str,
    pub org: &'a str,
    pub pipeline: &'a str,
}

/// One call: its status and JSON body (`Null` when empty or not JSON).
async fn call(
    token: &str,
    method: Method,
    url: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    headers.set("Accept", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers.clone());
    if let Some(b) = body {
        headers.set("Content-Type", "application/json")?;
        init.with_headers(headers)
            .with_body(Some(b.to_string().into()));
    }
    let mut res = Fetch::Request(Request::new_with_init(url, &init)?)
        .send()
        .await?;
    let status = res.status_code();
    let text = res.text().await.unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn refused(what: &str, status: u16, body: &Value) -> Error {
    // Buildkite's error bodies carry a `message`, never the token.
    let message = body["message"].as_str().unwrap_or("");
    let message: String = message.chars().take(200).collect();
    Error::RustError(format!("Buildkite {what}: {status} {message}"))
}

impl Pipeline<'_> {
    fn builds(&self) -> String {
        format!(
            "{API}/organizations/{}/pipelines/{}/builds",
            self.org, self.pipeline
        )
    }

    /// Creates a build from `request` (commit, branch, message, env, meta_data, …); answers
    /// the build.
    pub async fn create(&self, request: &Value) -> Result<Value> {
        let (status, body) = call(self.token, Method::Post, &self.builds(), Some(request)).await?;
        if status == 201 || status == 200 {
            Ok(body)
        } else {
            Err(refused("refused the build", status, &body))
        }
    }

    /// Build `number`, without its jobs.
    pub async fn get(&self, number: u64) -> Result<Value> {
        let url = format!("{}/{number}?exclude_jobs=true", self.builds());
        let (status, body) = call(self.token, Method::Get, &url, None).await?;
        if status == 200 {
            Ok(body)
        } else {
            Err(refused("did not answer for the build", status, &body))
        }
    }

    /// Cancels build `number`; one that can no longer be cancelled (422) is left alone.
    pub async fn cancel(&self, number: u64) -> Result<()> {
        let url = format!("{}/{number}/cancel", self.builds());
        let (status, body) = call(self.token, Method::Put, &url, None).await?;
        if status == 200 || status == 422 {
            Ok(())
        } else {
            Err(refused("did not cancel the build", status, &body))
        }
    }
}

/// The scopes API token `token` holds.
pub async fn scopes(token: &str) -> Result<Vec<String>> {
    let (status, body) = call(token, Method::Get, &format!("{API}/access-token"), None).await?;
    if status != 200 {
        return Err(refused("refused the API token", status, &body));
    }
    Ok(body["scopes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(String::from))
        .collect())
}

/// When API token `token` expires, as `GET /v2/access-token` answers it (`expires_at`):
/// `None` when the answer names no expiry.
pub async fn expires_at(token: &str) -> Result<Option<String>> {
    let (status, body) = call(token, Method::Get, &format!("{API}/access-token"), None).await?;
    if status != 200 {
        return Err(refused("refused the API token", status, &body));
    }
    Ok(body["expires_at"].as_str().map(String::from))
}

/// The JSON document at `url`, such as an OIDC issuer's keys.
pub async fn json(url: &str) -> Result<Value> {
    let mut res = Fetch::Url(
        url.parse()
            .map_err(|_| Error::RustError("not a URL".into()))?,
    )
    .send()
    .await?;
    if res.status_code() != 200 {
        return Err(Error::RustError(format!(
            "{url} answered {}",
            res.status_code()
        )));
    }
    res.json().await
}
