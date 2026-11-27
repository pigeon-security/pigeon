//! The one way out of the cloud gateway: HTTP requests and answers as plain data, a [`Transport`]
//! that sends them, and the list of hosts it may reach. The Worker's transport refuses any other
//! host before it fetches, so a bug or a hostile argument cannot send a credential elsewhere.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use worker::wasm_bindgen::JsValue;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Result};

/// A future that answers a [`worker::Result`].
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

/// The hosts the gateway may call: Scaleway's API and its three Object Storage regions.
pub const HOSTS: [&str; 4] = [
    "api.scaleway.com",
    "s3.fr-par.scw.cloud",
    "s3.nl-ams.scw.cloud",
    "s3.pl-waw.scw.cloud",
];

/// The DigitalOcean account connection probe.
pub(crate) const DIGITALOCEAN_ACCOUNT_URL: &str = "https://api.digitalocean.com/v2/account";

/// A request to send. It may carry a credential in its headers, so `Debug` shows no header
/// value and no body.
#[derive(Clone, PartialEq)]
pub struct HttpRequest {
    /// `GET`, `HEAD`, `PUT`, `POST` or `DELETE`.
    pub method: String,
    /// The whole URL, query included.
    pub url: String,
    /// The headers to send, `host` left out.
    pub headers: Vec<(String, String)>,
    /// The body; empty for none.
    pub body: String,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field(
                "url",
                &if crate::digitalocean_spaces::allowed_url(&self.url) {
                    "<DigitalOcean Spaces URL>"
                } else {
                    self.url.as_str()
                },
            )
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

impl HttpRequest {
    /// The value of header `name`, matched without regard to case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// What came back. `Debug` shows the status and the body's size only.
#[derive(Clone, PartialEq)]
pub struct HttpResponse {
    /// The HTTP status.
    pub status: u16,
    /// The headers.
    pub headers: Vec<(String, String)>,
    /// The body as text.
    pub body: String,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

impl HttpResponse {
    /// The value of header `name`, matched without regard to case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Sends requests. The Worker uses [`FetchTransport`]; the smoke suite uses a recording stub.
pub trait Transport {
    /// Sends `req` and answers what came back. A status of 400 or more is an answer, not an
    /// error.
    fn send<'a>(&'a self, req: HttpRequest) -> Fut<'a, HttpResponse>;
}

/// The authority part of `url` (what sits between `https://` and the first `/`, `?` or `#`),
/// when the URL is `https` at all.
fn authority(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    Some(rest.split(['/', '?', '#']).next().unwrap_or(rest))
}

fn digitalocean_read_url(url: &str) -> bool {
    url == DIGITALOCEAN_ACCOUNT_URL || crate::digitalocean_spaces::allowed_url(url)
}

/// Whether the gateway may call `url`: a DigitalOcean read endpoint, or HTTPS
/// with a host exactly on [`HOSTS`].
pub fn allowed(url: &str) -> bool {
    if digitalocean_read_url(url) {
        return true;
    }
    let Some(host) = authority(url) else {
        return false;
    };
    !host.contains(['@', ':', '\\', '%']) && HOSTS.contains(&host)
}

/// The allowed method on a provider endpoint. Ordinary cloud resources use
/// the host allowlist. DigitalOcean mutations are confined to Droplet paths.
pub fn allowed_request(method: &str, url: &str) -> bool {
    if digitalocean_read_url(url) {
        return method == "GET";
    }
    crate::digitalocean_droplet::allowed(method, url)
        || crate::digitalocean_ingest::allowed(method, url)
        || allowed(url)
}

/// The host of `url` as an error message may name it: no scheme, no user name, no path.
fn shown_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.chars().take(80).collect()
}

/// The transport the Worker uses: `fetch`, to an allowed host only, never following a redirect.
pub struct FetchTransport;

impl Transport for FetchTransport {
    fn send<'a>(&'a self, req: HttpRequest) -> Fut<'a, HttpResponse> {
        Box::pin(async move {
            if !allowed_request(&req.method, &req.url) {
                return Err(Error::RustError(format!(
                    "the cloud gateway does not call {}",
                    shown_host(&req.url)
                )));
            }
            let method = match req.method.as_str() {
                "GET" => Method::Get,
                "HEAD" => Method::Head,
                "PUT" => Method::Put,
                "POST" => Method::Post,
                "DELETE" => Method::Delete,
                other => {
                    return Err(Error::RustError(format!(
                        "the cloud gateway does not send {other}"
                    )));
                }
            };
            let headers = Headers::new();
            for (k, v) in req.headers.iter().filter(|(k, _)| k != "host") {
                headers.set(k, v)?;
            }
            let mut init = RequestInit::new();
            init.with_method(method)
                .with_headers(headers)
                // A redirect could carry the credential to another host.
                .with_redirect(RequestRedirect::Manual);
            if !req.body.is_empty() {
                init.with_body(Some(JsValue::from_str(&req.body)));
            }
            let request = Request::new_with_init(&req.url, &init)?;
            let mut res = Fetch::Request(request).send().await?;
            let status = res.status_code();
            let headers = res.headers().entries().collect();
            let body = res.text().await.unwrap_or_default();
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}
