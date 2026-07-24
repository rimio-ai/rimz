use serde::{Deserialize, Serialize};

/// Browser-access preferences for the machine-wide web daemon.
///
/// These are per-machine preferences: the backend is a closed enum naming the
/// daemon binary, the interface and port select the writable and broadcast
/// listeners, trusted-header authentication, user allowlists, and proxy CIDRs
/// constrain writable access, base URLs can name private hostnames or
/// reverse-proxy paths, font sources name read-only paths or HTTPS URLs, and no
/// field executes a configured command. The section therefore stays outside
/// the project trust hash.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebPrefs {
    pub backend: WebBackend,
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
    pub auth_users: Vec<String>,
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
            backend: WebBackend::default(),
            enabled: true,
            port: 8200,
            share_port: 8201,
            interface: "127.0.0.1".to_owned(),
            base_url: None,
            share_base_url: None,
            auth_header: None,
            auth_users: Vec::new(),
            trusted_proxies: Vec::new(),
            font: "JetBrainsMono Nerd Font Mono".to_owned(),
            font_source: None,
            style_client: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebBackend {
    #[default]
    Ttyd,
    Gotty,
}
