//! Who a proxied request is for, shared by core (the Lander's deploy grants) and the proxy.

use serde::{Deserialize, Serialize};

/// How many characters of a credential's hash make its id.
pub const ID_LEN: usize = 12;

/// Who holds the proxy credential a request carries.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProxyCaller {
    /// The credential's id.
    pub id: String,
    pub label: String,
    /// The person who issued it.
    pub person: String,
    /// The one Worker script it may deploy, when it is a landing's deploy job; `None` for a
    /// proxy credential, which may deploy any platform Worker.
    #[serde(default)]
    pub script: Option<String>,
}
