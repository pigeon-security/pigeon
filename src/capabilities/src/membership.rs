//! Per-instance membership (docs/plans/membership.md). Identity stays platform-level: one
//! sign-in and one set of passkeys per person, in the Directory. What a person may do on an
//! instance is their role there: `owner`, `operator` or `viewer`, kept in the Directory's
//! `members` table by (instance, person), not in the instance's record, so a record's digest
//! never changes with who belongs to it. A platform admin is `owner` everywhere without a row.
//!
//! - **Reads** need `viewer` or above on the instance they are about.
//! - **Writes**, and approving or denying an approval about an instance, need `operator`.
//! - **Managing members** needs `owner` there, or a platform admin; the last owner stays one
//!   unless the platform has an admin.
//! - **Platform-wide** reads and writes (no instance named) are about the home instance, so
//!   they need the same roles there.
//! - **Acting for a person:** a service user or OAuth app has the roles of the person it acts
//!   for ([`crate::users::person_of`]); one acting for no one is not a person and is not asked
//!   here (the platform's own automation), as before.
//!
//! The rules are the pure functions here ([`Access`], [`member_change`], [`write_need`],
//! [`tool_need`]), asked by `dispatch` (every MCP tool and console read), the console's write
//! path at `begin` and again at `finish`, the approvals list, and the instance switcher. Each
//! asks the Directory for the person's [`Access`] now, never a role carried on a caller.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::Context;
use crate::console::write::{Action, Handler, Target, Write};
use crate::users::{User, directory, person_of};

/// A person's role on one instance, weakest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Reads the instance.
    Viewer,
    /// Reads, writes and approves changes to the instance.
    Operator,
    /// As an operator, and manages who belongs to the instance.
    Owner,
}

impl Role {
    /// Every role, weakest first.
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Operator, Role::Owner];

    /// Its name, as the Directory keeps it.
    pub fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Owner => "owner",
        }
    }

    /// The role named `s`.
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.name() == s)
    }
}

/// A role to set, or `none` to remove a person from an instance: what `instance_member_set`
/// and the console's `member-set` take.
pub fn parse_setting(s: &str) -> std::result::Result<Option<Role>, String> {
    match s.trim() {
        "none" => Ok(None),
        t => Role::parse(t)
            .map(Some)
            .ok_or_else(|| "the role is owner, operator, viewer or none".to_string()),
    }
}

/// The members table: one row per (instance, person) with a role.
#[doc(hidden)]
pub const MEMBERS_TABLE: &str = "CREATE TABLE members (instance TEXT NOT NULL, person_id TEXT NOT NULL, role TEXT NOT NULL, PRIMARY KEY (instance, person_id))";

/// The migration that keeps everyone's access on deploy: every person (standard user) who
/// exists is `owner` of every instance in the index, the home instance's old id (R1) under its
/// new one. Run once, when the table is made; an admin narrows it after.
#[doc(hidden)]
pub const MEMBERS_SEED: &str = "INSERT OR IGNORE INTO members (instance, person_id, role) SELECT CASE j.value WHEN 'pigeon-security/platform' THEN 'pigeon.dev' ELSE j.value END, u.id, 'owner' FROM users u, settings s, json_each(s.value) j WHERE u.kind = 'standard' AND s.key = 'instances:index'";

/// The id an instance's membership is kept under: its id, or for an id renamed by R1 the id it
/// was renamed to, whether or not the rename has run.
pub fn key(id: &str) -> &str {
    crate::instances::rename::alias_of(id).unwrap_or(id)
}

/// Why the last owner stays one.
pub const LAST_OWNER: &str = "this is the instance's last owner and the platform has no admin; make another person owner first";

/// What a person may do across instances, read from the Directory now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Access {
    /// The person's id.
    pub person: String,
    /// Whether they are a platform admin: `owner` everywhere.
    pub admin: bool,
    /// Their role on each instance they belong to, by [`key`].
    pub roles: BTreeMap<String, Role>,
    /// The home instance's id, when there is one.
    pub home: Option<String>,
    /// Every instance's id, by [`key`].
    pub instances: Vec<String>,
}

impl Access {
    /// The instance a request naming `instance` is about: that one when it exists, else the
    /// home instance (a platform-wide request, or a new instance being made).
    pub fn concerned(&self, instance: Option<&str>) -> Option<String> {
        match instance.map(key) {
            Some(id) if self.instances.iter().any(|i| i == id) => Some(id.to_string()),
            _ => self.home.clone(),
        }
    }

    /// The person's role on `instance`: `owner` for an admin.
    pub fn role(&self, instance: &str) -> Option<Role> {
        if self.admin {
            Some(Role::Owner)
        } else {
            self.roles.get(key(instance)).copied()
        }
    }

    /// Whether the person has `need` or above on the instance a request naming `instance` is
    /// about ([`Access::concerned`]).
    pub fn check(&self, instance: Option<&str>, need: Role) -> std::result::Result<(), String> {
        if self.admin {
            return Ok(());
        }
        let Some(id) = self.concerned(instance) else {
            return Err("there is no home instance yet, so only an admin can do this".into());
        };
        match self.role(&id) {
            Some(have) if have >= need => Ok(()),
            have => Err(refusal(&id, need, have)),
        }
    }

    /// Of `ids`, the instances the person may see: all for an admin, else those they have a
    /// role on.
    pub fn visible<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        ids.into_iter()
            .filter(|id| self.role(id).is_some())
            .collect()
    }

    /// Whether the person may change who belongs to `instance`: an admin, or its owner.
    pub fn may_manage(&self, instance: &str) -> std::result::Result<(), String> {
        let id = key(instance);
        if !self.instances.iter().any(|i| i == id) {
            return Err(format!("no instance `{instance}`"));
        }
        match self.role(id) {
            Some(Role::Owner) => Ok(()),
            _ => Err(format!(
                "only an owner of {id} or a platform admin can change who belongs to it"
            )),
        }
    }
}

/// Why a person without `need` on `instance` is refused.
pub fn refusal(instance: &str, need: Role, have: Option<Role>) -> String {
    let have = have.map_or("not a member".to_string(), |r| format!("a {}", r.name()));
    format!(
        "this needs {} or above on instance {instance}; you are {have} there",
        need.name()
    )
}

/// Whether `target` may get `role` (or be removed, `None`) on an instance whose owners are
/// `owners`, on a platform with `admins` admins: the last owner stays one unless the platform
/// has an admin, who is owner everywhere. Who asks is checked by [`Access::may_manage`].
pub fn member_change(
    owners: &[String],
    admins: usize,
    target: &str,
    role: Option<Role>,
) -> std::result::Result<(), String> {
    let last = owners.len() <= 1 && owners.iter().any(|o| o == target);
    if last && role != Some(Role::Owner) && admins == 0 {
        return Err(LAST_OWNER.into());
    }
    Ok(())
}

/// What a console write needs of its person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Need {
    /// Its capability asks its own rules: the `users` writes (one's own passkey, admin-only
    /// people management, members by an owner).
    Own,
    /// Approving or denying: `operator` on the instance the approval is about.
    Approval,
    /// This role on the instance named (`None`: the home instance).
    On(Option<String>, Role),
}

/// What `action` on `target` needs: `operator` on the instance it targets (an instance, or a
/// site's instance), or on the home instance for a platform-wide write.
pub fn write_need(action: &Action, target: &Value) -> Need {
    if action.handler == Handler::Capability("users") {
        return Need::Own;
    }
    if matches!(action.id, "approval-approve" | "approval-deny") {
        return Need::Approval;
    }
    let instance = match action.target {
        Target::Instance => target.as_str(),
        Target::Site | Target::DigitalOceanTeam => target["instance"].as_str(),
        _ => None,
    };
    Need::On(instance.map(str::to_string), Role::Operator)
}

/// The capabilities whose tools ask their own rules of the person: people, passkeys, apps,
/// approvals and `call` (which dispatches again).
pub const OWN_RULES: &[&str] = &["users", "passkeys", "oauth", "approvals", "dispatch"];

/// Tools that ask membership themselves, or are about the person's own device.
pub const OWN_TOOLS: &[&str] = &[
    "instances_list",
    "console_manifest",
    "apns_register",
    "apns_unregister",
];

/// The role a person needs to run `tool` of `capability` with `args`, on the instance it is
/// about: `viewer` for a console read with only its read arguments
/// ([`crate::console::ops::allowed`]), `operator` for anything else; `None` when the tool asks
/// its own rules.
pub fn tool_need(capability: &str, tool: &str, args: &Value) -> Option<Role> {
    if OWN_RULES.contains(&capability) || OWN_TOOLS.contains(&tool) {
        return None;
    }
    Some(
        if matches!(
            tool,
            crate::digitalocean::VERIFY | crate::digitalocean::spaces::VERIFY
        ) {
            Role::Operator
        } else if crate::console::ops::allowed(tool, args) {
            Role::Viewer
        } else {
            Role::Operator
        },
    )
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The access of the person `person_id`, read from the Directory now.
pub async fn access(env: &Env, person_id: &str) -> Result<Access> {
    let v = directory(env, "members.access", json!({ "person_id": person_id })).await?;
    Ok(serde_json::from_value(v)?)
}

/// Refuses unless `person` has `need` on the instance a request naming `instance` is about.
pub async fn require(env: &Env, person: &User, instance: Option<&str>, need: Role) -> Result<()> {
    access(env, &person.id)
        .await?
        .check(instance, need)
        .map_err(bad)
}

/// Refuses a console write its person may not make ([`write_need`]); asked at `begin` and
/// again at `finish`.
pub async fn check_write(env: &Env, w: &Write<'_>) -> Result<()> {
    match write_need(w.action, &w.target) {
        Need::Own => Ok(()),
        Need::On(instance, role) => require(env, w.person, instance.as_deref(), role).await,
        Need::Approval => {
            let r: Option<pigeon_core::approval::Record> = serde_json::from_value(
                directory(env, "approval.get", json!({ "id": w.text() })).await?,
            )?;
            // A missing approval is refused by the write itself.
            let Some(r) = r else { return Ok(()) };
            let instance = crate::instances::manifest::approval_instance(&r);
            require(env, w.person, instance, Role::Operator).await
        }
    }
}

/// Refuses `tool` (of `capability`) for `cx`'s caller when they act as a person without the
/// role it needs ([`tool_need`]) on the instance it is about: the instance endpoint's, else the
/// `instance` argument's, else home.
pub async fn check_tool(
    cx: &Context<'_>,
    capability: &str,
    tool: &str,
    args: &Value,
) -> Result<()> {
    let Some(person) = cx.caller.and_then(person_of) else {
        return Ok(());
    };
    let Some(need) = tool_need(capability, tool, args) else {
        return Ok(());
    };
    let instance = cx
        .instance
        .map(|s| s.instance())
        .or_else(|| args["instance"].as_str().filter(|s| !s.is_empty()));
    require(cx.env, person, instance, need).await
}

/// Makes `person` `role` on `instance` (the home instance when `None`) unless they have a role
/// there already: the creator of a new instance, an invited person on home.
pub async fn grant(env: &Env, person: &str, instance: Option<&str>, role: Role) -> Result<()> {
    directory(
        env,
        "members.grant",
        json!({ "person_id": person, "instance": instance, "role": role.name() }),
    )
    .await?;
    Ok(())
}
