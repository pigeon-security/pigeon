//! Typed operations supported by an instance-owned DigitalOcean compute broker.

use serde::{Deserialize, Serialize};

use crate::compute::is_digitalocean_team_id;

pub const PATH: &str = "/ingest/digitalocean/compute";

/// Names one immutable secret generation without carrying the provider token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub team: String,
    pub generation: String,
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

impl Pin {
    pub fn binding(&self, instance: &str) -> Option<String> {
        if !is_digitalocean_team_id(&self.team) || !lower_hex(&self.generation, 24) {
            return None;
        }
        let identity = format!("{instance}:{}", self.team);
        let scope =
            pigeon_core::sign::hmac_sha256(b"pigeon-do-compute-binding-v1", identity.as_bytes());
        let slot = pigeon_core::sign::hex(&scope[..12]).to_ascii_uppercase();
        Some(format!(
            "DO_COMPUTE_{slot}_{}",
            self.generation.to_ascii_uppercase()
        ))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", deny_unknown_fields)]
pub enum Command {
    Check {
        region: String,
    },
    CreateVolume {
        run: String,
        region: String,
        gib: u16,
    },
    FindVolume {
        run: String,
        region: String,
        fence: bool,
    },
    CreateDroplet {
        run: String,
        region: String,
        volume: String,
        script: String,
    },
    FindDroplet {
        run: String,
        region: String,
        fence: bool,
    },
    DropletIp {
        run: String,
        region: String,
        id: u64,
    },
    RemoveDroplet {
        run: String,
        region: String,
        id: u64,
    },
    RemoveVolume {
        run: String,
        region: String,
        id: String,
    },
}

/// A missing provider row after a recorded create is still ambiguous. A winning
/// create fence independently proves that the broker never started the provider create.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lookup<T> {
    Found(T),
    Unresolved,
    NeverCreated,
}

impl<T> From<Option<T>> for Lookup<T> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Unresolved, Self::Found)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub pin: Pin,
    pub command: Command,
}
