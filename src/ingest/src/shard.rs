//! Sharded runs (docs/plans/ingest-bytes.md, slice b): one approved run split into up to
//! [`MAX_SHARDS`] runs on disjoint key sets of the same prefix, each its own container, grant,
//! checkpoint and report; their statuses, reports and terminal records combined into one.

use sha2::{Digest, Sha256};

use crate::container_job::{Outcome, Terminal};
use crate::manifest::{Counters, JobSpec, Report, RunState, Shard, Status};

/// Most shards of one run.
pub const MAX_SHARDS: u32 = 8;
/// Longest parent run id, so every shard run id is a valid run id.
pub const MAX_PARENT_RUN: usize = 88;

/// The shard of `count` that takes `key`: the first 8 bytes of the key's SHA-256, big-endian,
/// modulo `count`. Every key has exactly one shard; 0 when `count` is 0 or 1.
pub fn shard_of(key: &str, count: u32) -> u32 {
    if count <= 1 {
        return 0;
    }
    let digest = Sha256::digest(key.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(head) % u64::from(count)) as u32
}

/// Whether `job` takes `key`: always, unless it is a shard and `key` belongs to another.
pub fn takes(job: &JobSpec, key: &str) -> bool {
    job.shard
        .as_ref()
        .is_none_or(|s| shard_of(key, s.count) == s.index)
}

/// The run id of shard `index` of `parent`.
pub fn run_id(parent: &str, index: u32) -> String {
    format!("{parent}-s{index}")
}

/// Whether `count` shards are allowed for a parent run id `parent`.
pub fn valid(parent: &str, count: u32) -> bool {
    (1..=MAX_SHARDS).contains(&count)
        && parent.len() <= MAX_PARENT_RUN
        && crate::container_job::valid_run(parent)
}

/// The jobs of `count` shards of `parent`: the same job with each shard's run id and
/// [`Shard`]. One job, unchanged, for a count of 1.
pub fn jobs(parent: &JobSpec, count: u32) -> Vec<JobSpec> {
    if count <= 1 {
        return vec![parent.clone()];
    }
    (0..count)
        .map(|index| JobSpec {
            run: run_id(&parent.run, index),
            shard: Some(Shard {
                index,
                count,
                parent: parent.run.clone(),
            }),
            ..parent.clone()
        })
        .collect()
}

/// Each shard's share of an approved budget: the floor of `total / count`, so the shares never
/// add up to more than `total`.
pub fn share(total: u64, count: u32) -> u64 {
    total / u64::from(count.max(1))
}

/// Counters added up.
pub fn add_counters(all: &[&Counters]) -> Counters {
    let mut c = Counters::default();
    for n in all {
        c.listed += n.listed;
        c.listed_bytes += n.listed_bytes;
        c.files += n.files;
        c.members += n.members;
        c.bytes_read += n.bytes_read;
        c.uploaded += n.uploaded;
        c.uploaded_bytes += n.uploaded_bytes;
        c.would_upload += n.would_upload;
        c.would_upload_bytes += n.would_upload_bytes;
        c.duplicates += n.duplicates;
        c.duplicate_bytes += n.duplicate_bytes;
        c.conflicts += n.conflicts;
        c.failed += n.failed;
        c.skipped += n.skipped;
        c.archives_opened += n.archives_opened;
        c.archives_not_opened += n.archives_not_opened;
    }
    c
}

/// The state of runs together: failed if any failed, running while any runs, stopped if any
/// stopped (so the run can be continued), else done.
pub fn combined_state(states: &[RunState]) -> RunState {
    let any = |s: RunState| states.contains(&s);
    if states.is_empty() {
        RunState::Starting
    } else if any(RunState::Failed) {
        RunState::Failed
    } else if any(RunState::Running) || any(RunState::Listing) || any(RunState::Starting) {
        RunState::Running
    } else if any(RunState::Stopped) {
        RunState::Stopped
    } else {
        RunState::Done
    }
}

/// The shards' `status.json` as one, under the parent's run id.
pub fn combined_status(parent: &str, shards: &[Status]) -> Status {
    let counters = add_counters(&shards.iter().map(|s| &s.counters).collect::<Vec<_>>());
    let mut timings: Option<crate::timing::Timings> = None;
    for t in shards.iter().filter_map(|s| s.timings.as_ref()) {
        match timings.as_mut() {
            Some(all) => all.merge(t),
            None => timings = Some(t.clone()),
        }
    }
    Status {
        run: parent.to_string(),
        state: combined_state(&shards.iter().map(|s| s.state).collect::<Vec<_>>()),
        dry_run: shards.iter().any(|s| s.dry_run),
        counters,
        started_ms: shards.iter().map(|s| s.started_ms).min().unwrap_or(0),
        updated_ms: shards.iter().map(|s| s.updated_ms).max().unwrap_or(0),
        read_rate: shards.iter().map(|s| s.read_rate).sum(),
        eta_s: shards.iter().filter_map(|s| s.eta_s).max(),
        last_error: shards.iter().find_map(|s| s.last_error.clone()),
        timings,
    }
}

/// The shards' `report.json` as one, under the parent's run id: counters and flags added,
/// lists joined (each at most 200), the most repeated hashes counted again over all shards.
pub fn combined_report(parent: &str, shards: &[Report]) -> Report {
    use std::collections::{BTreeMap, HashMap};
    const LIST: usize = 200;
    let counters = add_counters(&shards.iter().map(|r| &r.counters).collect::<Vec<_>>());
    let mut flags = BTreeMap::new();
    let mut dups: HashMap<String, (u64, u64)> = HashMap::new();
    let joined = |pick: fn(&Report) -> &Vec<crate::manifest::ManifestLine>| {
        let mut out: Vec<_> = shards
            .iter()
            .flat_map(|r| pick(r).iter().cloned())
            .collect();
        out.truncate(LIST);
        out
    };
    let conflicts = joined(|r| &r.conflicts);
    let failures = joined(|r| &r.failures);
    let not_opened = joined(|r| &r.not_opened);
    let too_large = joined(|r| &r.too_large);
    for r in shards {
        for (f, n) in &r.flags {
            *flags.entry(*f).or_insert(0u64) += n;
        }
        for (sha, n, size) in &r.top_duplicates {
            let e = dups.entry(sha.clone()).or_insert((0, *size));
            e.0 += n;
        }
    }
    let mut top: Vec<(String, u64, u64)> = dups
        .into_iter()
        .map(|(s, (n, size))| (s, n, size))
        .collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(50);
    let mut timings: Option<crate::timing::Timings> = None;
    for t in shards.iter().filter_map(|r| r.timings.as_ref()) {
        match timings.as_mut() {
            Some(all) => all.merge(t),
            None => timings = Some(t.clone()),
        }
    }
    Report {
        run: parent.to_string(),
        state: combined_state(&shards.iter().map(|r| r.state).collect::<Vec<_>>()),
        dry_run: shards.iter().any(|r| r.dry_run),
        counters,
        flags: flags.into_iter().collect(),
        conflicts,
        failures,
        not_opened,
        top_duplicates: top,
        started_ms: shards.iter().map(|r| r.started_ms).min().unwrap_or(0),
        finished_ms: shards.iter().map(|r| r.finished_ms).max().unwrap_or(0),
        files_per_s: shards.iter().map(|r| r.files_per_s).sum(),
        bytes_per_s: shards.iter().map(|r| r.bytes_per_s).sum(),
        continues: None,
        too_large,
        budget_spent: shards.iter().any(|r| r.budget_spent),
        timings,
    }
}

/// The shards' terminal records as the parent's one: the earliest start, the latest finish,
/// counters added, and the worst outcome (failed, then timed out, then cancelled).
pub fn combined_terminal(parent: &str, shards: &[Terminal]) -> Option<Terminal> {
    let rank = |o: &Outcome| match o {
        Outcome::Succeeded => 0,
        Outcome::Cancelled => 1,
        Outcome::TimedOut => 2,
        Outcome::Failed => 3,
    };
    let outcome = shards.iter().map(|t| &t.outcome).max_by_key(|o| rank(o))?;
    Some(Terminal {
        run: parent.to_string(),
        started_ms: shards.iter().map(|t| t.started_ms).min()?,
        finished_ms: shards.iter().map(|t| t.finished_ms).max()?,
        outcome: *outcome,
        counters: add_counters(&shards.iter().map(|t| &t.counters).collect::<Vec<_>>()),
    })
}
