use serde::{Deserialize, Serialize};

/// Browser-access preferences for the machine-wide ttyd daemon.
///
/// These are per-machine preferences: the port selects the loopback listener,
/// base URLs can name private hostnames or reverse-proxy paths, font sources
/// name read-only paths or HTTPS URLs, and no field executes a command. The
/// section therefore stays outside the project trust hash.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebPrefs {
    pub enabled: bool,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub font: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_source: Option<String>,
    pub style_client: bool,
}

impl Default for WebPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8200,
            base_url: None,
            font: "JetBrainsMono Nerd Font Mono".to_owned(),
            font_source: None,
            style_client: true,
        }
    }
}
