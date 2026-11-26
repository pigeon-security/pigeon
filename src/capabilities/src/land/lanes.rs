//! Deploy lanes, one per Worker (docs/plans/workers-split.md, phase 5). Pure.
//!
//! Each Worker has a lane that runs at most one deploy at a time; lanes run in parallel, at most
//! [`MAX_PARALLEL_DEPLOYS`] deploys at once (Cloudflare's API rate limit). A lane
//! deploys the newest merged commit that affects its Worker ("deploy latest") and so carries
//! every older merged landing for that Worker: main moves forward only, and landings merge one
//! at a time with strictly increasing `merged_at`, so a deploy at the commit merged at `covers`
//! holds every landing merged at or before it.
//!
//! A landing that affects core and is `ordered` (it changed a path in the map's `core_first`,
//! such as the contract between the Workers) deploys core first: no other lane deploys a commit
//! at or after such a landing's merge until core has deployed one that covers it. Then the other
//! lanes run in parallel. Any other landing's lanes, core's included, run in parallel at once.
//!
//! A landing that changed the deploy proxy's allowlist ([`PROXY_FIRST_PATHS`]) is `proxy_first`:
//! no other lane, core's included, deploys a commit at or after it until the proxy has deployed
//! it or its deploy has failed. If it is also `ordered`, core goes first, then the proxy.
//!
//! A landing is landed once every Worker it affects has deployed a commit at or after its merge.
//! It is deploy_failed once an incident (a lane's deploy that failed, attributed by
//! [`bisect`]) finds it the cause, or finds its commit holds the cause, or cannot isolate the
//! cause; error once a lane's deploy carrying it could not be run on the gateway's side. A
//! failed deploy outside an incident (one an older Lander recorded) fails what it carried.

use super::state::Seen;

/// A live landing in main, waiting for its deploys: `merged` or `deploying`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub id: String,
    /// When it reached main; strictly increasing in main's order.
    pub merged_at: i64,
    /// Its commit on main.
    pub commit: String,
    /// The Workers it affects, by name.
    pub workers: Vec<String>,
    /// Whether the other lanes wait for core to deploy it first.
    pub ordered: bool,
    /// Whether no lane deploys it before the proxy has (it changed the deploy proxy's allowlist).
    pub proxy_first: bool,
}

/// The paths whose change makes a landing deploy the proxy first: the deploy proxy's allowlist.
pub const PROXY_FIRST_PATHS: [&str; 1] = ["src/proxies/src/cf_proxy.rs"];

/// The proxy's Worker name in `ci/workers.toml`.
pub const PROXY_WORKER: &str = "proxy";

/// Bit of `landings.ordered`: core deploys first.
pub const ORDER_CORE_FIRST: i64 = 1;

/// Bit of `landings.ordered`: the proxy deploys first.
pub const ORDER_PROXY_FIRST: i64 = 2;

/// Whether any of `changed` is a proxy-first path.
pub fn changes_proxy_first(changed: &[&str]) -> bool {
    changed.iter().any(|p| PROXY_FIRST_PATHS.contains(p))
}

/// The map a check classifies by: `main`'s map plus the `[[worker]]` entries `branch` ADDS, and
/// the names of those entries. An entry whose name is already in main's map is ignored from the
/// branch, so a branch never moves an existing path's lane; so is one that reuses a script main
/// already has. An entry the branch removes stays, as main has it. This is safe because
/// `ci/workers.toml` is guarded: adding an entry needs the person's gate approval.
///
/// Such a landing is proxy-first (the proxy builds its scope from the map compiled into it), then
/// the new Worker's first deploy reads the map at its commit ([`resolve_entry`]). Binding the new
/// Worker in core (`[[services]]`) stays a later landing: a binding's target must exist first.
pub fn with_added_workers(
    main: &pigeon_core::lanes::Map,
    branch: &pigeon_core::lanes::Map,
) -> (pigeon_core::lanes::Map, Vec<String>) {
    let mut map = main.clone();
    let mut added = Vec::new();
    for w in &branch.workers {
        let known = map
            .workers
            .iter()
            .any(|m| m.name == w.name || m.script == w.script);
        if !known {
            added.push(w.name.clone());
            map.workers.push(w.clone());
        }
    }
    (map, added)
}

/// How a lane's deploy stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployState {
    Running,
    Succeeded,
    /// Its CI job ended without passing.
    Failed,
    /// The gateway could not run it.
    Error,
}

impl DeployState {
    pub fn name(self) -> &'static str {
        match self {
            DeployState::Running => "running",
            DeployState::Succeeded => "succeeded",
            DeployState::Failed => "failed",
            DeployState::Error => "error",
        }
    }

    pub fn named(s: &str) -> Option<DeployState> {
        [
            DeployState::Running,
            DeployState::Succeeded,
            DeployState::Failed,
            DeployState::Error,
        ]
        .into_iter()
        .find(|d| d.name() == s)
    }
}

/// One deploy of one Worker, as the lanes need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deploy {
    pub worker: String,
    /// The `merged_at` of the landing whose commit it deploys: it carries every landing merged
    /// at or before.
    pub covers: i64,
    pub state: DeployState,
    /// Part of an incident ([`bisect`]): the deploy whose failure opened it, or one of its
    /// rollback, bisect or restore deploys. Its failure is the incident's to judge, so it fails
    /// no landing by itself; a success covers as any other.
    pub incident: bool,
}

/// A deploy a lane starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub worker: String,
    /// The landing whose commit it deploys: the newest the lane may deploy.
    pub landing: String,
    pub commit: String,
    pub covers: i64,
    /// The landings it carries, oldest first, the target last.
    pub carries: Vec<String>,
}

/// Whether `worker` has deployed a commit at or after `merged_at`.
pub fn covered(deploys: &[Deploy], worker: &str, merged_at: i64) -> bool {
    deploys
        .iter()
        .any(|d| d.worker == worker && d.state == DeployState::Succeeded && d.covers >= merged_at)
}

/// The deploy for `worker` that carried a landing merged at `merged_at` and did not pass, if
/// one did and the Worker never deployed the landing otherwise.
fn failed_for(deploys: &[Deploy], worker: &str, merged_at: i64) -> Option<DeployState> {
    if covered(deploys, worker, merged_at) {
        return None;
    }
    deploys
        .iter()
        .filter(|d| d.worker == worker && d.covers >= merged_at && !d.incident)
        .map(|d| d.state)
        .find(|s| matches!(s, DeployState::Failed | DeployState::Error))
}

/// What a live merged landing has come to, as the move its state takes: `JobEnded { passed:
/// true }` once every Worker it affects deployed it, `JobEnded { passed: false }` once a deploy
/// that carried it failed, `Failed` once one could not be run; `None` while it waits.
pub fn outcome(landing: &Merged, deploys: &[Deploy]) -> Option<Seen> {
    let mut all = true;
    for w in &landing.workers {
        match failed_for(deploys, w, landing.merged_at) {
            Some(DeployState::Error) => return Some(Seen::Failed),
            Some(_) => return Some(Seen::JobEnded { passed: false }),
            None => {}
        }
        all &= covered(deploys, w, landing.merged_at);
    }
    all.then_some(Seen::JobEnded { passed: true })
}

/// At most this many deploys run at once, across every lane, rollbacks included. Each deploy
/// makes the same Cloudflare API calls at its start, and the account's API rate limit (10429)
/// refused them when every lane started together; a lane waits for a free slot instead.
pub const MAX_PARALLEL_DEPLOYS: usize = 2;

/// The deploys the lanes start next, given the live merged landings (any order) and every
/// deploy so far; `core` names the Worker that deploys first. A lane with a running deploy, or
/// `held` by an open incident, starts nothing, and no more start than leave at most
/// [`MAX_PARALLEL_DEPLOYS`] running. Core first, then the other lanes by name.
pub fn starts(core: &str, landings: &[Merged], deploys: &[Deploy], held: &[&str]) -> Vec<Start> {
    let running = deploys
        .iter()
        .filter(|d| d.state == DeployState::Running)
        .count();
    let mut slots = MAX_PARALLEL_DEPLOYS.saturating_sub(running);
    let mut sorted: Vec<&Merged> = landings.iter().collect();
    sorted.sort_by_key(|l| l.merged_at);
    // Still waiting for `w`: affects it, not deployed by it, and no deploy of it failed for it.
    let waits = |l: &Merged, w: &str| -> bool {
        l.workers.iter().any(|x| x == w)
            && !covered(deploys, w, l.merged_at)
            && failed_for(deploys, w, l.merged_at).is_none()
    };
    let mut lanes: Vec<&str> = sorted
        .iter()
        .flat_map(|l| l.workers.iter().map(String::as_str))
        .collect();
    lanes.sort_by_key(|w| (*w != core, *w));
    lanes.dedup();
    let mut out = Vec::new();
    for w in lanes {
        if slots == 0 {
            break;
        }
        let busy = deploys
            .iter()
            .any(|d| d.worker == w && d.state == DeployState::Running);
        if busy || held.contains(&w) {
            continue;
        }
        // Another lane deploys nothing at or after an ordered landing core has still to deploy.
        let core_block = if w == core {
            i64::MAX
        } else {
            sorted
                .iter()
                .filter(|l| waits(l, w) && l.ordered && l.workers.iter().any(|x| x == core))
                .filter(|l| !covered(deploys, core, l.merged_at))
                .map(|l| l.merged_at)
                .min()
                .unwrap_or(i64::MAX)
        };
        // Nor does a lane but the proxy's deploy at or after a proxy-first landing the proxy has
        // still to deploy and has not failed. Core goes first for one that is also core first.
        let proxy_block = if w == PROXY_WORKER {
            i64::MAX
        } else {
            sorted
                .iter()
                .filter(|l| l.proxy_first && l.workers.iter().any(|x| x == PROXY_WORKER))
                .filter(|l| w != core || !l.ordered)
                .filter(|l| {
                    !covered(deploys, PROXY_WORKER, l.merged_at)
                        && failed_for(deploys, PROXY_WORKER, l.merged_at).is_none()
                })
                .map(|l| l.merged_at)
                .min()
                .unwrap_or(i64::MAX)
        };
        let block = core_block.min(proxy_block);
        let carried: Vec<&&Merged> = sorted
            .iter()
            .filter(|l| waits(l, w) && l.merged_at < block)
            .collect();
        let Some(target) = carried.last() else {
            continue;
        };
        slots -= 1;
        out.push(Start {
            worker: w.to_string(),
            landing: target.id.clone(),
            commit: target.commit.clone(),
            covers: target.merged_at,
            carries: carried.iter().map(|l| l.id.clone()).collect(),
        });
    }
    out
}

/// Which map a deploy's Worker entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapUsed {
    /// `ci/workers.toml` at the deploy's commit.
    Commit,
    /// The map compiled into the running core.
    Compiled,
}

/// The entry a deploy of `name` uses. `text` is what reading `ci/workers.toml` at the deploy's
/// commit gave: its text, no file, or an error. The commit's map wins when it parses and has the
/// Worker; otherwise the compiled map answers. The string says which map, and why when it is the
/// compiled one. `None` when neither map has the Worker.
pub fn resolve_entry(
    text: Result<Option<String>, String>,
    name: &str,
    compiled: &pigeon_core::lanes::Map,
) -> Option<(pigeon_core::lanes::Worker, MapUsed, String)> {
    let why = match text {
        Ok(Some(t)) => match pigeon_core::lanes::parse(&t) {
            Ok(map) => match map.worker(name) {
                Some(w) => {
                    return Some((
                        w.clone(),
                        MapUsed::Commit,
                        "map from the commit".to_string(),
                    ));
                }
                None => format!("the commit's map has no Worker `{name}`"),
            },
            Err(e) => format!("the commit's map does not parse: {e}"),
        },
        Ok(None) => "the commit has no map".to_string(),
        Err(e) => format!("the commit's map could not be read: {e}"),
    };
    compiled.worker(name).map(|w| {
        (
            w.clone(),
            MapUsed::Compiled,
            format!("compiled map ({why})"),
        )
    })
}

/// The script a deploy grant answers: the one recorded on the deploy row, or, for a row
/// without one, the compiled map's.
pub fn grant_script(recorded: Option<&str>, compiled: Option<&str>) -> Option<String> {
    recorded.or(compiled).map(str::to_string)
}

/// One deploy as a landing's note and `land_status` name it: the Worker, why it ran, its job,
/// the commit it deployed, and the landings it included with their commits, such as
/// `sites:lane:job7@1a2b3c4d5e6f[L1@0f0e0d0c0b0a+L2@1a2b3c4d5e6f]`.
pub fn deploy_note(
    worker: &str,
    purpose: Purpose,
    job: &str,
    commit: &str,
    included: &[Included],
) -> String {
    let short = |c: &str| c.chars().take(12).collect::<String>();
    let list: Vec<String> = included
        .iter()
        .map(|l| format!("{}@{}", l.id, short(&l.commit)))
        .collect();
    format!(
        "{worker}:{}:{job}@{}[{}]",
        purpose.name(),
        short(commit),
        list.join("+")
    )
}

// ---- incidents: a failed deploy, attributed ----
//
// "Deploy latest" lets one deploy carry several landings, so its failure must be attributed.
// When a lane's deploy fails, the lane opens an incident: it redeploys the Worker's last good
// build at once (a rollback), then deploys the commits of the landings the failed deploy
// included, one at a time, oldest first, until one fails (a bisect). A landing's commit on main
// holds every landing merged before it, so deploying landing X's commit tests X together with
// everything before it: oldest first therefore finds the first commit that fails, and the
// landing that made it is the cause. The newest included commit is already known to fail (the
// deploy that opened the incident), so it is never deployed again. After a bisect step fails,
// the newest good build is deployed again (a restore), so the Worker is not left on a failing
// build. At most [`REDEPLOYS`] redeploys per incident; a bisect step is only tried when a
// restore after it still fits.

/// How many redeploys one incident may make: its rollback, bisect steps and restores together.
pub const REDEPLOYS: usize = 4;

/// A landing a deploy included: its id, its commit on main and when it merged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Included {
    pub id: String,
    pub commit: String,
    pub merged_at: i64,
}

/// A build of a Worker known good: the commit of a deploy of it that passed, and the
/// `merged_at` that commit covers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Good {
    pub commit: String,
    pub covers: i64,
}

/// Why a deploy runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The lane's own deploy of its newest merged commit.
    Lane,
    /// An incident's first redeploy: the Worker's last good build.
    Rollback,
    /// An incident's deploy of one included landing's commit.
    Bisect,
    /// An incident's redeploy of the newest good build after a bisect step failed.
    Restore,
}

impl Purpose {
    pub fn name(self) -> &'static str {
        match self {
            Purpose::Lane => "lane",
            Purpose::Rollback => "rollback",
            Purpose::Bisect => "bisect",
            Purpose::Restore => "restore",
        }
    }

    pub fn named(s: &str) -> Option<Purpose> {
        [
            Purpose::Lane,
            Purpose::Rollback,
            Purpose::Bisect,
            Purpose::Restore,
        ]
        .into_iter()
        .find(|p| p.name() == s)
    }
}

/// How one of an incident's redeploys ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tried {
    Passed,
    /// Its job ended without passing.
    Failed,
    /// The gateway could not run it.
    Error,
}

/// What an incident does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Deploy `commit` (covering `covers`), carrying `carries`.
    Deploy {
        purpose: Purpose,
        commit: String,
        covers: i64,
        carries: Vec<String>,
    },
    /// It is over.
    Done(Verdict),
}

/// How an incident ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The landing found to be the cause, if one was.
    pub guilty: Option<String>,
    /// The included landings whose commits deployed and passed during the incident.
    pub passed: Vec<String>,
    /// Each included landing that ends `deploy_failed`, with why.
    pub failed: Vec<(String, String)>,
    /// The good build the Worker is left on; `None` when it may still be on a failing build.
    pub on: Option<Good>,
    /// The redeploys it made.
    pub redeploys: usize,
    /// One line for `land_status`.
    pub summary: String,
}

/// The incident text for a Worker whose first deploy failed: nothing was running, so there is
/// nothing to roll back. The one place this wording lives.
pub fn first_deploy_text(worker: &str) -> String {
    format!("first deploy of {worker} failed; nothing was running and nothing is rolled back")
}

/// The incident text for a Worker that had a good build but could not be returned to it.
/// The one place this wording lives.
pub fn failing_build_text(worker: &str) -> String {
    format!("{worker} may still be on a failing build: a person must look")
}

/// Which text says a Worker is left on no good build: the first-deploy one when it never had
/// a good build (`ever_good` is false), else the failing-build one.
pub fn not_on_good_text(worker: &str, ever_good: bool) -> String {
    if ever_good {
        failing_build_text(worker)
    } else {
        first_deploy_text(worker)
    }
}

/// The verdict once `included[..idx]` passed, with the cause `guilty` (its index and why) if
/// one was found, the Worker left `on` a good build or not, and `why` the cause was not found.
/// `ever_good` is whether the Worker had any good build, before the incident or during it.
#[allow(clippy::too_many_arguments)]
fn verdict(
    ever_good: bool,
    worker: &str,
    included: &[Included],
    idx: usize,
    guilty: Option<(usize, String)>,
    on: Option<Good>,
    used: usize,
    why: &str,
) -> Next {
    let passed: Vec<String> = included[..idx.min(included.len())]
        .iter()
        .map(|l| l.id.clone())
        .collect();
    let left = match &on {
        Some(g) => format!("{worker} is on the good build {}", g.commit),
        None => not_on_good_text(worker, ever_good),
    };
    let (cause, failed, summary) = match guilty {
        Some((g, reason)) => {
            let cause = &included[g];
            let mut failed = vec![(cause.id.clone(), reason)];
            for l in &included[g + 1..] {
                failed.push((
                    l.id.clone(),
                    format!(
                        "not deployed: its commit {} holds landing {}, which broke {worker}'s deploy",
                        l.commit, cause.id
                    ),
                ));
            }
            let summary = format!(
                "{worker}'s deploy failed; landing {} ({}) is the cause; {left}; {used} redeploys",
                cause.id, cause.commit
            );
            (Some(cause.id.clone()), failed, summary)
        }
        None => {
            let failed = included[idx.min(included.len())..]
                .iter()
                .map(|l| {
                    (
                        l.id.clone(),
                        format!("could not isolate: {worker}'s deploy failed and {why}"),
                    )
                })
                .collect();
            let summary = format!(
                "{worker}'s deploy failed; could not isolate the cause ({why}); {left}; {used} redeploys"
            );
            (None, failed, summary)
        }
    };
    Next::Done(Verdict {
        guilty: cause,
        passed,
        failed,
        on,
        redeploys: used,
        summary,
    })
}

/// The incident of `worker`'s lane: given the Worker's last good build before the failed
/// deploy (`None` when there is none), the landings that deploy included (oldest first; the
/// newest's commit is the one that failed), how each redeploy so far ended, in order, and the
/// redeploys allowed: what it deploys next, or its verdict.
pub fn bisect(
    worker: &str,
    last_good: Option<&Good>,
    included: &[Included],
    tried: &[Tried],
    budget: usize,
) -> Next {
    bisect_from(worker, last_good, included, tried, budget, false)
}

/// Whether a failed deploy job never uploaded: it `failed` with exit code 3, which the deploy
/// script uses only when it stopped before its first real upload. Any other end, or no exit
/// code (every job before that exit code existed), may have uploaded.
pub fn never_uploaded(state: &str, exit_code: Option<i64>) -> bool {
    state == "failed" && exit_code == Some(3)
}

/// The exit code a deploy's stored detail records as `(exit code N)`, if any. The last such
/// mark wins.
pub fn exit_code_in(detail: &str) -> Option<i64> {
    let at = detail.rfind("(exit code ")? + "(exit code ".len();
    let rest = &detail[at..];
    rest[..rest.find(')')?].trim().parse().ok()
}

/// Whether the rollback is skipped: nothing was uploaded, and the Worker's newest earlier
/// deploy of any purpose (`newest_earlier`, a deploy id) is the succeeded one `last_good`
/// names (`last_good`, a deploy id). Then the Worker is still on its good build.
pub fn skip_rollback(
    never_uploaded: bool,
    newest_earlier: Option<i64>,
    last_good: Option<i64>,
) -> bool {
    never_uploaded && last_good.is_some() && newest_earlier == last_good
}

/// [`bisect`], where `rolled_back` says the Worker is already on `last_good`, so no rollback
/// is deployed and `tried` holds only the bisect steps.
pub fn bisect_from(
    worker: &str,
    last_good: Option<&Good>,
    included: &[Included],
    tried: &[Tried],
    budget: usize,
    rolled_back: bool,
) -> Next {
    let k = included.len();
    let ids = |n: usize| -> Vec<String> { included[..n].iter().map(|l| l.id.clone()).collect() };
    let mut good: Option<Good> = last_good.cloned();
    // Whether the Worker is on a good build; at first it is on the one that failed.
    let mut on_good = rolled_back && last_good.is_some();
    // The included landings before `idx` deployed and passed.
    let mut idx = 0;
    let mut guilty: Option<(usize, String)> = None;
    let mut used = 0;
    let mut outcomes = tried.iter();
    loop {
        // Back to the newest good build first: the rollback, or a restore after a failed step.
        if !on_good {
            match good.clone() {
                Some(g) => {
                    if used >= budget {
                        let why = "the incident used its redeploys";
                        return verdict(
                            good.is_some(),
                            worker,
                            included,
                            idx,
                            guilty,
                            None,
                            used,
                            why,
                        );
                    }
                    match outcomes.next() {
                        None => {
                            let purpose = if used == 0 {
                                Purpose::Rollback
                            } else {
                                Purpose::Restore
                            };
                            return Next::Deploy {
                                purpose,
                                commit: g.commit,
                                covers: g.covers,
                                carries: ids(idx),
                            };
                        }
                        Some(Tried::Passed) => on_good = true,
                        // A good build that failed to deploy again is tried again, in budget.
                        Some(Tried::Failed) => {}
                        Some(Tried::Error) => {
                            let why = "a redeploy could not be run";
                            return verdict(
                                good.is_some(),
                                worker,
                                included,
                                idx,
                                guilty,
                                None,
                                used + 1,
                                why,
                            );
                        }
                    }
                    used += 1;
                    continue;
                }
                None if guilty.is_some() => {
                    let why = format!("there is no earlier good build of {worker}");
                    return verdict(
                        good.is_some(),
                        worker,
                        included,
                        idx,
                        guilty,
                        None,
                        used,
                        &why,
                    );
                }
                // No good build to go back to: bisect from the failing one.
                None => {}
            }
        }
        if guilty.is_some() {
            return verdict(
                good.is_some(),
                worker,
                included,
                idx,
                guilty,
                good,
                used,
                "",
            );
        }
        let on = good.clone().filter(|_| on_good);
        if k == 0 {
            return verdict(
                good.is_some(),
                worker,
                included,
                0,
                None,
                on,
                used,
                "it included no landing",
            );
        }
        // Every commit before the newest passed: the newest, known to fail, is the cause.
        if idx + 1 >= k {
            let last = &included[k - 1];
            let why = format!(
                "{worker}'s deploy failed at its commit {}, and every earlier commit it carried deployed",
                last.commit
            );
            guilty = Some((k - 1, why));
            continue;
        }
        // A step, and a restore should it fail.
        let need = if good.is_some() { 2 } else { 1 };
        if used + need > budget {
            let why = "the incident used its redeploys";
            return verdict(good.is_some(), worker, included, idx, None, on, used, why);
        }
        let step = &included[idx];
        match outcomes.next() {
            None => {
                return Next::Deploy {
                    purpose: Purpose::Bisect,
                    commit: step.commit.clone(),
                    covers: step.merged_at,
                    carries: ids(idx + 1),
                };
            }
            Some(Tried::Passed) => {
                good = Some(Good {
                    commit: step.commit.clone(),
                    covers: step.merged_at,
                });
                on_good = true;
                idx += 1;
            }
            Some(Tried::Failed) => {
                let why = format!(
                    "deploying its commit {} to {worker} failed: the first failing commit of those the deploy carried",
                    step.commit
                );
                guilty = Some((idx, why));
                on_good = false;
            }
            Some(Tried::Error) => {
                let why = "a bisect deploy could not be run";
                return verdict(
                    good.is_some(),
                    worker,
                    included,
                    idx,
                    None,
                    None,
                    used + 1,
                    why,
                );
            }
        }
        used += 1;
    }
}

/// The state of an incident that a later green lane deploy of its Worker covers.
pub const COVERED: &str = "covered";

/// The state of an incident that ended because a redeploy failed for the environment: no
/// bisect was made, and no landing is to blame.
pub const ENVIRONMENT: &str = "environment";

/// Marks a lane deploy's detail once it was deployed again after an environment failure.
pub const ENV_RETRY: &str = "(environment retry)";

/// Whether a deploy's detail records an environment failure ([`environment_detail`]).
pub fn is_environment_detail(detail: &str) -> bool {
    detail.starts_with("environment: ")
}

/// The detail a deploy and its landings get when it failed for the environment, why `why`.
pub fn environment_detail(why: &str) -> String {
    format!("environment: {why}; not this landing's fault; land again")
}

/// Whether the 3 digits at `at` in `s` are a 5xx status and end there.
fn is_5xx_at(s: &str, at: usize) -> bool {
    let b = s.as_bytes();
    b.len() >= at + 3
        && b[at] == b'5'
        && b[at + 1].is_ascii_digit()
        && b[at + 2].is_ascii_digit()
        && !b.get(at + 3).is_some_and(u8::is_ascii_digit)
}

/// Whether a job failed for the environment, not for the code: the reason, short, if so.
/// Job state `error` (the runner was lost) and `cancelled` are the environment. A `failed`
/// job is when its output `tail` names a known platform fault: Cloudflare `code: 10007`, a
/// Cloudflare 5xx status, or the deploy proxy being unreachable. Anything else, `timed_out`
/// included, is a code fault (`None`). The exit code is not used yet.
pub fn environment_fault(state: &str, _exit_code: Option<i64>, tail: &str) -> Option<String> {
    match state {
        "error" => return Some("the runner was lost".to_string()),
        "cancelled" => return Some("the job was cancelled".to_string()),
        "failed" => {}
        _ => return None,
    }
    let low = tail.to_ascii_lowercase();
    if low.contains("code: 10007") {
        return Some("Cloudflare error code 10007".to_string());
    }
    if low.contains("the proxy could not be reached") {
        return Some("the deploy proxy could not be reached".to_string());
    }
    for key in [
        "http ",
        "status ",
        "status: ",
        "status code ",
        "status code: ",
    ] {
        let mut from = 0;
        while let Some(i) = low[from..].find(key) {
            let at = from + i + key.len();
            if is_5xx_at(&low, at) {
                return Some(format!("Cloudflare answered {}", &low[at..at + 3]));
            }
            from = at;
        }
    }
    None
}

/// What to do with a deploy that failed for the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvStep {
    /// Deploy once more at the same commit with the same landings.
    Redeploy,
    /// End the deploy `error` with this detail; no landing is blamed.
    Error(String),
}

/// The step for an environment failure: a lane deploy not yet deployed again
/// (`retried` false, not part of an incident) is deployed once more; any other ends in error.
pub fn environment_step(retried: bool, in_incident: bool, why: &str) -> EnvStep {
    if retried || in_incident {
        EnvStep::Error(environment_detail(why))
    } else {
        EnvStep::Redeploy
    }
}

/// Whether a later green deploy makes an incident moot. True only for an `unisolated`,
/// `environment` or
/// `stuck` incident whose failed deploy (`failed_deploy`, carrying up to `failed_covers`) is
/// followed by a later deploy (`green_deploy`) that covers at least as much. The caller passes
/// only a succeeded lane deploy of the same Worker.
pub fn incident_covered(
    state: &str,
    failed_deploy: i64,
    failed_covers: i64,
    green_deploy: i64,
    green_covers: i64,
) -> bool {
    matches!(state, "unisolated" | "stuck" | ENVIRONMENT)
        && green_deploy > failed_deploy
        && green_covers >= failed_covers
}

/// Why each landing a failed core deploy carried ends `deploy_failed`.
pub const CORE_NOT_BISECTED: &str = "core deploy failed; not bisected (core runs the Lander)";

/// The incident of core's lane. It differs from [`bisect`] because the Lander runs in core:
/// every core deploy restarts it, so core is never bisected, and core is only rolled back to a
/// build that holds this Lander (one covering at least `lander_since`, the `merged_at` of the
/// first core deploy this Lander saw pass). Given core's last good build before the failed
/// deploy at `failed_commit`, the landings it included, and how the rollback went (if it
/// ran): the rollback, or the verdict. Every carried landing ends `deploy_failed`
/// ([`CORE_NOT_BISECTED`]). With no such build, or a rollback that did not pass, the incident
/// is stuck: core is left as the job left it, for a person to recover by hand.
pub fn core_incident(
    last_good: Option<&Good>,
    lander_since: Option<i64>,
    failed_commit: &str,
    included: &[Included],
    tried: &[Tried],
) -> Next {
    core_incident_from(
        last_good,
        lander_since,
        failed_commit,
        included,
        tried,
        false,
    )
}

/// [`core_incident`], where `rolled_back` says nothing was uploaded and core is still on
/// `last_good`: the incident ends at once with no redeploy.
pub fn core_incident_from(
    last_good: Option<&Good>,
    lander_since: Option<i64>,
    failed_commit: &str,
    included: &[Included],
    tried: &[Tried],
    rolled_back: bool,
) -> Next {
    if let (true, Some(good)) = (rolled_back, last_good) {
        return Next::Done(Verdict {
            guilty: None,
            passed: Vec::new(),
            failed: included
                .iter()
                .map(|l| (l.id.clone(), CORE_NOT_BISECTED.to_string()))
                .collect(),
            summary: format!(
                "the core deploy at commit {failed_commit} failed; nothing was uploaded, so core \
                 is still on the good build {}; the landings it carried were not bisected (core \
                 runs the Lander)",
                good.commit
            ),
            on: Some(good.clone()),
            redeploys: 0,
        });
    }
    let target = last_good
        .filter(|g| lander_since.is_some_and(|since| g.covers >= since))
        .cloned();
    let failed: Vec<(String, String)> = included
        .iter()
        .map(|l| (l.id.clone(), CORE_NOT_BISECTED.to_string()))
        .collect();
    let stuck = |why: &str, redeploys: usize| {
        Next::Done(Verdict {
            guilty: None,
            passed: Vec::new(),
            failed: failed.clone(),
            on: None,
            redeploys,
            summary: format!(
                "the core deploy at commit {failed_commit} failed; {why}; core is left as the job \
                 left it; recovery is by hand (docs/proxies.md, break-glass)"
            ),
        })
    };
    let Some(target) = target else {
        return stuck(
            "there is no earlier good build of core that holds this Lander",
            0,
        );
    };
    match tried.first() {
        None => Next::Deploy {
            purpose: Purpose::Rollback,
            commit: target.commit,
            covers: target.covers,
            carries: Vec::new(),
        },
        Some(Tried::Passed) => Next::Done(Verdict {
            guilty: None,
            passed: Vec::new(),
            failed: failed.clone(),
            summary: format!(
                "the core deploy at commit {failed_commit} failed; core is back on the good build \
                 {}; the landings it carried were not bisected (core runs the Lander)",
                target.commit
            ),
            on: Some(target),
            redeploys: 1,
        }),
        Some(_) => stuck(
            &format!("the rollback to {} did not pass", target.commit),
            1,
        ),
    }
}
