//! pigeon's own values: what makes this platform pigeon's rather than generic. Each capability
//! takes its settings from here.

use pigeon_capabilities::Capability;
use pigeon_capabilities::architecture::Architecture;
use pigeon_capabilities::deploys::Deploys;
use pigeon_capabilities::mirror::{self, Mirror};
use pigeon_capabilities::protected_host::ProtectedHost;
use pigeon_capabilities::site::{Retired, Site};
use pigeon_capabilities::source::Source;

/// This Worker's name, as deployed.
pub const WORKER: &str = "pigeon-platform";

/// The Secrets Store binding holding the parent token.
pub const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

const ZONE: &str = "pigeon.dev";
const NAMESPACE: &str = "pigeon-security";
const REPO: &str = "pigeon";
const BRANCH: &str = "main";

/// The capabilities this Worker hosts.
pub fn capabilities() -> Vec<Box<dyn Capability>> {
    vec![
        Box::new(ProtectedHost {
            zone: ZONE.into(),
            host: "platform.pigeon.dev".into(),
            worker: WORKER.into(),
            app: "pigeon-platform".into(),
            service_token: "pigeon-platform".into(),
            client_id_secret: "PLATFORM_ACCESS_CLIENT_ID".into(),
            client_secret_secret: "PLATFORM_ACCESS_CLIENT_SECRET".into(),
            // Claude sessions, which send their token in one header: Access takes only
            // Authorization for it.
            guests: vec!["pigeon-claude".into()],
            token_header: Some("Authorization".into()),
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
        Box::new(Deploys),
        Box::new(Architecture),
    ]
}
