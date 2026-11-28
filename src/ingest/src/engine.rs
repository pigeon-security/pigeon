//! One run, start to end (decision 13): list the source in key order, read each object once
//! into the spool while hashing, open zips, decide each file against R2, copy what is new,
//! and write manifest parts, checkpoints, the status and the report. Resumes from the last
//! checkpoint. Implemented over traits, so the runner gives it HTTPS and a spool directory and
//! the smoke suite gives it memory.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{Budget, Flag, Zip, is_zip};
use crate::decide::{Head, Lookup, Reason, Verdict, decide};
use crate::manifest::{
    Checkpoint, Counters, Hint, JobSpec, ManifestLine, Member, Origin, PART_BYTES, PART_LINES,
    Report, RunState, SourceSpec, Status, checkpoint_key, line_id, part_key, quarantine_key,
    report_key, status_key,
};
use crate::metrics::{Hasher, Kind, Metrics, hamming, normalise};
use crate::timing::{Stage, Stages};
use crate::{Error, blob_key};

/// One listed object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    pub modified: Option<String>,
}

/// One page of a listing, in key order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub objects: Vec<Object>,
    /// Pass back as `token` for the next page; none at the end.
    pub next: Option<String>,
}

/// What a run reads from. Read-only by construction: it has no write method.
pub trait Source: Sync {
    /// One page of objects after `after` (exclusive; ListObjectsV2 `start-after`), or the page
    /// `token` continues. Keys come in ascending byte order.
    fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error>;
    /// The object's bytes, streamed.
    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error>;
}

/// Where a run writes: the instance's R2 bucket.
pub trait Dest: Sync {
    /// The object at `key`: its length and `sha256`/`blake3` metadata, or none.
    fn head(&self, key: &str) -> Result<Option<Head>, Error>;
    /// [`Dest::head`] for many keys at once, in order: a hint the engine may use in place of one
    /// HEAD per blob. The default asks one at a time.
    fn head_many(&self, keys: &[String]) -> Result<Vec<Option<Head>>, Error> {
        keys.iter().map(|k| self.head(k)).collect()
    }
    /// Writes `size` bytes from `body` at `key` with `metrics` as metadata
    /// (`x-amz-meta-sha256`, `-blake3`, `-size`, `-mime`); a single PUT also sends
    /// `x-amz-checksum-sha256`, larger ones go multipart. The body may fail its read at the end
    /// when the bytes do not match `metrics` (see [`Verify`]): the write must then not complete.
    /// The write is create-only: when `key` already holds the same blob it answers
    /// [`Error::Exists`] and leaves it.
    fn put_blob(
        &self,
        key: &str,
        body: &mut dyn Read,
        size: u64,
        metrics: &Metrics,
    ) -> Result<(), Error>;
    /// Writes a small object (a manifest part, a checkpoint, the status, the report).
    fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), Error>;
    /// Reads a small object, or none.
    fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>, Error>;
}

/// A spooled file: written once, then rewound and read (and seeked, for zips).
pub trait SpoolFile: Read + Write + Seek + Send {}
impl<T: Read + Write + Seek + Send> SpoolFile for T {}

/// Where files wait between reading and writing.
pub trait Spool: Sync {
    /// A new empty spool file, removed when dropped.
    fn create(&self) -> Result<Box<dyn SpoolFile>, Error>;
    /// Bytes still free. Objects larger than this are hashed while streaming and read a second
    /// time to copy, never spooled or opened (flag `over_spool`).
    fn free_bytes(&self) -> u64;
    /// The largest object staged in memory rather than in a spool file (1 MiB by default).
    fn memory_limit(&self) -> u64 {
        MEMORY_BYTES
    }
    /// The most bytes staged in memory at once; past it objects go to spool files. Unbounded by
    /// default (the memory limit and the in-flight window bound it).
    fn memory_budget(&self) -> u64 {
        u64::MAX
    }
}

/// The time, given, so runs are reproducible in tests.
pub trait Clock: Sync {
    fn now_ms(&self) -> i64;
    /// A monotonic clock in microseconds, for stage timings only (docs/plans/ingest-bytes.md).
    /// By default the wall clock's milliseconds.
    fn now_us(&self) -> u64 {
        u64::try_from(self.now_ms())
            .unwrap_or(0)
            .saturating_mul(1000)
    }
}

/// A reader that hashes what passes and fails at its end when the bytes do not have the
/// expected SHA-256 and size: guards a second read of a source object. A stream longer than
/// expected fails as soon as it passes the expected size.
pub struct Verify<R: Read> {
    inner: R,
    expected: Metrics,
    sha: Sha256,
    seen: u64,
}

impl<R: Read> Verify<R> {
    /// Wraps `inner`, expecting `expected`'s size and SHA-256.
    pub fn new(inner: R, expected: Metrics) -> Self {
        Self {
            inner,
            expected,
            sha: Sha256::new(),
            seen: 0,
        }
    }
}

impl<R: Read> Read for Verify<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.sha.update(&buf[..n]);
            self.seen += n as u64;
            if self.seen > self.expected.size {
                return Err(std::io::Error::other(
                    "the source is longer than when it was hashed",
                ));
            }
            return Ok(n);
        }
        if self.seen != self.expected.size {
            return Err(std::io::Error::other(
                "the source is shorter than when it was hashed",
            ));
        }
        let got = hex(&self.sha.clone().finalize());
        if !got.eq_ignore_ascii_case(&self.expected.sha256) {
            return Err(std::io::Error::other(
                "the source changed since it was hashed",
            ));
        }
        Ok(0)
    }
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a run is given.
pub struct Run<'a> {
    pub job: &'a JobSpec,
    pub source: &'a dyn Source,
    pub dest: &'a dyn Dest,
    pub spool: &'a dyn Spool,
    pub clock: &'a dyn Clock,
    /// Set to stop: the run finishes its files in flight, writes its checkpoint and status
    /// (`stopped`) and returns.
    pub stop: &'a AtomicBool,
}

/// How many times a failed read, hash or write is tried again before the file is `Failed`.
/// There is no back-off: the clock only tells the time.
const RETRIES: u32 = 3;
/// Seconds of clock time between two status writes at most.
const STATUS_EVERY_MS: i64 = 30_000;
/// Most entries kept in the in-run hint tables, so memory stays bounded.
const HINT_CAP: usize = 200_000;
/// Most SimHashes compared against each new one (the scan is linear).
const SIM_CAP: usize = 20_000;
/// Most entries in the duplicate-group table.
const DUP_CAP: usize = 200_000;
/// Most rows in each report list.
const LIST_CAP: usize = 200;
/// SimHash distance, in bits, that counts as `near_text`.
const NEAR_BITS: u32 = 3;
/// Read buffer size.
const BUF: usize = 64 * 1024;
/// Files up to this size are staged in memory rather than in the spool, unless the spool says
/// otherwise ([`Spool::memory_limit`]).
pub const MEMORY_BYTES: u64 = 1024 * 1024;
/// Objects in flight (read, waiting in order, or uploading) per worker thread.
const WINDOW_PER_THREAD: usize = 4;
/// Most worker threads.
const MAX_THREADS: usize = 256;
/// Most blob keys in one batched existence check.
const HEAD_BATCH: usize = 500;
/// Most existence-check threads (and batches in flight) of the parallel engine.
const MAX_LOOKUPS: usize = 8;

/// What every thread of a run shares: the stage timings and the bytes staged in memory.
struct Shared {
    stages: Stages,
    memory: Arc<AtomicU64>,
}

impl Shared {
    fn new() -> Self {
        Self {
            stages: Stages::new(),
            memory: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Times `f` as one pass of `stage` over `bytes`.
    fn timed<T>(&self, r: &Run<'_>, stage: Stage, bytes: u64, f: impl FnOnce() -> T) -> T {
        let at = r.clock.now_us();
        let out = f();
        self.stages
            .add(stage, r.clock.now_us().saturating_sub(at), bytes);
        out
    }

    /// Reserves `size` bytes of the memory budget `budget`; false when they do not fit.
    fn reserve(&self, size: u64, budget: u64) -> bool {
        let mut held = self.memory.load(Ordering::SeqCst);
        loop {
            let Some(next) = held.checked_add(size).filter(|n| *n <= budget) else {
                return false;
            };
            match self
                .memory
                .compare_exchange_weak(held, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(now) => held = now,
            }
        }
    }
}

/// A file staged in memory; gives its reservation back when dropped.
struct MemFile {
    cursor: Cursor<Vec<u8>>,
    held: Arc<AtomicU64>,
    reserved: u64,
}

impl Read for MemFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl Write for MemFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.cursor.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for MemFile {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(to)
    }
}

impl Drop for MemFile {
    fn drop(&mut self) {
        self.held.fetch_sub(self.reserved, Ordering::SeqCst);
    }
}

/// Runs `f` and, when it fails, again up to [`RETRIES`] more times.
fn retry<T>(mut f: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let mut tried = 0;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e @ (Error::Exists | Error::Exhausted)) => return Err(e),
            Err(e) => {
                tried += 1;
                if tried > RETRIES {
                    return Err(e);
                }
            }
        }
    }
}

/// A file or member read once: its metrics and, when it fits, its bytes in a spool file.
struct Staged {
    metrics: Metrics,
    /// Rewound to the start; none when the object was too big to spool.
    file: Option<Box<dyn SpoolFile>>,
    over_spool: bool,
}

/// What is known of the file or member being handled, before its bytes are read.
struct Item<'x> {
    origin: &'x Origin,
    /// The key or member path, for its normalised name.
    name: String,
    member: Option<Member>,
    chain: Vec<u64>,
    /// Flags from the archive directory (`unsafe_path`).
    flags: Vec<Flag>,
    /// 0 for a listed object, 1 for a member of it, and so on.
    depth: u32,
    /// The size to record when the bytes were not read.
    declared: u64,
    /// The source key to read again, for a top-level object only.
    reopen: Option<&'x str>,
}

/// The state of one run.
struct Engine<'r, 'a> {
    r: &'r Run<'a>,
    sh: &'r Shared,
    counters: Counters,
    seq: u64,
    after_key: Option<String>,
    seen: HashSet<String>,
    names: HashMap<(String, u64), String>,
    sims: Vec<(u64, String)>,
    lines: Vec<ManifestLine>,
    part_bytes: usize,
    flags: BTreeMap<Flag, u64>,
    conflicts: Vec<ManifestLine>,
    failures: Vec<ManifestLine>,
    not_opened: Vec<ManifestLine>,
    too_large: Vec<ManifestLine>,
    /// The write budget was spent: stop before the object that found it.
    exhausted: bool,
    dups: HashMap<String, (u64, u64)>,
    started_ms: i64,
    /// The monotonic clock when this session started, for stage timings.
    started_us: u64,
    last_status_ms: i64,
    base_read: u64,
    base_listed: u64,
    last_error: Option<String>,
}

/// Whether an object of `size` bytes is more than this run writes (`job.skip_over`).
fn too_large(r: &Run<'_>, size: u64) -> bool {
    r.job.skip_over.is_some_and(|max| size > max)
}

/// The item for a listed object.
fn top_item<'x>(origin: &'x Origin, obj: &'x Object) -> Item<'x> {
    Item {
        origin,
        name: obj.key.clone(),
        member: None,
        chain: Vec::new(),
        flags: Vec::new(),
        depth: 0,
        declared: obj.size,
        reopen: Some(&obj.key),
    }
}

/// Whether `f` starts like a zip. Leaves `f` rewound.
fn file_is_zip(f: &mut dyn SpoolFile) -> bool {
    let mut head = [0u8; 4];
    let mut got = 0;
    if f.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    while got < head.len() {
        match f.read(&mut head[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    let _ = f.seek(SeekFrom::Start(0));
    is_zip(&head[..got])
}

/// Hashes a spool file from its start. Leaves it rewound.
fn hash_file(f: &mut dyn SpoolFile, fingerprint: bool) -> Result<Metrics, Error> {
    f.seek(SeekFrom::Start(0))?;
    let mut h = Hasher::new(fingerprint);
    let mut buf = vec![0u8; BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    f.seek(SeekFrom::Start(0))?;
    h.finish()
}

/// Reads one object once: into memory (files up to the spool's memory limit, within its memory
/// budget) or a spool file while hashing when it fits, else hashing only. The same bytes are
/// hashed and kept whichever holds them.
fn stage(r: &Run<'_>, sh: &Shared, key: &str, size: u64) -> Result<Staged, Error> {
    let opened = r.clock.now_us();
    let mut src = r.source.open(key)?;
    let mut at = r.clock.now_us();
    sh.stages
        .add(Stage::SourceOpen, at.saturating_sub(opened), 0);
    let mut h = Hasher::new(r.job.fingerprints);
    let mut buf = vec![0u8; BUF];
    let mut on_disk = false;
    let mut spool_file: Option<Box<dyn SpoolFile>> = if size > r.spool.free_bytes() {
        None
    } else if size <= r.spool.memory_limit() && sh.reserve(size, r.spool.memory_budget()) {
        Some(Box::new(MemFile {
            cursor: Cursor::new(Vec::with_capacity(size as usize)),
            held: Arc::clone(&sh.memory),
            reserved: size,
        }))
    } else {
        on_disk = true;
        Some(r.spool.create()?)
    };
    let (mut read_us, mut hash_us, mut write_us, mut total) = (0u64, 0u64, 0u64, 0u64);
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| Error::Source(e.to_string()))?;
        let read = r.clock.now_us();
        read_us += read.saturating_sub(at);
        if n == 0 {
            break;
        }
        total += n as u64;
        h.update(&buf[..n]);
        let hashed = r.clock.now_us();
        hash_us += hashed.saturating_sub(read);
        if let Some(f) = spool_file.as_mut() {
            f.write_all(&buf[..n])?;
        }
        at = r.clock.now_us();
        write_us += at.saturating_sub(hashed);
    }
    if let Some(f) = spool_file.as_mut() {
        f.seek(SeekFrom::Start(0))?;
    }
    let finishing = r.clock.now_us();
    let metrics = h.finish()?;
    hash_us += r.clock.now_us().saturating_sub(finishing);
    sh.stages.add(Stage::SourceRead, read_us, total);
    sh.stages.add(Stage::Hash, hash_us, total);
    if on_disk {
        sh.stages.add(Stage::SpoolWrite, write_us, total);
    }
    Ok(Staged {
        metrics,
        over_spool: spool_file.is_none(),
        file: spool_file,
    })
}

/// Writes the bytes of a staged file to `key`, from its spool file or a verified re-read.
fn put(
    r: &Run<'_>,
    sh: &Shared,
    key: &str,
    file: &mut Option<Box<dyn SpoolFile>>,
    metrics: &Metrics,
    reopen: Option<&str>,
) -> Result<(), Error> {
    retry(|| match file.as_mut() {
        Some(f) => {
            f.seek(SeekFrom::Start(0))?;
            sh.timed(r, Stage::Upload, metrics.size, || {
                r.dest.put_blob(key, f, metrics.size, metrics)
            })
        }
        None => {
            let src_key = reopen.ok_or_else(|| Error::Io("no source to copy from".into()))?;
            let src = r.source.open(src_key)?;
            let mut v = Verify::new(src, metrics.clone());
            sh.timed(r, Stage::Upload, metrics.size, || {
                r.dest.put_blob(key, &mut v, metrics.size, metrics)
            })
        }
    })
}

/// A blob's existence, as the destination answers it, for the coordinator's decisions.
type Answer = Result<Option<Head>, Error>;

/// Existence checks for first-seen SHA-256s, batched when there are several: a failed batch
/// falls back to one check each, so every hash gets its own answer.
fn look_up(r: &Run<'_>, sh: &Shared, shas: Vec<String>) -> Vec<(String, Answer)> {
    if shas.len() >= 2 {
        let keys: Vec<String> = shas.iter().map(|sha| blob_key(sha)).collect();
        let heads = sh.timed(r, Stage::Lookup, 0, || r.dest.head_many(&keys));
        if let Ok(heads) = heads
            && heads.len() == shas.len()
        {
            return shas.into_iter().zip(heads.into_iter().map(Ok)).collect();
        }
    }
    shas.into_iter()
        .map(|sha| {
            let key = blob_key(&sha);
            let answer = retry(|| sh.timed(r, Stage::Lookup, 0, || r.dest.head(&key)));
            (sha, answer)
        })
        .collect()
}

/// An existence-check thread: answers batches until the coordinator hangs up.
fn looker(
    r: &Run<'_>,
    sh: &Shared,
    batches: &Mutex<mpsc::Receiver<Vec<String>>>,
    done: mpsc::Sender<Done>,
    cancel: &AtomicBool,
) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let batch = match batches.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(batch) = batch else { return };
        let answers = look_up(r, sh, batch);
        if done.send(Done::Looked { answers }).is_err() {
            return;
        }
    }
}

/// Work for a worker thread of the parallel engine.
enum Task {
    /// Read and hash a listed object.
    Stage { seq: u64, key: String, size: u64 },
    /// Upload a decided blob.
    Put {
        seq: u64,
        key: String,
        file: Option<Box<dyn SpoolFile>>,
        metrics: Metrics,
        reopen: Option<String>,
    },
}

/// What a worker thread finished.
enum Done {
    Staged {
        seq: u64,
        result: Result<Staged, Error>,
    },
    Put {
        seq: u64,
        result: Result<(), Error>,
    },
    /// Existence answers for a batch of first-seen SHA-256s.
    Looked {
        answers: Vec<(String, Answer)>,
    },
}

/// A worker thread: takes tasks until the coordinator hangs up or cancels what is queued.
fn worker(
    r: &Run<'_>,
    sh: &Shared,
    tasks: &Mutex<mpsc::Receiver<Task>>,
    done: mpsc::Sender<Done>,
    cancel: &AtomicBool,
) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let task = match tasks.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(task) = task else { return };
        let out = match task {
            Task::Stage { seq, key, size } => Done::Staged {
                seq,
                result: retry(|| stage(r, sh, &key, size)),
            },
            Task::Put {
                seq,
                key,
                mut file,
                metrics,
                reopen,
            } => Done::Put {
                seq,
                result: put(r, sh, &key, &mut file, &metrics, reopen.as_deref()),
            },
        };
        if done.send(out).is_err() {
            return;
        }
    }
}

/// Objects handed to the pool to read, by sequence number, with their result once read.
type Reading = BTreeMap<u64, (Object, Option<Result<Staged, Error>>)>;

/// The coordinator's side of the existence-check pool.
struct Lookups {
    tx: mpsc::Sender<Vec<String>>,
    /// Threads, and so batches in flight at most.
    threads: usize,
    in_flight: usize,
    /// Hashes in a batch in flight.
    asked: HashSet<String>,
}

/// Where a decided object's upload stands.
enum Upload {
    /// Nothing to write.
    None,
    InFlight,
    Finished(Result<(), Error>),
}

/// A top-level object decided in listing order, kept until it and everything before it are
/// finished, so lines are kept in the serial engine's order.
struct Pending {
    seq: u64,
    obj: Object,
    bytes_read: u64,
    outcome: Outcome,
    upload: Upload,
}

/// What was decided for a pending object.
enum Outcome {
    /// Reading, hashing or the existence check failed.
    Failed(Option<Metrics>, Vec<Flag>, Error),
    /// Not read or not written, with the flags saying why.
    Skipped(Vec<Flag>),
    Decided {
        metrics: Metrics,
        verdict: Verdict,
        reason: Reason,
        flags: Vec<Flag>,
        r2_key: Option<String>,
        first_time: bool,
        not_opened: bool,
    },
}

impl<'r, 'a> Engine<'r, 'a> {
    fn dry(&self) -> bool {
        self.r.job.dry_run
    }

    /// Builds a manifest line.
    #[allow(clippy::too_many_arguments)]
    fn make_line(
        &self,
        item: &Item<'_>,
        m: Option<&Metrics>,
        verdict: Verdict,
        reason: Reason,
        flags: Vec<Flag>,
        hints: Vec<Hint>,
        r2_key: Option<String>,
        error: Option<String>,
    ) -> ManifestLine {
        ManifestLine {
            id: line_id(item.origin, &item.chain),
            origin: item.origin.clone(),
            member: item.member.clone(),
            sha256: m.map(|m| m.sha256.clone()).unwrap_or_default(),
            blake3: m.map(|m| m.blake3.clone()).unwrap_or_default(),
            size: m.map_or(item.declared, |m| m.size),
            mime: m.map_or_else(
                || "application/octet-stream".to_string(),
                |m| m.mime.clone(),
            ),
            kind: m.map_or(Kind::Other, |m| m.kind),
            norm_name: normalise(&item.name).0,
            simhash: m.and_then(|m| m.simhash.clone()),
            verdict,
            reason,
            flags,
            hints,
            r2_key,
            error,
            at_ms: self.r.clock.now_ms(),
        }
    }

    /// Keeps a line, counting it. Answers its index in the open part.
    fn push_line(&mut self, line: ManifestLine, not_opened: bool) -> usize {
        if line.member.is_some() {
            self.counters.members += 1;
        } else {
            self.counters.files += 1;
        }
        match line.verdict {
            Verdict::Upload => {
                self.counters.uploaded += 1;
                self.counters.uploaded_bytes += line.size;
            }
            Verdict::WouldUpload => {
                self.counters.would_upload += 1;
                self.counters.would_upload_bytes += line.size;
            }
            Verdict::Duplicate => {
                self.counters.duplicates += 1;
                self.counters.duplicate_bytes += line.size;
            }
            Verdict::Conflict => {
                self.counters.conflicts += 1;
                if self.conflicts.len() < LIST_CAP {
                    self.conflicts.push(line.clone());
                }
            }
            Verdict::Failed => {
                self.counters.failed += 1;
                if self.failures.len() < LIST_CAP {
                    self.failures.push(line.clone());
                }
            }
            Verdict::Skipped => {
                self.counters.skipped += 1;
                if line.flags.contains(&Flag::TooLarge) && self.too_large.len() < LIST_CAP {
                    self.too_large.push(line.clone());
                }
            }
        }
        for f in &line.flags {
            *self.flags.entry(*f).or_insert(0) += 1;
        }
        if !line.sha256.is_empty()
            && matches!(
                line.verdict,
                Verdict::Upload | Verdict::WouldUpload | Verdict::Duplicate | Verdict::Conflict
            )
        {
            let n = self.dups.len();
            match self.dups.get_mut(&line.sha256) {
                Some(e) => e.0 += 1,
                None if n < DUP_CAP => {
                    self.dups.insert(line.sha256.clone(), (1, line.size));
                }
                None => {}
            }
        }
        if not_opened {
            self.note_not_opened(&line);
        }
        self.part_bytes += serde_json::to_string(&line).map_or(0, |s| s.len() + 1);
        self.lines.push(line);
        self.lines.len() - 1
    }

    /// Counts an archive that was kept but not opened, and lists it.
    fn note_not_opened(&mut self, line: &ManifestLine) {
        self.counters.archives_not_opened += 1;
        if self.not_opened.len() < LIST_CAP {
            self.not_opened.push(line.clone());
        }
    }

    /// Adds a flag to a line of the open part that was kept before the flag was known.
    fn flag_line(&mut self, idx: usize, flag: Flag, not_opened: bool) {
        *self.flags.entry(flag).or_insert(0) += 1;
        let mut copy = None;
        if let Some(l) = self.lines.get_mut(idx) {
            l.flags.push(flag);
            copy = Some(l.clone());
        }
        if not_opened && let Some(l) = copy {
            self.note_not_opened(&l);
        }
    }

    /// Keeps a `Failed` line: never a duplicate.
    fn fail(&mut self, item: &Item<'_>, m: Option<&Metrics>, flags: Vec<Flag>, e: Error) {
        let msg = e.to_string();
        self.last_error = Some(msg.clone());
        let line = self.make_line(
            item,
            m,
            Verdict::Failed,
            Reason::Error,
            flags,
            Vec::new(),
            None,
            Some(msg),
        );
        self.push_line(line, false);
    }

    /// Keeps a `Skipped` line: the bytes were not read.
    fn skip(&mut self, item: &Item<'_>, flags: Vec<Flag>) {
        let line = self.make_line(
            item,
            None,
            Verdict::Skipped,
            Reason::Flagged,
            flags,
            Vec::new(),
            None,
            None,
        );
        self.push_line(line, false);
    }

    /// In-run hints for a first sighting of `sha`.
    fn hints_for(&mut self, item: &Item<'_>, m: &Metrics) -> Vec<Hint> {
        let mut hints = Vec::new();
        let key = (normalise(&item.name).0, m.size);
        match self.names.get(&key) {
            Some(other) if *other != m.sha256 => hints.push(Hint {
                kind: "same_name_size".into(),
                other: other.clone(),
                score: None,
            }),
            Some(_) => {}
            None => {
                if self.names.len() < HINT_CAP {
                    self.names.insert(key, m.sha256.clone());
                }
            }
        }
        let sim = m
            .simhash
            .as_deref()
            .and_then(|h| u64::from_str_radix(h, 16).ok());
        if let Some(sim) = sim {
            for (s, other) in &self.sims {
                let bits = hamming(*s, sim);
                if *other != m.sha256 && bits <= NEAR_BITS {
                    hints.push(Hint {
                        kind: "near_text".into(),
                        other: other.clone(),
                        score: Some(f64::from(bits)),
                    });
                    break;
                }
            }
            if self.sims.len() < SIM_CAP {
                self.sims.push((sim, m.sha256.clone()));
            }
        }
        hints
    }

    /// Looks up, decides, writes and records one file or member, then opens it when it is a
    /// zip that is not left behind. Failures become `Failed` lines.
    fn finish_file(&mut self, item: &Item<'_>, staged: Staged, budget: Option<&mut Budget>) {
        let r = self.r;
        let dry = self.dry();
        let Staged {
            metrics,
            mut file,
            over_spool,
        } = staged;
        let sha = metrics.sha256.clone();
        let first_time = !self.seen.contains(&sha);
        let mut flags = item.flags.clone();
        if over_spool {
            flags.push(Flag::OverSpool);
        }
        let lookup = if first_time {
            let key = blob_key(&sha);
            match retry(|| self.sh.timed(r, Stage::Lookup, 0, || r.dest.head(&key))) {
                Ok(None) => Lookup::Absent,
                Ok(Some(h)) => Lookup::Present(h),
                Err(e) => {
                    self.fail(item, Some(&metrics), flags, e);
                    return;
                }
            }
        } else {
            Lookup::SeenInRun
        };
        let d = decide(&metrics, &lookup, dry);
        let r2_key = match d.verdict {
            Verdict::Upload | Verdict::WouldUpload | Verdict::Duplicate => Some(blob_key(&sha)),
            Verdict::Conflict => Some(quarantine_key(&r.job.run, &sha)),
            _ => None,
        };
        let writes = !dry && matches!(d.verdict, Verdict::Upload | Verdict::Conflict);
        if writes && too_large(r, metrics.size) {
            flags.push(Flag::TooLarge);
            self.skip(item, flags);
            return;
        }
        let mut d = d;
        if writes && let Some(key) = &r2_key {
            match put(r, self.sh, key, &mut file, &metrics, item.reopen) {
                Ok(()) => {}
                // A listed object is left for a continuation; a member fails.
                Err(Error::Exhausted) if item.depth == 0 => {
                    self.exhausted = true;
                    return;
                }
                // The create-only write found the blob: R2 holds these bytes.
                Err(Error::Exists) => {
                    if d.verdict == Verdict::Upload {
                        d.verdict = Verdict::Duplicate;
                        d.reason = Reason::InR2;
                    }
                }
                Err(e) => {
                    self.fail(item, Some(&metrics), flags, e);
                    return;
                }
            }
        }
        // A conflicting file is not remembered: its blob key does not hold these bytes.
        if d.verdict != Verdict::Conflict {
            self.seen.insert(sha.clone());
        }
        let hints = if first_time {
            self.hints_for(item, &metrics)
        } else {
            Vec::new()
        };

        let zip = file.as_mut().is_some_and(|f| file_is_zip(f.as_mut()));
        let max_depth = r.job.limits.max_depth;
        let mut open = false;
        let mut not_opened = false;
        if zip && d.verdict != Verdict::Duplicate {
            if item.depth > max_depth {
                flags.push(Flag::DepthLimit);
                not_opened = true;
            } else {
                open = true;
            }
        } else if metrics.kind == Kind::Archive && (file.is_none() || !zip) {
            if !over_spool {
                flags.push(Flag::NotOpened);
            }
            not_opened = true;
        }
        let line = self.make_line(
            item,
            Some(&metrics),
            d.verdict,
            d.reason,
            flags,
            hints,
            r2_key,
            None,
        );
        let idx = self.push_line(line, not_opened);
        if open && let Some(f) = file {
            self.open_zip(f, item, &sha, idx, budget);
        }
    }

    /// Goes through a zip's members, each as a file of its own.
    fn open_zip(
        &mut self,
        file: Box<dyn SpoolFile>,
        item: &Item<'_>,
        parent_sha: &str,
        idx: usize,
        budget: Option<&mut Budget>,
    ) {
        let r = self.r;
        let mut own;
        let budget = match budget {
            Some(b) => b,
            None => {
                let mut limits = r.job.limits.clone();
                limits.max_total_bytes = limits.max_total_bytes.min(r.spool.free_bytes());
                own = Budget::new(limits);
                &mut own
            }
        };
        let mut zip = match Zip::open(file) {
            Ok(z) => z,
            Err(_) => {
                self.flag_line(idx, Flag::Unreadable, true);
                return;
            }
        };
        self.counters.archives_opened += 1;
        for entry in zip.entries() {
            if entry.is_dir {
                continue;
            }
            let mut chain = item.chain.clone();
            chain.push(entry.index);
            let child = Item {
                origin: item.origin,
                name: entry.path.clone(),
                member: Some(Member {
                    parent_sha256: parent_sha.to_string(),
                    path: entry.path.clone(),
                    raw_name: entry.raw_name.clone(),
                    index: entry.index,
                    depth: item.depth + 1,
                    chain: chain.clone(),
                }),
                chain,
                flags: entry
                    .flags
                    .iter()
                    .copied()
                    .filter(|f| *f == Flag::UnsafePath)
                    .collect(),
                depth: item.depth + 1,
                declared: entry.declared_size,
                reopen: None,
            };
            if entry
                .flags
                .iter()
                .any(|f| matches!(f, Flag::Encrypted | Flag::Symlink))
            {
                self.skip(&child, entry.flags.clone());
                continue;
            }
            let mut out = match r.spool.create() {
                Ok(f) => f,
                Err(e) => {
                    self.fail(&child, None, child.flags.clone(), e);
                    continue;
                }
            };
            match zip.extract(entry.index, budget, &mut out) {
                Ok(Ok(_)) => {}
                Ok(Err(flag @ (Flag::BombSuspect | Flag::TooManyEntries))) => {
                    self.flag_line(idx, flag, true);
                    break;
                }
                Ok(Err(flag)) => {
                    let mut flags = child.flags.clone();
                    flags.push(flag);
                    self.skip(&child, flags);
                    continue;
                }
                Err(e) => {
                    self.fail(&child, None, child.flags.clone(), e);
                    continue;
                }
            }
            let fingerprint = r.job.fingerprints;
            match retry(|| hash_file(out.as_mut(), fingerprint)) {
                Ok(metrics) => {
                    let staged = Staged {
                        metrics,
                        file: Some(out),
                        over_spool: false,
                    };
                    self.finish_file(&child, staged, Some(&mut *budget));
                }
                Err(e) => self.fail(&child, None, child.flags.clone(), e),
            }
        }
    }

    /// Where a listed object came from.
    fn origin_of(&self, obj: &Object) -> Origin {
        let (account, bucket) = match &self.r.job.source {
            SourceSpec::S3 {
                account, bucket, ..
            } => (account.clone(), bucket.clone()),
            SourceSpec::Local { account, root } => (account.clone(), root.clone()),
        };
        Origin {
            account,
            bucket,
            key: obj.key.clone(),
            etag: obj.etag.clone(),
            listed_size: obj.size,
            modified: obj.modified.clone(),
        }
    }

    /// Handles one listed object.
    fn object(&mut self, obj: &Object) {
        let r = self.r;
        if too_large(r, obj.size) {
            self.object_too_large(obj);
            return;
        }
        let staged = retry(|| stage(r, self.sh, &obj.key, obj.size));
        self.object_staged(obj, staged);
    }

    /// Records a listed object too large for this run, without reading it.
    fn object_too_large(&mut self, obj: &Object) {
        let origin = self.origin_of(obj);
        let item = top_item(&origin, obj);
        self.counters.listed += 1;
        self.counters.listed_bytes += obj.size;
        self.skip(&item, vec![Flag::TooLarge]);
        self.after_key = Some(obj.key.clone());
    }

    /// Records one listed object whose bytes were read (or failed to be read): looks it up,
    /// decides, writes and opens it.
    fn object_staged(&mut self, obj: &Object, staged: Result<Staged, Error>) {
        let origin = self.origin_of(obj);
        let item = top_item(&origin, obj);
        let before = self.counters.clone();
        self.counters.listed += 1;
        self.counters.listed_bytes += obj.size;
        match staged {
            Ok(staged) => {
                self.counters.bytes_read += staged.metrics.size;
                self.finish_file(&item, staged, None);
            }
            Err(e) => self.fail(&item, None, Vec::new(), e),
        }
        if self.exhausted {
            // Not recorded: the continuation starts at this object.
            self.counters = before;
            return;
        }
        self.after_key = Some(obj.key.clone());
    }

    /// After each finished top-level object: a checkpoint when the open part is full, else a
    /// status now and then.
    fn after_object(&mut self) -> Result<(), Error> {
        if self.lines.len() >= PART_LINES || self.part_bytes >= PART_BYTES {
            self.checkpoint()?;
            self.status(RunState::Running);
        } else if self.r.clock.now_ms() - self.last_status_ms >= STATUS_EVERY_MS {
            self.status(RunState::Running);
        }
        Ok(())
    }

    /// Decides a staged top-level object that is not a zip, in listing order: the same rules
    /// and state as [`Engine::finish_file`], with the upload handed to the pool.
    fn decide_one(
        &mut self,
        seq: u64,
        obj: Object,
        staged: Staged,
        hints: &mut HashMap<String, Answer>,
        tasks: &mpsc::Sender<Task>,
        uploading: &mut HashMap<String, u64>,
    ) -> Pending {
        let r = self.r;
        let Staged {
            metrics,
            file,
            over_spool,
        } = staged;
        let bytes_read = metrics.size;
        let sha = metrics.sha256.clone();
        let first_time = !self.seen.contains(&sha);
        let mut flags = Vec::new();
        if over_spool {
            flags.push(Flag::OverSpool);
        }
        // An answer for a hash already seen in this run is not needed any more.
        let answer = hints.remove(&sha);
        let lookup = if first_time {
            let head = match answer {
                Some(answer) => answer,
                None => {
                    let key = blob_key(&sha);
                    retry(|| self.sh.timed(r, Stage::Lookup, 0, || r.dest.head(&key)))
                }
            };
            match head {
                Ok(None) => Lookup::Absent,
                Ok(Some(h)) => Lookup::Present(h),
                Err(e) => {
                    return Pending {
                        seq,
                        obj,
                        bytes_read,
                        outcome: Outcome::Failed(Some(metrics), flags, e),
                        upload: Upload::None,
                    };
                }
            }
        } else {
            Lookup::SeenInRun
        };
        let d = decide(&metrics, &lookup, self.dry());
        let r2_key = match d.verdict {
            Verdict::Upload | Verdict::WouldUpload | Verdict::Duplicate => Some(blob_key(&sha)),
            Verdict::Conflict => Some(quarantine_key(&r.job.run, &sha)),
            _ => None,
        };
        let writes = !self.dry() && matches!(d.verdict, Verdict::Upload | Verdict::Conflict);
        if writes && too_large(r, metrics.size) {
            flags.push(Flag::TooLarge);
            return Pending {
                seq,
                obj,
                bytes_read,
                outcome: Outcome::Skipped(flags),
                upload: Upload::None,
            };
        }
        // Not a zip here, so an archive is kept unopened.
        let mut not_opened = false;
        if metrics.kind == Kind::Archive {
            if !over_spool {
                flags.push(Flag::NotOpened);
            }
            not_opened = true;
        }
        // Remembered now, so a later sighting is a duplicate; forgotten again if the upload
        // fails (a later sighting of an uploading blob waits for its result).
        if d.verdict != Verdict::Conflict {
            self.seen.insert(sha.clone());
        }
        let upload = match (&r2_key, writes) {
            (Some(key), true) => {
                let task = Task::Put {
                    seq,
                    key: key.clone(),
                    file,
                    metrics: metrics.clone(),
                    reopen: Some(obj.key.clone()),
                };
                if tasks.send(task).is_ok() {
                    if d.verdict != Verdict::Conflict {
                        uploading.insert(sha.clone(), seq);
                    }
                    Upload::InFlight
                } else {
                    if d.verdict != Verdict::Conflict {
                        self.seen.remove(&sha);
                    }
                    Upload::Finished(Err(Error::Io("the worker pool ended".into())))
                }
            }
            _ => Upload::None,
        };
        Pending {
            seq,
            obj,
            bytes_read,
            outcome: Outcome::Decided {
                metrics,
                verdict: d.verdict,
                reason: d.reason,
                flags,
                r2_key,
                first_time,
                not_opened,
            },
            upload,
        }
    }

    /// Records an upload's result on its pending object.
    fn put_done(
        &mut self,
        pending: &mut VecDeque<Pending>,
        uploading: &mut HashMap<String, u64>,
        seq: u64,
        result: Result<(), Error>,
    ) {
        let Some(p) = pending.iter_mut().find(|p| p.seq == seq) else {
            return;
        };
        let Outcome::Decided {
            metrics,
            verdict,
            reason,
            ..
        } = &mut p.outcome
        else {
            return;
        };
        let result = match result {
            // The create-only write found the blob: R2 holds these bytes.
            Err(Error::Exists) => {
                if *verdict == Verdict::Upload {
                    *verdict = Verdict::Duplicate;
                    *reason = Reason::InR2;
                }
                Ok(())
            }
            other => other,
        };
        if *verdict != Verdict::Conflict {
            if uploading.get(&metrics.sha256) == Some(&seq) {
                uploading.remove(&metrics.sha256);
            }
            if result.is_err() {
                self.seen.remove(&metrics.sha256);
            }
        }
        p.upload = Upload::Finished(result);
    }

    /// Keeps the lines of every finished object at the front of `pending`, in order.
    fn drain(&mut self, pending: &mut VecDeque<Pending>) -> Result<(), Error> {
        while pending
            .front()
            .is_some_and(|p| !matches!(p.upload, Upload::InFlight))
        {
            if pending
                .front()
                .is_some_and(|p| matches!(p.upload, Upload::Finished(Err(Error::Exhausted))))
            {
                // Stop before it: nothing from here on is recorded, so a continuation starts
                // at this object.
                self.exhausted = true;
                pending.clear();
                return Ok(());
            }
            let Some(p) = pending.pop_front() else { break };
            let origin = self.origin_of(&p.obj);
            let item = top_item(&origin, &p.obj);
            self.counters.listed += 1;
            self.counters.listed_bytes += p.obj.size;
            self.counters.bytes_read += p.bytes_read;
            match p.outcome {
                Outcome::Failed(m, flags, e) => self.fail(&item, m.as_ref(), flags, e),
                Outcome::Skipped(flags) => self.skip(&item, flags),
                Outcome::Decided {
                    metrics,
                    verdict,
                    reason,
                    flags,
                    r2_key,
                    first_time,
                    not_opened,
                } => match p.upload {
                    Upload::Finished(Err(e)) => self.fail(&item, Some(&metrics), flags, e),
                    _ => {
                        let hints = if first_time {
                            self.hints_for(&item, &metrics)
                        } else {
                            Vec::new()
                        };
                        let line = self.make_line(
                            &item,
                            Some(&metrics),
                            verdict,
                            reason,
                            flags,
                            hints,
                            r2_key,
                            None,
                        );
                        self.push_line(line, not_opened);
                    }
                },
            }
            self.after_key = Some(p.obj.key.clone());
            self.after_object()?;
        }
        Ok(())
    }

    /// Sends the first-seen hashes among the objects read so far to the existence-check pool,
    /// in batches of up to [`HEAD_BATCH`], while it has room. The coordinator never waits on a
    /// check: an object whose answer is still out waits in order, as one whose hash is uploading
    /// does (docs/plans/ingest-bytes.md).
    fn request_lookups(
        &self,
        reading: &Reading,
        hints: &mut HashMap<String, Answer>,
        uploading: &HashMap<String, u64>,
        lookups: &mut Lookups,
    ) {
        if hints.len() > 4 * HEAD_BATCH {
            // Unused answers (hashes decided another way); any still needed are asked again.
            hints.clear();
        }
        while lookups.in_flight < lookups.threads {
            let mut want: Vec<String> = Vec::new();
            let mut batch = HashSet::new();
            for (_, staged) in reading.values() {
                let Some(Ok(staged)) = staged else { continue };
                let sha = &staged.metrics.sha256;
                if !self.seen.contains(sha)
                    && !hints.contains_key(sha)
                    && !uploading.contains_key(sha)
                    && !lookups.asked.contains(sha)
                    && batch.insert(sha.clone())
                {
                    want.push(sha.clone());
                    if want.len() >= HEAD_BATCH {
                        break;
                    }
                }
            }
            if want.is_empty() {
                return;
            }
            lookups.asked.extend(want.iter().cloned());
            if lookups.tx.send(want).is_err() {
                return;
            }
            lookups.in_flight += 1;
        }
    }

    /// Decides every object read so far that is next in listing order.
    fn decide_ready(
        &mut self,
        reading: &mut Reading,
        pending: &mut VecDeque<Pending>,
        uploading: &mut HashMap<String, u64>,
        hints: &mut HashMap<String, Answer>,
        tasks: &mpsc::Sender<Task>,
        lookups: &mut Lookups,
    ) -> Result<(), Error> {
        self.request_lookups(reading, hints, uploading, lookups);
        while !self.exhausted
            && let Some(mut entry) = reading.first_entry()
        {
            let seq = *entry.key();
            let (zip, waits, unanswered) = match entry.get_mut().1.as_mut() {
                None => break,
                Some(Err(_)) => (false, false, false),
                Some(Ok(staged)) => {
                    let sha = &staged.metrics.sha256;
                    (
                        staged
                            .file
                            .as_mut()
                            .is_some_and(|f| file_is_zip(f.as_mut())),
                        uploading.contains_key(sha),
                        !self.seen.contains(sha) && !hints.contains_key(sha),
                    )
                }
            };
            // The same blob is uploading: its result decides whether this one is canonical.
            if waits {
                break;
            }
            // A zip is opened in order, after everything before it is finished.
            if zip && !pending.is_empty() {
                break;
            }
            // A first sighting waits for its existence answer (a zip looks itself up).
            if unanswered && !zip {
                break;
            }
            let (obj, staged) = entry.remove();
            let Some(staged) = staged else { break };
            if too_large(self.r, obj.size) {
                pending.push_back(Pending {
                    seq,
                    obj,
                    bytes_read: 0,
                    outcome: Outcome::Skipped(vec![Flag::TooLarge]),
                    upload: Upload::None,
                });
                self.drain(pending)?;
                continue;
            }
            if zip {
                self.object_staged(&obj, staged);
                self.after_object()?;
                continue;
            }
            let p = match staged {
                Ok(staged) => self.decide_one(seq, obj, staged, hints, tasks, uploading),
                Err(e) => Pending {
                    seq,
                    obj,
                    bytes_read: 0,
                    outcome: Outcome::Failed(None, Vec::new(), e),
                    upload: Upload::None,
                },
            };
            pending.push_back(p);
            self.drain(pending)?;
        }
        // Answers consumed above may leave a waiting first sighting with none: ask now.
        self.request_lookups(reading, hints, uploading, lookups);
        Ok(())
    }

    /// The coordinator of the parallel engine: lists ahead, hands reads and uploads to the
    /// pool, and decides and records in listing order. Answers whether a stop ended it.
    fn coordinate(
        &mut self,
        window: usize,
        tasks: mpsc::Sender<Task>,
        done: mpsc::Receiver<Done>,
        pages: mpsc::Receiver<Result<Page, Error>>,
        mut lookups: Lookups,
    ) -> Result<bool, Error> {
        let r = self.r;
        let stages = &self.sh.stages;
        let after = self.after_key.clone();
        let mut queue: VecDeque<Object> = VecDeque::new();
        let mut listing = true;
        let mut first = true;
        let mut list_error = None;
        let mut stopping = false;
        let mut next_seq = 0u64;
        let mut reading: Reading = BTreeMap::new();
        let mut pending: VecDeque<Pending> = VecDeque::new();
        let mut uploading: HashMap<String, u64> = HashMap::new();
        let mut hints: HashMap<String, Answer> = HashMap::new();
        loop {
            if !stopping && r.stop.load(Ordering::SeqCst) {
                stopping = true;
                queue.clear();
            }
            while listing && !stopping && queue.len() < window {
                let idle = queue.is_empty() && reading.is_empty() && pending.is_empty();
                let got = if idle {
                    let at = r.clock.now_us();
                    let page = pages.recv().map_err(|_| TryRecvError::Disconnected);
                    stages.wait(Stage::CoordinatorWait, r.clock.now_us().saturating_sub(at));
                    page
                } else {
                    pages.try_recv()
                };
                match got {
                    Ok(Ok(page)) => {
                        if first {
                            first = false;
                            self.status(RunState::Running);
                        }
                        queue.extend(
                            page.objects
                                .into_iter()
                                .filter(|o| after.as_deref().is_none_or(|a| o.key.as_str() > a)),
                        );
                    }
                    // Objects listed before the failure are still finished, as serially.
                    Ok(Err(e)) => {
                        list_error = Some(e);
                        listing = false;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => listing = false,
                }
            }
            while !stopping && reading.len() + pending.len() < window {
                let Some(obj) = queue.pop_front() else { break };
                if too_large(r, obj.size) {
                    reading.insert(next_seq, (obj, Some(Err(Error::Io("not read".into())))));
                    next_seq += 1;
                    continue;
                }
                tasks
                    .send(Task::Stage {
                        seq: next_seq,
                        key: obj.key.clone(),
                        size: obj.size,
                    })
                    .map_err(|_| Error::Io("the worker pool ended".into()))?;
                reading.insert(next_seq, (obj, None));
                next_seq += 1;
            }
            self.decide_ready(
                &mut reading,
                &mut pending,
                &mut uploading,
                &mut hints,
                &tasks,
                &mut lookups,
            )?;
            self.drain(&mut pending)?;
            if self.exhausted {
                return Ok(true);
            }
            if reading.is_empty() && pending.is_empty() {
                if (queue.is_empty() || stopping) && (!listing || stopping) {
                    if let Some(e) = list_error {
                        return Err(e);
                    }
                    return Ok(stopping || r.stop.load(Ordering::SeqCst));
                }
                continue;
            }
            let full = reading.len() + pending.len() >= window;
            let at = r.clock.now_us();
            let got = done.recv_timeout(Duration::from_millis(200));
            stages.wait(
                if full {
                    Stage::WindowFull
                } else {
                    Stage::CoordinatorWait
                },
                r.clock.now_us().saturating_sub(at),
            );
            let mut next = match got {
                Ok(d) => Some(d),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Io("the worker pool ended".into()));
                }
            };
            while let Some(d) = next {
                match d {
                    Done::Staged { seq, result } => {
                        if let Some(entry) = reading.get_mut(&seq) {
                            entry.1 = Some(result);
                        }
                    }
                    Done::Put { seq, result } => {
                        self.put_done(&mut pending, &mut uploading, seq, result)
                    }
                    Done::Looked { answers } => {
                        lookups.in_flight = lookups.in_flight.saturating_sub(1);
                        for (sha, answer) in answers {
                            lookups.asked.remove(&sha);
                            hints.insert(sha, answer);
                        }
                    }
                }
                next = done.try_recv().ok();
            }
        }
    }

    /// [`Engine::drive`] over `threads` worker threads.
    fn drive_parallel(&mut self, threads: usize) -> Result<bool, Error> {
        let r = self.r;
        let after = self.after_key.clone();
        let (task_tx, task_rx) = mpsc::channel::<Task>();
        let task_rx = Mutex::new(task_rx);
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        // One page listed ahead of the one being worked on.
        let (page_tx, page_rx) = mpsc::sync_channel::<Result<Page, Error>>(1);
        let (lookup_tx, lookup_rx) = mpsc::channel::<Vec<String>>();
        let lookup_rx = Mutex::new(lookup_rx);
        let lookup_threads = (threads / 8).clamp(2, MAX_LOOKUPS);
        let cancel = AtomicBool::new(false);
        let sh = self.sh;
        std::thread::scope(|scope| {
            for _ in 0..threads {
                let done = done_tx.clone();
                let (tasks, cancel) = (&task_rx, &cancel);
                scope.spawn(move || worker(r, sh, tasks, done, cancel));
            }
            for _ in 0..lookup_threads {
                let done = done_tx.clone();
                let (batches, cancel) = (&lookup_rx, &cancel);
                scope.spawn(move || looker(r, sh, batches, done, cancel));
            }
            drop(done_tx);
            scope.spawn(move || {
                let mut token: Option<String> = None;
                loop {
                    let page = retry(|| {
                        sh.timed(r, Stage::List, 0, || {
                            r.source.list(after.as_deref(), token.as_deref())
                        })
                    });
                    let next = page.as_ref().ok().and_then(|p| p.next.clone());
                    let failed = page.is_err();
                    if page_tx.send(page).is_err() || failed || r.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match next {
                        Some(t) => token = Some(t),
                        None => return,
                    }
                }
            });
            let lookups = Lookups {
                tx: lookup_tx,
                threads: lookup_threads,
                in_flight: 0,
                asked: HashSet::new(),
            };
            let ended = self.coordinate(
                threads * WINDOW_PER_THREAD,
                task_tx,
                done_rx,
                page_rx,
                lookups,
            );
            // Queued work is not needed once the coordinator has ended.
            cancel.store(true, Ordering::SeqCst);
            ended
        })
    }

    /// Writes the open part, if any, then the checkpoint that covers it.
    fn checkpoint(&mut self) -> Result<(), Error> {
        let r = self.r;
        if !self.lines.is_empty() {
            let mut body = String::new();
            for l in &self.lines {
                body.push_str(&serde_json::to_string(l).map_err(|e| Error::Io(e.to_string()))?);
                body.push('\n');
            }
            let key = part_key(&r.job.run, self.seq);
            let size = body.len() as u64;
            retry(|| {
                self.sh.timed(r, Stage::Control, size, || {
                    r.dest
                        .put_object(&key, body.as_bytes(), "application/x-ndjson")
                })
            })?;
            self.seq += 1;
            self.lines.clear();
            self.part_bytes = 0;
        }
        let cp = Checkpoint {
            seq: self.seq,
            after_key: self.after_key.clone(),
            counters: self.counters.clone(),
        };
        let body = serde_json::to_vec(&cp).map_err(|e| Error::Io(e.to_string()))?;
        let key = checkpoint_key(&r.job.run);
        let size = body.len() as u64;
        retry(|| {
            self.sh.timed(r, Stage::Control, size, || {
                r.dest.put_object(&key, &body, "application/json")
            })
        })
    }

    /// This session's stage timings.
    fn timings(&self) -> crate::timing::Timings {
        let r = self.r;
        let elapsed = r.clock.now_us().saturating_sub(self.started_us) / 1000;
        self.sh
            .stages
            .snapshot(r.job.threads.clamp(1, MAX_THREADS as u32), elapsed)
    }

    /// Writes `status.json`. A failed write is noted, not fatal.
    fn status(&mut self, state: RunState) {
        let r = self.r;
        let now = r.clock.now_ms();
        self.last_status_ms = now;
        let secs = ((now - self.started_ms) as f64 / 1000.0).max(0.001);
        let read = self.counters.bytes_read.saturating_sub(self.base_read);
        let st = Status {
            run: r.job.run.clone(),
            state,
            dry_run: r.job.dry_run,
            counters: self.counters.clone(),
            started_ms: self.started_ms,
            updated_ms: now,
            read_rate: read as f64 / secs,
            eta_s: None,
            last_error: self.last_error.clone(),
            timings: Some(self.timings()),
        };
        if let Ok(body) = serde_json::to_vec(&st) {
            let key = status_key(&r.job.run);
            let size = body.len() as u64;
            if let Err(e) = retry(|| {
                self.sh.timed(r, Stage::Control, size, || {
                    r.dest.put_object(&key, &body, "application/json")
                })
            }) {
                self.last_error = Some(e.to_string());
            }
        }
    }

    /// Lists and handles every object after the checkpoint. Answers whether a stop ended it.
    /// With more than one thread the parallel engine makes the same decisions in the same order.
    fn drive(&mut self) -> Result<bool, Error> {
        let r = self.r;
        let threads = (r.job.threads as usize).min(MAX_THREADS);
        if threads > 1 {
            return self.drive_parallel(threads);
        }
        let after = self.after_key.clone();
        let mut token: Option<String> = None;
        let mut first = true;
        loop {
            let page = retry(|| {
                self.sh.timed(r, Stage::List, 0, || {
                    r.source.list(after.as_deref(), token.as_deref())
                })
            })?;
            if first {
                first = false;
                self.status(RunState::Running);
            }
            for obj in &page.objects {
                if r.stop.load(Ordering::SeqCst) {
                    return Ok(true);
                }
                if after.as_deref().is_some_and(|a| obj.key.as_str() <= a) {
                    continue;
                }
                self.object(obj);
                if self.exhausted {
                    return Ok(true);
                }
                self.after_object()?;
            }
            match page.next {
                Some(t) => token = Some(t),
                None => return Ok(r.stop.load(Ordering::SeqCst)),
            }
        }
    }

    /// The report for the state the run ended in.
    fn report(&self, state: RunState) -> Report {
        let mut top: Vec<(String, u64, u64)> = self
            .dups
            .iter()
            .filter(|(_, (n, _))| *n >= 2)
            .map(|(sha, (n, size))| (sha.clone(), *n, *size))
            .collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top.truncate(50);
        let finished_ms = self.r.clock.now_ms();
        let elapsed = finished_ms.saturating_sub(self.started_ms);
        let rate = |n: u64| {
            if elapsed > 0 {
                n as f64 * 1000.0 / elapsed as f64
            } else {
                0.0
            }
        };
        Report {
            run: self.r.job.run.clone(),
            state,
            dry_run: self.r.job.dry_run,
            counters: self.counters.clone(),
            flags: self.flags.iter().map(|(f, n)| (*f, *n)).collect(),
            conflicts: self.conflicts.clone(),
            failures: self.failures.clone(),
            not_opened: self.not_opened.clone(),
            top_duplicates: top,
            started_ms: self.started_ms,
            finished_ms,
            files_per_s: rate(self.counters.listed.saturating_sub(self.base_listed)),
            bytes_per_s: rate(self.counters.bytes_read.saturating_sub(self.base_read)),
            continues: self.r.job.resume.as_ref().map(|r| r.run.clone()),
            too_large: self.too_large.clone(),
            budget_spent: self.exhausted,
            timings: Some(self.timings()),
        }
    }

    /// The whole run after the engine is set up.
    fn go(&mut self) -> Result<Report, Error> {
        let r = self.r;
        self.status(RunState::Listing);
        let stopped = self.drive()?;
        self.checkpoint()?;
        let state = if stopped {
            RunState::Stopped
        } else {
            RunState::Done
        };
        let report = self.report(state);
        let body = serde_json::to_vec(&report).map_err(|e| Error::Io(e.to_string()))?;
        let key = report_key(&r.job.run);
        retry(|| r.dest.put_object(&key, &body, "application/json"))?;
        self.status(state);
        Ok(report)
    }
}

/// Runs the job to its end (or a stop), resuming from `checkpoint.json` if there is one (else
/// from the checkpoint of the run it continues, `job.resume`), and answers the report it wrote.
///
/// With `job.threads` above one, workers read, hash and upload while one coordinator decides in
/// listing order (docs/plans/ingest-throughput.md); the decisions, counters, manifest and report
/// are those of the serial engine. After a resume the report's
/// flag counts, lists and duplicate groups cover only this session, while its counters carry
/// on from the checkpoint. A dry run remembers would-be uploads, so a repeat is a duplicate.
pub fn run(run: &Run<'_>) -> Result<Report, Error> {
    let cp = match retry(|| run.dest.get_object(&checkpoint_key(&run.job.run)))? {
        Some(bytes) => serde_json::from_slice::<Checkpoint>(&bytes)
            .map_err(|e| Error::Io(format!("checkpoint.json is not readable: {e}")))?,
        // A continuation starts its own manifest at part 0 from the checkpoint it was given.
        None => run
            .job
            .resume
            .as_ref()
            .map_or_else(Checkpoint::default, |r| Checkpoint {
                seq: 0,
                ..r.checkpoint.clone()
            }),
    };
    let now = run.clock.now_ms();
    let shared = Shared::new();
    let mut e = Engine {
        r: run,
        sh: &shared,
        started_us: run.clock.now_us(),
        base_read: cp.counters.bytes_read,
        base_listed: cp.counters.listed,
        counters: cp.counters,
        seq: cp.seq,
        after_key: cp.after_key,
        seen: HashSet::new(),
        names: HashMap::new(),
        sims: Vec::new(),
        lines: Vec::new(),
        part_bytes: 0,
        flags: BTreeMap::new(),
        conflicts: Vec::new(),
        failures: Vec::new(),
        not_opened: Vec::new(),
        too_large: Vec::new(),
        exhausted: false,
        dups: HashMap::new(),
        started_ms: now,
        last_status_ms: now,
        last_error: None,
    };
    match e.go() {
        Ok(report) => Ok(report),
        Err(err) => {
            e.last_error = Some(err.to_string());
            e.status(RunState::Failed);
            Err(err)
        }
    }
}
