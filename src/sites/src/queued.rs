//! The private receipt for a push-started publish sent by core.

use pigeon_contract::Signed;
use serde_json::Value;

pub const TOOL: &str = "site_deploy_queued";

pub struct Receipt<'a> {
    pub instance: &'a str,
    pub revision: &'a str,
    pub binding: &'a str,
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
}
