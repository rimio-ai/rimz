use serde::{Deserialize, Serialize};

/// Browser-access preferences for host-local room access.
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
    pub tmux: TmuxWebPrefs,
}

impl Default for WebPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            zellij: ZellijWebPrefs::default(),
            tmux: TmuxWebPrefs::default(),
        }
    }
}

/// tmux browser-terminal preferences.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TmuxWebPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub auto_start: bool,
}

impl Default for TmuxWebPrefs {
    fn default() -> Self {
        Self {
            base_url: None,
            auto_start: true,
        }
    }
}

/// Zellij web server and browser-terminal preferences.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ZellijWebPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub auto_start: bool,
    pub font: String,
    pub style_client: bool,
}

impl Default for ZellijWebPrefs {
    fn default() -> Self {
        Self {
            base_url: None,
            auto_start: true,
            font: "JetBrainsMono Nerd Font Mono".to_owned(),
            style_client: true,
        }
    }
}
