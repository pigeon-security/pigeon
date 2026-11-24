//! Public gateway declarations shared by the models Worker and platform cost reporting.

use crate::Gateway;
use pigeon_core::ai_gateway::GatewayKind;

/// Anthropic on a stored key. Also the default for older model jobs.
pub const BYOK_GATEWAY: &str = "pigeon-anthropic";
/// Cloudflare Unified Billing, with no stored provider key.
pub const CREDITS_GATEWAY: &str = "pigeon-cloudflare";
/// OpenAI on a stored key, even if not provisioned yet.
pub const OPENAI_GATEWAY: &str = "pigeon-openai";

/// A platform gateway and its public cost display metadata.
pub struct Declaration {
    /// Cloudflare gateway ID.
    pub id: &'static str,
    /// Who pays for the calls.
    pub kind: GatewayKind,
    /// Slug of the configured payer, or `other` for the retired gateway.
    pub provider: &'static str,
    /// Display name for cost reporting.
    pub label: &'static str,
}

impl Declaration {
    /// The gateway to use for routing and log reads.
    pub fn gateway(&self) -> Gateway {
        Gateway {
            id: self.id.into(),
            kind: self.kind,
        }
    }

    /// The billing method in the cost response.
    pub fn billing(&self) -> &'static str {
        match self.kind {
            GatewayKind::Credits => "unified",
            GatewayKind::Byok | GatewayKind::Retired => "byok",
        }
    }
}

/// Every platform gateway, including the retired BYOK gateway used for history only.
pub const GATEWAYS: &[Declaration] = &[
    Declaration {
        id: BYOK_GATEWAY,
        kind: GatewayKind::Byok,
        provider: "anthropic",
        label: "Anthropic BYOK",
    },
    Declaration {
        id: CREDITS_GATEWAY,
        kind: GatewayKind::Credits,
        provider: "cloudflare",
        label: "Cloudflare Unified Billing",
    },
    Declaration {
        id: OPENAI_GATEWAY,
        kind: GatewayKind::Byok,
        provider: "openai",
        label: "OpenAI BYOK",
    },
    Declaration {
        id: "pigeon",
        kind: GatewayKind::Retired,
        provider: "other",
        label: "Legacy BYOK",
    },
];
