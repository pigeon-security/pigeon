//! `pigeon-sandboxd`: a tiny HTTP/1.1 exec server for an AI agent's sandbox.
//!
//! It serves files, commands and git for one checkout. It handles one connection at a time.
//! Every operation is `POST /op/<name>` with a JSON body.

use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// A JSON object, as sent in request bodies.
type JsonObject = Map<String, Value>;
/// The result of an operation: a JSON answer or an HTTP error.
type Reply = Result<Value, ApiError>;

/// Largest request header block we accept.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Largest request body we accept.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Socket read and write timeout.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
/// Total time allowed to receive one whole request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(120);
/// Time limit for every git child.
const GIT_TIMEOUT: Duration = Duration::from_secs(300);
/// Largest file that `read` and `edit` will open.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Most lines one `read` may return.
const MAX_READ_LINES: u64 = 2000;
/// Most entries one `list` may return.
const MAX_LIST_ENTRIES: usize = 1000;
/// Most entries `list` gathers before it stops walking.
const LIST_COLLECT_CAP: usize = 20_000;
/// Most lines one `grep` may return.
const MAX_GREP_LINES: usize = 200;
/// Longest single grep line we keep, in bytes.
const MAX_GREP_LINE_BYTES: usize = 2000;
/// How much of a command log is echoed back.
const OUTPUT_TAIL: usize = 16 * 1024;
/// How much child output we keep in memory.
const CAPTURE_CAP: usize = 8 * 1024 * 1024;
/// Directory that holds command logs.
const OUT_DIR: &str = "/tmp/out";
/// Names of all operations.
const OPS: [&str; 10] = [
    "prepare",
    "fetch",
    "read",
    "write",
    "edit",
    "list",
    "grep",
    "exec",
    "commit_push",
    "diff",
];
/// Directory names that `list` never shows.
const SKIP_DIRS: [&str; 3] = [".git", "target", "node_modules"];
/// Counter for command log file names.
static LOG_COUNTER: AtomicU64 = AtomicU64::new(1);
/// The repository URL `prepare` was given. Every git command that carries the credential names
/// this URL itself, never a remote from the checkout's config, which the run can change.
static REPO_URL: Mutex<Option<String>> = Mutex::new(None);

/// Settings read once from the environment at start-up.
struct Config {
    /// The shared secret callers must present.
    key: String,
    /// The TCP port to listen on.
    port: u16,
    /// The checkout directory.
    root: PathBuf,
    /// Optional token for git over HTTP.
    git_auth: Option<String>,
}

/// An HTTP error: a status code and a message for the JSON body.
struct ApiError {
    /// The HTTP status code.
    code: u16,
    /// A short human-readable message.
    msg: String,
}

/// Builds a 400 error.
fn bad(m: impl Into<String>) -> ApiError {
    ApiError {
        code: 400,
        msg: m.into(),
    }
}

/// Builds a 500 error.
fn fail(m: impl Into<String>) -> ApiError {
    ApiError {
        code: 500,
        msg: m.into(),
    }
}

/// Builds a 401 error.
fn unauthorized() -> ApiError {
    ApiError {
        code: 401,
        msg: "unauthorized".to_string(),
    }
}

/// Builds a 404 error.
fn not_found() -> ApiError {
    ApiError {
        code: 404,
        msg: "not found".to_string(),
    }
}

/// Builds a 413 error.
fn too_large(m: impl Into<String>) -> ApiError {
    ApiError {
        code: 413,
        msg: m.into(),
    }
}

/// Reads and checks the environment. Returns a message on failure.
fn load_config() -> Result<Config, String> {
    let key = std::env::var("PIGEON_SANDBOX_KEY")
        .map_err(|_| "PIGEON_SANDBOX_KEY is required".to_string())?;
    if key.chars().count() < 32 {
        return Err("PIGEON_SANDBOX_KEY must be at least 32 characters".to_string());
    }
    let port = match std::env::var("PIGEON_SANDBOX_PORT") {
        Ok(v) => v
            .trim()
            .parse::<u16>()
            .map_err(|_| "PIGEON_SANDBOX_PORT is not a valid port".to_string())?,
        Err(_) => 8080,
    };
    let root = std::env::var_os("PIGEON_SANDBOX_ROOT")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/work"));
    let git_auth = match std::env::var("PIGEON_GIT_AUTH") {
        Ok(v) if !v.is_empty() => {
            if v.chars().any(|c| c.is_control()) {
                return Err("PIGEON_GIT_AUTH must not contain control characters".to_string());
            }
            Some(v)
        }
        _ => None,
    };
    Ok(Config {
        key,
        port,
        root,
        git_auth,
    })
}

/// Starts the server and handles connections one at a time.
fn main() {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(m) => {
            eprintln!("pigeon-sandboxd: {m}");
            std::process::exit(2);
        }
    };
    let listener = match TcpListener::bind(("0.0.0.0", cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pigeon-sandboxd: cannot bind port {}: {e}", cfg.port);
            std::process::exit(1);
        }
    };
    eprintln!("pigeon-sandboxd: listening on 0.0.0.0:{}", cfg.port);
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_conn(&cfg, stream)
                }));
                if run.is_err() {
                    eprintln!("pigeon-sandboxd: a handler panicked");
                }
            }
            Err(e) => {
                eprintln!("pigeon-sandboxd: accept failed: {e}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ---------------------------------------------------------------- HTTP ----

/// A parsed request line and its headers.
struct Head {
    /// The HTTP method, such as `GET`.
    method: String,
    /// The request target, such as `/health`.
    target: String,
    /// Header pairs, with names in lower case.
    headers: Vec<(String, String)>,
}

impl Head {
    /// Returns the first header with this lower-case name.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Compares two byte strings without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    std::hint::black_box(diff) == 0
}

/// Checks the bearer token. Answers 401 when it is missing or wrong.
fn authorize(cfg: &Config, head: &Head) -> Result<(), ApiError> {
    let value = head.header("authorization").ok_or_else(unauthorized)?;
    let (scheme, token) = value.split_once(' ').ok_or_else(unauthorized)?;
    let same = constant_time_eq(token.trim().as_bytes(), cfg.key.as_bytes());
    if scheme.eq_ignore_ascii_case("bearer") && same {
        Ok(())
    } else {
        Err(unauthorized())
    }
}

/// Finds the first place where `needle` starts in `hay`.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parses the request line and headers.
fn parse_head(raw: &[u8]) -> Result<Head, ApiError> {
    let text = std::str::from_utf8(raw).map_err(|_| bad("headers must be UTF-8"))?;
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let parts: Vec<&str> = first.split(' ').collect();
    if parts.len() != 3 || !parts[2].starts_with("HTTP/1.") {
        return Err(bad("malformed request line"));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| bad("malformed header line"))?;
        if name.is_empty() || name.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(bad("malformed header name"));
        }
        let value = value.trim_matches(|c| c == ' ' || c == '\t');
        headers.push((name.to_ascii_lowercase(), value.to_string()));
    }
    Ok(Head {
        method: parts[0].to_string(),
        target: parts[1].to_string(),
        headers,
    })
}

/// Reads the header block. Returns the head and any body bytes already read.
fn read_head(stream: &mut TcpStream, deadline: Instant) -> Result<(Head, Vec<u8>), ApiError> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            if pos + 4 > MAX_HEADER_BYTES {
                return Err(bad("request headers too large"));
            }
            let head = parse_head(&buf[..pos])?;
            return Ok((head, buf[pos + 4..].to_vec()));
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return Err(bad("request headers too large"));
        }
        if Instant::now() >= deadline {
            return Err(bad("request timed out"));
        }
        let n = stream
            .read(&mut chunk)
            .map_err(|_| bad("could not read the request"))?;
        if n == 0 {
            return Err(bad("connection closed before the headers ended"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Returns the Content-Length, if there is one. Rejects odd values.
fn content_length(head: &Head) -> Result<Option<u64>, ApiError> {
    let mut found: Option<u64> = None;
    for (name, value) in &head.headers {
        if name.as_str() == "transfer-encoding" {
            return Err(bad("transfer-encoding is not supported"));
        }
        if name.as_str() != "content-length" {
            continue;
        }
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("invalid Content-Length"));
        }
        let parsed = value.parse::<u64>().unwrap_or(u64::MAX);
        match found {
            Some(prev) if prev != parsed => {
                return Err(bad("conflicting Content-Length headers"));
            }
            _ => found = Some(parsed),
        }
    }
    Ok(found)
}

/// Reads exactly `len` body bytes, starting with what we already have.
fn read_body(
    stream: &mut TcpStream,
    mut body: Vec<u8>,
    len: usize,
    deadline: Instant,
) -> Result<Vec<u8>, ApiError> {
    body.truncate(len);
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < len {
        if Instant::now() >= deadline {
            return Err(bad("request timed out"));
        }
        let want = (len - body.len()).min(chunk.len());
        let n = stream
            .read(&mut chunk[..want])
            .map_err(|_| bad("could not read the request body"))?;
        if n == 0 {
            return Err(bad("connection closed before the body ended"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(body)
}

/// Returns the standard reason phrase for a status code.
fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    }
}

/// Writes the response, closes our side, and drains a little input.
fn send(stream: &mut TcpStream, code: u16, body: &Value) {
    let payload = serde_json::to_vec(body)
        .unwrap_or_else(|_| br#"{"error":"could not encode the reply"}"#.to_vec());
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        reason(code),
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    // Drain unread input so the peer sees our answer, not a reset.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut sink = [0u8; 8192];
    let mut total = 0usize;
    while total < 256 * 1024 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

/// Serves one connection from start to finish.
fn handle_conn(cfg: &Config, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let (code, body) = match respond(cfg, &mut stream, deadline) {
        Ok(v) => (200, v),
        Err(e) => (e.code, json!({ "error": e.msg })),
    };
    send(&mut stream, code, &body);
}

/// Reads one request, checks it, and runs the operation it asks for.
fn respond(cfg: &Config, stream: &mut TcpStream, deadline: Instant) -> Reply {
    let (head, leftover) = read_head(stream, deadline)?;
    if head.method == "GET" && head.target == "/health" {
        return Ok(json!({ "ok": true }));
    }
    authorize(cfg, &head)?;
    let name = match head.target.strip_prefix("/op/") {
        Some(n) if head.method == "POST" && OPS.contains(&n) => n,
        _ => return Err(not_found()),
    };
    let len = match content_length(&head)? {
        Some(l) => l,
        None => return Err(too_large("Content-Length is required")),
    };
    if len > MAX_BODY_BYTES as u64 {
        return Err(too_large("request body is larger than 2 MiB"));
    }
    let body = read_body(stream, leftover, len as usize, deadline)?;
    let value: Value = if body.iter().all(|b| b.is_ascii_whitespace()) {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?
    };
    let obj = match value {
        Value::Object(m) => m,
        _ => return Err(bad("the body must be a JSON object")),
    };
    dispatch(cfg, name, &obj)
}

/// Calls the function for an operation name.
fn dispatch(cfg: &Config, name: &str, o: &JsonObject) -> Reply {
    match name {
        "prepare" => op_prepare(cfg, o),
        "read" => op_read(cfg, o),
        "write" => op_write(cfg, o),
        "edit" => op_edit(cfg, o),
        "list" => op_list(cfg, o),
        "grep" => op_grep(cfg, o),
        "exec" => op_exec(cfg, o),
        "fetch" => op_fetch(cfg, o),
        "commit_push" => op_commit_push(cfg, o),
        "diff" => op_diff(cfg),
        _ => Err(not_found()),
    }
}

// -------------------------------------------------------- field helpers ----

/// Gets a required string field.
fn req_str<'a>(o: &'a JsonObject, k: &str) -> Result<&'a str, ApiError> {
    match o.get(k) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(bad(format!("field `{k}` must be a string"))),
        None => Err(bad(format!("missing field `{k}`"))),
    }
}

/// Gets an optional string field. Null counts as missing.
fn opt_str<'a>(o: &'a JsonObject, k: &str) -> Result<Option<&'a str>, ApiError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(bad(format!("field `{k}` must be a string"))),
    }
}

/// Gets an optional non-negative integer field. Null counts as missing.
fn opt_u64(o: &JsonObject, k: &str) -> Result<Option<u64>, ApiError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| bad(format!("field `{k}` must be a non-negative integer"))),
    }
}

// ----------------------------------------------------------------- paths ----

/// Checks a request path by its text and returns it in clean form. Refuses absolute paths,
/// `..`, NUL bytes and a first component of `.git`.
fn clean_rel(p: &str, allow_empty: bool) -> Result<PathBuf, ApiError> {
    if p.contains('\0') {
        return Err(bad("path must not contain a NUL byte"));
    }
    let mut out = PathBuf::new();
    for c in Path::new(p).components() {
        match c {
            Component::Normal(s) => {
                if out.as_os_str().is_empty() && s == ".git" {
                    return Err(bad("the .git directory is not accessible"));
                }
                out.push(s);
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(bad("path must not contain `..`")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(bad("path must be relative to the root"));
            }
        }
    }
    if out.as_os_str().is_empty() && !allow_empty {
        return Err(bad("path must not be empty"));
    }
    Ok(out)
}

/// Returns the canonical root, or 400 if the root does not exist yet.
fn root_canon(cfg: &Config) -> Result<PathBuf, ApiError> {
    fs::canonicalize(&cfg.root).map_err(|_| bad("sandbox root is not ready; call prepare first"))
}

/// Checks that a canonical path is under the canonical root and not in `.git`.
fn inside_root(root: &Path, canon: &Path) -> Result<(), ApiError> {
    let rel = canon
        .strip_prefix(root)
        .map_err(|_| bad("path escapes the sandbox root"))?;
    if let Some(Component::Normal(first)) = rel.components().next()
        && first == ".git"
    {
        return Err(bad("the .git directory is not accessible"));
    }
    Ok(())
}

/// Resolves an existing path and checks that it stays inside the root.
fn resolve_existing(root: &Path, rel: &Path) -> Result<PathBuf, ApiError> {
    let canon = fs::canonicalize(root.join(rel)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            bad("no such file or directory")
        } else {
            fail(format!("cannot resolve path: {e}"))
        }
    })?;
    inside_root(root, &canon)?;
    Ok(canon)
}

/// Resolves a path that may not exist yet, for writing. The deepest existing part is
/// canonicalized and checked.
fn resolve_for_write(root: &Path, rel: &Path) -> Result<PathBuf, ApiError> {
    let full = root.join(rel);
    let mut anchor = full.clone();
    loop {
        match fs::symlink_metadata(&anchor) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !anchor.pop() {
                    return Err(bad("path has no existing parent"));
                }
            }
            Err(e) => return Err(fail(format!("cannot inspect path: {e}"))),
        }
    }
    let canon =
        fs::canonicalize(&anchor).map_err(|_| bad("path cannot be resolved (a dangling link?)"))?;
    inside_root(root, &canon)?;
    let tail = full
        .strip_prefix(&anchor)
        .map_err(|_| fail("internal path error"))?;
    if tail.as_os_str().is_empty() {
        Ok(canon)
    } else {
        Ok(canon.join(tail))
    }
}

/// Reads a regular file of at most 10 MiB into memory.
fn read_file_bytes(path: &Path) -> Result<Vec<u8>, ApiError> {
    let meta = fs::metadata(path).map_err(|e| fail(format!("cannot stat file: {e}")))?;
    if !meta.is_file() {
        return Err(bad("path is not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(bad("file is larger than 10 MiB"));
    }
    let file = File::open(path).map_err(|e| fail(format!("cannot open file: {e}")))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| fail(format!("cannot read file: {e}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(bad("file is larger than 10 MiB"));
    }
    Ok(bytes)
}

// ------------------------------------------------------------ text tools ----

/// Converts bytes to text. If they start mid-file, skips partial characters.
fn lossy_trim_start(bytes: &[u8], mid_file: bool) -> String {
    let mut slice = bytes;
    if mid_file {
        let skip = slice
            .iter()
            .take(3)
            .take_while(|b| (**b & 0xC0) == 0x80)
            .count();
        slice = &slice[skip..];
    }
    String::from_utf8_lossy(slice).into_owned()
}

/// Returns the last `n` bytes as text, starting at a character boundary.
fn tail_lossy(bytes: &[u8], n: usize) -> String {
    let start = bytes.len().saturating_sub(n);
    lossy_trim_start(&bytes[start..], start > 0)
}

/// Keeps at most `max` bytes from the start, ending at a character boundary.
fn cut_head(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Counts how often `needle` occurs in `hay`, counting overlaps.
fn count_overlapping(hay: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while start <= hay.len() {
        let Some(pos) = hay[start..].find(needle) else {
            break;
        };
        count += 1;
        let abs = start + pos;
        let step = hay[abs..].chars().next().map(char::len_utf8).unwrap_or(1);
        start = abs + step;
    }
    count
}

// ------------------------------------------------------------- processes ----

/// What a finished child left behind.
struct Captured {
    /// The exit code, or None if a signal ended it.
    code: Option<i32>,
    /// Captured standard output.
    stdout: Vec<u8>,
    /// Captured standard error.
    stderr: Vec<u8>,
    /// True if we killed it for running too long.
    timed_out: bool,
}

/// Reads a pipe to the end. Keeps at most `cap` bytes and drops the rest.
fn drain_capped<R: Read>(mut r: R, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < cap {
                    let room = cap - out.len();
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    }
    out
}

/// Kills a whole process group with `kill -KILL -- -<pid>`. If there is no `kill` program,
/// bash's built-in `kill` is used instead.
fn kill_group(pid: u32) {
    let target = format!("-{pid}");
    let ran = Command::new("kill")
        .arg("-KILL")
        .arg("--")
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if ran.is_err() {
        let _ = Command::new("bash")
            .arg("-c")
            .arg("kill -KILL -- \"$0\"")
            .arg(&target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Runs a prepared command with a time limit and captures its output. The child gets its own
/// process group and a null stdin.
fn run_capture(mut cmd: Command, limit: Duration) -> Result<Captured, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start `{program}`: {e}"))?;
    let out_reader = child
        .stdout
        .take()
        .map(|p| thread::spawn(move || drain_capped(p, CAPTURE_CAP)));
    let err_reader = child
        .stderr
        .take()
        .map(|p| thread::spawn(move || drain_capped(p, CAPTURE_CAP)));
    let pid = child.id();
    let deadline = Instant::now() + limit;
    let mut timed_out = false;
    let mut status: Option<ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(_) => {
                kill_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_group(pid);
            let _ = child.kill();
            status = child.wait().ok();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let stdout = out_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = err_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    Ok(Captured {
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
    })
}

// ------------------------------------------------------------------- git ----

/// Hides the git token in a piece of text.
fn redact(cfg: &Config, s: String) -> String {
    match &cfg.git_auth {
        Some(a) if !a.is_empty() => s.replace(a.as_str(), "***"),
        _ => s,
    }
}

/// Builds a git command with a clean environment. Secrets are removed. The git token is added
/// only when `auth` is true, and then only for the prepared repository's URL, with hooks off and
/// TLS verified: the run can write the checkout's `.git` (hooks, remotes, `http.*` settings),
/// so a command that carries the token must not take any of those from it.
fn git_command(cfg: &Config, dir: Option<&Path>, args: &[&str], auth: bool) -> Command {
    let mut c = Command::new("git");
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    c.env_remove("PIGEON_SANDBOX_KEY");
    c.env_remove("PIGEON_GIT_AUTH");
    c.env("GIT_TERMINAL_PROMPT", "0");
    c.env("GIT_ALLOW_PROTOCOL", allowed_protocols());
    let repo = repo_url();
    match (auth, &cfg.git_auth, repo) {
        (true, Some(v), Some(url)) => {
            c.env("GIT_CONFIG_GLOBAL", "/dev/null");
            c.env("GIT_CONFIG_NOSYSTEM", "1");
            c.env("GIT_CONFIG_COUNT", "4");
            c.env("GIT_CONFIG_KEY_0", format!("http.{url}.extraHeader"));
            c.env("GIT_CONFIG_VALUE_0", format!("Authorization: Bearer {v}"));
            c.env("GIT_CONFIG_KEY_1", "core.hooksPath");
            c.env("GIT_CONFIG_VALUE_1", "/dev/null");
            c.env("GIT_CONFIG_KEY_2", "http.sslVerify");
            c.env("GIT_CONFIG_VALUE_2", "true");
            c.env("GIT_CONFIG_KEY_3", "credential.helper");
            c.env("GIT_CONFIG_VALUE_3", "");
        }
        _ => {
            c.env_remove("GIT_CONFIG_COUNT");
        }
    }
    c
}

/// The repository URL `prepare` was given, if it ran.
fn repo_url() -> Option<String> {
    REPO_URL.lock().ok().and_then(|g| g.clone())
}

/// Removes everything inside `root`, not `root` itself.
fn empty_root(root: &Path) -> Result<(), ApiError> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(fail(format!("cannot read root: {e}"))),
    };
    for entry in entries {
        let entry = entry.map_err(|e| fail(format!("cannot read root: {e}")))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| fail(format!("cannot read root: {e}")))?;
        let gone = if kind.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        gone.map_err(|e| fail(format!("cannot empty root: {e}")))?;
    }
    Ok(())
}

/// The transports git may use: http and https only, so `ext::` and the like never run. A local
/// test (`PIGEON_SANDBOX_TEST_FILE_GIT=1`) may also use plain paths.
fn allowed_protocols() -> &'static str {
    if std::env::var("PIGEON_SANDBOX_TEST_FILE_GIT").as_deref() == Ok("1") {
        "https:http:file"
    } else {
        "https:http"
    }
}

/// Runs git and returns its output whatever the exit code. Fails only if git cannot start or
/// runs past the time limit.
fn run_git(
    cfg: &Config,
    dir: Option<&Path>,
    args: &[&str],
    auth: bool,
) -> Result<Captured, ApiError> {
    if auth && let Some(d) = dir {
        check_config_clean(cfg, d)?;
    }
    let cap = run_capture(git_command(cfg, dir, args, auth), GIT_TIMEOUT).map_err(fail)?;
    if cap.timed_out {
        let name = args.first().copied().unwrap_or("?");
        return Err(fail(format!("git {name} timed out")));
    }
    Ok(cap)
}

/// Config keys (lowercased prefixes) that could send a command's traffic, and so the
/// credential, somewhere else, or run a program inside it. The run can write the checkout's
/// config; a command that carries the credential refuses to run while one of these is set there.
const UNSAFE_CONFIG: [&str; 10] = [
    "http.",
    "url.",
    "include.",
    "includeif.",
    "credential",
    "core.hookspath",
    "core.fsmonitor",
    "core.sshcommand",
    "core.gitproxy",
    "core.askpass",
];

/// Refuses when the checkout's config (all the config an authenticated command reads besides
/// sandboxd's own) sets one of [`UNSAFE_CONFIG`].
fn check_config_clean(cfg: &Config, dir: &Path) -> Result<(), ApiError> {
    let mut c = git_command(cfg, Some(dir), &["config", "--list", "--name-only"], false);
    c.env("GIT_CONFIG_GLOBAL", "/dev/null");
    c.env("GIT_CONFIG_NOSYSTEM", "1");
    let cap = run_capture(c, GIT_TIMEOUT).map_err(fail)?;
    let names = String::from_utf8_lossy(&cap.stdout).to_lowercase();
    if let Some(key) = names
        .lines()
        .find(|k| UNSAFE_CONFIG.iter().any(|p| k.starts_with(p)))
    {
        return Err(bad(format!(
            "the checkout's git config sets `{key}`; remove it (git config --unset) and try again"
        )));
    }
    Ok(())
}

/// Runs git and demands exit code 0. Otherwise answers 500 with the stderr tail.
fn git_ok(
    cfg: &Config,
    dir: Option<&Path>,
    args: &[&str],
    auth: bool,
) -> Result<Captured, ApiError> {
    let cap = run_git(cfg, dir, args, auth)?;
    if cap.code != Some(0) {
        let name = args.first().copied().unwrap_or("?");
        let tail = redact(cfg, tail_lossy(&cap.stderr, 2048));
        return Err(fail(format!("git {name} failed: {tail}")));
    }
    Ok(cap)
}

/// Returns the full SHA of HEAD.
fn git_head(cfg: &Config, root: &Path) -> Result<String, ApiError> {
    let cap = git_ok(cfg, Some(root), &["rev-parse", "HEAD"], false)?;
    Ok(String::from_utf8_lossy(&cap.stdout).trim().to_string())
}

/// True for bytes that may appear in a ref name here.
fn ref_byte_ok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'/' || b == b'-'
}

/// Checks the character rules shared by branch and base names.
fn check_ref_chars(name: &str, what: &str) -> Result<(), ApiError> {
    let broken = name.is_empty()
        || name.len() > 200
        || !name.bytes().all(ref_byte_ok)
        || name.starts_with('-')
        || name.starts_with('/')
        || name.contains("..")
        || name.contains("//")
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name
            .split('/')
            .any(|c| c.starts_with('.') || c.ends_with(".lock"));
    if broken {
        Err(bad(format!("invalid {what} name")))
    } else {
        Ok(())
    }
}

/// Checks a branch name. It must start with `claude/`.
fn check_branch(b: &str) -> Result<(), ApiError> {
    if !b.starts_with("claude/") {
        return Err(bad("branch must start with `claude/`"));
    }
    check_ref_chars(b, "branch")
}

/// Turns `base` into a start point: a 40-hex commit or `origin/<base>`.
fn start_point(base: &str) -> Result<String, ApiError> {
    if base.len() == 40 && base.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(base.to_string());
    }
    check_ref_chars(base, "base")?;
    Ok(format!("origin/{base}"))
}

/// Checks a repository URL. Only http and https are allowed (and, in a local test, a plain
/// path).
fn check_repo_url(u: &str) -> Result<(), ApiError> {
    if allowed_protocols().ends_with(":file") && u.starts_with('/') {
        return Ok(());
    }
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or_else(|| bad("repo_url must start with https:// or http://"))?;
    if rest.is_empty()
        || u.len() > 2048
        || u.chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err(bad("invalid repo_url"));
    }
    Ok(())
}

/// Checks a git user name or email.
fn check_ident(label: &str, v: &str) -> Result<(), ApiError> {
    if v.is_empty()
        || v.len() > 200
        || v.starts_with('-')
        || v.chars().any(|c| c.is_control() || c == '<' || c == '>')
    {
        return Err(bad(format!("invalid {label}")));
    }
    Ok(())
}

// ------------------------------------------------------------------- ops ----

/// `prepare`: clone the repo and set up the run's branch, once. A try that fails leaves `root`
/// empty, so the next try is a fresh clone and fails, if it does, for its own reason.
fn op_prepare(cfg: &Config, o: &JsonObject) -> Reply {
    let repo_url = req_str(o, "repo_url")?;
    let base = req_str(o, "base")?;
    let branch = req_str(o, "branch")?;
    let name = req_str(o, "name")?;
    let email = req_str(o, "email")?;
    check_repo_url(repo_url)?;
    check_branch(branch)?;
    let start = start_point(base)?;
    check_ident("name", name)?;
    check_ident("email", email)?;
    if let Ok(mut g) = REPO_URL.lock() {
        *g = Some(repo_url.to_string());
    }
    let root = cfg.root.as_path();
    if root.join(".git").exists() {
        let head = git_head(cfg, root)?;
        return Ok(json!({ "ready": true, "head": head }));
    }
    // Whatever is here is not a checkout: an earlier try's leftovers.
    empty_root(root)?;
    fs::create_dir_all(root).map_err(|e| fail(format!("cannot create root: {e}")))?;
    let root_str = root
        .to_str()
        .ok_or_else(|| fail("root path is not UTF-8"))?;
    // A full clone: a partial one would fetch missing blobs later, without the credential.
    let result = git_ok(
        cfg,
        None,
        &["clone", "--no-tags", "--", repo_url, root_str],
        true,
    )
    .and_then(|_| finish_prepare(cfg, root, repo_url, branch, &start, name, email));
    if result.is_err() {
        // Do not leave a half-prepared checkout that later looks ready, nor files that make the
        // next clone fail.
        let _ = empty_root(root);
    }
    result
}

/// Second half of `prepare`: branch, identity, and resume from the remote.
fn finish_prepare(
    cfg: &Config,
    root: &Path,
    repo_url: &str,
    branch: &str,
    start: &str,
    name: &str,
    email: &str,
) -> Reply {
    git_ok(
        cfg,
        Some(root),
        &["checkout", "-B", branch, start, "--"],
        false,
    )?;
    git_ok(cfg, Some(root), &["config", "user.name", name], false)?;
    git_ok(cfg, Some(root), &["config", "user.email", email], false)?;
    let remote_ref = format!("refs/heads/{branch}");
    let ls = run_git(
        cfg,
        Some(root),
        &["ls-remote", "--exit-code", "--heads", repo_url, &remote_ref],
        true,
    )?;
    match ls.code {
        Some(0) => {
            git_ok(cfg, Some(root), &["fetch", repo_url, &remote_ref], true)?;
            git_ok(cfg, Some(root), &["reset", "--hard", "FETCH_HEAD"], false)?;
        }
        Some(2) => {}
        _ => {
            let tail = redact(cfg, tail_lossy(&ls.stderr, 2048));
            return Err(fail(format!("git ls-remote failed: {tail}")));
        }
    }
    let head = git_head(cfg, root)?;
    Ok(json!({ "ready": true, "head": head }))
}

/// `fetch`: fetch one branch (or a full commit id) of the repository into the checkout, with the
/// credential, which the run's own commands never get. A branch lands at `origin/<name>`.
fn op_fetch(cfg: &Config, o: &JsonObject) -> Reply {
    let name = req_str(o, "ref")?;
    let root = cfg.root.as_path();
    if !root.join(".git").exists() {
        return Err(bad("sandbox is not prepared; call prepare first"));
    }
    let url = repo_url().ok_or_else(|| bad("sandbox is not prepared; call prepare first"))?;
    let is_commit = name.len() == 40 && name.bytes().all(|b| b.is_ascii_hexdigit());
    let spec = if is_commit {
        name.to_string()
    } else {
        check_ref_chars(name, "ref")?;
        let short = name.strip_prefix("refs/heads/").unwrap_or(name);
        format!("+refs/heads/{short}:refs/remotes/origin/{short}")
    };
    git_ok(cfg, Some(root), &["fetch", "--no-tags", &url, &spec], true)?;
    let cap = git_ok(cfg, Some(root), &["rev-parse", "FETCH_HEAD"], false)?;
    let commit = String::from_utf8_lossy(&cap.stdout).trim().to_string();
    let at = if is_commit {
        "FETCH_HEAD".to_string()
    } else {
        format!(
            "origin/{}",
            name.strip_prefix("refs/heads/").unwrap_or(name)
        )
    };
    Ok(json!({ "commit": commit, "at": at }))
}

/// `read`: return numbered lines of a file.
fn op_read(cfg: &Config, o: &JsonObject) -> Reply {
    let rel = clean_rel(req_str(o, "path")?, false)?;
    let from = opt_u64(o, "from_line")?.unwrap_or(1);
    if from < 1 {
        return Err(bad("from_line must be at least 1"));
    }
    let max_to = from.saturating_add(MAX_READ_LINES - 1);
    let to = match opt_u64(o, "to_line")? {
        Some(t) => t.min(max_to),
        None => max_to,
    };
    if to < from {
        return Err(bad("to_line must not be before from_line"));
    }
    let root = root_canon(cfg)?;
    let path = resolve_existing(&root, &rel)?;
    let bytes = read_file_bytes(&path)?;
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    let start = usize::try_from(from - 1).unwrap_or(usize::MAX);
    let end = usize::try_from(to).unwrap_or(usize::MAX).min(total);
    let mut out = String::new();
    if start < end {
        for (i, line) in text.lines().enumerate().skip(start).take(end - start) {
            out.push_str(&(i + 1).to_string());
            out.push('\t');
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(json!({ "text": out, "lines": total, "from": from, "to": end }))
}

/// `write`: create or replace a file.
fn op_write(cfg: &Config, o: &JsonObject) -> Reply {
    let rel = clean_rel(req_str(o, "path")?, false)?;
    let content = req_str(o, "content")?;
    let root = root_canon(cfg)?;
    let target = resolve_for_write(&root, &rel)?;
    if target.is_dir() {
        return Err(bad("path is a directory"));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| fail(format!("cannot create directories: {e}")))?;
        let canon =
            fs::canonicalize(parent).map_err(|e| fail(format!("cannot resolve parent: {e}")))?;
        inside_root(&root, &canon)?;
    }
    fs::write(&target, content.as_bytes()).map_err(|e| fail(format!("write failed: {e}")))?;
    Ok(json!({ "bytes": content.len() }))
}

/// `edit`: replace one exact occurrence of `old` with `new`.
fn op_edit(cfg: &Config, o: &JsonObject) -> Reply {
    let rel = clean_rel(req_str(o, "path")?, false)?;
    let old = req_str(o, "old")?;
    let new = req_str(o, "new")?;
    if old.is_empty() {
        return Err(bad("`old` must not be empty"));
    }
    let root = root_canon(cfg)?;
    let path = resolve_existing(&root, &rel)?;
    let bytes = read_file_bytes(&path)?;
    let text = String::from_utf8(bytes).map_err(|_| bad("file is not valid UTF-8; cannot edit"))?;
    let count = count_overlapping(&text, old);
    if count != 1 {
        return Err(bad(format!(
            "`old` occurs {count} times; it must occur exactly once"
        )));
    }
    let updated = text.replacen(old, new, 1);
    fs::write(&path, updated.as_bytes()).map_err(|e| fail(format!("write failed: {e}")))?;
    Ok(json!({ "ok": true }))
}

/// Walks a directory to a given depth and collects entry names. Returns true if it stopped
/// early because `cap` was reached.
fn walk(dir: &Path, prefix: &str, level: u32, max: u32, out: &mut Vec<String>, cap: usize) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    let mut items: Vec<(String, OsString, bool)> = Vec::new();
    for ent in rd.flatten() {
        let raw = ent.file_name();
        let shown = raw.to_string_lossy().into_owned();
        if SKIP_DIRS.contains(&shown.as_str()) {
            continue;
        }
        let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        items.push((shown, raw, is_dir));
    }
    items.sort();
    for (shown, raw, is_dir) in items {
        if out.len() >= cap {
            return true;
        }
        let rel = if prefix.is_empty() {
            shown
        } else {
            format!("{prefix}/{shown}")
        };
        if is_dir {
            out.push(format!("{rel}/"));
            if level < max && walk(&dir.join(&raw), &rel, level + 1, max, out, cap) {
                return true;
            }
        } else {
            out.push(rel);
        }
    }
    false
}

/// `list`: show a directory tree, sorted and capped.
fn op_list(cfg: &Config, o: &JsonObject) -> Reply {
    let rel = clean_rel(opt_str(o, "path")?.unwrap_or("."), true)?;
    let depth = opt_u64(o, "depth")?.unwrap_or(2).clamp(1, 4) as u32;
    let root = root_canon(cfg)?;
    let dir = resolve_existing(&root, &rel)?;
    if !dir.is_dir() {
        return Err(bad("path is not a directory"));
    }
    let prefix = dir
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut entries: Vec<String> = Vec::new();
    let hit_cap = walk(&dir, &prefix, 1, depth, &mut entries, LIST_COLLECT_CAP);
    entries.sort();
    let truncated = hit_cap || entries.len() > MAX_LIST_ENTRIES;
    entries.truncate(MAX_LIST_ENTRIES);
    Ok(json!({ "entries": entries, "truncated": truncated }))
}

/// Checks a grep glob. It becomes a git pathspec, so magic prefixes are refused.
fn check_glob(g: &str) -> Result<(), ApiError> {
    let broken = g.is_empty()
        || g.contains('\0')
        || g.starts_with(':')
        || g.starts_with('/')
        || g.split('/').any(|c| c == "..")
        || g.split('/').next() == Some(".git");
    if broken {
        Err(bad("invalid glob"))
    } else {
        Ok(())
    }
}

/// `grep`: search tracked files with `git grep`, no shell.
fn op_grep(cfg: &Config, o: &JsonObject) -> Reply {
    let pattern = req_str(o, "pattern")?;
    if pattern.is_empty() || pattern.contains('\0') {
        return Err(bad("invalid pattern"));
    }
    let path = opt_str(o, "path")?;
    let glob = opt_str(o, "glob")?;
    if path.is_some() && glob.is_some() {
        return Err(bad("give either `path` or `glob`, not both"));
    }
    let root = root_canon(cfg)?;
    let mut spec: Option<String> = None;
    if let Some(p) = path {
        let rel = clean_rel(p, true)?;
        if !rel.as_os_str().is_empty() {
            spec = Some(format!(":(literal){}", rel.to_string_lossy()));
        }
    }
    if let Some(g) = glob {
        check_glob(g)?;
        spec = Some(g.to_string());
    }
    let mut args: Vec<&str> = vec!["grep", "-n", "-I", "-E", "--no-color", "-e", pattern];
    if let Some(s) = &spec {
        args.push("--");
        args.push(s.as_str());
    }
    let cap = run_git(cfg, Some(&root), &args, false)?;
    match cap.code {
        Some(0) | Some(1) => {}
        _ => {
            let tail = redact(cfg, tail_lossy(&cap.stderr, 2048));
            return Err(fail(format!("git grep failed: {tail}")));
        }
    }
    let text = String::from_utf8_lossy(&cap.stdout);
    let all: Vec<&str> = text.lines().collect();
    let truncated = all.len() > MAX_GREP_LINES;
    let matches: Vec<String> = all
        .iter()
        .take(MAX_GREP_LINES)
        .map(|l| cut_head(l, MAX_GREP_LINE_BYTES))
        .collect();
    Ok(json!({ "count": matches.len(), "matches": matches, "truncated": truncated }))
}

/// Opens a fresh numbered log file under `/tmp/out`. Never reuses a name.
fn open_log() -> Result<(File, PathBuf), ApiError> {
    fs::create_dir_all(OUT_DIR).map_err(|e| fail(format!("cannot create {OUT_DIR}: {e}")))?;
    for _ in 0..10_000 {
        let n = LOG_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(OUT_DIR).join(format!("{n}.log"));
        match OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fail(format!("cannot create log file: {e}"))),
        }
    }
    Err(fail("cannot find a free log file name"))
}

/// Reads the last 16 KiB of a log as text.
fn log_tail(path: &Path, size: u64) -> String {
    let start = size.saturating_sub(OUTPUT_TAIL as u64);
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if f.take(OUTPUT_TAIL as u64).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    lossy_trim_start(&bytes, start > 0)
}

/// `exec`: run a command with `bash -c` (not a login shell: Debian's /etc/profile would reset
/// PATH and drop the image's cargo and node) and log its output to a file.
fn op_exec(cfg: &Config, o: &JsonObject) -> Reply {
    let cmd = req_str(o, "cmd")?;
    if cmd.is_empty() || cmd.contains('\0') {
        return Err(bad("invalid cmd"));
    }
    let secs = opt_u64(o, "timeout_s")?.unwrap_or(600).clamp(1, 1800);
    let root = root_canon(cfg)?;
    let (log, path) = open_log()?;
    let log_err = log
        .try_clone()
        .map_err(|e| fail(format!("cannot share log file: {e}")))?;
    // A leading dash would be read by bash as an option, so pad it.
    let script = if cmd.starts_with('-') {
        format!(" {cmd}")
    } else {
        cmd.to_string()
    };
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(&script)
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .env_remove("PIGEON_SANDBOX_KEY")
        .env_remove("PIGEON_GIT_AUTH")
        .process_group(0);
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| fail(format!("cannot start bash: {e}")))?;
    drop(command);
    let pid = child.id();
    let deadline = started + Duration::from_secs(secs);
    let mut timed_out = false;
    let status: Option<ExitStatus> = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => {
                kill_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(format!("cannot wait for the command: {e}")));
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_group(pid);
            let _ = child.kill();
            break child.wait().ok();
        }
        thread::sleep(Duration::from_millis(100));
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let output = log_tail(&path, size);
    Ok(json!({
        "exit": status.and_then(|s| s.code()),
        "timed_out": timed_out,
        "ms": ms,
        "output": output,
        "bytes": size,
        "saved_at": path.to_string_lossy(),
    }))
}

/// `commit_push`: stage everything, commit, and push without force.
fn op_commit_push(cfg: &Config, o: &JsonObject) -> Reply {
    let message = req_str(o, "message")?;
    let branch = req_str(o, "branch")?;
    if message.trim().is_empty() || message.contains('\0') {
        return Err(bad("invalid message"));
    }
    check_branch(branch)?;
    let root = cfg.root.as_path();
    if !root.join(".git").exists() {
        return Err(bad("sandbox is not prepared; call prepare first"));
    }
    git_ok(cfg, Some(root), &["add", "-A"], false)?;
    let staged = run_git(cfg, Some(root), &["diff", "--cached", "--quiet"], false)?;
    match staged.code {
        Some(0) => {
            let head = git_head(cfg, root)?;
            return Ok(json!({ "commit": head, "pushed": false, "nothing": true }));
        }
        Some(1) => {}
        _ => {
            let tail = redact(cfg, tail_lossy(&staged.stderr, 2048));
            return Err(fail(format!("git diff failed: {tail}")));
        }
    }
    git_ok(cfg, Some(root), &["commit", "-q", "-m", message], false)?;
    let target = format!("HEAD:refs/heads/{branch}");
    let url = repo_url().ok_or_else(|| bad("sandbox is not prepared; call prepare first"))?;
    let push = run_git(cfg, Some(root), &["push", &url, &target], true)?;
    if push.code != Some(0) {
        let tail = redact(cfg, tail_lossy(&push.stderr, 2048));
        return Err(fail(format!("git push failed: {tail}")));
    }
    let head = git_head(cfg, root)?;
    Ok(json!({ "commit": head, "pushed": true }))
}

/// `diff`: show the change summary and the short status.
fn op_diff(cfg: &Config) -> Reply {
    let root = cfg.root.as_path();
    if !root.join(".git").exists() {
        return Err(bad("sandbox is not prepared; call prepare first"));
    }
    let stat = git_ok(
        cfg,
        Some(root),
        &["diff", "--no-color", "--stat", "HEAD"],
        false,
    )?;
    let status = git_ok(cfg, Some(root), &["status", "--porcelain"], false)?;
    Ok(json!({
        "stat": cut_head(&String::from_utf8_lossy(&stat.stdout), 16 * 1024),
        "status": cut_head(&String::from_utf8_lossy(&status.stdout), 16 * 1024),
    }))
}
