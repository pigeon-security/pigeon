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
/// it. Empty when the client asked for neither report-status nor side-band: the caller answers
/// an HTTP error instead.
pub fn refusal(commands: &[Command], caps: &[String], refused: &[(String, String)]) -> Vec<u8> {
    let has = |c: &str| caps.iter().any(|x| x == c);
    let clean = |s: &str| s.replace(['\n', '\r'], " ");
    let first = refused
        .first()
        .map_or_else(|| "the push is refused".to_string(), |(_, why)| clean(why));
    let mut report = Vec::new();
    if has("report-status") {
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

#[cfg(test)]
mod push_tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn line(s: &str) -> Vec<u8> {
        pkt(s.as_bytes())
    }

    fn body(lines: &[&str]) -> Vec<u8> {
        let mut out: Vec<u8> = lines.iter().flat_map(|l| line(l)).collect();
        out.extend(b"0000");
        out
    }

    #[test]
    fn reads_commands_and_caps() {
        let first = format!("{A} {B} refs/heads/main\0report-status side-band-64k agent=git/2\n");
        let second = format!("{ZERO} {B} refs/heads/claude/x\n");
        let mut data = body(&[
            "shallow 3333333333333333333333333333333333333333\n",
            &first,
            &second,
        ]);
        let end = data.len();
        data.extend(b"PACK");
        let c = commands(&data).unwrap().unwrap();
        assert_eq!(c.end, end);
        assert_eq!(c.caps, ["report-status", "side-band-64k", "agent=git/2"]);
        assert_eq!(
            c.commands,
            [
                Command {
                    old: A.into(),
                    new: B.into(),
                    name: "refs/heads/main".into()
                },
                Command {
                    old: ZERO.into(),
                    new: B.into(),
                    name: "refs/heads/claude/x".into()
                },
            ]
        );
        assert_eq!(pack_start(&data, &c).unwrap(), Some(end));
    }

    #[test]
    fn waits_for_more_bytes() {
        let data = body(&[&format!("{A} {B} refs/heads/x\0report-status")]);
        for cut in [0, 3, 10, data.len() - 1] {
            assert!(commands(&data[..cut]).unwrap().is_none(), "{cut}");
        }
        assert!(commands(&data).unwrap().is_some());
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        let cases: Vec<Vec<u8>> = vec![
            b"00x1".to_vec(),
            b"0003".to_vec(),
            b"0001".to_vec(),
            b"0000".to_vec(),
            body(&[&format!("{} {B} refs/heads/x", "A".repeat(40))]),
            body(&[&format!("{A} {B}")]),
            body(&[&format!("{A} {B} refs/heads/a b")]),
            body(&[
                &format!("{A} {B} refs/heads/x\0caps"),
                &format!("{A} {B} refs/heads/y\0more"),
            ]),
            body(&["push-cert\0report-status"]),
            vec![b'0'; COMMANDS_MAX + 5],
        ];
        for data in cases {
            assert!(
                commands(&data).is_err(),
                "{:?}",
                String::from_utf8_lossy(&data[..data.len().min(60)])
            );
        }
        let long = format!("{A} {B} refs/heads/{}", "x".repeat(60_000));
        let data = body(&[&long, &long]);
        assert!(commands(&data).is_err());
    }

    #[test]
    fn finds_the_pack_past_push_options() {
        let mut data = body(&[&format!("{A} {B} refs/heads/x\0push-options")]);
        let end = data.len();
        data.extend(line("opt1\n"));
        let c = commands(&data).unwrap().unwrap();
        assert_eq!(c.end, end);
        assert_eq!(pack_start(&data, &c).unwrap(), None);
        data.extend(b"0000");
        let start = data.len();
        data.extend(b"PACK");
        assert_eq!(pack_start(&data, &c).unwrap(), Some(start));
    }

    fn cmds() -> Vec<Command> {
        vec![
            Command {
                old: A.into(),
                new: ZERO.into(),
                name: "refs/heads/x".into(),
            },
            Command {
                old: ZERO.into(),
                new: B.into(),
                name: "refs/heads/claude/y".into(),
            },
        ]
    }

    #[test]
    fn a_refusal_reports_every_ref() {
        let caps = vec!["report-status".to_string()];
        let got = refusal(
            &cmds(),
            &caps,
            &[("refs/heads/x".into(), "deletes\nrefused".into())],
        );
        let mut want = line("unpack ok\n");
        want.extend(line("ng refs/heads/x deletes refused\n"));
        want.extend(line(
            "ng refs/heads/claude/y not pushed: another ref was refused\n",
        ));
        want.extend(b"0000");
        assert_eq!(got, want);
        assert!(refusal(&cmds(), &[], &[("refs/heads/x".into(), "no".into())]).is_empty());
    }

    #[test]
    fn a_refusal_goes_in_side_band_when_asked() {
        let plain = refusal(
            &cmds(),
            &["report-status".into()],
            &[("refs/heads/x".into(), "no".into())],
        );
        let caps = vec!["report-status".to_string(), "side-band-64k".to_string()];
        let got = refusal(&cmds(), &caps, &[("refs/heads/x".into(), "no".into())]);
        assert!(got.ends_with(b"0000"));
        let (first, mut at) = read_pkt(&got, 0).unwrap();
        assert_eq!(first.unwrap(), b"\x02pigeon: no\n");
        let mut unframed: Vec<u8> = Vec::new();
        while let (Some(l), next) = read_pkt(&got, at).unwrap() {
            assert_eq!(l[0], 1);
            unframed.extend_from_slice(&l[1..]);
            at = next;
        }
        assert_eq!(unframed, plain);
    }
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

#[cfg(test)]
mod upload_tests {
    use super::*;

    fn p(s: &str) -> Vec<u8> {
        format!("{:04x}{}\n", s.len() + 5, s).into_bytes()
    }

    const A: &str = "0123456789abcdef0123456789abcdef01234567";
    const B: &str = "fedcba9876543210fedcba9876543210fedcba98";

    #[test]
    fn reads_a_v0_fetch() {
        let b = [
            p(&format!("want {A} multi_ack ofs-delta")),
            p(&format!("want {B}")),
            b"0000".to_vec(),
            p("done"),
        ]
        .concat();
        let r = upload_request(&b).unwrap();
        assert_eq!(r.command, None);
        assert_eq!(r.wants, [A, B]);
    }

    #[test]
    fn reads_a_v2_fetch_and_ls_refs() {
        let b = [
            p("command=fetch"),
            p("agent=git/2.4"),
            p("object-format=sha1"),
            b"0001".to_vec(),
            p("thin-pack"),
            p(&format!("want {A}")),
            p("done"),
            b"0000".to_vec(),
        ]
        .concat();
        let r = upload_request(&b).unwrap();
        assert_eq!(r.command.as_deref(), Some("fetch"));
        assert_eq!(r.wants, [A]);
        let b = [
            p("command=ls-refs"),
            b"0001".to_vec(),
            p("peel"),
            p("ref-prefix refs/heads/"),
            b"0000".to_vec(),
        ]
        .concat();
        let r = upload_request(&b).unwrap();
        assert_eq!(r.command.as_deref(), Some("ls-refs"));
        assert!(r.wants.is_empty());
        let r = upload_request(&p(&format!("want {}", A.to_uppercase()))).unwrap();
        assert_eq!(r.wants, [A]);
    }

    #[test]
    fn refuses_what_it_cannot_check() {
        for b in [
            p("want-ref refs/heads/main"),
            p(&format!("wantx {A}")),
            p("want 1234"),
            b"0003".to_vec(),
            b"0010want".to_vec(),
            [p("done"), b"xy".to_vec()].concat(),
            [p("command=fetch"), p("command=ls-refs")].concat(),
            [b"0001".to_vec(), p("command=fetch")].concat(),
            vec![b'0'; UPLOAD_REQUEST_MAX + 4],
            b"0007\xff\xfe\n".to_vec(),
        ] {
            assert!(
                upload_request(&b).is_err(),
                "{:?}",
                String::from_utf8_lossy(&b[..b.len().min(40)])
            );
        }
    }
}
