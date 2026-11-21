//! The console's own writes: removing a passkey, disconnecting an app, inviting a person,
//! approving a landing, setting whether landings need approval, and approving or denying a
//! single action (a gated tool call). Each is
//! approved with a passkey of the signed-in person whose challenge purpose names the write and
//! its target, so an approval covers that one change and no other. Pure: what a write is, what
//! it may target, and its purpose.

use pigeon_core::oauth::hash;

/// A write the console makes, each approved with its own passkey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    /// Removes a passkey, by its id.
    RemovePasskey,
    /// Revokes an app's tokens and removes its link, by its user id.
    DisconnectApp,
    /// Makes a person and a single-use link to enrol their passkey, by their name.
    Invite,
    /// Approves a landing waiting for a person, by its id.
    LandApprove,
    /// Sets whether every landing waits for approval: `on` or `off`.
    LandApproval,
    /// Approves a single action, by its approval's id. Its purpose names the action's digest
    /// too, which only the Directory knows: see [`Write::bound_to_digest`].
    ApprovalApprove,
    /// Denies a single action, by its approval's id.
    ApprovalDeny,
}

impl Write {
    /// The write a URL's action names: `passkey-remove`, `app-disconnect` or `invite`.
    pub fn named(action: &str) -> Option<Write> {
        match action {
            "passkey-remove" => Some(Write::RemovePasskey),
            "app-disconnect" => Some(Write::DisconnectApp),
            "invite" => Some(Write::Invite),
            "land-approve" => Some(Write::LandApprove),
            "land-approval" => Some(Write::LandApproval),
            "approval-approve" => Some(Write::ApprovalApprove),
            "approval-deny" => Some(Write::ApprovalDeny),
            _ => None,
        }
    }

    /// `target` as this write takes it, trimmed: an id of 1 to 200 base64url characters, or
    /// for an invite a name of 1 to 64 characters with no control characters.
    pub fn target(self, target: &str) -> Result<String, String> {
        let t = target.trim();
        match self {
            Write::Invite => {
                let n = t.chars().count();
                if n == 0 || n > 64 || t.chars().any(char::is_control) {
                    Err("a name is 1 to 64 characters, none of them control characters".into())
                } else {
                    Ok(t.to_string())
                }
            }
            Write::LandApproval => match t {
                "on" | "off" => Ok(t.to_string()),
                _ => Err("on or off".into()),
            },
            Write::RemovePasskey
            | Write::DisconnectApp
            | Write::LandApprove
            | Write::ApprovalApprove
            | Write::ApprovalDeny => {
                let ok = (1..=200).contains(&t.len())
                    && t.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
                if ok {
                    Ok(t.to_string())
                } else {
                    Err("not an id".into())
                }
            }
        }
    }

    /// The challenge purpose that binds an approval to this write of `target`. A name is hashed,
    /// so the purpose holds no free text.
    pub fn purpose(self, target: &str) -> String {
        match self {
            Write::RemovePasskey => format!("console:passkey-remove:{target}"),
            Write::DisconnectApp => format!("console:app-disconnect:{target}"),
            Write::Invite => format!("console:invite:{}", hash(target)),
            Write::LandApprove => format!("console:land-approve:{target}"),
            Write::LandApproval => format!("console:land-approval:{target}"),
            Write::ApprovalApprove => format!("console:approval:{target}"),
            Write::ApprovalDeny => format!("console:approval-deny:{target}"),
        }
    }

    /// Whether its purpose is the approval's, bound to the approved action's digest
    /// (`pigeon_core::approval::purpose`), rather than [`Write::purpose`].
    pub fn bound_to_digest(self) -> bool {
        self == Write::ApprovalApprove
    }
}
