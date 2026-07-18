//! Zellij pane-topology cache published by the presence plugin through
//! `rimz sidebar wake`.
//!
//! The cache is Zellij's authoritative pane roster: it carries the topology
//! fields RimZ needs for pane projection, the attached-client view when the
//! plugin has sampled it, plus the plugin-retained live foreground command.
//! `terminal_command` remains the pane's spawn command; `pane_command` is the
//! foreground display command.
//! `pane_cwd` follows Zellij's cwd events and `pane_pid` identifies each pane's
//! root process; targeted `/proc` reads supply cwd and resource enrichment.

use serde::{Deserialize, Serialize};

use crate::pane::SIDEBAR_CHROME_TITLE;

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
    #[serde(default)]
    pub views: Vec<TopologyClientView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyClientView {
    pub client_id: u32,
    pub pane_id: TopologyClientPane,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum TopologyClientPane {
    Terminal(u64),
    Plugin(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyWriter {
    pub plugin_id: u32,
    pub loaded_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
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
    pub pane_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_command: Option<String>,
}

impl PaneTopologyPane {
    /// A tiled terminal pane for geometry and sidebar reconcile: not plugin
    /// chrome, not suppressed, and not a floating overlay. Held and exited panes
    /// still occupy layout cells until Zellij closes them.
    pub(super) fn is_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.is_floating
    }

    /// A live terminal pane that belongs in the listing feed. Floating panes are
    /// included because agent discovery follows visible terminals, while
    /// geometry and reconcile use [`Self::is_terminal`] to exclude overlays.
    pub(super) fn is_listed_pane(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.is_held && !self.exited
    }

    /// A live tiled terminal pane. Command fields may still be absent for an
    /// implicit shell; the producer repairs raced-null fields when possible.
    pub(super) fn is_live_terminal(&self) -> bool {
        self.is_listed_pane() && !self.is_floating
    }

    /// The live foreground command last observed by the presence plugin.
    pub(super) fn foreground_command(&self) -> Option<&str> {
        self.pane_command
            .as_deref()
            .filter(|value| !value.is_empty())
    }

    /// The launch command Zellij received when the pane was spawned.
    pub(super) fn spawn_command(&self) -> Option<&str> {
        self.terminal_command
            .as_deref()
            .filter(|command| !command.is_empty())
    }

    /// The display command for pane projection. Title-identified sidebar
    /// chrome wins because Zellij can omit its command fields.
    pub(super) fn display_command(&self) -> Option<String> {
        if !self.is_plugin && self.title.as_deref() == Some(SIDEBAR_CHROME_TITLE) {
            return Some(SIDEBAR_CHROME_TITLE.to_owned());
        }
        self.foreground_command().map(str::to_owned)
    }

    pub(super) fn view_position(&self) -> u64 {
        self.tab_position
    }
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
                loaded_at_ms: 1000,
                build: None,
                config: None,
            }),
        );
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["writer"]["plugin_id"], 9);
        assert_eq!(encoded["writer"]["loaded_at_ms"], 1000);
        assert!(encoded["writer"].get("build").is_none());
        assert!(encoded["writer"].get("config").is_none());

        let mut current = cache;
        let writer = current.writer.as_mut().unwrap();
        writer.build = Some("wasm-build".to_owned());
        writer.config = Some("config-hash".to_owned());
        let encoded = serde_json::to_value(current).expect("current topology serializes");
        assert_eq!(encoded["writer"]["build"], "wasm-build");
        assert_eq!(encoded["writer"]["config"], "config-hash");
    }

    #[test]
    fn topology_clients_round_trip_and_legacy_payloads_parse() {
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "clients": {
                    "human_clients": 2,
                    "viewed_panes": [7],
                    "views": [
                        { "client_id": 3, "pane_id": { "kind": "terminal", "id": 7 } },
                        { "client_id": 4, "pane_id": { "kind": "plugin", "id": 9 } }
                    ]
                },
                "panes": []
            }"#,
        )
        .expect("topology parses");

        assert_eq!(
            cache.clients,
            Some(TopologyClients {
                human_clients: 2,
                viewed_panes: vec![7],
                views: vec![
                    TopologyClientView {
                        client_id: 3,
                        pane_id: TopologyClientPane::Terminal(7),
                    },
                    TopologyClientView {
                        client_id: 4,
                        pane_id: TopologyClientPane::Plugin(9),
                    },
                ],
            }),
        );
        let encoded = serde_json::to_value(&cache).expect("topology serializes");
        assert_eq!(encoded["clients"]["human_clients"], 2);
        assert_eq!(encoded["clients"]["viewed_panes"], serde_json::json!([7]));
        assert_eq!(encoded["clients"]["views"][1]["pane_id"]["kind"], "plugin");

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
    fn topology_pane_pid_round_trips_and_legacy_panes_parse() {
        let current: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": [{ "id": 7, "tab_position": 0, "pane_pid": 707 }]
            }"#,
        )
        .expect("current topology parses");
        assert_eq!(current.panes[0].pane_pid, Some(707));
        let encoded = serde_json::to_value(current).expect("current topology serializes");
        assert_eq!(encoded["panes"][0]["pane_pid"], 707);

        let legacy: PaneTopologyCache = serde_json::from_str(
            r#"{
                "session_name": "rimz-test",
                "produced_at_ms": 42,
                "panes": [{ "id": 7, "tab_position": 0 }]
            }"#,
        )
        .expect("legacy topology parses");
        assert_eq!(legacy.panes[0].pane_pid, None);
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
