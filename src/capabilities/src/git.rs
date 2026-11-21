//! Git's smart HTTP protocol (v0), enough to copy refs between two remotes without a git binary:
//! read a remote's refs, fetch a pack from one, push that pack to the other.

use std::collections::BTreeMap;

use worker::js_sys::Uint8Array;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

/// The all-zero object ID: a ref that does not exist.
pub(crate) const ZERO: &str = "0000000000000000000000000000000000000000";

/// A remote and the `Authorization` header that reaches it.
pub(crate) struct Remote {
    /// Its URL, ending in `.git`.
    pub url: String,
    pub authorization: String,
}

/// A remote's refs, by name, peeled tag entries (`^{}`) left out, and its capabilities.
pub(crate) struct Refs {
    pub refs: BTreeMap<String, String>,
    pub caps: Vec<String>,
}

/// How long to wait before each retry of a request refused with 401: a token minted moments
/// before may not have reached the remote yet.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// `remote`'s refs, as `service` (`git-upload-pack` or `git-receive-pack`) advertises them.
pub(crate) async fn refs(remote: &Remote, service: &str) -> Result<Refs> {
    let url = format!("{}/info/refs?service={service}", remote.url);
    let body = send(remote, Method::Get, &url, None, None).await?;
    let mut refs = BTreeMap::new();
    let mut caps = Vec::new();
    for line in lines(&body)? {
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches('\n');
        if line.starts_with('#') {
            continue;
        }
        let (head, rest) = line.split_once('\0').unwrap_or((line, ""));
        if !rest.is_empty() {
            caps = rest.split(' ').map(String::from).collect();
        }
        let Some((sha, name)) = head.split_once(' ') else {
            continue;
        };
        if name.ends_with("^{}") || name == "capabilities^{}" {
            continue;
        }
        refs.insert(name.to_string(), sha.to_string());
    }
    Ok(Refs { refs, caps })
}

/// What [`fetch`] got: the object IDs the remote acknowledged having, and the pack.
pub(crate) struct Fetched {
    pub acked: Vec<String>,
    pub pack: Vec<u8>,
}

/// A pack from `remote` holding `wants` and what they reach, less what `haves` reach. The pack
/// is complete (not thin), so another remote can take it as it is.
pub(crate) async fn fetch(
    remote: &Remote,
    caps: &[String],
    wants: &[String],
    haves: &[String],
) -> Result<Fetched> {
    let asked: Vec<&str> = ["ofs-delta", "no-progress"]
        .into_iter()
        .filter(|c| caps.iter().any(|have| have == c))
        .collect();
    let mut body = Vec::new();
    for (i, want) in wants.iter().enumerate() {
        let line = if i == 0 {
            format!("want {want} {} agent=pigeon\n", asked.join(" "))
        } else {
            format!("want {want}\n")
        };
        body.extend(pkt(line.as_bytes()));
    }
    body.extend(b"0000");
    for have in haves {
        body.extend(pkt(format!("have {have}\n").as_bytes()));
    }
    body.extend(pkt(b"done\n"));
    let url = format!("{}/git-upload-pack", remote.url);
    let answer = send(
        remote,
        Method::Post,
        &url,
        Some("application/x-git-upload-pack-request"),
        Some(&body),
    )
    .await?;
    let mut acked = Vec::new();
    let mut at = 0;
    while at + 4 <= answer.len() && &answer[at..at + 4] != b"PACK" {
        let (line, next) = read_pkt(&answer, at)?;
        at = next;
        let Some(line) = line else { continue };
        let line = String::from_utf8_lossy(&line);
        if let Some(sha) = line.strip_prefix("ACK ") {
            acked.push(sha.split_whitespace().next().unwrap_or("").to_string());
        } else if let Some(why) = line.strip_prefix("ERR ") {
            return Err(Error::RustError(format!("fetch: {}", why.trim())));
        }
    }
    if at >= answer.len() {
        return Err(Error::RustError("fetch: the remote sent no pack".into()));
    }
    Ok(Fetched {
        acked,
        pack: answer[at..].to_vec(),
    })
}

/// One ref update: from `old` (or [`ZERO`] to create) to `new`.
pub(crate) struct Update {
    pub name: String,
    pub old: String,
    pub new: String,
}

/// Pushes `updates` and `pack` to `remote`; answers each ref's status line from the remote,
/// such as `ok refs/heads/main` or `ng refs/heads/main non-fast-forward`.
pub(crate) async fn push(
    remote: &Remote,
    caps: &[String],
    updates: &[Update],
    pack: &[u8],
) -> Result<Vec<String>> {
    let report = caps.iter().any(|c| c == "report-status");
    let mut body = Vec::new();
    for (i, u) in updates.iter().enumerate() {
        let line = if i == 0 && report {
            format!(
                "{} {} {}\0report-status agent=pigeon\n",
                u.old, u.new, u.name
            )
        } else {
            format!("{} {} {}\n", u.old, u.new, u.name)
        };
        body.extend(pkt(line.as_bytes()));
    }
    body.extend(b"0000");
    body.extend(pack);
    let url = format!("{}/git-receive-pack", remote.url);
    let answer = send(
        remote,
        Method::Post,
        &url,
        Some("application/x-git-receive-pack-request"),
        Some(&body),
    )
    .await?;
    let mut status = Vec::new();
    for line in lines(&answer)? {
        let line = String::from_utf8_lossy(&line).trim_end().to_string();
        if let Some(why) = line.strip_prefix("unpack ") {
            if why != "ok" {
                return Err(Error::RustError(format!("push: unpack {why}")));
            }
        } else if !line.is_empty() {
            status.push(line);
        }
    }
    Ok(status)
}

/// One request to `remote`, retried while it answers 401; the answer's body.
async fn send(
    remote: &Remote,
    method: Method,
    url: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut waits = NOT_YET_ACTIVE_WAITS_MS.iter();
    loop {
        let headers = Headers::new();
        headers.set("Authorization", &remote.authorization)?;
        headers.set("User-Agent", "git/2.47.0 (pigeon)")?;
        if let Some(ct) = content_type {
            headers.set("Content-Type", ct)?;
            headers.set("Accept", &ct.replace("-request", "-result"))?;
        }
        let mut init = RequestInit::new();
        init.with_method(method.clone()).with_headers(headers);
        if let Some(b) = body {
            init.with_body(Some(Uint8Array::from(b).into()));
        }
        let mut response = Fetch::Request(Request::new_with_init(url, &init)?)
            .send()
            .await?;
        let status = response.status_code();
        if status == 401 {
            if let Some(ms) = waits.next() {
                worker::Delay::from(std::time::Duration::from_millis(*ms)).await;
                continue;
            }
        }
        let bytes = response.bytes().await?;
        if !(200..300).contains(&status) {
            let shown = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).to_string();
            // The URL, never the Authorization header.
            return Err(Error::RustError(format!(
                "{url}: {status}: {}",
                shown.trim()
            )));
        }
        return Ok(bytes);
    }
}

/// `data` as one pkt-line.
fn pkt(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend(data);
    out
}

/// The pkt-line at `at`: its data (`None` for a flush or delimiter) and where the next starts.
fn read_pkt(data: &[u8], at: usize) -> Result<(Option<Vec<u8>>, usize)> {
    let bad = || Error::RustError("git: a malformed pkt-line".into());
    let head = data.get(at..at + 4).ok_or_else(bad)?;
    let len = usize::from_str_radix(std::str::from_utf8(head).map_err(|_| bad())?, 16)
        .map_err(|_| bad())?;
    if len < 4 {
        return Ok((None, at + 4));
    }
    let line = data.get(at + 4..at + len).ok_or_else(bad)?;
    Ok((Some(line.to_vec()), at + len))
}

/// Every pkt-line in `data`, flushes and delimiters left out.
fn lines(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let (line, next) = read_pkt(data, at)?;
        out.extend(line);
        at = next;
    }
    Ok(out)
}

/// `bytes` in standard base64, padded.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
