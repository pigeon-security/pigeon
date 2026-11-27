//! pigeon's own values: what makes this platform pigeon's rather than generic. Each capability
//! takes its settings from here.

use pigeon_capabilities::Capability;
use pigeon_capabilities::apple::Apple;
use pigeon_capabilities::approvals::Approvals;
use pigeon_capabilities::architecture::{Architecture, Wrangler};
use pigeon_capabilities::changelog::Changelog;
use pigeon_capabilities::ci;
use pigeon_capabilities::console::Console;
use pigeon_capabilities::dispatch::Dispatch;
use pigeon_capabilities::github::Github;
use pigeon_capabilities::ingest;
use pigeon_capabilities::instances::{Instances, MultiInstance};
use pigeon_capabilities::land::{self, Land};
use pigeon_capabilities::linked::Linked;
use pigeon_capabilities::mirror::{self, Mirror};
use pigeon_capabilities::oauth::OAuth;
use pigeon_capabilities::observability::Observability;
use pigeon_capabilities::passkeys::Passkeys;
use pigeon_capabilities::policy::Policy;
use pigeon_capabilities::proxy::{Backup, CfProxy, GitProxy, Tokens, cf_proxy};
use pigeon_capabilities::public_host::PublicHost;
use pigeon_capabilities::remote::Binding;
use pigeon_capabilities::secrets::Secrets;
use pigeon_capabilities::site::{self, Site};
use pigeon_capabilities::site_passkeys::SitePasskeys;
use pigeon_capabilities::source::Source;
use pigeon_capabilities::split::Split;
use pigeon_capabilities::storage::{self, Storage};
use pigeon_capabilities::users::Users;
use pigeon_capabilities::versioning::Versioning;

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
/// The Worker `setup_proxy_host` attaches the proxy hostname to by default: the proxy Worker,
/// which has served it since the cutover (docs/plans/workers-split.md, phase 6). Core keeps the
/// same proxies as a backup ([`backup`]); switching to it is one call,
/// `setup_proxy_host {"worker": "pigeon-platform"}`, and back is `{"worker": "pigeon-proxy"}`.
pub const PROXY_WORKER: &str = "pigeon-proxy";
const NAMESPACE: &str = "pigeon-security";
const REPO: &str = "pigeon";
// CI's values live in `pigeon_ci::pigeon`, which `pigeon-ci-gateway` builds too: they must be
// this Worker's.
const _: () = assert!(
    same(pigeon_ci::pigeon::HOST, HOST) && same(pigeon_ci::pigeon::PROXY_HOST, PROXY_HOST),
    "pigeon_ci::pigeon's hostnames must be this Worker's"
);

/// Whether `a` and `b` are the same text. At build time.
const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
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

/// The public site on the zone's apex, served by this Worker: the home instance's published
/// landing release from its bucket (bound as [`site::HOME_SITE`]), with the architecture pages
/// built in (docs/plans/platform-self-hosting.md, S1). No other Worker owns it.
pub fn site() -> Site {
    Site {
        zone: ZONE.into(),
        worker: WORKER.into(),
        release: Some(site::HOME_SITE.into()),
    }
}

/// The capability Workers bound to this one, by their Service Bindings in wrangler.toml
/// (docs/plans/workers-split.md). Their capabilities join this Worker's own at runtime, from
/// each one's manifest.
pub const BINDINGS: &[Binding] = &[
    Binding {
        binding: "SITES",
        worker: "pigeon-sites",
    },
    Binding {
        binding: "MODELS",
        worker: "pigeon-model-gateway",
    },
    Binding {
        binding: "CI",
        worker: "pigeon-ci-gateway",
    },
    Binding {
        binding: "AI_AGENTS",
        worker: "pigeon-ai-agents",
    },
];

/// The proxies core serves as the proxy Worker's backup, as that Worker builds them: the same
/// repository and push rules, and the same deploy scope (this Worker's script, queues and
/// container classes, and the other platform Workers').
pub fn backup() -> Backup {
    Backup {
        // The git proxy: the source repository for cloud sessions holding a proxy credential.
        // Its push rules are the policy's: session branches are pushed, rewritten and deleted;
        // main moves only by a landing.
        git: GitProxy {
            host: PROXY_HOST.into(),
            namespace: NAMESPACE.into(),
            repo: REPO.into(),
            protected: format!("refs/heads/{BRANCH}"),
            prefix: format!("refs/heads/{SESSION_PREFIX}/"),
        },
        // The Cloudflare API proxy: the calls `wrangler deploy` of this Worker makes, for cloud
        // sessions holding a proxy credential.
        cf: CfProxy {
            host: PROXY_HOST.into(),
            script: WORKER.into(),
            // The other platform Workers, from the map built into this Worker.
            others: map()
                .workers
                .into_iter()
                .filter(|w| w.is_worker() && w.script != WORKER)
                .map(|w| cf_proxy::Other {
                    script: w.script,
                    queues: w.queues,
                    containers: w.containers,
                })
                .collect(),
            // The queues this Worker produces to and its container classes, as wrangler.toml
            // declares them.
            queues: wrangler().producers,
            containers: wrangler().containers.into_iter().map(|c| c.class).collect(),
            land_repo: format!("{NAMESPACE}/{REPO}"),
            // The artifact lanes, whose deploy grants reach only their builds' objects.
            artifacts: map()
                .workers
                .iter()
                .filter_map(|w| w.artifact())
                .copied()
                .collect(),
        },
    }
}

/// The object storage accounts. Empty on purpose: storage stays off until the person declares
/// an account here (see docs/plans/storage-providers.md).
pub fn storage_accounts() -> Vec<storage::Account> {
    Vec::new()
}

/// The instances that take ingest runs (docs/plans/ingest.md). Nothing runs without a person's
/// approval and the keys they bind.
pub fn ingest_instances() -> Vec<ingest::Target> {
    vec![ingest::Target::new("willowgraysen")]
}

/// The capabilities this Worker hosts.
pub fn capabilities() -> Vec<Box<dyn Capability>> {
    vec![
        Box::new(pigeon_capabilities::cloud_scope::Inline),
        Box::new(pigeon_capabilities::digitalocean::DigitalOcean),
        // Every path on the platform checks its callers itself (/mcp and /oauth with OAuth,
        // /auth and the console with passkeys) or is open to all (/health), so no Access is in front of it.
        Box::new(PublicHost {
            zone: ZONE.into(),
            host: HOST.into(),
            worker: WORKER.into(),
            also: Vec::new(),
            tool: "setup_platform",
        }),
        // The proxies' hostname, in public: the proxies check the proxy credential themselves.
        Box::new(PublicHost {
            zone: ZONE.into(),
            host: PROXY_HOST.into(),
            worker: PROXY_WORKER.into(),
            // Core keeps the same proxies as a backup (`backup()`), so switching to it and back
            // is one call each, `setup_proxy_host {worker}`.
            also: vec![WORKER.into()],
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
            // What an app's `session_credential` reaches: the git proxy (docs/proxies.md).
            proxy_remote: Some(format!("https://{PROXY_HOST}/git/{NAMESPACE}/{REPO}.git")),
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
        // App Store Connect (docs/plans/ios.md): slots, a read-only status, and the console's
        // passkey-bound writes that adopt the bundle id and make pigeon's signing identity.
        Box::new(Apple),
        // Read-only object storage; with no accounts declared its tools only explain setup.
        Box::new(Storage {
            accounts: storage_accounts(),
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
            parent: PARENT_TOKEN.into(),
        }),
        // Proxy credentials: what a cloud session holds to reach /git/ and /cf/, issued on
        // the console's Access › Tokens; and the proxies themselves, as a backup to the proxy
        // Worker. They answer only while the proxy hostname is attached to this Worker
        // (`setup_proxy_host {"worker": "pigeon-platform"}`); docs/proxies.md.
        Box::new(Tokens {
            proxy_host: PROXY_HOST.into(),
            backup: Some(backup()),
        }),
        // Passkey approval of the tool calls the policy gates: `approvals_status`, and the calls
        // waiting in the console's Inbox.
        Box::new(Approvals),
        // The person's policy, .pigeon/policy.toml: `policy_check`, as their delegate.
        Box::new(Policy {
            session_prefix: format!("{SESSION_PREFIX}/"),
            main: BRANCH.into(),
        }),
        // Models: served by pigeon-model-gateway (BINDINGS); core hosts only its job objects.
        // CI: served by pigeon-ci-gateway (BINDINGS); core hosts only its scheduler and runners,
        // which the Lander and the proxies read through their bindings.
        // Landing: a session branch tested, moved into main and deployed, through `land`.
        Box::new(Land { settings: land() }),
        // `observability_summary`: Worker metrics, error logs, deploys and cost, read only; and
        // `containers_images`, what the container applications run and the registry holds.
        Box::new(Observability { map: map() }),
        // `call {tool, args}`: any tool above by name, for sessions whose tool list predates it.
        Box::new(Dispatch),
        // The capability Workers: the caller key's setup and asking their manifests again.
        Box::new(Split { bindings: BINDINGS }),
        // The GitHub App: its key and webhook secret, /github/hook, installation tokens.
        Box::new(Github),
        Box::new(Architecture),
        // Versions: what pigeon declares, instance pins and upgrade plans, read-only.
        Box::new(Versioning),
        // Instances: what pigeon runs, here and in linked accounts. Only their shape is code;
        // every value is data a person approves on the console with their passkey.
        // Linked accounts: another Cloudflare account linked to an instance with Cloudflare's
        // OAuth consent, its tokens sealed in the Directory.
        Box::new(Linked { host: HOST.into() }),
        // Ingest: copies a storage account's bucket into an instance's R2 bucket. Latent until
        // the person binds its keys; every run needs a passkey approval.
        Box::new(ingest::Ingest {
            targets: ingest_instances(),
            accounts: storage_accounts(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        // Email (docs/plans/email.md): mail in the instance's data store, read through its data
        // Worker. Off until a record enables `email` and an instance plan makes the store.
        Box::new(pigeon_capabilities::email::Email),
        Box::new(Instances {
            zone: ZONE.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        // Instances › All instances and Platform › Endpoints: the home-only side of instances.
        Box::new(MultiInstance),
        // Passkeys for the sites pigeon-sites serves: its calls only.
        Box::new(SitePasskeys { zone: ZONE.into() }),
        // The console: one page at the root, signed in to once with a passkey.
        Box::new(Console {
            host: HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        // The changelog of main (docs/plans/changelog.md): last of this Worker's own, so a
        // queue batch runs it after the mirror; its event handler never fails the batch.
        Box::new(Changelog {
            namespace: NAMESPACE.into(),
            repo: REPO.into(),
            branch: BRANCH.into(),
            bucket: "pigeon-changelog".into(),
        }),
    ]
}

/// The capabilities the Lander runs tools through: this Worker's, and CI's own, served here
/// rather than by pigeon-ci-gateway, so landing never depends on the CI gateway answering
/// (docs/plans/workers-split.md). The Lander keeps reading the scheduler through CI_SCHEDULER.
pub fn lander_capabilities() -> Vec<Box<dyn Capability>> {
    let mut caps = capabilities();
    caps.push(Box::new(ci::Local {
        ci: ci::Ci { settings: ci() },
    }));
    caps
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
        portal_host: format!("mcp.{ZONE}"),
        // Each CI connector's setup (Buildkite's) runs once a landing has deployed.
        setup_connectors: true,
        map: map(),
    }
}

/// The CI gateway's settings: pigeon's (`pigeon_ci::pigeon`), acting with the parent token,
/// which this Worker holds.
pub fn ci() -> pigeon_ci::Settings {
    pigeon_ci::pigeon::settings(pigeon_ci::Accounts::Parent(PARENT_TOKEN.into()))
}
