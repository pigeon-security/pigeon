//! Throughput and cost estimates for a run (decision 13.7), for `data_ingest_plan`.

use serde::{Deserialize, Serialize};

/// Where a run executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Host {
    /// A Droplet in the Space's region.
    DigitalOcean,
    /// An Instance in the bucket's region.
    Scaleway,
    /// A Cloudflare Container in the instance's account (for Backblaze B2).
    Cloudflare,
    /// The person's machine.
    Local,
}

/// What a run over `bytes` in `objects` objects is expected to take and cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub host: Host,
    pub bytes: u64,
    pub objects: u64,
    /// The share assumed unique, 0 to 1.
    pub unique_share: f64,
    /// Hours at the slow and fast read rates.
    pub hours_low: f64,
    pub hours_high: f64,
    /// US dollars, each line and the total, at the slow rate.
    pub host_usd: f64,
    pub egress_usd: f64,
    pub requests_usd: f64,
    pub total_usd: f64,
    /// The assumptions, one per line, for the person.
    pub assumptions: Vec<String>,
    /// Files a second assumed (measured, or per-object latency over concurrency).
    #[serde(default)]
    pub files_per_s: f64,
    /// `bytes` or `files`: which bound gave the slow time.
    #[serde(default)]
    pub bound: String,
    /// `high`, or `low` when the object count was extrapolated from small sampled objects.
    #[serde(default)]
    pub confidence: String,
    /// The run whose measured rate was used, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_run: Option<String>,
}

/// A rate measured by an earlier run on the same host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    pub run: String,
    pub files_per_s: f64,
    pub bytes_per_s: f64,
}

/// What the time estimate assumes about the runner.
#[derive(Debug, Clone, PartialEq)]
pub struct Throughput {
    /// Milliseconds of round trips per object, serially.
    pub per_object_ms: f64,
    /// Objects in flight at once (1 for the serial engine).
    pub concurrency: u32,
    /// The last measured rate for this host, preferred when present.
    pub measured: Option<Measured>,
    /// The object count was extrapolated from a sample, not given.
    pub extrapolated: bool,
}

impl Throughput {
    /// The defaults for `host` with `concurrency` objects in flight and no measurement.
    pub fn new(host: Host, concurrency: u32) -> Self {
        Self {
            per_object_ms: per_object_ms(host),
            concurrency: concurrency.max(1),
            measured: None,
            extrapolated: false,
        }
    }
}

/// Serial milliseconds per object before any run is measured: a Cloudflare Container reaching B2
/// and the Data Worker (measured about 70 ms on 2026-11-28), a VM in the bucket's region, a
/// local disk.
pub fn per_object_ms(host: Host) -> f64 {
    match host {
        Host::Cloudflare => 70.0,
        Host::DigitalOcean | Host::Scaleway => 50.0,
        Host::Local => 20.0,
    }
}

/// The longest a run may be given on `host`, in minutes: a Cloudflare Container's 24 hours, a
/// Droplet's reviewed 24 hours, a Scaleway Instance's fixed 24-hour lifetime.
pub fn ceiling_minutes(_host: Host) -> u64 {
    24 * 60
}

/// The deadline for an estimate: 1.5 times its slow time plus 10 minutes, at least 15 minutes,
/// at most `ceiling` minutes.
pub fn deadline_minutes(estimate: &Estimate, ceiling: u64) -> u64 {
    let minutes = (estimate.hours_high * 60.0).mul_add(1.5, 10.0).ceil();
    let minutes = if minutes.is_finite() {
        minutes as u64
    } else {
        ceiling
    };
    minutes.clamp(15.min(ceiling), ceiling)
}

/// A rate from a run's counters: `listed` objects and `bytes` read over `elapsed_ms`. None for a
/// run too short (under 100 objects or a minute) to say anything.
pub fn measure(run: &str, listed: u64, bytes: u64, elapsed_ms: i64) -> Option<Measured> {
    if listed < 100 || elapsed_ms < 60_000 {
        return None;
    }
    let secs = elapsed_ms as f64 / 1000.0;
    Some(Measured {
        run: run.to_string(),
        files_per_s: listed as f64 / secs,
        bytes_per_s: bytes as f64 / secs,
    })
}

/// The run time in minutes for `estimate` on `host`: `given` (an override, 10 minutes to the
/// host's ceiling) or [`deadline_minutes`].
pub fn run_minutes(host: Host, estimate: &Estimate, given: Option<u64>) -> Result<u64, String> {
    let ceiling = ceiling_minutes(host);
    match given {
        Some(m) if (10..=ceiling).contains(&m) => Ok(m),
        Some(_) => Err(format!("max_minutes must be from 10 to {ceiling}")),
        None => Ok(deadline_minutes(estimate, ceiling)),
    }
}

/// The whole run's bytes and objects from a first listing page of `count` objects holding
/// `bytes` (`truncated` when there are more pages), the declared `size_bytes` and `objects`.
/// Answers whether the count was extrapolated from the sample.
pub fn totals(
    count: u64,
    bytes: u64,
    truncated: bool,
    size_bytes: Option<u64>,
    objects: Option<u64>,
) -> (u64, u64, bool) {
    let total = size_bytes.unwrap_or(bytes).max(bytes);
    match objects {
        Some(n) => (total, n.max(count), false),
        None if total == bytes && !truncated => (total, count, false),
        None => {
            let n = ((u128::from(total) * u128::from(count)) / u128::from(bytes.max(1)))
                .max(u128::from(count))
                .min(u128::from(u64::MAX)) as u64;
            (total, n, true)
        }
    }
}

/// Average object size under which a count extrapolated from a sample is low confidence.
const SMALL_OBJECT: u64 = 64 * 1024;
/// Average bytes per object from which a measured run's byte rate is its own bound.
const BYTE_HEAVY: f64 = 256.0 * 1024.0;

/// Bytes in a GiB.
const GIB: f64 = (1u64 << 30) as f64;
/// R2 Class B price per HEAD, dollars.
const HEAD_USD: f64 = 0.36 / 1e6;
/// R2 Class A price per PUT, dollars.
const PUT_USD: f64 = 4.50 / 1e6;

/// The estimate for `host` over `bytes` in `objects` objects with `unique_share` unique, for the
/// serial engine with no measurement. See [`estimate_run`].
pub fn estimate(host: Host, bytes: u64, objects: u64, unique_share: f64) -> Estimate {
    estimate_run(
        host,
        bytes,
        objects,
        unique_share,
        &Throughput::new(host, 1),
    )
}

/// The estimate for `host` over `bytes` in `objects` objects, with `unique_share` unique.
/// `hours_low` is the time at the fast rate and `hours_high` the time at the slow rate; each is
/// the longer of the byte time and the file time. The first dry run measures the real numbers,
/// and a later plan passes them in `throughput.measured`.
pub fn estimate_run(
    host: Host,
    bytes: u64,
    objects: u64,
    unique_share: f64,
    throughput: &Throughput,
) -> Estimate {
    let share = if unique_share.is_finite() {
        unique_share.clamp(0.0, 1.0)
    } else {
        0.3
    };
    let b = bytes as f64;
    let o = objects as f64;
    // A Cloudflare Container reaches B2 and the Data Worker over the internet, one object's
    // round trips after another per thread: far slower than a VM in the bucket's region
    // (6.6 MB/s measured on 2026-11-28 before the fixes of docs/plans/ingest-bytes.md).
    // A measured run of large objects (at least 256 KiB on average) sets the byte rate itself.
    let measured_bytes = throughput.measured.as_ref().and_then(|m| {
        (m.bytes_per_s.is_finite()
            && m.bytes_per_s > 0.0
            && m.files_per_s > 0.0
            && m.bytes_per_s / m.files_per_s >= BYTE_HEAVY)
            .then_some(m.bytes_per_s)
    });
    let byte_measured = measured_bytes.is_some();
    let (slow, fast) = match (measured_bytes, host) {
        (Some(rate), _) => (rate, rate * 1.25),
        (None, Host::Cloudflare) => (20e6, 50e6),
        (None, _) => (100e6, 150e6),
    };
    let (fps_slow, fps_fast) = match &throughput.measured {
        Some(m) if m.files_per_s.is_finite() && m.files_per_s > 0.0 => {
            (m.files_per_s, m.files_per_s * 1.25)
        }
        _ => {
            let fps = f64::from(throughput.concurrency.max(1)) * 1000.0
                / throughput.per_object_ms.max(1.0);
            (fps, fps * 1.5)
        }
    };
    let (byte_high, file_high) = (b / slow / 3600.0, o / fps_slow / 3600.0);
    let hours_high = byte_high.max(file_high);
    let hours_low = (b / fast / 3600.0).max(o / fps_fast / 3600.0);
    let bound = if file_high > byte_high {
        "files"
    } else {
        "bytes"
    };
    let confidence = if throughput.extrapolated && bytes / objects.max(1) < SMALL_OBJECT {
        "low"
    } else {
        "high"
    };
    let unique_gib = b * share / GIB;
    let (host_usd, egress_usd, mut notes) = match host {
        Host::DigitalOcean => {
            // The Droplet accrues about 180 GiB of transfer a day.
            let free = 180.0 * (hours_high / 24.0).ceil().max(1.0);
            (
                hours_high * 0.071,
                (unique_gib - free).max(0.0) * 0.01,
                vec![
                    "Host: an s-4vcpu-8gb Droplet at $0.071 an hour, in the Space's region."
                        .to_string(),
                    "Reading a Space from a Droplet in its region is free.".to_string(),
                    "Copy out: the Droplet's transfer pool is about 180 GiB a day; the rest is $0.01 a GiB."
                        .to_string(),
                ],
            )
        }
        Host::Scaleway => (
            hours_high * 0.16 + 1.1,
            0.0,
            vec![
                "Host: a POP2-4C-16G Instance at about 0.15 euro an hour (taken as $0.16) plus a 40 GB boot volume and 200 GB scratch volume."
                    .to_string(),
                "Object Storage reads in region are free and Instances have no egress fee."
                    .to_string(),
            ],
        ),
        Host::Cloudflare => (
            hours_high * 0.40,
            (unique_gib - 1000.0).max(0.0) * 0.025,
            vec![
                "Host: a standard-4 Cloudflare Container at about $0.40 an hour.".to_string(),
                "B2 to Cloudflare is free under the partner programme.".to_string(),
                "Container egress to R2: 1 TB a month included, then $0.025 a GB.".to_string(),
            ],
        ),
        Host::Local => (
            0.0,
            0.0,
            vec![
                "Host: your own machine, so no host cost. Time depends on your disk and uplink."
                    .to_string(),
            ],
        ),
    };
    let requests_usd = o * HEAD_USD + o * share * PUT_USD;
    notes.insert(
        0,
        format!(
            "Read rate {:.1}-{:.1} MB/s{}; {:.0}% of the bytes assumed unique.",
            slow / 1e6,
            fast / 1e6,
            if byte_measured { " (measured)" } else { "" },
            share * 100.0
        ),
    );
    notes.insert(
        1,
        match &throughput.measured {
            Some(m) => format!(
                "{fps_slow:.1} files a second, measured by run {} on this host; the slower of the byte and file times is used.",
                m.run
            ),
            None => format!(
                "{fps_slow:.1} files a second: about {:.0} ms per object, {} in flight (not yet measured on this host); the slower of the byte and file times is used.",
                throughput.per_object_ms,
                throughput.concurrency.max(1)
            ),
        },
    );
    if confidence == "low" {
        notes.push(
            "Low confidence: the object count was extrapolated from a sample of small objects. Pass `objects` (the count in the provider's console) for a better estimate."
                .to_string(),
        );
    }
    notes.push(
        "Requests: one R2 HEAD per object ($0.36 per million) and one PUT per unique object ($4.50 per million)."
            .to_string(),
    );
    Estimate {
        host,
        bytes,
        objects,
        unique_share: share,
        hours_low,
        hours_high,
        host_usd,
        egress_usd,
        requests_usd,
        total_usd: host_usd + egress_usd + requests_usd,
        assumptions: notes,
        files_per_s: fps_slow,
        bound: bound.to_string(),
        confidence: confidence.to_string(),
        measured_run: throughput.measured.as_ref().map(|m| m.run.clone()),
    }
}
