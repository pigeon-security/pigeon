//! One run, start to end (decision 13): list the source in key order, read each object once
//! into the spool while hashing, open zips, decide each file against R2, copy what is new,
//! and write manifest parts, checkpoints, the status and the report. Resumes from the last
//! checkpoint. Implemented over traits, so the runner gives it HTTPS and a spool directory and
//! the smoke suite gives it memory.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Writes `size` bytes from `body` at `key` with `metrics` as metadata
    /// (`x-amz-meta-sha256`, `-blake3`, `-size`, `-mime`); a single PUT also sends
    /// `x-amz-checksum-sha256`, larger ones go multipart. The body may fail its read at the end
    /// when the bytes do not match `metrics` (see [`Verify`]): the write must then not complete.
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
}

/// The time, given, so runs are reproducible in tests.
pub trait Clock: Sync {
    fn now_ms(&self) -> i64;
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

/// Runs `f` and, when it fails, again up to [`RETRIES`] more times.
fn retry<T>(mut f: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let mut tried = 0;
    loop {
        match f() {
            Ok(v) => return Ok(v),
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
    dups: HashMap<String, (u64, u64)>,
    started_ms: i64,
    last_status_ms: i64,
    base_read: u64,
    last_error: Option<String>,
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

impl<'r, 'a> Engine<'r, 'a> {
    fn dry(&self) -> bool {
        self.r.job.dry_run
    }

    /// Reads one object once: into a spool file while hashing when it fits, else hashing only.
    fn stage(&self, key: &str, size: u64) -> Result<Staged, Error> {
        let r = self.r;
        let mut src = r.source.open(key)?;
        let mut h = Hasher::new(r.job.fingerprints);
        let mut buf = vec![0u8; BUF];
        let mut spool_file = if size <= r.spool.free_bytes() {
            Some(r.spool.create()?)
        } else {
            None
        };
        loop {
            let n = src
                .read(&mut buf)
                .map_err(|e| Error::Source(e.to_string()))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            if let Some(f) = spool_file.as_mut() {
                f.write_all(&buf[..n])?;
            }
        }
        if let Some(f) = spool_file.as_mut() {
            f.seek(SeekFrom::Start(0))?;
        }
        Ok(Staged {
            metrics: h.finish()?,
            over_spool: spool_file.is_none(),
            file: spool_file,
        })
    }

    /// Writes the bytes of a staged file to `key`, from its spool file or a verified re-read.
    fn put(
        &self,
        key: &str,
        file: &mut Option<Box<dyn SpoolFile>>,
        metrics: &Metrics,
        reopen: Option<&str>,
    ) -> Result<(), Error> {
        let r = self.r;
        retry(|| match file.as_mut() {
            Some(f) => {
                f.seek(SeekFrom::Start(0))?;
                r.dest.put_blob(key, f, metrics.size, metrics)
            }
            None => {
                let src_key = reopen.ok_or_else(|| Error::Io("no source to copy from".into()))?;
                let src = r.source.open(src_key)?;
                let mut v = Verify::new(src, metrics.clone());
                r.dest.put_blob(key, &mut v, metrics.size, metrics)
            }
        })
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
            Verdict::Skipped => self.counters.skipped += 1,
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
            match retry(|| r.dest.head(&blob_key(&sha))) {
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
        if writes
            && let Some(key) = &r2_key
            && let Err(e) = self.put(key, &mut file, &metrics, item.reopen)
        {
            self.fail(item, Some(&metrics), flags, e);
            return;
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

    /// Handles one listed object.
    fn object(&mut self, obj: &Object) {
        let (account, bucket) = match &self.r.job.source {
            SourceSpec::S3 {
                account, bucket, ..
            } => (account.clone(), bucket.clone()),
            SourceSpec::Local { account, root } => (account.clone(), root.clone()),
        };
        let origin = Origin {
            account,
            bucket,
            key: obj.key.clone(),
            etag: obj.etag.clone(),
            listed_size: obj.size,
            modified: obj.modified.clone(),
        };
        let item = Item {
            origin: &origin,
            name: obj.key.clone(),
            member: None,
            chain: Vec::new(),
            flags: Vec::new(),
            depth: 0,
            declared: obj.size,
            reopen: Some(&obj.key),
        };
        self.counters.listed += 1;
        self.counters.listed_bytes += obj.size;
        match retry(|| self.stage(&obj.key, obj.size)) {
            Ok(staged) => {
                self.counters.bytes_read += staged.metrics.size;
                self.finish_file(&item, staged, None);
            }
            Err(e) => self.fail(&item, None, Vec::new(), e),
        }
        self.after_key = Some(obj.key.clone());
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
            retry(|| {
                r.dest
                    .put_object(&key, body.as_bytes(), "application/x-ndjson")
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
        retry(|| r.dest.put_object(&key, &body, "application/json"))
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
        };
        if let Ok(body) = serde_json::to_vec(&st) {
            let key = status_key(&r.job.run);
            if let Err(e) = retry(|| r.dest.put_object(&key, &body, "application/json")) {
                self.last_error = Some(e.to_string());
            }
        }
    }

    /// Lists and handles every object after the checkpoint. Answers whether a stop ended it.
    fn drive(&mut self) -> Result<bool, Error> {
        let r = self.r;
        let after = self.after_key.clone();
        let mut token: Option<String> = None;
        let mut first = true;
        loop {
            let page = retry(|| r.source.list(after.as_deref(), token.as_deref()))?;
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
                if self.lines.len() >= PART_LINES || self.part_bytes >= PART_BYTES {
                    self.checkpoint()?;
                    self.status(RunState::Running);
                } else if r.clock.now_ms() - self.last_status_ms >= STATUS_EVERY_MS {
                    self.status(RunState::Running);
                }
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
            finished_ms: self.r.clock.now_ms(),
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

/// Runs the job to its end (or a stop), resuming from `checkpoint.json` if there is one, and
/// answers the report it wrote.
///
/// Objects are handled one at a time; `job.threads` is future work. After a resume the report's
/// flag counts, lists and duplicate groups cover only this session, while its counters carry
/// on from the checkpoint. A dry run remembers would-be uploads, so a repeat is a duplicate.
pub fn run(run: &Run<'_>) -> Result<Report, Error> {
    let cp = match retry(|| run.dest.get_object(&checkpoint_key(&run.job.run)))? {
        Some(bytes) => serde_json::from_slice::<Checkpoint>(&bytes)
            .map_err(|e| Error::Io(format!("checkpoint.json is not readable: {e}")))?,
        None => Checkpoint::default(),
    };
    let now = run.clock.now_ms();
    let mut e = Engine {
        r: run,
        base_read: cp.counters.bytes_read,
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
