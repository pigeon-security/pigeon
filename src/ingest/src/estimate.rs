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
}

/// Bytes in a GiB.
const GIB: f64 = (1u64 << 30) as f64;
/// R2 Class B price per HEAD, dollars.
const HEAD_USD: f64 = 0.36 / 1e6;
/// R2 Class A price per PUT, dollars.
const PUT_USD: f64 = 4.50 / 1e6;

/// The estimate for `host` over `bytes` in `objects` objects, with `unique_share` unique.
/// `hours_low` is the time at the fast rate and `hours_high` the time at the slow rate.
/// The first dry run measures the real numbers.
pub fn estimate(host: Host, bytes: u64, objects: u64, unique_share: f64) -> Estimate {
    let share = if unique_share.is_finite() {
        unique_share.clamp(0.0, 1.0)
    } else {
        0.3
    };
    let b = bytes as f64;
    let o = objects as f64;
    // Containers read B2 a little slower than a VM reads in region.
    let (slow, fast) = match host {
        Host::Cloudflare => (80e6, 120e6),
        _ => (100e6, 150e6),
    };
    let hours_high = b / slow / 3600.0;
    let hours_low = b / fast / 3600.0;
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
            "Read rate {}-{} MB/s; {:.0}% of the bytes assumed unique.",
            (slow / 1e6) as u64,
            (fast / 1e6) as u64,
            share * 100.0
        ),
    );
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
    }
}
