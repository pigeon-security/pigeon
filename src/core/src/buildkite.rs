//! Buildkite's REST API, as pigeon uses it. The CI gateway creates a build on a pipeline, reads
//! one, cancels one, and checks what an API token may do. The setup reads and writes an
//! organization's clusters, hosted queues, the pipeline and notification services. The token
//! goes in the `Authorization` header only; no error repeats it.

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

/// One call whose status must be among `ok`: its JSON body, else a refusal named `what`.
async fn expect(
    token: &str,
    method: Method,
    url: &str,
    body: Option<&Value>,
    ok: &[u16],
    what: &str,
) -> Result<Value> {
    let (status, answer) = call(token, method, url, body).await?;
    if ok.contains(&status) {
        Ok(answer)
    } else {
        Err(refused(what, status, &answer))
    }
}

/// One list call: the items of a page, as Buildkite answers them, a bare array (clusters,
/// queues) or `{items, links}` (notification services, cursor-paginated), and the next page's
/// URL when there is one. pigeon's few resources fit in a page of 100.
pub fn page(
    answer: Value,
    noun: &str,
) -> std::result::Result<(Vec<Value>, Option<String>), String> {
    match answer {
        Value::Array(items) => Ok((items, None)),
        Value::Object(mut o) => {
            let next = o
                .get("links")
                .and_then(|l| l["next"].as_str())
                .filter(|n| n.starts_with(&format!("{API}/")))
                .map(String::from);
            match o.remove("items") {
                Some(Value::Array(items)) => Ok((items, next)),
                _ => Err(format!("Buildkite answered {noun} that is not a list")),
            }
        }
        _ => Err(format!("Buildkite answered {noun} that is not a list")),
    }
}

/// Every page of a list, following `links.next` (on Buildkite's API only) for at most ten
/// pages.
async fn list(token: &str, url: &str, noun: &str) -> Result<Vec<Value>> {
    let what = format!("did not list {noun}");
    let mut all = Vec::new();
    let mut url = url.to_string();
    for _ in 0..10 {
        let answer = expect(token, Method::Get, &url, None, &[200], &what).await?;
        let (items, next) = page(answer, noun).map_err(Error::RustError)?;
        all.extend(items);
        match next {
            Some(n) => url = n,
            None => return Ok(all),
        }
    }
    Err(Error::RustError(format!(
        "Buildkite answered more than ten pages of {noun}"
    )))
}

/// A path segment, checked: non-empty ASCII alphanumerics, `-`, `_` and `.`, at most 100
/// bytes, and neither `.` nor `..`.
fn segment(s: &str) -> Result<&str> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.';
    if s.is_empty() || s.len() > 100 || s == "." || s == ".." || !s.chars().all(allowed) {
        return Err(Error::RustError("not a Buildkite identifier".into()));
    }
    Ok(s)
}

/// An organization, reached with an API token: what the setup reads and writes.
pub struct Org<'a> {
    pub token: &'a str,
    pub org: &'a str,
}

impl Org<'_> {
    fn base(&self) -> Result<String> {
        Ok(format!("{API}/organizations/{}", segment(self.org)?))
    }

    /// The clusters: `GET …/clusters`.
    pub async fn clusters(&self) -> Result<Vec<Value>> {
        let url = format!("{}/clusters?per_page=100", self.base()?);
        list(self.token, &url, "clusters").await
    }

    /// Creates a cluster: `POST …/clusters`.
    pub async fn create_cluster(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/clusters", self.base()?);
        expect(
            self.token,
            Method::Post,
            &url,
            Some(body),
            &[201, 200],
            "did not create the cluster",
        )
        .await
    }

    /// Updates a cluster: `PUT …/clusters/{id}`.
    pub async fn update_cluster(&self, id: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/clusters/{}", self.base()?, segment(id)?);
        expect(
            self.token,
            Method::Put,
            &url,
            Some(body),
            &[200],
            "did not update the cluster",
        )
        .await
    }

    /// A cluster's queues: `GET …/clusters/{cluster}/queues`.
    pub async fn queues(&self, cluster: &str) -> Result<Vec<Value>> {
        let url = format!(
            "{}/clusters/{}/queues?per_page=100",
            self.base()?,
            segment(cluster)?
        );
        list(self.token, &url, "queues").await
    }

    /// Creates a queue, hosted when the body says so: `POST …/clusters/{cluster}/queues`.
    pub async fn create_queue(&self, cluster: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/clusters/{}/queues", self.base()?, segment(cluster)?);
        expect(
            self.token,
            Method::Post,
            &url,
            Some(body),
            &[201, 200],
            "did not create the queue",
        )
        .await
    }

    /// Updates a queue: `PUT …/clusters/{cluster}/queues/{id}`.
    pub async fn update_queue(&self, cluster: &str, id: &str, body: &Value) -> Result<Value> {
        let url = format!(
            "{}/clusters/{}/queues/{}",
            self.base()?,
            segment(cluster)?,
            segment(id)?
        );
        expect(
            self.token,
            Method::Put,
            &url,
            Some(body),
            &[200],
            "did not update the queue",
        )
        .await
    }

    /// Pipeline `slug`, or `None` when there is none: `GET …/pipelines/{slug}`.
    pub async fn pipeline(&self, slug: &str) -> Result<Option<Value>> {
        let url = format!("{}/pipelines/{}", self.base()?, segment(slug)?);
        let (status, body) = call(self.token, Method::Get, &url, None).await?;
        match status {
            200 => Ok(Some(body)),
            404 => Ok(None),
            _ => Err(refused("did not answer for the pipeline", status, &body)),
        }
    }

    /// Creates a pipeline: `POST …/pipelines`.
    pub async fn create_pipeline(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/pipelines", self.base()?);
        expect(
            self.token,
            Method::Post,
            &url,
            Some(body),
            &[201, 200],
            "did not create the pipeline",
        )
        .await
    }

    /// Updates a pipeline: `PATCH …/pipelines/{slug}`.
    pub async fn update_pipeline(&self, slug: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/pipelines/{}", self.base()?, segment(slug)?);
        expect(
            self.token,
            Method::Patch,
            &url,
            Some(body),
            &[200],
            "did not update the pipeline",
        )
        .await
    }

    /// The notification services: `GET …/services`.
    pub async fn services(&self) -> Result<Vec<Value>> {
        let url = format!("{}/services?per_page=100", self.base()?);
        list(self.token, &url, "notification services").await
    }

    /// Creates a notification service: `POST …/services`.
    pub async fn create_service(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/services", self.base()?);
        expect(
            self.token,
            Method::Post,
            &url,
            Some(body),
            &[201, 200],
            "did not create the notification service",
        )
        .await
    }

    /// Updates a notification service: `PATCH …/services/{id}`.
    pub async fn update_service(&self, id: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/services/{}", self.base()?, segment(id)?);
        expect(
            self.token,
            Method::Patch,
            &url,
            Some(body),
            &[200],
            "did not update the notification service",
        )
        .await
    }
}
