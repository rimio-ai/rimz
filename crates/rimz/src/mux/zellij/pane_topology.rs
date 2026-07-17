//! Zellij pane-topology cache published by the presence plugin through
//! `rimz sidebar wake`.
//!
//! The cache is Zellij's authoritative pane roster: it carries the topology
//! fields RimZ needs for pane projection, the attached-client view when the
//! plugin has sampled it, plus the plugin-retained live foreground command.
//! `terminal_command` remains the pane's spawn command; `pane_command` is the
//! foreground display command.
//! `pane_cwd` carries the plugin's cwd baseline for implicit shell panes;
//! `/proc` remains the fallback for process id, cwd, and resource enrichment.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTopologyCache {
    pub session_name: String,
    pub produced_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer: Option<TopologyWriter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clients: Option<TopologyClients>,
    #[serde(default)]
    pub panes: Vec<PaneTopologyPane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyClients {
    pub human_clients: u32,
    #[serde(default)]
    pub viewed_panes: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyWriter {
    pub plugin_id: u32,
    pub loaded_at_ms: u64,
}

impl TopologyWriter {
    pub fn generation(&self) -> (u64, u32) {
        (self.loaded_at_ms, self.plugin_id)
    }
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
    pub pane_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_command: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_without_focus_resolution_parses() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": []
            }"#,
        )
        .expect("topology parses");

        assert_eq!(cache.focused_pane, None);
        assert_eq!(cache.writer, None);
        assert_eq!(cache.clients, None);
    }

    #[test]
    fn topology_focus_resolution_round_trips() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "focused_pane": 7,
                "panes": []
            }"#,
        )
        .expect("topology parses");

        assert_eq!(cache.focused_pane, Some(7));
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["focused_pane"], 7);
    }

    #[test]
    fn topology_writer_round_trips_and_legacy_payloads_parse() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "writer": { "plugin_id": 9, "loaded_at_ms": 1000 },
                "panes": []
            }"#,
        )
        .expect("topology parses");

        assert_eq!(
            cache.writer,
            Some(TopologyWriter {
                plugin_id: 9,
                loaded_at_ms: 1000
            }),
        );
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["writer"]["plugin_id"], 9);
        assert_eq!(encoded["writer"]["loaded_at_ms"], 1000);
    }

    #[test]
    fn topology_clients_round_trip_and_legacy_payloads_parse() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "clients": { "human_clients": 2, "viewed_panes": [7, 9] },
                "panes": []
            }"#,
        )
        .expect("topology parses");

        assert_eq!(
            cache.clients,
            Some(TopologyClients {
                human_clients: 2,
                viewed_panes: vec![7, 9]
            }),
        );
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["clients"]["human_clients"], 2);
        assert_eq!(
            encoded["clients"]["viewed_panes"],
            serde_json::json!([7, 9])
        );

        let legacy: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": []
            }"#,
        )
        .expect("legacy topology parses");
        assert_eq!(legacy.clients, None);
    }

    #[test]
    fn legacy_focus_resolution_field_is_ignored() {
        let field = ["active", "panes"].join("_");
        let raw = format!(
            r#"{{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "{field}": {{ "0": 7, "1": 11 }},
                "panes": []
            }}"#
        );
        let cache: PaneTopologyCache = serde_json::from_str(&raw).expect("legacy topology parses");

        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert!(encoded.get(field.as_str()).is_none());
    }

    #[test]
    fn pane_cwd_round_trips_and_legacy_payloads_parse() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": [{
                    "id": 7,
                    "tab_position": 0,
                    "pane_command": "zsh",
                    "pane_cwd": "/repo/main"
                }]
            }"#,
        )
        .expect("topology with cwd parses");

        assert_eq!(cache.panes[0].pane_cwd.as_deref(), Some("/repo/main"));
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["panes"][0]["pane_cwd"], "/repo/main");

        let legacy: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": [{ "id": 8, "tab_position": 0, "pane_command": "zsh" }]
            }"#,
        )
        .expect("legacy topology parses");
        assert_eq!(legacy.panes[0].pane_cwd, None);
    }
}
