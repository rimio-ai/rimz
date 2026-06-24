//! Zellij pane-topology cache published by the presence plugin through
//! `rimz sidebar wake`.
//!
//! The cache is a latency hint for Zellij's expensive JSON `list-panes` path:
//! it carries the topology fields Rimz needs for pane projection, plus the
//! plugin-retained live foreground command. `terminal_command` remains the
//! pane's spawn command; `pane_command` is the foreground display command.
//! Process id, cwd, and resource enrichment still come from the existing
//! `/proc` lanes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTopologyCache {
    pub session_name: String,
    pub produced_at_ms: u64,
    /// Presence-plugin resolved active panes by tab position. Raw per-pane
    /// focus marks stay on `panes`; this field carries the authoritative,
    /// transition-derived single active pane when the plugin can resolve one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_panes: BTreeMap<u64, u64>,
    #[serde(default)]
    pub panes: Vec<PaneTopologyPane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTopologyPane {
    pub id: u64,
    #[serde(default)]
    pub is_plugin: bool,
    #[serde(default)]
    pub is_held: bool,
    #[serde(default)]
    pub exited: bool,
    #[serde(default)]
    pub is_suppressed: bool,
    #[serde(default)]
    pub is_floating: bool,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(alias = "tab_id")]
    pub tab_position: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_columns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_x: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_command: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_topology_without_active_panes_parses() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": []
            }"#,
        )
        .expect("legacy topology parses");

        assert!(cache.active_panes.is_empty());
    }

    #[test]
    fn active_panes_round_trips_when_present() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "active_panes": { "0": 7, "1": 11 },
                "panes": []
            }"#,
        )
        .expect("topology with active panes parses");

        assert_eq!(cache.active_panes.get(&0), Some(&7));
        assert_eq!(cache.active_panes.get(&1), Some(&11));
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["active_panes"]["0"], 7);
        assert_eq!(encoded["active_panes"]["1"], 11);
    }
}
