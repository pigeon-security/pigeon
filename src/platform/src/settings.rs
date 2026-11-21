//! pigeon's own values: what makes this platform pigeon's rather than generic. Each capability
//! takes its settings from here.

use pigeon_capabilities::Capability;
use pigeon_capabilities::architecture::Architecture;
use pigeon_capabilities::deploys::Deploys;
use pigeon_capabilities::mirror::{self, Mirror};
use pigeon_capabilities::models::{Dialect, Models, Provider};
use pigeon_capabilities::oauth::OAuth;
use pigeon_capabilities::passkeys::Passkeys;
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
const NAMESPACE: &str = "pigeon-security";
const REPO: &str = "pigeon";
const BRANCH: &str = "main";

/// The capabilities this Worker hosts.
pub fn capabilities() -> Vec<Box<dyn Capability>> {
    vec![
        // Every path on the platform checks its callers itself (/mcp and /oauth with OAuth,
        // /auth with passkeys) or is open to all (/health), so no Access is in front of it.
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
                branches: vec![BRANCH.into()],
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
        Box::new(Deploys),
        Box::new(Architecture),
    ]
}
