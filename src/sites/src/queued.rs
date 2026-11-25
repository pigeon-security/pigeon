//! The private receipt for a push-started publish sent by core.
//!
//! Only [`Receipt::read`] makes a [`Receipt`], so only the private `site_deploy_queued` call can
//! ask the deploy workflow to enforce a queued publishing binding. The public `site_deploy` tool
//! refuses the old scheduled-only `publishing_binding` argument ([`refuse_public_binding`]).

use pigeon_contract::Signed;
use serde_json::Value;

pub const TOOL: &str = "site_deploy_queued";

/// The argument older core schedulers sent through the public `site_deploy` tool.
pub const PUBLIC_BINDING_ARG: &str = "publishing_binding";

/// A checked queued publish: the signed instance, its exact commit and the approved record digest.
pub struct Receipt<'a> {
    instance: &'a str,
    revision: &'a str,
    binding: &'a str,
}

/// The public `site_deploy` tool takes no queued binding: scheduled publishes arrive only through
/// the private receipt. Refusing, rather than ignoring, keeps an older scheduler from publishing
/// without the digest and exact-commit checks, and makes such a call visible.
pub fn refuse_public_binding(args: &Value) -> Result<(), &'static str> {
    if args.get(PUBLIC_BINDING_ARG).is_some() {
        return Err(
            "`publishing_binding` is not accepted by site_deploy; queued publishes use the private site_deploy_queued receipt",
        );
    }
    Ok(())
}

impl<'a> Receipt<'a> {
    pub fn read(caller: &Signed, args: &'a Value) -> Result<Self, &'static str> {
        let instance = args["instance"]
            .as_str()
            .ok_or("expected a queued instance")?;
        caller
            .require_instance(instance)
            .map_err(|_| "queued publishing needs matching signed instance context")?;
        if caller.caller["user"]["kind"] != "standard"
            || caller.caller["user"]["id"]
                .as_str()
                .is_none_or(str::is_empty)
            || !caller.caller["acting_for"].is_null()
        {
            return Err("queued publishing needs the linked account owner's caller");
        }
        let allowed = ["instance", "rev", "publishing_binding"];
        if !args
            .as_object()
            .is_some_and(|fields| fields.keys().all(|field| allowed.contains(&field.as_str())))
        {
            return Err("unknown arguments in queued publish");
        }
        let revision = args["rev"]
            .as_str()
            .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or("expected an exact queued commit")?;
        let binding = args["publishing_binding"]
            .as_str()
            .filter(|value| {
                value.len() == 43
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
            .ok_or("expected the approved publishing record digest")?;
        Ok(Self {
            instance,
            revision,
            binding,
        })
    }

    /// The signed instance the publish is for.
    pub fn instance(&self) -> &'a str {
        self.instance
    }

    /// The exact commit the push queued.
    pub fn revision(&self) -> &'a str {
        self.revision
    }

    /// The current approved record must still be the one the push was bound to.
    pub fn check_record(&self, record: &Value) -> Result<(), &'static str> {
        if pigeon_core::oauth::hash(&record.to_string()) != self.binding {
            return Err("the queued push no longer matches the approved publishing record");
        }
        Ok(())
    }

    /// GitHub's archive must be the queued commit, not whatever a ref now names.
    pub fn check_commit(&self, commit: &str) -> Result<(), &'static str> {
        if !commit.eq_ignore_ascii_case(self.revision) {
            return Err("GitHub's archive does not match the queued commit");
        }
        Ok(())
    }
}
