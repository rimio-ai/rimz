use serde::{Deserialize, Serialize};

/// Browser-access preferences for host-local Zellij web access.
///
/// These are per-machine preferences: base URLs can name private hostnames,
/// loopback tunnels, or reverse-proxy paths, and no field executes a command;
/// `enabled` gates auto-granting Rimz's presence-plugin permissions and
/// enabling browser sharing. The section therefore stays outside the project
/// trust hash.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebPrefs {
    pub enabled: bool,
    pub zellij: ZellijWebPrefs,
}

impl Default for WebPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            zellij: ZellijWebPrefs::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ZellijWebPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub auto_start: bool,
}

impl Default for ZellijWebPrefs {
    fn default() -> Self {
        Self {
            base_url: None,
            auto_start: true,
        }
    }
}
