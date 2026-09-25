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
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub init_options: Option<serde_json::Value>,
    #[serde(default)]
    pub policy: LspPolicy,
    #[serde(default = "default_wait_timeout")]
    pub wait_timeout: String,
    #[serde(default = "default_memory_estimate")]
    pub memory_estimate: String,
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
