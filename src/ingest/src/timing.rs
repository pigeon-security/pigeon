//! Where a run's time goes (docs/plans/ingest-bytes.md): time, count and bytes per stage,
//! summed over threads, with a cheap latency histogram for the per-object stages. Atomic
//! counters only, so workers record without a lock or an allocation.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// A stage of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// One listing page.
    List,
    /// `Source::open` to the response headers: signing and the provider's time to first byte.
    SourceOpen,
    /// Reading an object's body.
    SourceRead,
    /// Writing to the disk spool.
    SpoolWrite,
    /// SHA-256, BLAKE3 and the kind sniff.
    Hash,
    /// Existence checks.
    Lookup,
    /// `Dest::put_blob`: the round trip and the transfer.
    Upload,
    /// Manifest parts, checkpoints, status and report writes.
    Control,
    /// The coordinator waiting for workers with room in its window.
    CoordinatorWait,
    /// The coordinator waiting with its in-flight window full.
    WindowFull,
}

const STAGES: usize = 10;

impl Stage {
    fn index(self) -> usize {
        self as usize
    }

    /// The histogram slot of a per-object stage.
    fn histogram(self) -> Option<usize> {
        match self {
            Stage::SourceOpen => Some(0),
            Stage::Lookup => Some(1),
            Stage::Upload => Some(2),
            _ => None,
        }
    }
}

/// Upper bounds of the latency buckets, in milliseconds; one more bucket holds the rest.
const BOUNDS_MS: [u64; 15] = [
    1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 2_000, 5_000, 10_000, 30_000, 60_000,
];
const BUCKETS: usize = BOUNDS_MS.len() + 1;

/// The live counters of one run session.
pub struct Stages {
    count: [AtomicU64; STAGES],
    us: [AtomicU64; STAGES],
    bytes: [AtomicU64; STAGES],
    histogram: [[AtomicU64; BUCKETS]; 3],
}

impl Default for Stages {
    fn default() -> Self {
        Self::new()
    }
}

impl Stages {
    pub fn new() -> Self {
        Self {
            count: std::array::from_fn(|_| AtomicU64::new(0)),
            us: std::array::from_fn(|_| AtomicU64::new(0)),
            bytes: std::array::from_fn(|_| AtomicU64::new(0)),
            histogram: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        }
    }

    /// Records one pass of `stage` that took `us` microseconds over `bytes` bytes.
    pub fn add(&self, stage: Stage, us: u64, bytes: u64) {
        let i = stage.index();
        self.count[i].fetch_add(1, Ordering::Relaxed);
        self.us[i].fetch_add(us, Ordering::Relaxed);
        self.bytes[i].fetch_add(bytes, Ordering::Relaxed);
        if let Some(h) = stage.histogram() {
            let ms = us / 1000;
            let b = BOUNDS_MS
                .iter()
                .position(|bound| ms < *bound)
                .unwrap_or(BOUNDS_MS.len());
            self.histogram[h][b].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Adds time to `stage` without counting a pass (time spent waiting, in slices).
    pub fn wait(&self, stage: Stage, us: u64) {
        self.us[stage.index()].fetch_add(us, Ordering::Relaxed);
    }

    fn time(&self, stage: Stage) -> StageTime {
        let i = stage.index();
        let (p50_ms, p95_ms) = match stage.histogram() {
            Some(h) => {
                let counts: Vec<u64> = self.histogram[h]
                    .iter()
                    .map(|c| c.load(Ordering::Relaxed))
                    .collect();
                (percentile(&counts, 0.50), percentile(&counts, 0.95))
            }
            None => (None, None),
        };
        StageTime {
            count: self.count[i].load(Ordering::Relaxed),
            ms: self.us[i].load(Ordering::Relaxed) / 1000,
            bytes: self.bytes[i].load(Ordering::Relaxed),
            p50_ms,
            p95_ms,
        }
    }

    /// What the counters say now, for a session of `elapsed_ms` on `threads` worker threads.
    pub fn snapshot(&self, threads: u32, elapsed_ms: u64) -> Timings {
        let mut t = Timings {
            threads,
            elapsed_ms,
            list: self.time(Stage::List),
            source_open: self.time(Stage::SourceOpen),
            source_read: self.time(Stage::SourceRead),
            spool_write: self.time(Stage::SpoolWrite),
            hash: self.time(Stage::Hash),
            lookup: self.time(Stage::Lookup),
            upload: self.time(Stage::Upload),
            control: self.time(Stage::Control),
            coordinator_wait: self.time(Stage::CoordinatorWait),
            window_full: self.time(Stage::WindowFull),
            worker_busy_share: 0.0,
            bottleneck: String::new(),
        };
        let workers = [
            ("source_open", t.source_open.ms),
            ("source_read", t.source_read.ms),
            ("spool_write", t.spool_write.ms),
            ("hash", t.hash.ms),
            ("upload", t.upload.ms),
        ];
        let busy: u64 = workers.iter().map(|(_, ms)| ms).sum();
        let capacity = u64::from(threads.max(1)).saturating_mul(elapsed_ms);
        if capacity > 0 {
            t.worker_busy_share = busy as f64 / capacity as f64;
        }
        if let Some((name, ms)) = workers.iter().max_by_key(|(_, ms)| *ms)
            && *ms > 0
        {
            t.bottleneck = (*name).to_string();
        }
        t
    }
}

/// The upper bound, in milliseconds, of the bucket holding the `q` quantile; none when empty.
fn percentile(counts: &[u64], q: f64) -> Option<f64> {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let want = ((total as f64) * q).ceil().max(1.0) as u64;
    let mut seen = 0;
    for (i, c) in counts.iter().enumerate() {
        seen += c;
        if seen >= want {
            let bound = BOUNDS_MS
                .get(i)
                .copied()
                .unwrap_or(2 * BOUNDS_MS[BOUNDS_MS.len() - 1]);
            return Some(bound as f64);
        }
    }
    None
}

/// One stage's totals: passes, milliseconds summed over threads, bytes, and for per-object
/// stages the bucket bounds of the median and 95th percentile latency.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageTime {
    pub count: u64,
    pub ms: u64,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<f64>,
}

impl StageTime {
    /// Bytes a second while in this stage, per thread (`bytes / ms`).
    pub fn bytes_per_s(&self) -> f64 {
        if self.ms == 0 {
            0.0
        } else {
            self.bytes as f64 * 1000.0 / self.ms as f64
        }
    }

    fn merge(&mut self, other: &StageTime) {
        self.count += other.count;
        self.ms += other.ms;
        self.bytes += other.bytes;
        // Percentiles do not add: keep the worse of the two.
        self.p50_ms = max_opt(self.p50_ms, other.p50_ms);
        self.p95_ms = max_opt(self.p95_ms, other.p95_ms);
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// `timings` in `status.json` and `report.json`: this session's stages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    pub threads: u32,
    pub elapsed_ms: u64,
    pub list: StageTime,
    pub source_open: StageTime,
    pub source_read: StageTime,
    pub spool_write: StageTime,
    pub hash: StageTime,
    pub lookup: StageTime,
    pub upload: StageTime,
    pub control: StageTime,
    pub coordinator_wait: StageTime,
    pub window_full: StageTime,
    /// The worker stages' summed time over `threads × elapsed_ms`.
    pub worker_busy_share: f64,
    /// The worker stage that took the most time, if any.
    pub bottleneck: String,
}

impl Timings {
    /// Adds another session's or shard's timings (threads and elapsed add up, percentiles keep
    /// the worse).
    pub fn merge(&mut self, other: &Timings) {
        self.threads += other.threads;
        self.elapsed_ms = self.elapsed_ms.max(other.elapsed_ms);
        for (a, b) in [
            (&mut self.list, &other.list),
            (&mut self.source_open, &other.source_open),
            (&mut self.source_read, &other.source_read),
            (&mut self.spool_write, &other.spool_write),
            (&mut self.hash, &other.hash),
            (&mut self.lookup, &other.lookup),
            (&mut self.upload, &other.upload),
            (&mut self.control, &other.control),
            (&mut self.coordinator_wait, &other.coordinator_wait),
            (&mut self.window_full, &other.window_full),
        ] {
            a.merge(b);
        }
        if self.bottleneck.is_empty() {
            self.bottleneck = other.bottleneck.clone();
        }
    }
}
