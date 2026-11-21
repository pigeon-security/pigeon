//! pigeon's own values: what makes this platform pigeon's rather than generic. Each capability
//! takes its settings from here.

use pigeon_capabilities::Capability;
use pigeon_capabilities::architecture::Architecture;
use pigeon_capabilities::cf_proxy::CfProxy;
use pigeon_capabilities::ci::{self, Ci, ProxyFiles};
use pigeon_capabilities::deploys::Deploys;
use pigeon_capabilities::git_proxy::GitProxy;
use pigeon_capabilities::mirror::{self, Mirror};
use pigeon_capabilities::models::{Dialect, Models, Provider};
use pigeon_capabilities::oauth::OAuth;
use pigeon_capabilities::passkeys::Passkeys;
use pigeon_capabilities::proxy::Tokens;
use pigeon_capabilities::public_host::{PublicHost, Retire};
use pigeon_capabilities::secrets::Secrets;
use pigeon_capabilities::site::{Retired, Site};
use pigeon_capabilities::source::Source;
use pigeon_capabilities::users::Users;

/// This Worker's name, as deployed.
pub const WORKER: &str = "pigeon-platform";

/// The Secrets Store binding holding the parent token.
pub const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

const ZONE: &str = "pigeon.dev";

/// The platform's hostname, in public: its paths check their callers themselves.
pub const HOST: &str = "platform.pigeon.dev";
/// The hostname the proxies (`/git/`, `/cf/`) answer on, and nothing else but `/health`. A
/// hostname of their own, so the proxy credential a cloud environment's network proxy adds
/// for it never reaches `/mcp` or replaces the MCP connector's own token.
pub const PROXY_HOST: &str = "proxy.pigeon.dev";
const NAMESPACE: &str = "pigeon-security";
const REPO: &str = "pigeon";
const BRANCH: &str = "main";

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

/// The capabilities this Worker hosts.
pub fn capabilities() -> Vec<Box<dyn Capability>> {
    vec![
        // Every path on the platform checks its callers itself (/mcp and /oauth with OAuth,
        // /auth, /secrets and /tokens with passkeys) or is open to all (/health), so no Access is in front of it.
        Box::new(PublicHost {
            zone: ZONE.into(),
            host: HOST.into(),
            worker: WORKER.into(),
            retire: Retire {
                apps: ["", "/auth", "/oauth", "/.well-known", "/mcp"]
                    .iter()
                    .map(|path| format!("{HOST}{path}"))
                    .collect(),
                policies: vec!["pigeon-platform".into(), "pigeon-platform-public".into()],
                service_tokens: vec!["pigeon-platform".into(), "pigeon-claude".into()],
                secrets: vec![
                    "PLATFORM_ACCESS_CLIENT_ID".into(),
                    "PLATFORM_ACCESS_CLIENT_SECRET".into(),
                ],
            },
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
        Box::new(Site {
            zone: ZONE.into(),
            worker: "pigeon".into(),
            compatibility_date: "2026-11-01",
            retire: Some(Retired {
                app: "pigeon".into(),
                service_token: "pigeon-site".into(),
                secrets: [
                    "SITE_ACCESS_CLIENT_ID".into(),
                    "SITE_ACCESS_CLIENT_SECRET".into(),
                ],
            }),
        }),
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
        // the passkey-gated /tokens page.
        Box::new(Tokens {
            proxy_host: PROXY_HOST.into(),
            passkeys: Passkeys {
                host: HOST.into(),
                site_name: "pigeon".into(),
            },
        }),
        // The git proxy: the source repository for cloud sessions holding a proxy credential.
        // Sessions push to branches under claude/; main only moves forward.
        Box::new(GitProxy {
            host: PROXY_HOST.into(),
            namespace: NAMESPACE.into(),
            repo: REPO.into(),
            protected: format!("refs/heads/{BRANCH}"),
            prefix: "refs/heads/claude/".into(),
        }),
        // The Cloudflare API proxy: the calls `wrangler deploy` of this Worker makes, for cloud
        // sessions holding a proxy credential.
        Box::new(CfProxy {
            host: PROXY_HOST.into(),
            script: WORKER.into(),
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
            secrets_page: format!("https://{HOST}/secrets"),
        }),
        Box::new(Ci { settings: ci() }),
        Box::new(Deploys),
        Box::new(Architecture),
    ]
}

/// The CI gateway's settings: its connectors in the order jobs prefer them (Containers, then
/// Buildkite, then GitHub Actions, each added in its own phase), the repositories it runs, and
/// how long a queued job waits for its first choice.
pub fn ci() -> ci::Settings {
    ci::Settings {
        connectors: Vec::new(),
        files: Box::new(ProxyFiles),
        repos: vec![format!("{NAMESPACE}/{REPO}")],
        host: HOST.into(),
        proxy_host: PROXY_HOST.into(),
        parent: PARENT_TOKEN.into(),
        wait_limit_ms: 5 * 60_000,
    }
}
