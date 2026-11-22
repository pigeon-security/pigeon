//! pigeon's own values: what makes this platform pigeon's rather than generic. Each capability
//! takes its settings from here.

use pigeon_capabilities::Capability;
use pigeon_capabilities::approvals::Approvals;
use pigeon_capabilities::architecture::{Architecture, Wrangler};
use pigeon_capabilities::cf_proxy::CfProxy;
use pigeon_capabilities::ci::buildkite::{self, Buildkite};
use pigeon_capabilities::ci::class::Size;
use pigeon_capabilities::ci::containers::Containers;
use pigeon_capabilities::ci::{self, Ci, ProxyFiles};
use pigeon_capabilities::console::Console;
use pigeon_capabilities::deploys::Deploys;
use pigeon_capabilities::dispatch::Dispatch;
use pigeon_capabilities::git_proxy::GitProxy;
use pigeon_capabilities::images::Images;
use pigeon_capabilities::instances::Instances;
use pigeon_capabilities::land::{self, Land};
use pigeon_capabilities::linked::Linked;
use pigeon_capabilities::mirror::{self, Mirror};
use pigeon_capabilities::models::{Dialect, Models, Provider};
use pigeon_capabilities::oauth::OAuth;
use pigeon_capabilities::passkeys::Passkeys;
use pigeon_capabilities::policy::Policy;
use pigeon_capabilities::proxy::Tokens;
use pigeon_capabilities::public_host::{PublicHost, Retire};
use pigeon_capabilities::remote::Binding;
use pigeon_capabilities::secrets::Secrets;
use pigeon_capabilities::site::Site;
use pigeon_capabilities::source::Source;
use pigeon_capabilities::split::Split;
use pigeon_capabilities::users::Users;

/// This Worker's wrangler.toml, the authority for its bindings, queues and containers.
pub fn wrangler() -> Wrangler {
    Wrangler::read(include_str!("../wrangler.toml"))
}

/// The path map (ci/workers.toml) this Worker was built with: which platform Workers there
/// are, and which Worker owns each path. Checked by the smoke suite; an unreadable map leaves
/// this Worker alone.
pub fn map() -> pigeon_core::lanes::Map {
    pigeon_core::lanes::parse(include_str!("../../../ci/workers.toml"))
        .unwrap_or_else(|_| pigeon_capabilities::land::merge::fallback())
}

/// This Worker's name, as deployed.
pub const WORKER: &str = "pigeon-platform";

/// The Secrets Store binding holding the parent token.
pub const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

/// The zone, whose apex serves the public site.
pub const ZONE: &str = "pigeon.dev";

/// The platform's hostname, in public: its paths check their callers themselves.
pub const HOST: &str = "platform.pigeon.dev";
/// The hostname the proxies (`/git/`, `/cf/`) answer on, and nothing else but `/health`. A
/// hostname of their own, so the proxy credential a cloud environment's network proxy adds
/// for it never reaches `/mcp` or replaces the MCP connector's own token.
pub const PROXY_HOST: &str = "proxy.pigeon.dev";
const NAMESPACE: &str = "pigeon-security";
const REPO: &str = "pigeon";
const BRANCH: &str = "main";
/// The prefix of the branches sessions push, without its slash.
const SESSION_PREFIX: &str = "claude";

/// The branches the GitHub mirror copies: `main` only. Session branches (`claude/…`) stay in
/// Artifacts and never reach GitHub.
const MIRRORED: &[&str] = &[BRANCH];
const _: () = assert!(
    mirrors_no_session_branch(MIRRORED),
    "the GitHub mirror must not copy session branches"
);

/// Whether none of `branches` is a session branch, under `claude/`. At build time.
const fn mirrors_no_session_branch(branches: &[&str]) -> bool {
    let mut i = 0;
    while i < branches.len() {
        let b = branches[i].as_bytes();
        let p = b"claude/";
        if b.len() >= p.len() {
            let mut j = 0;
            while j < p.len() && b[j] == p[j] {
                j += 1;
            }
            if j == p.len() {
                return false;
            }
        }
        i += 1;
    }
    true
}

/// The public site on the zone's apex, served by this Worker.
pub fn site() -> Site {
    Site {
        zone: ZONE.into(),
        worker: WORKER.into(),
    }
}

/// The capability Workers bound to this one, by their Service Bindings in wrangler.toml
/// (docs/plans/workers-split.md). Their capabilities join this Worker's own at runtime, from
/// each one's manifest.
pub const BINDINGS: &[Binding] = &[];

/// The capabilities this Worker hosts.
pub fn capabilities() -> Vec<Box<dyn Capability>> {
    vec![
        // Every path on the platform checks its callers itself (/mcp and /oauth with OAuth,
        // /auth, /secrets and /tokens with passkeys) or is open to all (/health), so no Access is in front of it.
        Box::new(PublicHost {
            zone: ZONE.into(),
            host: HOST.into(),
            worker: WORKER.into(),
            // Its Access setup was retired, and that is done: nothing is retired again, so
            // setting the host up never deletes an Access application, policy or token.
            retire: Retire::default(),
            tool: "setup_platform",
        }),
        // The proxies' hostname, in public: the proxies check the proxy credential themselves.
        Box::new(PublicHost {
            zone: ZONE.into(),
            host: PROXY_HOST.into(),
            worker: WORKER.into(),
            retire: Retire::default(),
            tool: "setup_proxy_host",
        }),
        Box::new(site()),
        Box::new(Source {
            namespace: NAMESPACE.into(),
            repo: REPO.into(),
            default_branch: BRANCH.into(),
            description: "pigeon-security/pigeon".into(),
            import_from: Some("https://github.com/pigeon-security/pigeon".into()),
            queue: "pigeon-source-events".into(),
        }),
        Box::new(Mirror {
            source: mirror::Source {
                namespace: NAMESPACE.into(),
                repo: REPO.into(),
                branches: MIRRORED.iter().map(|b| (*b).into()).collect(),
            },
            url: "https://github.com/pigeon-security/pigeon.git".into(),
            token_binding: "GITHUB_MIRROR_TOKEN".into(),
            tool: "source_mirror",
        }),
        Box::new(Users { host: HOST.into() }),
        Box::new(Passkeys {
            host: HOST.into(),
            site_name: "pigeon".into(),
        }),
        Box::new(OAuth {
            host: HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        Box::new(Secrets {
            host: HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
            parent: PARENT_TOKEN.into(),
        }),
        // Proxy credentials: what a cloud session holds to reach /git/ and /cf/, issued on
        // the console's Settings → Keys (the old /tokens page redirects there).
        Box::new(Tokens {
            proxy_host: PROXY_HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        // Passkey approval of the tool calls the policy gates: `approvals_status`, and the calls
        // waiting on the console's Approvals tab.
        Box::new(Approvals),
        // The person's policy, .pigeon/policy.toml: `policy_check`, as their delegate.
        Box::new(Policy {
            session_prefix: format!("{SESSION_PREFIX}/"),
            main: BRANCH.into(),
        }),
        // The git proxy: the source repository for cloud sessions holding a proxy credential.
        // Its push rules are the policy's: session branches are pushed, rewritten and deleted;
        // main moves only by a landing.
        Box::new(GitProxy {
            host: PROXY_HOST.into(),
            namespace: NAMESPACE.into(),
            repo: REPO.into(),
            protected: format!("refs/heads/{BRANCH}"),
            prefix: format!("refs/heads/{SESSION_PREFIX}/"),
        }),
        // The Cloudflare API proxy: the calls `wrangler deploy` of this Worker makes, for cloud
        // sessions holding a proxy credential.
        Box::new(CfProxy {
            host: PROXY_HOST.into(),
            script: WORKER.into(),
            // The other platform Workers, from the map built into this Worker.
            others: map()
                .scripts()
                .into_iter()
                .filter(|s| *s != WORKER)
                .map(str::to_string)
                .collect(),
            // The queues this Worker produces to and its container classes, as wrangler.toml
            // declares them.
            queues: wrangler().producers,
            containers: wrangler().containers.into_iter().map(|c| c.class).collect(),
            land_repo: format!("{NAMESPACE}/{REPO}"),
        }),
        Box::new(Models {
            gateway: "pigeon".into(),
            providers: vec![Provider {
                slug: "anthropic",
                alias: "default",
                dialect: Dialect::Anthropic,
                default_model: "claude-haiku-5-5",
                key_help: "A Claude Platform API key, from the Claude Console's API keys page.",
            }],
            secrets_page: format!("https://{HOST}/access#credentials"),
            host: HOST.into(),
            proxy_host: PROXY_HOST.into(),
        }),
        Box::new(Ci { settings: ci() }),
        // Landing: a session branch tested, moved into main and deployed, through `land`.
        Box::new(Land { settings: land() }),
        Box::new(Deploys),
        // What the container applications run and the registry holds, read only.
        Box::new(Images),
        // `call {tool, args}`: any tool above by name, for sessions whose tool list predates it.
        Box::new(Dispatch),
        // The capability Workers: the caller key's setup and asking their manifests again.
        Box::new(Split { bindings: BINDINGS }),
        Box::new(Architecture),
        // Instances: what pigeon runs, here and in linked accounts. Only their shape is code;
        // every value is data a person approves on the console with their passkey.
        // Linked accounts: another Cloudflare account linked to an instance with Cloudflare's
        // OAuth consent, its tokens sealed in the Directory.
        Box::new(Linked {
            host: HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        Box::new(Instances {
            zone: ZONE.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        // The console: one page at the root, signed in to once with a passkey.
        Box::new(Console {
            host: HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
    ]
}

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

/// Where landings go: the source repository's main, gated by CI jobs `test` and `deploy`.
pub fn land() -> land::Settings {
    land::Settings {
        namespace: NAMESPACE.into(),
        repo: REPO.into(),
        main: format!("refs/heads/{BRANCH}"),
        parent: PARENT_TOKEN.into(),
        test_job: "test".into(),
        deploy_job: "deploy".into(),
        // Its runner hands the deploy job its token; see ci/runner/entrypoint.sh.
        deploy_connector: Some("containers".into()),
        // The MCP server portal on mcp.pigeon.dev lists this platform as server `pigeon`
        // (docs/plans/mcp-portal.md).
        portal_servers: vec!["pigeon".into()],
        // Each CI connector's setup (Buildkite's) runs once a landing has deployed.
        setup_connectors: true,
        map: map(),
    }
}

/// What async model jobs run with: the gateway `Models` sends through, and the parent token.
pub fn model_jobs() -> pigeon_capabilities::models::runner::Settings {
    pigeon_capabilities::models::runner::Settings {
        gateway: "pigeon".into(),
        parent: PARENT_TOKEN.into(),
    }
}

/// The CI gateway's settings: its connectors in the order jobs prefer them (Containers, then
/// Buildkite, then GitHub Actions, each added in its own phase), the repositories it runs, and
/// how long a queued job waits for its first choice.
pub fn ci() -> ci::Settings {
    ci::Settings {
        connectors: vec![
            Box::new(Containers {
                // The base image's key under [containers.images] in wrangler.toml.
                image: Some("base".into()),
                limit: 4,
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
                parent: PARENT_TOKEN.into(),
                queues: buildkite_queues(),
                hosted_only: true,
            }),
        ],
        files: Box::new(ProxyFiles {
            parent: PARENT_TOKEN.into(),
        }),
        repos: vec![format!("{NAMESPACE}/{REPO}")],
        host: HOST.into(),
        proxy_host: PROXY_HOST.into(),
        parent: PARENT_TOKEN.into(),
        wait_limit_ms: 5 * 60_000,
    }
}
