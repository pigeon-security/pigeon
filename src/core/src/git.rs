//! Git's smart HTTP protocol (v0), enough to copy refs between two remotes without a git binary:
//! read a remote's refs, fetch a pack from one, push that pack to the other.

use std::collections::BTreeMap;

use worker::js_sys::Uint8Array;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

/// The all-zero object ID: a ref that does not exist.
pub const ZERO: &str = "0000000000000000000000000000000000000000";

/// A remote and the `Authorization` header that reaches it.
pub struct Remote {
    /// Its URL, ending in `.git`.
    pub url: String,
    pub authorization: String,
}

/// A remote's refs, by name, peeled tag entries (`^{}`) left out, and its capabilities.
pub struct Refs {
    pub refs: BTreeMap<String, String>,
    pub caps: Vec<String>,
}

/// How long to wait before each retry of a request refused with 401: a token minted moments
/// before may not have reached the remote yet.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// `remote`'s refs, as `service` (`git-upload-pack` or `git-receive-pack`) advertises them.
pub async fn refs(remote: &Remote, service: &str) -> Result<Refs> {
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
pub struct Fetched {
    pub acked: Vec<String>,
    pub pack: Vec<u8>,
}

/// A pack from `remote` holding `wants` and what they reach, less what `haves` reach. The pack
/// is complete (not thin), so another remote can take it as it is.
pub async fn fetch(
    remote: &Remote,
    caps: &[String],
    wants: &[String],
    haves: &[String],
) -> Result<Fetched> {
    fetch_filtered(remote, caps, wants, haves, None).await
}

/// [`fetch`], leaving out what `filter` (such as `tree:0`, commits only) excludes, when the
/// remote can filter; when it can't, the pack is whole.
pub async fn fetch_filtered(
    remote: &Remote,
    caps: &[String],
    wants: &[String],
    haves: &[String],
    filter: Option<&str>,
) -> Result<Fetched> {
    fetch_with(remote, caps, wants, haves, filter, None).await
}

/// [`fetch_filtered`], and with `depth` only that many commits of history, when the remote
/// can make a shallow pack; when it can't, the history is whole.
pub async fn fetch_with(
    remote: &Remote,
    caps: &[String],
    wants: &[String],
    haves: &[String],
    filter: Option<&str>,
    depth: Option<u32>,
) -> Result<Fetched> {
    let filter = filter.filter(|_| caps.iter().any(|c| c == "filter"));
    let depth = depth.filter(|_| caps.iter().any(|c| c == "shallow"));
    let asked: Vec<&str> = ["ofs-delta", "no-progress"]
        .into_iter()
        .filter(|c| caps.iter().any(|have| have == c))
        .chain(filter.map(|_| "filter"))
        .chain(depth.map(|_| "shallow"))
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
    if let Some(d) = depth {
        body.extend(pkt(format!("deepen {d}\n").as_bytes()));
    }
    if let Some(f) = filter {
        body.extend(pkt(format!("filter {f}\n").as_bytes()));
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
    fetched(&answer).map_err(|e| Error::RustError(format!("fetch: {e}")))
}

/// What an upload-pack answer holds: the IDs acknowledged before the pack, and the pack. Some
/// servers (Artifacts among them) end the answer with a flush after the pack; it is dropped
/// when the pack's own SHA-1 trailer shows it is not part of the pack.
pub fn fetched(answer: &[u8]) -> std::result::Result<Fetched, String> {
    let mut acked = Vec::new();
    let mut at = 0;
    while at + 4 <= answer.len() && &answer[at..at + 4] != b"PACK" {
        let (line, next) = read_pkt(answer, at).map_err(|e| e.to_string())?;
        at = next;
        let Some(line) = line else { continue };
        let line = String::from_utf8_lossy(&line);
        if let Some(sha) = line.strip_prefix("ACK ") {
            acked.push(sha.split_whitespace().next().unwrap_or("").to_string());
        } else if let Some(why) = line.strip_prefix("ERR ") {
            return Err(why.trim().to_string());
        }
    }
    if at >= answer.len() {
        return Err("the remote sent no pack".into());
    }
    Ok(Fetched {
        acked,
        pack: without_trailing_flush(&answer[at..]).to_vec(),
    })
}

/// `pack` without a flush (`0000`) after it: dropped only when the 20 bytes before it are the
/// SHA-1 of everything before them, so a pack whose own trailer ends in `0000` is kept whole.
fn without_trailing_flush(pack: &[u8]) -> &[u8] {
    use sha1::{Digest, Sha1};
    if pack.len() >= 36 && pack.ends_with(b"0000") {
        let body = &pack[..pack.len() - 24];
        let trailer = &pack[pack.len() - 24..pack.len() - 4];
        if Sha1::digest(body)[..] == *trailer {
            return &pack[..pack.len() - 4];
        }
    }
    pack
}

/// The regular file at `path` (such as `.pigeon/ci.toml`) in `commit` of `remote`, or `None`
/// when there is none: the commit alone is fetched, one commit deep, and its tree read.
pub async fn read_file(remote: &Remote, commit: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let commit = commit.to_ascii_lowercase();
    if !is_id(&commit) {
        return Err(Error::RustError("not a commit id".into()));
    }
    let advertised = refs(remote, "git-upload-pack").await?;
    let fetched = fetch_with(
        remote,
        &advertised.caps,
        std::slice::from_ref(&commit),
        &[],
        None,
        Some(1),
    )
    .await?;
    let objects = crate::pack::objects(&fetched.pack).map_err(Error::RustError)?;
    crate::pack::read_path(&objects, &commit, path).map_err(Error::RustError)
}

/// The IDs of what each of `paths` names in `commit` of `remote` (see `pack::path_id`): the
/// commit alone is fetched, one commit deep and without blobs where the remote can filter,
/// once for all of them.
pub async fn path_ids(
    remote: &Remote,
    commit: &str,
    paths: &[&str],
) -> Result<Vec<Option<String>>> {
    let commit = commit.to_ascii_lowercase();
    if !is_id(&commit) {
        return Err(Error::RustError("not a commit id".into()));
    }
    let advertised = refs(remote, "git-upload-pack").await?;
    let fetched = fetch_with(
        remote,
        &advertised.caps,
        std::slice::from_ref(&commit),
        &[],
        Some("blob:none"),
        Some(1),
    )
    .await?;
    let objects = crate::pack::objects(&fetched.pack).map_err(Error::RustError)?;
    paths
        .iter()
        .map(|p| crate::pack::path_id(&objects, &commit, p).map_err(Error::RustError))
        .collect()
}

/// One ref update: from `old` (or [`ZERO`] to create) to `new`.
pub struct Update {
    pub name: String,
    pub old: String,
    pub new: String,
}

/// Pushes `updates` and `pack` to `remote`; answers each ref's status line from the remote,
/// such as `ok refs/heads/main` or `ng refs/heads/main non-fast-forward`.
pub async fn push(
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
        if status == 401
            && let Some(ms) = waits.next()
        {
            worker::Delay::from(std::time::Duration::from_millis(*ms)).await;
            continue;
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
pub fn base64(bytes: &[u8]) -> String {
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

// ---- the start of a push, as a proxy reads it ----

/// How long the command section of a push may be, at most.
pub const COMMANDS_MAX: usize = 64 * 1024;

/// One ref update a push asks for: `old` to `new`, either all zeros for none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub old: String,
    pub new: String,
    pub name: String,
}

/// The command section of a `git-receive-pack` request.
#[derive(Debug)]
pub struct Commands {
    pub commands: Vec<Command>,
    /// The capabilities the client asked for, from the first command.
    pub caps: Vec<String>,
    /// Where the flush closing the commands ends.
    pub end: usize,
}

/// The pkt-line at `at`: `Ok(None)` when `data` ends first; else its data (`None` for a
/// flush) and where the next starts.
#[allow(clippy::type_complexity)]
fn next_pkt(data: &[u8], at: usize) -> std::result::Result<Option<(Option<&[u8]>, usize)>, String> {
    let Some(head) = data.get(at..at + 4) else {
        return Ok(None);
    };
    let bad = || "a malformed pkt-line length".to_string();
    if !head.iter().all(u8::is_ascii_hexdigit) {
        return Err(bad());
    }
    let len = usize::from_str_radix(std::str::from_utf8(head).map_err(|_| bad())?, 16)
        .map_err(|_| bad())?;
    match len {
        0 => Ok(Some((None, at + 4))),
        1..=3 => Err("an unexpected pkt-line delimiter".into()),
        _ if at + len > data.len() => Ok(None),
        _ => Ok(Some((Some(&data[at + 4..at + len]), at + len))),
    }
}

/// Whether `s` is a SHA-1 object ID, in lowercase hex.
pub fn is_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads the command section at the start of a `git-receive-pack` request body: `Ok(None)`
/// while more bytes are needed. Shallow lines before it are skipped; a signed push, a section
/// over [`COMMANDS_MAX`] or anything malformed is an error.
pub fn commands(data: &[u8]) -> std::result::Result<Option<Commands>, String> {
    let mut at = 0;
    let mut commands = Vec::new();
    let mut caps = Vec::new();
    loop {
        let Some((line, next)) = next_pkt(data, at)? else {
            if data.len() > COMMANDS_MAX {
                return Err("the push's ref updates are too long".into());
            }
            return Ok(None);
        };
        if next > COMMANDS_MAX {
            return Err("the push's ref updates are too long".into());
        }
        at = next;
        let Some(line) = line else {
            if commands.is_empty() {
                return Err("the push has no ref updates".into());
            }
            return Ok(Some(Commands {
                commands,
                caps,
                end: at,
            }));
        };
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        if line.starts_with(b"push-cert") {
            return Err("signed pushes are not accepted".into());
        }
        let line =
            std::str::from_utf8(line).map_err(|_| "a ref update is not UTF-8".to_string())?;
        if commands.is_empty() && line.starts_with("shallow ") {
            continue;
        }
        let body = match line.split_once('\0') {
            Some((body, asked)) if commands.is_empty() => {
                caps = asked
                    .split(' ')
                    .filter(|c| !c.is_empty())
                    .map(String::from)
                    .collect();
                body
            }
            Some(_) => return Err("a ref update is malformed".into()),
            None => line,
        };
        let mut parts = body.splitn(3, ' ');
        let (old, new, name) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        if !is_id(old) || !is_id(new) || name.is_empty() || name.contains(' ') {
            return Err("a ref update is malformed".into());
        }
        commands.push(Command {
            old: old.into(),
            new: new.into(),
            name: name.into(),
        });
    }
}

/// Where the pack begins in a push whose commands are `c`: past the push options, when the
/// client sends them. `Ok(None)` while more bytes are needed.
pub fn pack_start(data: &[u8], c: &Commands) -> std::result::Result<Option<usize>, String> {
    let mut at = c.end;
    if c.caps.iter().any(|x| x == "push-options") {
        loop {
            let Some((line, next)) = next_pkt(data, at)? else {
                return Ok(None);
            };
            at = next;
            if line.is_none() {
                break;
            }
        }
    }
    Ok(Some(at))
}

/// A `git-receive-pack` result refusing the whole push, so `git push` prints why: each ref in
/// `refused` with its reason, the others as not pushed. In side-band when the client asked for
/// it. Empty when the client asked for no report (report-status or report-status-v2) and no
/// side-band: the caller answers an HTTP error instead.
pub fn refusal(commands: &[Command], caps: &[String], refused: &[(String, String)]) -> Vec<u8> {
    let has = |c: &str| caps.iter().any(|x| x == c);
    let clean = |s: &str| s.replace(['\n', '\r'], " ");
    let first = refused
        .first()
        .map_or_else(|| "the push is refused".to_string(), |(_, why)| clean(why));
    let mut report = Vec::new();
    // Git 2.29 and later ask for report-status-v2; its `ng <ref> <reason>` lines are v1's.
    if has("report-status") || has("report-status-v2") {
        report.extend(pkt(b"unpack ok\n"));
        for c in commands {
            let why = refused
                .iter()
                .find(|(name, _)| *name == c.name)
                .map_or_else(
                    || "not pushed: another ref was refused".into(),
                    |(_, w)| clean(w),
                );
            report.extend(pkt(format!("ng {} {why}\n", c.name).as_bytes()));
        }
        report.extend(b"0000");
    }
    let band = if has("side-band-64k") {
        65_515
    } else if has("side-band") {
        995
    } else {
        return report;
    };
    let mut out = Vec::new();
    out.extend(pkt(
        &[&[2u8][..], format!("pigeon: {first}\n").as_bytes()].concat()
    ));
    for chunk in report.chunks(band) {
        out.extend(pkt(&[&[1u8][..], chunk].concat()));
    }
    out.extend(b"0000");
    out
}

// ---- what a fetch asks for, as a proxy reads it ----

/// The most an upload-pack request body may be, when its wants are checked.
pub const UPLOAD_REQUEST_MAX: usize = 64 * 1024;

/// What an upload-pack request asks for.
#[derive(Debug, PartialEq, Eq)]
pub struct UploadRequest {
    /// Protocol v2's command, such as `fetch` or `ls-refs`; `None` for protocol v0.
    pub command: Option<String>,
    /// Every object ID it wants, lowercase.
    pub wants: Vec<String>,
}

/// What a complete `git-upload-pack` request body asks for, in protocol v0 or v2. A want of a
/// ref by name (`want-ref`), or anything malformed, is an error.
pub fn upload_request(body: &[u8]) -> std::result::Result<UploadRequest, String> {
    let bad = |what: &str| format!("the request is malformed: {what}");
    if body.len() > UPLOAD_REQUEST_MAX {
        return Err(format!(
            "the request is too large: over {UPLOAD_REQUEST_MAX} bytes"
        ));
    }
    let mut command: Option<String> = None;
    let mut wants = Vec::new();
    let mut seen_delim = false;
    let mut i = 0usize;
    while i < body.len() {
        let head = body
            .get(i..i + 4)
            .ok_or_else(|| bad("bytes after the last pkt-line"))?;
        let mut len = 0usize;
        for &b in head {
            let d = (b as char)
                .to_digit(16)
                .ok_or_else(|| bad("a pkt-line length is not hex"))?;
            len = len * 16 + d as usize;
        }
        match len {
            0 | 2 => {
                i += 4;
                continue;
            }
            1 => {
                seen_delim = true;
                i += 4;
                continue;
            }
            3 => return Err(bad("a pkt-line of length 3")),
            _ => {}
        }
        let raw = body
            .get(i + 4..i + len)
            .ok_or_else(|| bad("a pkt-line runs past the end"))?;
        i += len;
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|_| bad("a pkt-line is not UTF-8"))?;
        if let Some(name) = line.strip_prefix("command=") {
            if seen_delim {
                return Err(bad("a command after the delimiter"));
            }
            if command.is_some() {
                return Err(bad("more than one command"));
            }
            command = Some(name.to_string());
        } else if line.starts_with("want-ref ") {
            return Err("want-ref is not accepted".to_string());
        } else if let Some(rest) = line.strip_prefix("want ") {
            let id = if command.is_some() {
                rest
            } else {
                rest.split_whitespace().next().unwrap_or("")
            };
            let id = id.to_ascii_lowercase();
            if !is_id(&id) {
                return Err(bad("a want is not an object ID"));
            }
            wants.push(id);
        } else if line.starts_with("want") {
            return Err(bad("an unknown want line"));
        }
    }
    Ok(UploadRequest { command, wants })
}
