//! Ingest throughput (docs/plans/ingest-throughput.md): run time from the file count, deadlines
//! and continuations; the parallel engine's equivalence with the serial one; live runs.

use pigeon_ingest::Error;
use pigeon_ingest::archive::Limits;
use pigeon_ingest::decide::Head;
use pigeon_ingest::engine::{Clock, Dest, Object, Page, Run, Source, Spool, SpoolFile, run};
use pigeon_ingest::manifest::{
    Checkpoint, DestSpec, JobSpec, ManifestLine, Report, Resume, SourceSpec, checkpoint_key,
};
use pigeon_ingest::metrics::Metrics;
use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A listing in pages of `page` objects; reads may be delayed to shuffle worker finishing order.
struct MemSource {
    objs: BTreeMap<String, Vec<u8>>,
    page: usize,
    /// Microseconds of delay for a read, from a key's hash.
    jitter_us: u64,
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
        let mut objs = self.objs.lock().unwrap();
        if objs.contains_key(key) {
            return Err(Error::Dest("the blob exists".into()));
        }
        self.blob_puts.fetch_add(1, Ordering::SeqCst);
        objs.insert(
            key.to_string(),
            (buf, Some(m.sha256.clone()), Some(m.blake3.clone())),
        );
        Ok(())
    }
    fn put_object(&self, key: &str, body: &[u8], _: &str) -> Result<(), Error> {
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
    }
}

fn go(job: &JobSpec, source: &MemSource, dest: &MemDest) -> Report {
    let stop = AtomicBool::new(false);
    run(&Run {
        job,
        source,
        dest,
        spool: &MemSpool,
        clock: &Fixed,
        stop: &stop,
    })
    .unwrap()
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
