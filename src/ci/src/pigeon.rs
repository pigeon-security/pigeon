//! pigeon's own CI values, in one place for both Workers that serve CI: core
//! (`pigeon-platform`), which hosts the scheduler and the runners, and `pigeon-ci-gateway`.

use crate::buildkite::{self, Buildkite};
use crate::class::Size;
use crate::containers::Containers;
use crate::{Accounts, ProxyFiles, Settings};

/// The platform's hostname: runners report here, and Buildkite's steps fetch the bootstrap
/// from it. Core checks at build time that its own is the same.
pub const HOST: &str = "platform.pigeon.dev";
/// The proxies' hostname: runners fetch through its git proxy.
pub const PROXY_HOST: &str = "proxy.pigeon.dev";
/// The one repository CI runs, as `namespace/name`.
pub const REPO: &str = "pigeon-security/pigeon";

/// The Buildkite organization's slug; empty leaves the connector off and sets nothing up.
const BUILDKITE_ORG: &str = "pigeon-security";
/// Buildkite's hosted queues in cluster `pigeon`, one row per class: the queue's key, its
/// instance shape, the class, and the shape's vCPU, memory (GB) and disk (GB). Hosted Linux
/// agents have docker, so each queue also runs its class's `-docker` jobs. Linux only for now.
/// The setup makes each queue (docs/plans/buildkite-mcp.md).
const BUILDKITE_QUEUES: [(&str, &str, Size, bool, u32, u32, u32); 4] = [
    (
        "pigeon-linux-small",
        "LINUX_AMD64_2X4",
        Size::Small,
        false,
        2,
        4,
        47,
    ),
    (
        "pigeon-linux-small",
        "LINUX_AMD64_2X4",
        Size::Small,
        true,
        2,
        4,
        47,
    ),
    (
        "pigeon-linux-medium",
        "LINUX_AMD64_4X16",
        Size::Medium,
        false,
        4,
        16,
        95,
    ),
    (
        "pigeon-linux-medium",
        "LINUX_AMD64_4X16",
        Size::Medium,
        true,
        4,
        16,
        95,
    ),
];

fn buildkite_queues() -> Vec<buildkite::Queue> {
    BUILDKITE_QUEUES
        .iter()
        .map(|&(key, shape, size, docker, cpu, mem, disk)| {
            buildkite::queue(key, shape, buildkite::linux(size, docker), cpu, mem, disk)
        })
        .collect()
}

/// pigeon's CI settings: its connectors in the order jobs prefer them (Containers, then
/// Buildkite, then GitHub Actions, each added in its own phase), the repositories it runs, and
/// how long a queued job waits for its first choice. `accounts` is where its Cloudflare account
/// comes from: the parent token in core, a scoped credential from core in the gateway.
pub fn settings(accounts: Accounts) -> Settings {
    Settings {
        connectors: vec![
            Box::new(Containers {
                // The base image's key under [containers.images] in core's wrangler.toml.
                image: Some("base".into()),
                // Concurrent CI containers (standard-4: 4 vCPU, 12 GiB each). 8 covers a shared
                // landing's six deploys with tests beside them; the account's default limits
                // (15,000 vCPU, 60 TiB concurrent) leave room for far more, and the class's
                // durable_object scheduling sets no max_instances.
                limit: 8,
                entrypoint: vec!["/usr/local/bin/pigeon-ci-runner".into()],
            }),
            Box::new(Buildkite {
                // The organization's slug, from its Buildkite URL; empty leaves the connector
                // unconfigured.
                org: BUILDKITE_ORG.into(),
                pipeline: "pigeon".into(),
                cluster: "pigeon".into(),
                // Never cloned: every step skips checkout and fetches through the git proxy.
                repository: "https://github.com/pigeon-security/pigeon.git".into(),
                // Buildkite's MCP for sessions, behind the portal with the same token.
                portal: Some((
                    "buildkite".into(),
                    "https://mcp.buildkite.com/direct".into(),
                )),
                queues: buildkite_queues(),
                hosted_only: true,
            }),
        ],
        files: Box::new(ProxyFiles),
        repos: vec![REPO.into()],
        host: HOST.into(),
        proxy_host: PROXY_HOST.into(),
        accounts,
        wait_limit_ms: 5 * 60_000,
    }
}
