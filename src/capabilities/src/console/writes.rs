//! The console's own writes: removing a passkey, disconnecting an app, inviting a person,
//! approving a landing and setting whether landings need approval. Each is
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
            Write::RemovePasskey | Write::DisconnectApp | Write::LandApprove => {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_is_bound_to_one_write_and_one_target() {
        let a = Write::RemovePasskey.purpose("abc");
        assert_ne!(a, Write::RemovePasskey.purpose("abd"));
        assert_ne!(a, Write::DisconnectApp.purpose("abc"));
        assert_ne!(Write::Invite.purpose("Ada"), Write::Invite.purpose("Ana"));
        assert_ne!(
            Write::LandApprove.purpose("L1"),
            Write::LandApprove.purpose("L2")
        );
        assert_ne!(
            Write::LandApproval.purpose("on"),
            Write::LandApproval.purpose("off")
        );
        assert_ne!(
            Write::LandApprove.purpose("on"),
            Write::LandApproval.purpose("on")
        );
        assert!(!Write::Invite.purpose("Ada").contains("Ada"));
    }

    #[test]
    fn only_the_console_writes_are_named() {
        assert_eq!(Write::named("invite"), Some(Write::Invite));
        assert_eq!(Write::named("land-approve"), Some(Write::LandApprove));
        assert_eq!(Write::named("land-approval"), Some(Write::LandApproval));
        assert_eq!(Write::named("passkey-remove"), Some(Write::RemovePasskey));
        assert_eq!(Write::named("users-remove"), None);
    }

    #[test]
    fn targets_are_ids_or_names() {
        assert_eq!(Write::DisconnectApp.target(" a_B-9 "), Ok("a_B-9".into()));
        assert!(Write::RemovePasskey.target("a/b").is_err());
        assert!(Write::RemovePasskey.target("").is_err());
        assert_eq!(
            Write::Invite.target("  Ada Lovelace "),
            Ok("Ada Lovelace".into())
        );
        assert!(Write::Invite.target("a\nb").is_err());
        assert!(Write::Invite.target(&"x".repeat(65)).is_err());
        assert_eq!(Write::LandApproval.target(" on "), Ok("on".into()));
        assert!(Write::LandApproval.target("yes").is_err());
        assert!(Write::LandApprove.target("L1/../x").is_err());
    }
}
