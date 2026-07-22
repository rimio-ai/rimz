use serde::{Deserialize, Serialize};

/// Browser-access preferences for the machine-wide ttyd daemon.
///
/// These are per-machine preferences: the interface and port select the
/// writable and broadcast listeners, trusted-header authentication and proxy
/// CIDRs constrain writable access, base URLs can name private hostnames or
/// reverse-proxy paths, font sources name read-only paths or HTTPS URLs, and no
/// field executes a command. The section therefore stays outside the project
/// trust hash.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebPrefs {
    pub enabled: bool,
    pub port: u16,
    pub share_port: u16,
    pub interface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_proxies: Vec<String>,
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
            share_port: 8201,
            interface: "127.0.0.1".to_owned(),
            base_url: None,
            share_base_url: None,
            auth_header: None,
            trusted_proxies: Vec::new(),
            font: "JetBrainsMono Nerd Font Mono".to_owned(),
            font_source: None,
            style_client: true,
        }
    }
}
