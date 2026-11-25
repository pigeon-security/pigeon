//! The contract between pigeon's core Worker (`pigeon-platform`) and its capability Workers
//! (`pigeon-sites`, `pigeon-model-gateway`, `pigeon-ci-gateway`, `pigeon-proxy`), spoken over
//! Service Bindings as HTTP fetch with JSON bodies. See docs/plans/workers-split.md.
//!
//! Core is the front door: it authenticates the caller and runs the policy and approval gate
//! before it forwards anything to a capability Worker. Capability Workers have no public
//! surface; they answer only core, and trust the caller context core signs ([`caller`]).
//!
//! Fields are only ever added. Optional fields default and unknown fields are ignored, so a peer
//! one version behind or ahead still parses. Removing or changing a field needs a [`CONTRACT`]
//! bump.

pub mod ask;
pub mod caller;
pub mod serve;
pub mod wire;

pub use ask::{
    WORKER_HEADER, ask, ask_with_context, identify, identity, manifest, manifest_with_context,
};
pub use caller::{
    HEADER, MAX_AGE_MS, Refused, Signed, WORKER_CONTEXT_VERSION, WorkerContext, sign,
    sign_with_context, verify, verify_for,
};
pub use serve::{Service, serve};
pub use wire::{
    Answer, ApprovalRequest, CALLERLESS_OPS, CONTRACT, CORE_OPS, CallRequest, CapabilityManifest,
    Credential, CredentialRequest, EventsRequest, Failure, FailureKind, GrantRequest, Manifest,
    OPS, SecretOp, SecretRequest, SectionSpec, SitesScopeRequest, SlotSpec, ToolSpec,
    VerifyRequest, WorkerManifest, accepts, wants,
};

/// The current time in milliseconds since the epoch; the one place that reads the clock.
fn now_ms() -> i64 {
    i64::try_from(worker::Date::now().as_millis()).unwrap_or(i64::MAX)
}
