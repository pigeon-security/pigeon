//! Ingest throughput (docs/plans/ingest-throughput.md): run time from the file count, deadlines
//! and continuations; the parallel engine's equivalence with the serial one; live runs.

use pigeon_ingest::archive::Limits;
use pigeon_ingest::decide::Head;
use pigeon_ingest::decide::{Reason, Verdict};
use pigeon_ingest::engine::{Clock, Dest, Object, Page, Run, Source, Spool, SpoolFile, run};
use pigeon_ingest::manifest::{
    Checkpoint, DestSpec, JobSpec, ManifestLine, Report, Resume, SourceSpec, checkpoint_key,
};
use pigeon_ingest::metrics::Metrics;
use pigeon_ingest::{Error, blob_key};
use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A listing in pages of `page` objects; reads may be delayed to shuffle worker finishing order.
struct MemSource {
    objs: BTreeMap<String, Vec<u8>>,
    page: usize,
    /// Microseconds of delay for a read, from a key's hash.
    jitter_us: u64,
}

/// Sets `flag` once `key` is opened: a stop arriving mid-run.
struct StopAt<'a> {
    inner: &'a MemSource,
    key: String,
    flag: Arc<AtomicBool>,
}

impl Source for StopAt<'_> {
    fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error> {
        self.inner.list(after, token)
    }
    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error> {
        if key == self.key {
            self.flag.store(true, Ordering::SeqCst);
        }
        self.inner.open(key)
    }
}

impl Source for MemSource {
    fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error> {
        let start = token.or(after);
        let all: Vec<Object> = self
            .objs
            .iter()
            .filter(|(k, _)| start.is_none_or(|s| k.as_str() > s))
            .map(|(k, v)| Object {
                key: k.clone(),
                size: v.len() as u64,
                etag: None,
                modified: None,
            })
            .collect();
        let next = (all.len() > self.page).then(|| all[self.page - 1].key.clone());
        Ok(Page {
            objects: all.into_iter().take(self.page).collect(),
            next,
        })
    }
    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error> {
        if self.jitter_us > 0 {
            let h = key
                .bytes()
                .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
            std::thread::sleep(std::time::Duration::from_micros(h % self.jitter_us));
        }
        self.objs
            .get(key)
            .map(|v| Box::new(Cursor::new(v.clone())) as Box<dyn Read + Send>)
            .ok_or_else(|| Error::Source("no such key".into()))
    }
}

type Stored = (Vec<u8>, Option<String>, Option<String>);

/// R2 in memory: blob writes are create-only, like the managed destination and `If-None-Match`.
#[derive(Default)]
struct MemDest {
    objs: Mutex<HashMap<String, Stored>>,
    blob_puts: AtomicU64,
    heads: AtomicU64,
    head_many: AtomicU64,
    /// Blob keys whose next writes fail, and how many.
    fail_first: Mutex<HashMap<String, u32>>,
    /// Answers every HEAD as absent: another writer stores blobs after the check.
    blind: bool,
    /// Blob writes left in the run's grant, when limited.
    budget: Mutex<Option<u64>>,
    /// Control writes (parts, checkpoints, status, report) that fail next, as a hiccup.
    flaky_control: Mutex<u32>,
}

impl MemDest {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.objs.lock().unwrap().get(key).map(|v| v.0.clone())
    }
    fn lines(&self, run_id: &str) -> Vec<ManifestLine> {
        let prefix = format!("ingest/runs/{run_id}/manifest/");
        let mut keys: Vec<String> = self
            .objs
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        keys.sort();
        keys.iter()
            .flat_map(|k| {
                let body = self.get(k).unwrap();
                String::from_utf8(body)
                    .unwrap()
                    .lines()
                    .map(|l| serde_json::from_str(l).unwrap())
                    .collect::<Vec<ManifestLine>>()
            })
            .collect()
    }
    fn part_keys(&self, run_id: &str) -> Vec<String> {
        let prefix = format!("ingest/runs/{run_id}/manifest/");
        let mut keys: Vec<String> = self
            .objs
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        keys.sort();
        keys
    }
}

impl Dest for MemDest {
    fn head(&self, key: &str) -> Result<Option<Head>, Error> {
        self.heads.fetch_add(1, Ordering::SeqCst);
        if self.blind {
            return Ok(None);
        }
        Ok(self.objs.lock().unwrap().get(key).map(|(b, s, h)| Head {
            size: b.len() as u64,
            sha256: s.clone(),
            blake3: h.clone(),
        }))
    }
    fn put_blob(
        &self,
        key: &str,
        body: &mut dyn Read,
        size: u64,
        m: &Metrics,
    ) -> Result<(), Error> {
        let mut buf = Vec::new();
        body.read_to_end(&mut buf)
            .map_err(|e| Error::Dest(e.to_string()))?;
        if buf.len() as u64 != size {
            return Err(Error::Dest("wrong length".into()));
        }
        if let Some(left) = self.budget.lock().unwrap().as_mut() {
            if *left == 0 {
                return Err(Error::Exhausted);
            }
            *left -= 1;
        }
        if let Some(left) = self.fail_first.lock().unwrap().get_mut(key)
            && *left > 0
        {
            *left -= 1;
            return Err(Error::Dest("flaky write".into()));
        }
        let mut objs = self.objs.lock().unwrap();
        if objs.contains_key(key) {
            return Err(Error::Exists);
        }
        self.blob_puts.fetch_add(1, Ordering::SeqCst);
        objs.insert(
            key.to_string(),
            (buf, Some(m.sha256.clone()), Some(m.blake3.clone())),
        );
        Ok(())
    }
    fn head_many(&self, keys: &[String]) -> Result<Vec<Option<Head>>, Error> {
        self.head_many.fetch_add(1, Ordering::SeqCst);
        let heads = self.heads.load(Ordering::SeqCst);
        let out = keys.iter().map(|k| self.head(k)).collect();
        self.heads.store(heads, Ordering::SeqCst);
        out
    }
    fn put_object(&self, key: &str, body: &[u8], _: &str) -> Result<(), Error> {
        {
            let mut left = self.flaky_control.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Err(Error::Dest("the Data Worker is busy".into()));
            }
        }
        self.objs
            .lock()
            .unwrap()
            .insert(key.to_string(), (body.to_vec(), None, None));
        Ok(())
    }
    fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.get(key))
    }
}

struct MemSpool;
impl Spool for MemSpool {
    fn create(&self) -> Result<Box<dyn SpoolFile>, Error> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }
    fn free_bytes(&self) -> u64 {
        1 << 30
    }
}

struct Fixed;
impl Clock for Fixed {
    fn now_ms(&self) -> i64 {
        1000
    }
}

fn job(run_id: &str, dry: bool, threads: u32) -> JobSpec {
    JobSpec {
        run: run_id.into(),
        instance: "t".into(),
        source: SourceSpec::S3 {
            account: "acct".into(),
            endpoint: "https://s3.test".into(),
            region: "r".into(),
            bucket: "src".into(),
            prefix: String::new(),
        },
        dest: DestSpec {
            endpoint: "https://r2.test".into(),
            bucket: "dst".into(),
        },
        dry_run: dry,
        keep_archives: true,
        fingerprints: false,
        limits: Limits::default(),
        threads,
        commit: "c".into(),
        max_minutes: None,
        resume: None,
        skip_over: None,
        shard: None,
        only_over: None,
    }
}

fn go(job: &JobSpec, source: &dyn Source, dest: &MemDest) -> Report {
    go_until(job, source, dest, &AtomicBool::new(false))
}

/// The run's report without its stage timings, which differ between engines (the timing test
/// reads them).
fn go_until(job: &JobSpec, source: &dyn Source, dest: &MemDest, stop: &AtomicBool) -> Report {
    let mut report = run(&Run {
        job,
        source,
        dest,
        spool: &MemSpool,
        clock: &Fixed,
        stop,
    })
    .unwrap();
    assert!(report.timings.is_some(), "every report has its timings");
    report.timings = None;
    report
}

fn metrics(b: &[u8]) -> Metrics {
    let mut h = pigeon_ingest::metrics::Hasher::new(false);
    h.update(b);
    h.finish().unwrap()
}

/// Every object R2 holds, sorted, with its bytes and identity.
fn stored(dest: &MemDest, prefix: &str) -> Vec<(String, Stored)> {
    let mut out: Vec<(String, Stored)> = dest
        .objs
        .lock()
        .unwrap()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.sort();
    out
}

/// A zip holding `files`.
fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        w.start_file(*name, SimpleFileOptions::default()).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// A source of `n` small files with planted duplicates: runs of identical neighbours (decided
/// in the same batch), repeats far apart, blobs R2 already holds, a conflict, a zip holding a
/// duplicate, an archive left unopened, and a blob whose first upload fails.
fn planted(n: usize) -> (MemSource, MemDest) {
    let mut objs = BTreeMap::new();
    for i in 0..n {
        let body = match i % 10 {
            // Neighbours with the same bytes.
            3 | 4 => format!("pair {}\n", i / 10),
            // The same bytes every 70 objects, across pages and batches.
            7 => format!("far {}\n", i % 70),
            _ => format!("unique {i}\n"),
        };
        objs.insert(format!("o/{i:05}"), body.into_bytes());
    }
    objs.insert("p/known.txt".into(), b"already in r2\n".to_vec());
    objs.insert("p/known-again.txt".into(), b"already in r2\n".to_vec());
    objs.insert("p/conflict.bin".into(), b"conflicting\n".to_vec());
    objs.insert(
        "p/z.zip".into(),
        zip_of(&[("a.txt", b"pair 1\n"), ("b.txt", b"only in the zip\n")]),
    );
    objs.insert("p/flaky-1.txt".into(), b"flaky blob\n".to_vec());
    objs.insert("p/flaky-2.txt".into(), b"flaky blob\n".to_vec());
    objs.insert("p/flaky-3.txt".into(), b"flaky blob\n".to_vec());
    let dest = MemDest::default();
    let known = metrics(b"already in r2\n");
    dest.objs.lock().unwrap().insert(
        blob_key(&known.sha256),
        (
            b"already in r2\n".to_vec(),
            Some(known.sha256.clone()),
            Some(known.blake3.clone()),
        ),
    );
    let conflict = metrics(b"conflicting\n");
    dest.objs.lock().unwrap().insert(
        blob_key(&conflict.sha256),
        (b"something else".to_vec(), Some("0".repeat(64)), None),
    );
    // Four failures exhaust the retries of the first upload; the next sighting succeeds.
    dest.fail_first
        .lock()
        .unwrap()
        .insert(blob_key(&metrics(b"flaky blob\n").sha256), 4);
    (
        MemSource {
            objs,
            page: 97,
            jitter_us: 100,
        },
        dest,
    )
}

/// The serial engine (one thread) and the parallel engine (64 threads, reads finishing in a
/// shuffled order) make identical decisions: the same counters, manifest lines and parts,
/// checkpoint, report and stored blobs, for live and dry runs.
#[test]
fn ingest_parallel_engine_matches_the_serial_engine() {
    for dry in [false, true] {
        // More than one manifest part (5,000 lines each).
        let (source, serial_dest) = planted(5_600);
        let (_, parallel_dest) = planted(5_600);
        let serial = go(&job("run", dry, 1), &source, &serial_dest);
        let parallel = go(&job("run", dry, 64), &source, &parallel_dest);
        assert_eq!(serial, parallel, "dry {dry}");
        assert_eq!(
            serial_dest.lines("run"),
            parallel_dest.lines("run"),
            "dry {dry}"
        );
        assert_eq!(serial_dest.part_keys("run").len(), 2);
        assert_eq!(serial_dest.part_keys("run"), parallel_dest.part_keys("run"));
        assert_eq!(
            serial_dest.get(&checkpoint_key("run")),
            parallel_dest.get(&checkpoint_key("run"))
        );
        assert_eq!(
            stored(&serial_dest, "blobs/"),
            stored(&parallel_dest, "blobs/")
        );
        assert_eq!(
            stored(&serial_dest, "quarantine/"),
            stored(&parallel_dest, "quarantine/")
        );
        // The batched existence check stood in for most single HEADs.
        assert_eq!(serial_dest.head_many.load(Ordering::SeqCst), 0);
        assert!(parallel_dest.head_many.load(Ordering::SeqCst) > 0);
        assert!(
            parallel_dest.heads.load(Ordering::SeqCst) * 4
                < serial_dest.heads.load(Ordering::SeqCst)
        );

        let c = &serial.counters;
        assert_eq!(c.listed, 5_607);
        assert_eq!(c.conflicts, 1);
        assert_eq!(c.archives_opened, 1);
        let lines = serial_dest.lines("run");
        let by_key = |k: &str| {
            lines
                .iter()
                .find(|l| l.origin.key == k && l.member.is_none())
                .unwrap()
                .clone()
        };
        // Neighbours: the first is canonical, the second a duplicate seen in this run.
        let first = by_key("o/00003");
        let second = by_key("o/00004");
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(second.verdict, Verdict::Duplicate);
        assert_eq!(second.reason, Reason::SeenInRun);
        assert_eq!(
            by_key("p/known-again.txt").reason,
            Reason::InR2,
            "already held blobs are duplicates"
        );
        if dry {
            assert_eq!(first.verdict, Verdict::WouldUpload);
            assert_eq!(serial_dest.blob_puts.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(first.verdict, Verdict::Upload);
            // A failed upload is never a duplicate source: the next sighting is canonical.
            assert_eq!(by_key("p/flaky-1.txt").verdict, Verdict::Failed);
            assert_eq!(by_key("p/flaky-2.txt").verdict, Verdict::Upload);
            assert_eq!(by_key("p/flaky-3.txt").verdict, Verdict::Duplicate);
            // Every source object kept its sighting, pointing at the canonical blob.
            assert!(lines.iter().all(|l| l.verdict == Verdict::Failed
                || l.verdict == Verdict::Skipped
                || l.r2_key.is_some()));
            assert_eq!(
                c.uploaded + c.conflicts,
                serial_dest.blob_puts.load(Ordering::SeqCst)
            );
        }
    }
}

/// Two writers of the same blob: one stores it, the other is told it exists, and the engine
/// records that as a duplicate, never an error, on either engine.
#[test]
fn ingest_create_only_race_records_a_duplicate() {
    let dest = Arc::new(MemDest::default());
    let body = b"raced blob\n".to_vec();
    let m = metrics(&body);
    let key = blob_key(&m.sha256);
    let results: Vec<Result<(), Error>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..2)
            .map(|_| {
                let (dest, body, m, key) = (&dest, &body, &m, &key);
                s.spawn(move || {
                    dest.put_blob(key, &mut Cursor::new(body.clone()), body.len() as u64, m)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results.iter().filter(|r| **r == Err(Error::Exists)).count(),
        1
    );
    assert_eq!(dest.blob_puts.load(Ordering::SeqCst), 1);

    // Another writer stores the blob after the existence check (the HEAD says absent): the
    // create-only upload finds it and the line is a duplicate in R2.
    for threads in [1, 64] {
        let mut objs = BTreeMap::new();
        objs.insert("a.txt".to_string(), body.clone());
        objs.insert("b.txt".to_string(), b"fresh\n".to_vec());
        let source = MemSource {
            objs,
            page: 10,
            jitter_us: 0,
        };
        let dest = MemDest {
            blind: true,
            ..MemDest::default()
        };
        dest.objs.lock().unwrap().insert(
            key.clone(),
            (body.clone(), Some(m.sha256.clone()), Some(m.blake3.clone())),
        );
        let report = go(&job("race", false, threads), &source, &dest);
        assert_eq!(report.counters.failed, 0);
        assert_eq!(report.counters.duplicates, 1);
        assert_eq!(report.counters.uploaded, 1);
        let lines = dest.lines("race");
        assert_eq!(lines[0].verdict, Verdict::Duplicate);
        assert_eq!(lines[0].reason, Reason::InR2);
        assert_eq!(lines[0].r2_key.as_deref(), Some(key.as_str()));
    }
}

/// A parallel run stopped mid-way resumes from its checkpoint: every object is decided exactly
/// once across both sessions, none skipped, and only first sightings were copied.
#[test]
fn ingest_parallel_engine_resumes_without_skips_or_repeats() {
    let (source, dest) = planted(1_200);
    let flag = Arc::new(AtomicBool::new(false));
    let stopping = StopAt {
        inner: &source,
        key: "o/00600".into(),
        flag: Arc::clone(&flag),
    };
    let first = go_until(&job("resume", false, 64), &stopping, &dest, &flag);
    assert_eq!(first.state, pigeon_ingest::manifest::RunState::Stopped);
    assert!(first.counters.listed >= 601 && first.counters.listed < 1_207);
    let second = go(&job("resume", false, 64), &source, &dest);
    assert_eq!(second.counters.listed, 1_207);
    let lines = dest.lines("resume");
    let mut keys: Vec<&str> = lines
        .iter()
        .filter(|l| l.member.is_none())
        .map(|l| l.origin.key.as_str())
        .collect();
    let in_order = keys.clone();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), 1_207, "each object once");
    assert_eq!(in_order, keys, "in listing order");
    assert_eq!(
        keys,
        source.objs.keys().map(String::as_str).collect::<Vec<_>>()
    );
    // Each blob was written once: the second session found the first session's blobs in R2.
    let (_, whole_dest) = planted(1_200);
    let whole = go(&job("whole", false, 1), &source, &whole_dest);
    assert_eq!(
        stored(&dest, "blobs/"),
        stored(&whole_dest, "blobs/"),
        "the same blobs as an uninterrupted run"
    );
    assert_eq!(
        dest.blob_puts.load(Ordering::SeqCst),
        whole_dest.blob_puts.load(Ordering::SeqCst)
    );
    assert_eq!(whole.counters.listed, 1_207);
}

/// Run time is the slower of the byte time and the file time; tiny files are file-count-bound,
/// deadlines are 1.5 times the slow time plus 10 minutes within the host's ceiling, and a
/// measured rate is preferred.
#[test]
fn ingest_estimates_from_file_count_and_deadlines() {
    use pigeon_ingest::estimate::{
        Host, Measured, Throughput, ceiling_minutes, deadline_minutes, estimate_run, measure,
        run_minutes, totals,
    };

    // willowgraysen's txt/ prefix: about 471,000 files of about 640 bytes.
    let (bytes, files) = (301_440_000u64, 471_000u64);
    let serial = estimate_run(
        Host::Cloudflare,
        bytes,
        files,
        0.3,
        &Throughput::new(Host::Cloudflare, 1),
    );
    assert_eq!(serial.bound, "files");
    // 471,000 x 70 ms is about 9.2 hours; bytes alone would be seconds.
    assert!(
        serial.hours_high > 9.0 && serial.hours_high < 9.3,
        "{serial:?}"
    );
    assert_eq!(serial.confidence, "high");
    // 1.5 x 549.5 minutes + 10.
    assert_eq!(deadline_minutes(&serial, 24 * 60), 835);

    // The measured 3.5 files a second wins over the default.
    let measured = Throughput {
        measured: Some(Measured {
            run: "20261128-b2-6f65616b335f326c6a3438616d507530".into(),
            files_per_s: 3.5,
            bytes_per_s: 2240.0,
        }),
        ..Throughput::new(Host::Cloudflare, 32)
    };
    let m = estimate_run(Host::Cloudflare, bytes, files, 0.3, &measured);
    assert!(m.hours_high > 37.0 && m.hours_high < 38.0, "{m:?}");
    assert_eq!(
        m.measured_run.as_deref(),
        Some("20261128-b2-6f65616b335f326c6a3438616d507530")
    );
    assert!(m.assumptions.iter().any(|a| a.contains("measured by run")));
    // Clamped to the container's 24 hours; continue covers the rest.
    assert_eq!(
        deadline_minutes(&m, ceiling_minutes(Host::Cloudflare)),
        1440
    );

    // Parallel: 32 in flight at 70 ms is about 457 files a second.
    let p = estimate_run(
        Host::Cloudflare,
        bytes,
        files,
        0.3,
        &Throughput::new(Host::Cloudflare, 32),
    );
    assert!((p.files_per_s - 457.1).abs() < 0.1, "{p:?}");
    assert!(p.hours_high < 0.3);
    // 1.5 x 17 minutes + 10.
    assert_eq!(deadline_minutes(&p, 1440), 36);

    // Large files stay byte-bound; a short run gets at least 15 minutes.
    let big = estimate_run(
        Host::DigitalOcean,
        1_000_000_000,
        10,
        0.3,
        &Throughput::new(Host::DigitalOcean, 1),
    );
    assert_eq!(big.bound, "bytes");
    assert_eq!(deadline_minutes(&big, 1440), 15);
    // 360 GB at 100 MB/s is an hour: 1.5 x 60 + 10.
    let hour = estimate_run(
        Host::Scaleway,
        360_000_000_000,
        1000,
        0.3,
        &Throughput::new(Host::Scaleway, 1),
    );
    assert_eq!(deadline_minutes(&hour, 1440), 100);
    // DigitalOcean's max_minutes overrides the computed default, within 10 to 1440.
    assert_eq!(run_minutes(Host::DigitalOcean, &hour, Some(45)), Ok(45));
    assert_eq!(run_minutes(Host::DigitalOcean, &hour, None), Ok(100));
    assert!(run_minutes(Host::DigitalOcean, &hour, Some(5)).is_err());
    assert!(run_minutes(Host::DigitalOcean, &hour, Some(1441)).is_err());

    // `objects` and `size_bytes`: the declared totals win; without `objects` the count is
    // extrapolated from the sample, and small extrapolated objects are low confidence.
    assert_eq!(
        totals(1000, 640_000, true, Some(bytes), Some(files)),
        (bytes, files, false)
    );
    let (b, o, extrapolated) = totals(1000, 640_000, true, Some(bytes), None);
    assert_eq!((b, o, extrapolated), (bytes, 471_000, true));
    let low = estimate_run(
        Host::Scaleway,
        b,
        o,
        0.3,
        &Throughput {
            extrapolated,
            ..Throughput::new(Host::Scaleway, 1)
        },
    );
    assert_eq!(low.confidence, "low");
    assert!(low.assumptions.iter().any(|a| a.contains("Pass `objects`")));
    // A fully listed page needs nothing declared; a declared size below the sample is raised.
    assert_eq!(totals(10, 5000, false, None, None), (5000, 10, false));
    assert_eq!(totals(10, 5000, false, Some(10), None), (5000, 10, false));
    // A named Scaleway or Spaces connection honours size_bytes like every other source (it
    // used to keep only the first page).
    assert_eq!(
        totals(1000, 1_000_000, true, Some(10_000_000), None),
        (10_000_000, 10_000, true)
    );

    // A measured rate needs at least 100 objects over a minute.
    assert!(measure("r", 99, 1, 600_000).is_none());
    assert!(measure("r", 1000, 1, 59_000).is_none());
    let rate = measure("r", 1260, 806_400, 360_000).unwrap();
    assert!((rate.files_per_s - 3.5).abs() < 1e-9);
}

/// The runner stops itself shortly before its approved time ends.
#[test]
fn ingest_runner_stops_before_its_deadline() {
    let mut j = job("r", true, 1);
    assert_eq!(j.stop_at_ms(0), None);
    j.max_minutes = Some(60);
    assert_eq!(j.stop_at_ms(1000), Some(1000 + 55 * 60_000));
    j.max_minutes = Some(20);
    assert_eq!(j.stop_at_ms(0), Some(18 * 60_000));
    // Older jobs (no max_minutes, no resume) keep their exact JSON, so their approvals hold.
    let text = serde_json::to_string(&job("r", true, 1)).unwrap();
    assert!(!text.contains("max_minutes") && !text.contains("resume"));
}

/// A continuation starts from the continued run's checkpoint: nothing at or before its key is
/// read again, its counters carry on, and its own manifest starts at part 0.
#[test]
fn ingest_continues_from_another_runs_checkpoint() {
    let mut objs = BTreeMap::new();
    for i in 0..30u32 {
        objs.insert(format!("k/{i:03}"), format!("file {i}\n").into_bytes());
    }
    let source = MemSource {
        objs,
        page: 7,
        jitter_us: 0,
    };
    let whole = MemDest::default();
    let full = go(&job("whole", false, 1), &source, &whole);
    assert_eq!(full.counters.listed, 30);

    // The first run stopped after k/011 (12 objects).
    let first = MemDest::default();
    let partial = MemSource {
        objs: source
            .objs
            .iter()
            .take(12)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        page: 7,
        jitter_us: 0,
    };
    let stopped = go(&job("first", false, 1), &partial, &first);
    let cp: Checkpoint =
        serde_json::from_slice(&first.get(&checkpoint_key("first")).unwrap()).unwrap();
    assert_eq!(cp.after_key.as_deref(), Some("k/011"));
    assert_eq!(cp.counters, stopped.counters);

    let mut next = job("second", false, 1);
    next.resume = Some(Resume {
        run: "first".into(),
        checkpoint: cp,
    });
    let cont = go(&next, &source, &first);
    assert_eq!(cont.continues.as_deref(), Some("first"));
    assert_eq!(cont.counters.listed, 30);
    assert_eq!(cont.counters.uploaded, 30);
    let lines = first.lines("second");
    assert_eq!(lines.len(), 18);
    assert_eq!(lines[0].origin.key, "k/012");
    assert_eq!(
        first.part_keys("second")[0],
        "ingest/runs/second/manifest/00000000.jsonl"
    );
    // Every object was copied exactly once across both runs.
    assert_eq!(first.blob_puts.load(Ordering::SeqCst), 30);
    // The continuation's own checkpoint wins over the carried one on a restart.
    let again = go(&next, &source, &first);
    assert_eq!(again.counters.listed, 30);
    assert_eq!(first.lines("second").len(), 18);
}

/// The Data Worker's batched existence check stays inside the run's grant: 1 to 500 distinct
/// payload keys of this run, nothing else.
#[test]
fn ingest_head_many_stays_inside_the_grant() {
    use pigeon_ingest::managed_destination::{HEAD_MANY, HeadMany, head_many_keys};
    use pigeon_ingest::manifest::quarantine_key;
    let sha = |i: usize| format!("{i:064x}");
    let batch = |keys: Vec<String>| HeadMany { keys };
    assert!(head_many_keys(
        "run",
        &batch(vec![blob_key(&sha(1)), quarantine_key("run", &sha(2))])
    ));
    assert!(head_many_keys(
        "run",
        &batch((0..HEAD_MANY).map(|i| blob_key(&sha(i))).collect())
    ));
    assert!(!head_many_keys(
        "run",
        &batch((0..=HEAD_MANY).map(|i| blob_key(&sha(i))).collect())
    ));
    assert!(!head_many_keys("run", &batch(Vec::new())));
    assert!(!head_many_keys(
        "run",
        &batch(vec![blob_key(&sha(1)), blob_key(&sha(1))])
    ));
    assert!(!head_many_keys(
        "run",
        &batch(vec![quarantine_key("other", &sha(2))])
    ));
    assert!(!head_many_keys("run", &batch(vec![checkpoint_key("run")])));
    // The body is a strict shape.
    assert!(serde_json::from_str::<HeadMany>(r#"{"keys":[],"more":1}"#).is_err());
}

/// The one live guard for every host: dry runs always; live runs across many pages when the
/// whole size is declared and within the cap; objects over 16 MiB through multipart when the
/// Data Worker has it, else skipped and listed.
#[test]
fn ingest_live_guard_allows_runs_beyond_one_page() {
    use pigeon_ingest::immutable_multipart::MAX_BYTES;
    use pigeon_ingest::managed_destination::{LIVE_CAP_BYTES, MAX_BLOB, live_guard};
    let gb = 1_000_000_000u64;
    // A dry run is never limited.
    let dry = live_guard(true, true, None, 0, 100 * MAX_BLOB, false, 1).unwrap();
    assert_eq!(dry.skip_over, None);
    // A live run over many pages needs the declared size, and stays within the cap.
    assert!(live_guard(false, true, None, gb, 10, false, LIVE_CAP_BYTES).is_err());
    let many = live_guard(false, true, Some(5 * gb), gb, 10, false, LIVE_CAP_BYTES).unwrap();
    assert_eq!(many.skip_over, Some(MAX_BLOB));
    assert!(many.notes.iter().any(|n| n.contains("skipped and listed")));
    let over = live_guard(false, true, Some(11 * gb), gb, 10, true, LIVE_CAP_BYTES);
    assert!(over.unwrap_err().contains("max_live_bytes"));
    // The person raises the cap in the approved plan.
    assert!(live_guard(false, true, Some(11 * gb), gb, 10, true, 20 * gb).is_ok());
    // With multipart configured, large objects are written.
    let big = live_guard(false, false, None, gb, 2 * MAX_BLOB, true, LIVE_CAP_BYTES).unwrap();
    assert_eq!(big.skip_over, Some(MAX_BYTES));
    // A fully listed page needs nothing declared.
    assert!(live_guard(false, false, None, gb, 10, false, LIVE_CAP_BYTES).is_ok());
}

/// Objects larger than the run takes are not read: skipped, flagged `too_large` and listed in
/// the report, identically on both engines.
#[test]
fn ingest_skips_and_lists_objects_too_large_for_the_run() {
    use pigeon_ingest::archive::Flag;
    let mut objs = BTreeMap::new();
    for i in 0..40u32 {
        let size = if i % 9 == 0 { 300 } else { 20 };
        objs.insert(format!("k/{i:03}"), vec![b'a' + (i % 26) as u8; size]);
    }
    let source = MemSource {
        objs,
        page: 8,
        jitter_us: 50,
    };
    let mut reports = Vec::new();
    for threads in [1, 64] {
        let dest = MemDest::default();
        let mut j = job("big", false, threads);
        j.skip_over = Some(100);
        let report = go(&j, &source, &dest);
        assert_eq!(report.counters.skipped, 5);
        assert_eq!(report.too_large.len(), 5);
        assert!(
            report
                .too_large
                .iter()
                .all(|l| l.flags == vec![Flag::TooLarge] && l.sha256.is_empty())
        );
        assert_eq!(report.counters.listed, 40);
        reports.push((report, dest.lines("big")));
    }
    assert_eq!(reports[0], reports[1]);
}

/// docs/plans/multipart-enable.md, slice c: a run that skipped objects as too large is picked
/// up by one that takes exactly those (`only_over` behind a fenced resume), on both engines;
/// a runner that ignores `only_over` resumes after every key and does nothing.
#[test]
fn ingest_picks_up_exactly_the_too_large_objects() {
    use pigeon_capabilities::ingest::throughput::too_large_of;
    use pigeon_ingest::manifest::{FENCE, fenced, is_fenced};
    use serde_json::{Value, json};
    let mut objs = BTreeMap::new();
    for i in 0..40u32 {
        let size = if i % 9 == 0 { 300 + i as usize } else { 20 };
        objs.insert(format!("k/{i:03}"), vec![b'a' + (i % 26) as u8; size]);
    }
    let source = MemSource {
        objs,
        page: 8,
        jitter_us: 50,
    };
    for threads in [1, 64] {
        let dest = MemDest::default();
        let mut first = job("skipping", false, threads);
        first.skip_over = Some(100);
        let skipped = go(&first, &source, &dest);
        let mut wanted: Vec<String> = skipped
            .too_large
            .iter()
            .map(|l| l.origin.key.clone())
            .collect();
        wanted.sort();
        assert_eq!(wanted.len(), 5);

        // The pick-up: from the start, only objects over the skipped size.
        let mut pick = job("pickup", false, threads);
        pick.only_over = Some(100);
        pick.resume = Some(fenced("skipping"));
        assert!(is_fenced(pick.resume.as_ref().unwrap()));
        let picked = go(&pick, &source, &dest);
        assert_eq!(picked.counters.listed, 5);
        assert!(picked.too_large.is_empty());
        let mut got: Vec<String> = dest
            .lines("pickup")
            .iter()
            .map(|l| l.origin.key.clone())
            .collect();
        got.sort();
        got.dedup();
        assert_eq!(got, wanted);

        // An older runner reads the same job without `only_over`: it lists nothing.
        let mut old = pick.clone();
        old.run = "old-runner".into();
        old.only_over = None;
        let none = go(&old, &source, &MemDest::default());
        assert_eq!(none.counters.listed, 0);
        assert!(source.objs.keys().all(|k| k.as_str() < FENCE));
    }

    // What the plan reads from the prior run: its threshold and count, live runs only.
    let mut prior = job("prior", false, 1);
    prior.skip_over = Some(16 << 20);
    let report = json!({
        "flags": [["too_large", 262], ["duplicate", 3]],
        "too_large": [{"origin": {"listed_size": 20_000_000}}, {"origin": {"listed_size": 30_000_000}}],
    });
    let found = too_large_of(&prior, &report, "t").unwrap();
    assert_eq!((found.over, found.count, found.listed), (16 << 20, 262, 2));
    assert_eq!(found.listed_bytes, 50_000_000);
    assert!(too_large_of(&prior, &report, "other").is_err());
    assert!(too_large_of(&prior, &Value::Null, "t").is_err());
    assert!(too_large_of(&prior, &json!({"flags": [], "too_large": []}), "t").is_err());
    let mut dry = prior.clone();
    dry.dry_run = true;
    assert!(too_large_of(&dry, &report, "t").is_err());
    prior.skip_over = None;
    assert!(too_large_of(&prior, &report, "t").is_err());
}

/// A spent write grant stops the run before the object that found it, so a continuation with a
/// new grant finishes the rest: every object exactly once, none lost.
#[test]
fn ingest_spent_grant_stops_cleanly_for_a_continuation() {
    for threads in [1, 64] {
        let (source, dest) = planted(400);
        *dest.budget.lock().unwrap() = Some(100);
        let first = go(&job("spent", false, threads), &source, &dest);
        assert_eq!(first.state, pigeon_ingest::manifest::RunState::Stopped);
        assert!(first.budget_spent);
        assert_eq!(first.counters.failed, 0, "nothing is failed for the budget");
        let cp: Checkpoint =
            serde_json::from_slice(&dest.get(&checkpoint_key("spent")).unwrap()).unwrap();
        let after = cp.after_key.clone().unwrap();
        assert_eq!(cp.counters.listed, first.counters.listed);
        // A continuation with a new grant.
        *dest.budget.lock().unwrap() = None;
        let mut next = job("spent-2", false, threads);
        next.resume = Some(Resume {
            run: "spent".into(),
            checkpoint: cp,
        });
        let second = go(&next, &source, &dest);
        assert_eq!(second.counters.listed, 407);
        let mut keys: Vec<String> = dest
            .lines("spent")
            .into_iter()
            .chain(dest.lines("spent-2"))
            .filter(|l| l.member.is_none())
            .map(|l| l.origin.key)
            .collect();
        assert!(keys.iter().any(|k| *k > after));
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "no object twice");
        assert_eq!(n, 407, "no object lost");
    }
}

// ---- the D1 index (docs/plans/ingest-index-dry-run.md) ----

/// The lite store's D1 with the index migration applied, in memory.
fn index_db() -> rusqlite::Connection {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let m = pigeon_ingest::datastore::MIGRATIONS
        .iter()
        .find(|m| m.id == pigeon_ingest::datastore::INDEX_MIGRATION)
        .unwrap();
    db.execute(pigeon_ingest::datastore::MIGRATIONS_TABLE, [])
        .unwrap();
    for s in pigeon_ingest::datastore::migration_statements(m, "now") {
        sql_run(&db, &s);
    }
    db
}

fn sql_params(s: &pigeon_ingest::index::Statement) -> Vec<rusqlite::types::Value> {
    use serde_json::Value;
    s.params
        .iter()
        .map(|v| match v {
            Value::Null => rusqlite::types::Value::Null,
            Value::Number(n) if n.is_i64() => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
            Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap()),
            Value::String(t) => rusqlite::types::Value::Text(t.clone()),
            other => rusqlite::types::Value::Text(other.to_string()),
        })
        .collect()
}

fn sql_run(db: &rusqlite::Connection, s: &pigeon_ingest::index::Statement) {
    assert!(
        s.params.len() <= pigeon_ingest::index::MAX_PARAMS,
        "{}",
        s.sql
    );
    db.execute(&s.sql, rusqlite::params_from_iter(sql_params(s)))
        .unwrap_or_else(|e| panic!("{e}: {}", s.sql));
}

/// The rows of a query as JSON objects, as D1 answers them.
fn sql_rows(
    db: &rusqlite::Connection,
    s: &pigeon_ingest::index::Statement,
) -> Vec<serde_json::Value> {
    use rusqlite::types::ValueRef;
    let mut q = db.prepare(&s.sql).unwrap();
    let names: Vec<String> = q.column_names().iter().map(|n| n.to_string()).collect();
    q.query_map(rusqlite::params_from_iter(sql_params(s)), |r| {
        let mut row = serde_json::Map::new();
        for (i, n) in names.iter().enumerate() {
            let v = match r.get_ref(i)? {
                ValueRef::Null => serde_json::Value::Null,
                ValueRef::Integer(n) => serde_json::json!(n),
                ValueRef::Real(f) => serde_json::json!(f),
                ValueRef::Text(t) => serde_json::json!(String::from_utf8_lossy(t)),
                ValueRef::Blob(_) => serde_json::Value::Null,
            };
            row.insert(n.clone(), v);
        }
        Ok(serde_json::Value::Object(row))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn sql_count(db: &rusqlite::Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Applies every manifest part of `run_id` in `dest` to `db`, as the data Worker does, with
/// `dry_run` from the grant; `legacy` rewrites the statements for the old unprefixed tables.
fn index_run(db: &rusqlite::Connection, dest: &MemDest, run_id: &str, dry_run: bool, legacy: bool) {
    for key in dest.part_keys(run_id) {
        let text = String::from_utf8(dest.get(&key).unwrap()).unwrap();
        let lines: Vec<ManifestLine> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        for mut s in pigeon_ingest::index::apply_part(&key, &lines, dry_run, 5) {
            if legacy {
                s.sql = s.sql.replace("index_", "");
            }
            sql_run(db, &s);
        }
    }
}

/// A dry run indexes nothing as stored (nor its sightings); a live run indexes exactly the blobs
/// R2 holds, and a sighting for every source object, duplicates included, each pointing at a
/// blob the index holds.
#[test]
fn ingest_index_holds_only_blobs_r2_stores() {
    let db = index_db();
    let (source, dry_dest) = planted(300);
    go(&job("dry", true, 8), &source, &dry_dest);
    assert!(
        dry_dest
            .lines("dry")
            .iter()
            .any(|l| l.verdict == Verdict::Duplicate && l.reason == Reason::SeenInRun)
    );
    index_run(&db, &dry_dest, "dry", true, false);
    assert_eq!(sql_count(&db, "SELECT count(*) FROM index_blobs"), 0);
    assert_eq!(sql_count(&db, "SELECT count(*) FROM index_sightings"), 0);
    assert_eq!(
        sql_count(&db, "SELECT count(*) FROM index_applied"),
        dry_dest.part_keys("dry").len() as i64
    );

    let (source, live_dest) = planted(300);
    go(&job("live", false, 8), &source, &live_dest);
    // Twice: applying is idempotent; and the dry run again after it changes nothing.
    for _ in 0..2 {
        index_run(&db, &live_dest, "live", false, false);
        index_run(&db, &dry_dest, "dry", true, false);
    }
    let lines = live_dest.lines("live");
    assert_eq!(
        sql_count(&db, "SELECT count(*) FROM index_sightings"),
        lines.len() as i64
    );
    assert_eq!(
        sql_count(
            &db,
            "SELECT count(*) FROM index_sightings WHERE run <> 'live'"
        ),
        0
    );
    let blobs = sql_rows(
        &db,
        &pigeon_ingest::index::Statement {
            sql: "SELECT sha256, r2_key, size FROM index_blobs".into(),
            params: Vec::new(),
        },
    );
    let mut want: Vec<String> = lines
        .iter()
        .filter(|l| pigeon_ingest::index::proves_stored(l))
        .map(|l| l.sha256.clone())
        .collect();
    want.sort();
    want.dedup();
    assert_eq!(blobs.len(), want.len());
    for row in &blobs {
        let sha = row["sha256"].as_str().unwrap();
        let head = live_dest
            .head(row["r2_key"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(head.sha256.as_deref(), Some(sha));
        assert_eq!(Some(head.size), row["size"].as_u64());
    }
    // Every duplicate's sighting points at an indexed blob; a conflict's blob is not indexed.
    assert_eq!(
        sql_count(
            &db,
            "SELECT count(*) FROM index_sightings s WHERE s.decision = 'duplicate' AND NOT EXISTS \
             (SELECT 1 FROM index_blobs b WHERE b.sha256 = s.sha256)"
        ),
        0
    );
    assert!(
        sql_count(
            &db,
            "SELECT count(*) FROM index_sightings WHERE decision = 'duplicate'"
        ) > 0
    );
    let conflict = metrics(b"conflicting\n").sha256;
    assert_eq!(
        sql_count(
            &db,
            &format!("SELECT count(*) FROM index_blobs WHERE sha256 = '{conflict}'")
        ),
        0
    );
    // The flaky blob: its first sighting failed, the next stored it, so it is indexed.
    let flaky = metrics(b"flaky blob\n").sha256;
    assert_eq!(
        sql_count(
            &db,
            &format!("SELECT count(*) FROM index_blobs WHERE sha256 = '{flaky}'")
        ),
        1
    );
}

/// The repair of an index the old code made (it applied dry runs as live): on the lite tables
/// and the old unprefixed ones, the dry runs' rows go, a blob a later live run stored is
/// re-marked with that run, a blob R2 holds is kept, and only rows R2 has no object for are
/// deleted. Running it again changes nothing.
#[test]
fn ingest_index_cleanup_removes_dry_run_rows_and_keeps_real_ones() {
    use pigeon_ingest::index::{Statement, Tables, cleanup, cleanup_candidates};
    for legacy in [false, true] {
        let (t, db) = if legacy {
            let db = rusqlite::Connection::open_in_memory().unwrap();
            for s in pigeon_ingest::index::schema_statements() {
                db.execute(&s.sql.replace("index_", ""), []).unwrap();
            }
            (Tables::LEGACY, db)
        } else {
            (Tables::LITE, index_db())
        };
        let (source, dest) = planted(300);
        go(&job("dry1", true, 8), &source, &dest);
        // The old behaviour: a dry run's part applied as if live.
        index_run(&db, &dest, "dry1", false, legacy);
        let blobs_of = |db: &rusqlite::Connection| -> Vec<(String, String)> {
            let mut out: Vec<(String, String)> = sql_rows(
                db,
                &Statement {
                    sql: format!("SELECT sha256, first_run FROM {}", t.blobs),
                    params: Vec::new(),
                },
            )
            .iter()
            .map(|r| {
                (
                    r["sha256"].as_str().unwrap().to_string(),
                    r["first_run"].as_str().unwrap().to_string(),
                )
            })
            .collect();
            out.sort();
            out
        };
        // pair 1: a later live run stores it; pair 2: R2 got it otherwise (unindexed);
        // pair 3: never stored. All three were dry-run "duplicates" (seen in run).
        let (pair1, pair2, pair3) = (
            metrics(b"pair 1\n"),
            metrics(b"pair 2\n"),
            metrics(b"pair 3\n"),
        );
        let bad = blobs_of(&db);
        for m in [&pair1, &pair2, &pair3] {
            assert!(bad.contains(&(m.sha256.clone(), "dry1".into())));
        }
        let live = MemSource {
            objs: BTreeMap::from([("l/a".to_string(), b"pair 1\n".to_vec())]),
            page: 10,
            jitter_us: 0,
        };
        go(&job("live1", false, 2), &live, &dest);
        index_run(&db, &dest, "live1", false, legacy);
        dest.objs.lock().unwrap().insert(
            blob_key(&pair2.sha256),
            (
                b"pair 2\n".to_vec(),
                Some(pair2.sha256.clone()),
                Some(pair2.blake3.clone()),
            ),
        );

        let mut first = Vec::new();
        for round in 0..2 {
            let absent: Vec<String> = sql_rows(&db, &cleanup_candidates(&t))
                .iter()
                .filter(|r| dest.head(r["r2_key"].as_str().unwrap()).unwrap().is_none())
                .map(|r| r["sha256"].as_str().unwrap().to_string())
                .collect();
            for s in cleanup(&t, &["dry1".to_string()], &absent) {
                sql_run(&db, &s);
            }
            if round == 0 {
                first = blobs_of(&db);
            }
        }
        let after = blobs_of(&db);
        assert_eq!(first, after, "idempotent");
        assert!(
            after.contains(&(pair1.sha256.clone(), "live1".into())),
            "legacy {legacy}"
        );
        assert!(after.iter().any(|(sha, _)| sha == &pair2.sha256));
        assert!(!after.iter().any(|(sha, _)| sha == &pair3.sha256));
        // What is left is in R2; no row whose blob R2 holds was deleted.
        for (sha, _) in &after {
            assert!(dest.head(&blob_key(sha)).unwrap().is_some());
        }
        for (sha, _) in &bad {
            if dest.head(&blob_key(sha)).unwrap().is_some() {
                assert!(after.iter().any(|(s, _)| s == sha));
            }
        }
        assert_eq!(
            sql_count(
                &db,
                &format!("SELECT count(*) FROM {} WHERE run = 'dry1'", t.sightings)
            ),
            0
        );
        assert_eq!(
            sql_count(
                &db,
                &format!("SELECT count(*) FROM {} WHERE run = 'dry1'", t.hints)
            ),
            0
        );
        assert!(sql_count(&db, &format!("SELECT count(*) FROM {}", t.sightings)) > 0);
    }
}

/// `head-many` from D1 first, R2 for the misses, answers exactly what R2 alone answers, for
/// blobs a live run stored, blobs only a dry run saw, a blob R2 holds unindexed, an absent one
/// and a quarantine key; and before the migration (no index) it is R2 alone.
#[test]
fn ingest_head_many_from_the_index_matches_r2() {
    use pigeon_ingest::managed_destination::{
        index_misses, indexable_sha, known_heads, merge_heads,
    };
    let db = index_db();
    let (source, dest) = planted(300);
    go(&job("live", false, 8), &source, &dest);
    index_run(&db, &dest, "live", false, false);
    let (dry_source, _) = planted(50);
    let mut dry_objs = dry_source.objs.clone();
    dry_objs.insert("d/new.txt".into(), b"seen only by a dry run\n".to_vec());
    dry_objs.insert(
        "d/new-again.txt".into(),
        b"seen only by a dry run\n".to_vec(),
    );
    let dry_source = MemSource {
        objs: dry_objs,
        page: 97,
        jitter_us: 0,
    };
    go(&job("dry", true, 4), &dry_source, &dest);
    index_run(&db, &dest, "dry", true, false);
    let unindexed = metrics(b"stored by another writer\n");
    dest.objs.lock().unwrap().insert(
        blob_key(&unindexed.sha256),
        (
            b"stored by another writer\n".to_vec(),
            Some(unindexed.sha256.clone()),
            Some(unindexed.blake3.clone()),
        ),
    );

    let mut keys: Vec<String> = dest
        .lines("live")
        .iter()
        .chain(dest.lines("dry").iter())
        .filter(|l| !l.sha256.is_empty())
        .map(|l| blob_key(&l.sha256))
        .collect();
    keys.push(blob_key(&unindexed.sha256));
    keys.push(blob_key(&"ab".repeat(32)));
    let conflict = metrics(b"conflicting\n").sha256;
    keys.push(pigeon_ingest::manifest::quarantine_key("live", &conflict));
    let mut seen = std::collections::HashSet::new();
    keys.retain(|k| seen.insert(k.clone()));

    let r2_only: Vec<Option<Head>> = keys.iter().map(|k| dest.head(k).unwrap()).collect();
    for indexed in [true, false] {
        let known = if indexed {
            let shas: Vec<String> = keys
                .iter()
                .filter_map(|k| indexable_sha(k).map(str::to_string))
                .collect();
            let rows: Vec<serde_json::Value> = pigeon_ingest::index::known_blobs(&shas)
                .iter()
                .flat_map(|s| sql_rows(&db, s))
                .collect();
            known_heads(&rows)
        } else {
            HashMap::new()
        };
        let misses = index_misses(&keys, &known);
        if indexed {
            assert!(misses.len() * 2 < keys.len(), "D1 answered most");
        } else {
            assert_eq!(misses.len(), keys.len());
        }
        let heads = misses.iter().map(|k| dest.head(k).unwrap()).collect();
        assert_eq!(merge_heads(&keys, &known, heads), Some(r2_only.clone()));
    }
    // The dry run's own blob is absent in both.
    let dry_only = blob_key(&metrics(b"seen only by a dry run\n").sha256);
    let at = keys.iter().position(|k| *k == dry_only).unwrap();
    assert_eq!(r2_only[at], None);
    // A wrong-length miss list is refused.
    assert_eq!(merge_heads(&keys, &HashMap::new(), Vec::new()), None);
}

// ---- docs/plans/ingest-bytes.md: stage timings, memory staging, sharded runs ----

/// A monotonic clock that moves 100 µs each time it is read; the wall clock stays fixed, so
/// manifests are the same as under [`Fixed`].
#[derive(Default)]
struct Ticking(AtomicU64);
impl Clock for Ticking {
    fn now_ms(&self) -> i64 {
        1000
    }
    fn now_us(&self) -> u64 {
        self.0.fetch_add(100, Ordering::SeqCst)
    }
}

/// A spool in memory with a chosen memory limit and budget and free space.
struct SizedSpool {
    limit: u64,
    budget: u64,
    free: u64,
}
impl Spool for SizedSpool {
    fn create(&self) -> Result<Box<dyn SpoolFile>, Error> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }
    fn free_bytes(&self) -> u64 {
        self.free
    }
    fn memory_limit(&self) -> u64 {
        self.limit
    }
    fn memory_budget(&self) -> u64 {
        self.budget
    }
}

fn go_with(job: &JobSpec, source: &dyn Source, dest: &MemDest, spool: &dyn Spool) -> Report {
    run(&Run {
        job,
        source,
        dest,
        spool,
        clock: &Ticking::default(),
        stop: &AtomicBool::new(false),
    })
    .unwrap()
}

/// Every stage is counted: each object read is opened, read and hashed once with its bytes,
/// each upload is timed with the bytes it sent, existence checks, listing pages and control
/// writes are counted, and `status.json` carries the same timings as the report.
#[test]
fn ingest_stage_timings_are_counted() {
    for threads in [1, 64] {
        let (source, dest) = planted(1_200);
        let spool = SizedSpool {
            limit: 8,
            budget: u64::MAX,
            free: 1 << 30,
        };
        let report = go_with(&job("timed", false, threads), &source, &dest, &spool);
        let t = report.timings.clone().expect("timings");
        let c = &report.counters;
        assert_eq!(t.threads, threads);
        assert!(t.elapsed_ms > 0, "{t:?}");
        // Every listed object was read: none is too large here.
        assert_eq!(t.source_open.count, c.listed, "threads {threads}");
        assert_eq!(t.source_read.count, c.listed);
        assert_eq!(t.source_read.bytes, c.bytes_read);
        assert_eq!(t.hash.bytes, c.bytes_read);
        assert!(t.hash.count >= c.listed);
        // Files over the 8-byte memory limit went to the spool, the rest stayed in memory.
        assert!(t.spool_write.count > 0 && t.spool_write.count < c.listed);
        // Uploads: one per stored blob plus the retried failures, with the bytes sent.
        let puts = dest.blob_puts.load(Ordering::SeqCst);
        assert!(t.upload.count >= puts);
        assert!(t.upload.bytes >= c.uploaded_bytes);
        assert!(t.lookup.count > 0);
        assert_eq!(t.list.count, (c.listed as usize).div_ceil(97) as u64);
        assert!(t.control.count >= 3 && t.control.bytes > 0);
        for stage in [&t.source_open, &t.source_read, &t.hash, &t.upload, &t.list] {
            assert!(stage.ms > 0 || stage.count == 0, "{t:?}");
        }
        assert!(t.source_open.p50_ms.is_some() && t.upload.p95_ms.is_some());
        assert!(!t.bottleneck.is_empty());
        assert!(t.worker_busy_share > 0.0);
        let status: pigeon_ingest::manifest::Status = serde_json::from_slice(
            &dest
                .get(&pigeon_ingest::manifest::status_key("timed"))
                .unwrap(),
        )
        .unwrap();
        let st = status.timings.expect("status timings");
        assert_eq!(st.source_open.count, t.source_open.count);
        // Serialised, the timings round-trip; an older report without them still reads.
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["timings"]["upload"]["ms"].is_u64());
        let mut old = json.clone();
        old.as_object_mut().unwrap().remove("timings");
        let back: Report = serde_json::from_value(old).unwrap();
        assert!(back.timings.is_none());
    }
}

/// The bytes stored are the same whichever holds a file between reading and writing: memory
/// (up to the limit, within the budget), a spool file, or neither (hashed while streaming and
/// read again to copy). Manifests, counters and blobs are identical on both engines.
#[test]
fn ingest_memory_staging_stores_what_the_spool_path_stores() {
    let mut objs = BTreeMap::new();
    for i in 0..300u32 {
        let size = [5usize, 900, 70_000, 1_200_000][(i % 4) as usize];
        let body: Vec<u8> = (0..size)
            .map(|j| (j as u32).wrapping_mul(31).wrapping_add(i % 37) as u8)
            .collect();
        objs.insert(format!("m/{i:04}"), body);
    }
    let source = MemSource {
        objs,
        page: 50,
        jitter_us: 30,
    };
    let spools = [
        // Everything through spool files.
        SizedSpool {
            limit: 0,
            budget: u64::MAX,
            free: 1 << 30,
        },
        // Everything up to 16 MiB in memory.
        SizedSpool {
            limit: 16 << 20,
            budget: u64::MAX,
            free: 1 << 30,
        },
        // In memory until a small budget is held, then spool files.
        SizedSpool {
            limit: 16 << 20,
            budget: 1_000_000,
            free: 1 << 30,
        },
        // No room at all: hashed while streaming, read again and verified to copy.
        SizedSpool {
            limit: 0,
            budget: 0,
            free: 0,
        },
    ];
    let mut outcomes = Vec::new();
    for (i, spool) in spools.iter().enumerate() {
        for threads in [1, 16] {
            let dest = MemDest::default();
            let mut report = go_with(&job("mem", false, threads), &source, &dest, spool);
            let t = report.timings.take().unwrap();
            match i {
                0 => assert_eq!(t.spool_write.count, report.counters.listed),
                1 => assert_eq!(t.spool_write.count, 0),
                2 => assert!(t.spool_write.count > 0 && t.spool_write.count < 300),
                _ => assert_eq!(t.spool_write.count, 0),
            }
            let mut lines = dest.lines("mem");
            if i == 3 {
                // The only difference: those lines say the file was too big to spool.
                for l in &mut lines {
                    l.flags
                        .retain(|f| *f != pigeon_ingest::archive::Flag::OverSpool);
                }
                report.flags.clear();
            }
            outcomes.push((report, lines, stored(&dest, "blobs/")));
        }
    }
    for o in &outcomes[1..] {
        assert_eq!(o.0.counters, outcomes[0].0.counters);
        assert_eq!(o.1, outcomes[0].1);
        assert_eq!(o.2, outcomes[0].2);
    }
    // Bodies repeat every 148 objects: the rest are duplicates.
    assert_eq!(outcomes[0].2.len(), 148);
}

/// A measured run of large objects sets the byte rate, so a byte-heavy bucket is no longer
/// estimated at the default; before any measurement Cloudflare assumes 20-50 MB/s.
#[test]
fn ingest_estimate_uses_a_measured_byte_rate() {
    use pigeon_ingest::estimate::{Host, Measured, Throughput, estimate_run};
    // The willowgraysen bucket: 211.4 GB in 145,991 files.
    let (bytes, files) = (211_400_000_000u64, 145_991u64);
    let live = Throughput {
        measured: Some(Measured {
            run: "20261128-b2-53426b6b6f3575734248697746656444".into(),
            files_per_s: 21.0,
            bytes_per_s: 6.6e6,
        }),
        ..Throughput::new(Host::Cloudflare, 64)
    };
    let e = estimate_run(Host::Cloudflare, bytes, files, 0.3, &live);
    assert_eq!(e.bound, "bytes");
    // 211.4 GB at 6.6 MB/s is about 8.9 hours.
    assert!(e.hours_high > 8.8 && e.hours_high < 9.0, "{e:?}");
    assert!(
        e.assumptions[0].contains("(measured)"),
        "{:?}",
        e.assumptions
    );
    // A measured run of tiny files says nothing about bytes: the default byte rate stays.
    let small = Throughput {
        measured: Some(Measured {
            run: "small".into(),
            files_per_s: 36.0,
            bytes_per_s: 1.4e6,
        }),
        ..Throughput::new(Host::Cloudflare, 64)
    };
    let s = estimate_run(Host::Cloudflare, bytes, files, 0.3, &small);
    assert!(!s.assumptions[0].contains("(measured)"));
    // 211.4 GB at 20 MB/s is about 2.9 hours, over the files' 1.1 hours.
    assert_eq!(s.bound, "bytes");
    assert!(s.hours_high > 2.9 && s.hours_high < 3.0, "{s:?}");
    // Eight shards: each takes an eighth of the time; the host cost is their total.
    let mut sharded = e.clone();
    pigeon_ingest::estimate::split(&mut sharded, 8);
    assert_eq!(sharded.shards, 8);
    assert!((sharded.hours_high * 8.0 - e.hours_high).abs() < 1e-9);
    assert_eq!(sharded.host_usd, e.host_usd);
    assert_eq!(sharded.total_usd, e.total_usd);
    assert!(
        sharded
            .assumptions
            .iter()
            .any(|a| a.starts_with("8 shards"))
    );
    let mut single = e.clone();
    pigeon_ingest::estimate::split(&mut single, 1);
    assert_eq!(single, e);
    assert!(serde_json::to_value(&e).unwrap().get("shards").is_none());
}

/// `get_many` batches are bounded so the request stays under the Data Worker's request limit,
/// and an older Data Worker's reading of the operation fails (it answers 400), never succeeds.
#[test]
fn ingest_source_signs_downloads_in_bounded_batches() {
    use pigeon_ingest::source_credentials::{
        GET_MANY, GET_MANY_KEY_BYTES, REQUEST_MAX, Read as SourceRead, valid_batch,
    };
    let keys: Vec<String> = (0..GET_MANY).map(|i| format!("k/{i}")).collect();
    assert!(valid_batch(&keys));
    assert!(!valid_batch(&[]));
    assert!(!valid_batch(&[String::new()]));
    let mut more = keys.clone();
    more.push("k/extra".into());
    assert!(!valid_batch(&more));
    let long = vec!["x".repeat(1000); 25];
    assert!(!valid_batch(&long));
    let widest: Vec<String> = vec!["x".repeat(GET_MANY_KEY_BYTES / GET_MANY); GET_MANY];
    assert!(valid_batch(&widest));
    let body = serde_json::to_vec(&SourceRead::GetMany { keys: widest }).unwrap();
    assert!(body.len() < REQUEST_MAX, "{}", body.len());
    // The operation an older Worker parses has no `get_many`.
    #[derive(serde::Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
    #[allow(dead_code)]
    enum Older {
        List {
            after: Option<String>,
            cursor: Option<String>,
        },
        Get {
            key: String,
        },
    }
    let batch = serde_json::to_vec(&SourceRead::GetMany { keys }).unwrap();
    assert!(serde_json::from_slice::<Older>(&batch).is_err());
    let single = serde_json::to_vec(&SourceRead::Get { key: "k/1".into() }).unwrap();
    assert!(serde_json::from_slice::<Older>(&single).is_ok());
}

/// Every key belongs to exactly one shard, for every shard count, and shards are balanced.
#[test]
fn ingest_shard_partition_covers_every_key_once() {
    use pigeon_ingest::shard::{MAX_SHARDS, jobs, run_id, shard_of, takes, valid};
    let keys: Vec<String> = (0..20_000)
        .map(|i| format!("photos/{}/{i:06}.jpg", i % 13))
        .collect();
    for count in 1..=MAX_SHARDS {
        let parent = job("20261128-b2-0123456789abcdef0123456789abcdef", false, 64);
        let shards = jobs(&parent, count);
        assert_eq!(shards.len(), count as usize);
        let mut per = vec![0usize; count as usize];
        for key in &keys {
            let takers: Vec<u32> = shards
                .iter()
                .filter(|j| takes(j, key))
                .map(|j| j.shard.as_ref().map_or(0, |s| s.index))
                .collect();
            assert_eq!(takers.len(), 1, "{key} in {count} shards");
            assert_eq!(takers[0], shard_of(key, count));
            per[takers[0] as usize] += 1;
        }
        let even = keys.len() / count as usize;
        assert!(per.iter().all(|n| n.abs_diff(even) * 10 < even), "{per:?}");
        if count > 1 {
            for (i, j) in shards.iter().enumerate() {
                assert_eq!(j.run, run_id(&parent.run, i as u32));
                assert!(pigeon_ingest::container_job::valid_run(&j.run));
                let s = j.shard.as_ref().unwrap();
                assert_eq!(
                    (s.index, s.count, s.parent.as_str()),
                    (i as u32, count, parent.run.as_str())
                );
                // The shard is in the job, so it is in the approval digest and job.json.
                let back: JobSpec =
                    serde_json::from_str(&serde_json::to_string(j).unwrap()).unwrap();
                assert_eq!(&back, j);
            }
        } else {
            assert_eq!(shards[0], parent);
        }
        assert!(valid(&parent.run, count));
    }
    assert!(!valid("r", 0) && !valid("r", MAX_SHARDS + 1));
    assert!(!valid(&"a".repeat(89), 2));
    // An unsharded job's JSON has no shard field: older approvals keep their digests.
    let plain = serde_json::to_value(job("plain", false, 1)).unwrap();
    assert!(plain.get("shard").is_none());
}

/// Each shard's grant gets the floor of the approved budget over the shard count: the shares
/// never add up to more than the approved cap, and lose less than one unit per shard.
#[test]
fn ingest_shard_budget_split_never_exceeds_the_approved_cap() {
    use pigeon_ingest::managed_destination::run_budget;
    use pigeon_ingest::shard::{MAX_SHARDS, share};
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..5_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let total = x % 1_000_000_000_000_000;
        for count in 1..=MAX_SHARDS {
            let each = share(total, count);
            let sum = each * u64::from(count);
            assert!(sum <= total, "{total} over {count}");
            assert!(total - sum < u64::from(count));
        }
    }
    // The approved grant of the willowgraysen bucket, split over 8 shards.
    let (bytes, objects) = run_budget(211_400_000_000, 145_991, false);
    for count in [2, 4, 8] {
        assert!(share(bytes, count) * u64::from(count) <= bytes);
        assert!(share(objects, count) * u64::from(count) <= objects);
        // Each shard's share still covers its part of the source with the grant's headroom.
        assert!(share(bytes, count) >= 211_400_000_000 / u64::from(count));
    }
}

/// The stored blobs and every sighting of a sharded run are those of a serial run. What may
/// differ is which sighting of a blob seen in two shards is the upload, and whether a later
/// sighting's reason is `seen_in_run` or `in_r2`; each stored blob still has exactly one upload.
#[test]
fn ingest_sharded_run_stores_what_a_serial_run_stores() {
    use pigeon_ingest::manifest::RunState;
    use pigeon_ingest::shard::{combined_report, jobs};
    use std::collections::BTreeSet;
    let (source, serial_dest) = planted(3_000);
    let (_, sharded_dest) = planted(3_000);
    // The flaky blob's failures land on whichever shard tries first; leave them out here.
    serial_dest.fail_first.lock().unwrap().clear();
    sharded_dest.fail_first.lock().unwrap().clear();
    let serial = go(&job("whole", false, 1), &source, &serial_dest);
    let parent = job("sharded", false, 16);
    let shards = jobs(&parent, 4);
    let reports: Vec<Report> = std::thread::scope(|s| {
        let hs: Vec<_> = shards
            .iter()
            .map(|j| {
                let (source, dest) = (&source, &sharded_dest);
                s.spawn(move || go(j, source, dest))
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(reports.iter().all(|r| r.state == RunState::Done));
    let all = combined_report("sharded", &reports);
    assert_eq!(all.state, RunState::Done);
    assert_eq!(all.counters.listed, serial.counters.listed);
    assert_eq!(all.counters.files, serial.counters.files);
    assert_eq!(all.counters.members, serial.counters.members);
    assert_eq!(all.counters.bytes_read, serial.counters.bytes_read);
    assert_eq!(all.counters.conflicts, serial.counters.conflicts);
    assert_eq!(all.counters.failed, 0);
    assert_eq!(
        all.counters.uploaded + all.counters.duplicates,
        serial.counters.uploaded + serial.counters.duplicates
    );
    // The same blobs, byte for byte.
    assert_eq!(
        stored(&sharded_dest, "blobs/"),
        stored(&serial_dest, "blobs/")
    );
    // The same sightings: every object and member once, with its hash and where its bytes are.
    type Sighting = (String, Option<String>, String, Option<String>, bool);
    let sightings = |lines: Vec<ManifestLine>| -> BTreeSet<Sighting> {
        lines
            .into_iter()
            .map(|l| {
                let conflict = l.verdict == Verdict::Conflict;
                (
                    l.origin.key,
                    l.member.map(|m| m.path),
                    l.sha256,
                    // A conflict's quarantine key names its run; compare blob keys only.
                    l.r2_key.filter(|_| !conflict),
                    conflict,
                )
            })
            .collect()
    };
    let serial_lines = serial_dest.lines("whole");
    let mut sharded_lines = Vec::new();
    for j in &shards {
        sharded_lines.extend(sharded_dest.lines(&j.run));
    }
    assert_eq!(sharded_lines.len(), serial_lines.len());
    assert_eq!(
        sightings(sharded_lines.clone()),
        sightings(serial_lines.clone())
    );
    // Exactly one upload per blob this run stored, in both.
    let uploads = |lines: &[ManifestLine]| {
        let mut n: BTreeMap<String, usize> = BTreeMap::new();
        for l in lines.iter().filter(|l| l.verdict == Verdict::Upload) {
            *n.entry(l.sha256.clone()).or_default() += 1;
        }
        n
    };
    let (a, b) = (uploads(&serial_lines), uploads(&sharded_lines));
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    assert!(b.values().all(|n| *n == 1));
    // Blobs held before the run are duplicates in both. Serially the second sighting is
    // `seen_in_run`; in another shard it is `in_r2`: the reason may differ, never the verdict.
    for key in ["p/known.txt", "p/known-again.txt"] {
        let of = |lines: &[ManifestLine]| {
            lines
                .iter()
                .find(|l| l.origin.key == key)
                .map(|l| (l.verdict, l.reason))
                .unwrap()
        };
        let (a, b) = (of(&serial_lines), of(&sharded_lines));
        assert_eq!((a.0, b.0), (Verdict::Duplicate, Verdict::Duplicate));
        assert!(matches!(b.1, Reason::InR2 | Reason::SeenInRun));
    }
}

/// A stopped shard is continued on its own: the continuation keeps the shard, starts from its
/// checkpoint, and every key of that shard is decided exactly once across both runs.
#[test]
fn ingest_shard_continues_on_its_own() {
    use pigeon_ingest::manifest::RunState;
    use pigeon_ingest::shard::{jobs, shard_of};
    let (source, dest) = planted(1_500);
    let shards = jobs(&job("parent", false, 32), 3);
    let mine: Vec<&String> = source.objs.keys().filter(|k| shard_of(k, 3) == 1).collect();
    let stop_key = mine[mine.len() / 2].clone();
    let flag = Arc::new(AtomicBool::new(false));
    let stopping = StopAt {
        inner: &source,
        key: stop_key,
        flag: Arc::clone(&flag),
    };
    let first = go_until(&shards[1], &stopping, &dest, &flag);
    assert_eq!(first.state, RunState::Stopped);
    assert!(first.counters.listed < mine.len() as u64);
    let cp: Checkpoint =
        serde_json::from_slice(&dest.get(&checkpoint_key(&shards[1].run)).unwrap()).unwrap();
    // The continuation: a new run id, the same shard, the shard's checkpoint.
    let mut next = shards[1].clone();
    next.run = "parent-s1-continued".into();
    next.resume = Some(Resume {
        run: shards[1].run.clone(),
        checkpoint: cp,
    });
    let cont = go(&next, &source, &dest);
    assert_eq!(cont.state, RunState::Done);
    assert_eq!(cont.counters.listed, mine.len() as u64);
    let mut keys: Vec<String> = dest
        .lines(&shards[1].run)
        .into_iter()
        .chain(dest.lines("parent-s1-continued"))
        .filter(|l| l.member.is_none())
        .map(|l| l.origin.key)
        .collect();
    let in_order = keys.clone();
    keys.sort();
    keys.dedup();
    assert_eq!(in_order, keys, "each key once, in order");
    assert_eq!(keys.iter().collect::<Vec<_>>(), mine);
    // The other shards' keys were not touched.
    assert!(keys.iter().all(|k| shard_of(k, 3) == 1));
}

/// The shards' statuses and terminal records combine into the parent's: counters add up, the
/// state is running while any shard runs, and the worst outcome wins.
#[test]
fn ingest_shard_status_and_terminal_combine() {
    use pigeon_ingest::container_job::{Outcome, Terminal};
    use pigeon_ingest::manifest::{Counters, RunState, Status};
    use pigeon_ingest::shard::{combined_state, combined_status, combined_terminal};
    let counters = |n: u64| Counters {
        listed: n,
        bytes_read: n * 10,
        uploaded: n / 2,
        ..Counters::default()
    };
    let status = |state, n, started, updated| Status {
        run: "r-s0".into(),
        state,
        dry_run: false,
        counters: counters(n),
        started_ms: started,
        updated_ms: updated,
        read_rate: 5.0,
        eta_s: None,
        last_error: None,
        timings: None,
    };
    let all = combined_status(
        "r",
        &[
            status(RunState::Done, 10, 100, 900),
            status(RunState::Running, 20, 50, 800),
        ],
    );
    assert_eq!(all.run, "r");
    assert_eq!(all.state, RunState::Running);
    assert_eq!(
        all.counters,
        pigeon_ingest::shard::add_counters(&[&counters(10), &counters(20)])
    );
    assert_eq!((all.started_ms, all.updated_ms), (50, 900));
    assert_eq!(all.read_rate, 10.0);
    assert_eq!(
        combined_state(&[RunState::Done, RunState::Stopped]),
        RunState::Stopped
    );
    assert_eq!(
        combined_state(&[RunState::Stopped, RunState::Failed]),
        RunState::Failed
    );
    assert_eq!(
        combined_state(&[RunState::Done, RunState::Done]),
        RunState::Done
    );
    let terminal = |outcome, n, s, f| Terminal {
        run: "r-s1".into(),
        started_ms: s,
        finished_ms: f,
        outcome,
        counters: counters(n),
    };
    let t = combined_terminal(
        "r",
        &[
            terminal(Outcome::Succeeded, 4, 10, 50),
            terminal(Outcome::TimedOut, 6, 5, 70),
        ],
    )
    .unwrap();
    assert_eq!(t.run, "r");
    assert_eq!(t.outcome, Outcome::TimedOut);
    assert_eq!((t.started_ms, t.finished_ms), (5, 70));
    assert_eq!(t.counters.listed, 10);
    assert!(combined_terminal("r", &[]).is_none());
}

/// A clock that counts the pauses it is asked for, instead of sleeping.
#[derive(Default)]
struct Pausing(AtomicU64);
impl Clock for Pausing {
    fn now_ms(&self) -> i64 {
        1000
    }
    fn pause(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// A Data Worker hiccup on control writes (here the first eight: status, part, checkpoint and
/// status again) no longer ends the run: control writes wait and try again for about a minute.
/// A failure longer than that still ends it, with a `failed` status once the writes recover.
#[test]
fn ingest_control_writes_ride_out_a_hiccup() {
    for threads in [1, 16] {
        let (source, dest) = planted(800);
        *dest.flaky_control.lock().unwrap() = 8;
        let clock = Pausing::default();
        let report = run(&Run {
            job: &job("hiccup", false, threads),
            source: &source,
            dest: &dest,
            spool: &MemSpool,
            clock: &clock,
            stop: &AtomicBool::new(false),
        })
        .expect("the run outlives the hiccup");
        assert_eq!(report.state, pigeon_ingest::manifest::RunState::Done);
        assert_eq!(report.counters.listed, 807);
        assert!(
            clock.0.load(Ordering::SeqCst) >= 500,
            "it waited before trying again"
        );
        assert!(
            dest.get(&pigeon_ingest::manifest::report_key("hiccup"))
                .is_some()
        );

        // Longer than every pause: the run fails rather than run on unrecorded.
        let (source, dest) = planted(50);
        *dest.flaky_control.lock().unwrap() = 1_000;
        let failed = run(&Run {
            job: &job("down", false, threads),
            source: &source,
            dest: &dest,
            spool: &MemSpool,
            clock: &Pausing::default(),
            stop: &AtomicBool::new(false),
        });
        assert!(failed.is_err());
    }
}

/// A stop records the finished watermark at once (a checkpoint and a `stopped` status), so a
/// host that kills the container soon after SIGTERM still leaves a run to continue.
#[test]
fn ingest_stop_records_a_checkpoint_at_once() {
    use pigeon_ingest::manifest::{RunState, Status, status_key};
    let (source, dest) = planted(1_200);
    let flag = Arc::new(AtomicBool::new(false));
    let stopping = StopAt {
        inner: &source,
        key: "o/00700".into(),
        flag: Arc::clone(&flag),
    };
    let report = go_until(&job("stop-now", false, 32), &stopping, &dest, &flag);
    assert_eq!(report.state, RunState::Stopped);
    let status: Status =
        serde_json::from_slice(&dest.get(&status_key("stop-now")).unwrap()).unwrap();
    assert_eq!(status.state, RunState::Stopped);
    let cp: Checkpoint =
        serde_json::from_slice(&dest.get(&checkpoint_key("stop-now")).unwrap()).unwrap();
    assert_eq!(cp.counters, report.counters);
    // The early checkpoint closed a part: more parts than lines alone would make, all in order.
    let lines = dest.lines("stop-now");
    assert_eq!(
        lines.len() as u64,
        report.counters.files + report.counters.members
    );
    assert!(dest.part_keys("stop-now").len() >= 2);
}
