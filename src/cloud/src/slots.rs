//! The Scaleway secrets as the Worker sees them: bound or not, set or not. A binding to a
//! Secrets Store secret that does not exist yet may stop a deploy, so the bindings land after
//! the person sets the secrets (docs/plans/cloud-gateway.md, phase 6). Until then every slot
//! reads as [`SlotState::Unbound`].

use std::fmt;

use pigeon_core::secrets_store;
use worker::Env;

use crate::transport::Fut;

/// What the Worker can tell of one secret slot.
#[derive(Clone, PartialEq, Eq)]
pub enum SlotState {
    /// The Worker has no binding for it.
    Unbound,
    /// It is bound, and holds nothing.
    Unset,
    /// It is bound and holds this value.
    Set(String),
}

impl fmt::Debug for SlotState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SlotState::Unbound => "Unbound",
            SlotState::Unset => "Unset",
            SlotState::Set(_) => "Set(<hidden>)",
        })
    }
}

/// Reads the Worker's secret slots. The Worker reads its bindings; the smoke suite stubs this.
pub trait Slots {
    /// The state of slot `name`.
    fn read<'a>(&'a self, name: &'a str) -> Fut<'a, SlotState>;
}

/// The slots as the Worker's environment binds them.
pub struct EnvSlots<'e>(pub &'e Env);

impl Slots for EnvSlots<'_> {
    fn read<'a>(&'a self, name: &'a str) -> Fut<'a, SlotState> {
        Box::pin(async move {
            let Ok(store) = self.0.secret_store(name) else {
                return Ok(SlotState::Unbound);
            };
            Ok(match secrets_store::read(&store).await {
                Ok(Some(value)) if !value.trim().is_empty() => {
                    SlotState::Set(value.trim().to_string())
                }
                _ => SlotState::Unset,
            })
        })
    }
}
