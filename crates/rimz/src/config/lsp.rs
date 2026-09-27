//! Shared language-server declarations and machine memory policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct LspConfig {
    pub reserve_percent: u8,
    pub reserve_min: String,
    pub kill_floor_percent: u8,
    pub idle_timeout: String,
    pub servers: BTreeMap<String, LspServerConfig>,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            reserve_percent: 10,
            reserve_min: "8G".to_owned(),
            kill_floor_percent: 5,
            idle_timeout: "10m".to_owned(),
            servers: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct LspServerConfig {
    #[serde(default)]
    pub kind: Option<LspServerKind>,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub init_options: Option<serde_json::Value>,
    #[serde(default)]
    pub editor_check_on_save: bool,
    #[serde(default)]
    pub policy: LspPolicy,
    #[serde(default = "default_wait_timeout")]
    pub wait_timeout: String,
    #[serde(default = "default_memory_estimate")]
    pub memory_estimate: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LspServerKind {
    RustAnalyzer,
    Pyright,
    Basedpyright,
    Ruff,
    Generic,
}

impl LspServerConfig {
    pub(crate) fn resolved_kind(&self) -> LspServerKind {
        self.kind.unwrap_or_else(|| {
            self.command
                .iter()
                .find_map(
                    |token| match std::path::Path::new(token).file_name()?.to_str()? {
                        "rust-analyzer" => Some(LspServerKind::RustAnalyzer),
                        "pyright-langserver" => Some(LspServerKind::Pyright),
                        "basedpyright-langserver" => Some(LspServerKind::Basedpyright),
                        "ruff" => Some(LspServerKind::Ruff),
                        _ => None,
                    },
                )
                .unwrap_or(LspServerKind::Generic)
        })
    }
}

impl std::fmt::Display for LspServerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RustAnalyzer => "rust-analyzer",
            Self::Pyright => "pyright",
            Self::Basedpyright => "basedpyright",
            Self::Ruff => "ruff",
            Self::Generic => "generic",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn server_kind_resolves_explicit_then_command_tokens() {
        for (command, expected) in [
            (vec!["rust-analyzer"], LspServerKind::RustAnalyzer),
            (
                vec!["rustup", "run", "stable", "rust-analyzer"],
                LspServerKind::RustAnalyzer,
            ),
            (vec!["/opt/bin/ruff", "server"], LspServerKind::Ruff),
            (
                vec!["uv", "tool", "run", "ruff", "server"],
                LspServerKind::Ruff,
            ),
            (
                vec!["npx", "pyright-langserver", "--stdio"],
                LspServerKind::Pyright,
            ),
            (
                vec!["basedpyright-langserver", "--stdio"],
                LspServerKind::Basedpyright,
            ),
            (vec!["pylsp"], LspServerKind::Generic),
            (vec!["ruff", "rust-analyzer"], LspServerKind::Ruff),
        ] {
            let mut config: LspServerConfig = serde_json::from_value(
                json!({"command":command,"extensions":["py"],"root-markers":["pyproject.toml"]}),
            )
            .unwrap();
            assert_eq!(config.resolved_kind(), expected, "{command:?}");
            config.kind = Some(LspServerKind::Pyright);
            assert_eq!(config.resolved_kind(), LspServerKind::Pyright);
        }
    }

    #[test]
    fn unknown_server_kind_is_refused() {
        assert!(serde_json::from_value::<LspServerConfig>(json!({"kind":"typo","command":["ruff"],"extensions":["py"],"root-markers":["pyproject.toml"]})).is_err());
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LspPolicy {
    #[default]
    Optional,
    Required,
}

fn default_wait_timeout() -> String {
    "10m".to_owned()
}

fn default_memory_estimate() -> String {
    "8G".to_owned()
}
