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

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::ids::{MuxName, PaneId};
use crate::mux::{ClientPaneView, ClientView, MuxClientId, PaneListing};
use crate::pane::{PaneRef, SIDEBAR_CHROME_TITLE};

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

impl PaneTopologyCache {
    pub(crate) fn projected_session_focus(&self) -> Option<PaneId> {
        let clients = self.clients.clone().map(TopologyClients::into_client_view);
        project_session_focus(&self.panes, clients.as_ref(), self.focused_pane)
    }

    pub(super) fn into_pane_listing(self, session_name: String) -> PaneListing {
        let Self {
            produced_at_ms,
            focused_pane,
            clients,
            panes,
            ..
        } = self;
        let client_view = clients.map(TopologyClients::into_client_view);
        let session_focus = project_session_focus(&panes, client_view.as_ref(), focused_pane);
        PaneListing {
            panes: panes
                .into_iter()
                .filter_map(|mut pane| {
                    if !pane.is_listed_pane() {
                        return None;
                    }
                    let command = pane.display_command();
                    Some(PaneRef {
                        pane_id: PaneId::from(pane.native_id()),
                        session_name: session_name.clone(),
                        view_id: Some(format!("tab_{}", pane.view_position())),
                        view_kind: Some(crate::mux::view_kind(MuxName::Zellij)),
                        view_name: pane.tab_name.take(),
                        title: pane.title.take(),
                        is_floating: pane.is_floating,
                        pane_pid: pane.pane_pid,
                        pane_process_start: None,
                        hosted_agent_kind: None,
                        hosted_agent_process_start: None,
                        command,
                        foreground_cmdline: None,
                        spawn_command: pane.spawn_command().map(str::to_owned),
                        cwd: pane.pane_cwd.take(),
                        resumed_session_id: None,
                        elevated_agent: None,
                        first_seen_at_ms: None,
                    })
                })
                .collect(),
            observed_at_ms: produced_at_ms,
            session_focus,
            client_view,
        }
    }
}

fn project_session_focus(
    panes: &[PaneTopologyPane],
    clients: Option<&ClientView>,
    legacy_focus: Option<u64>,
) -> Option<PaneId> {
    let live = panes
        .iter()
        .filter(|pane| pane.is_listed_pane())
        .map(|pane| PaneId::from(pane.native_id()))
        .collect::<HashSet<_>>();
    match clients {
        Some(clients) => {
            ClientView::unique_live_focus(&clients.clients, &clients.viewed_panes, &live)
        }
        None => legacy_focus
            .map(|id| PaneId::from(ZellijPaneId::Terminal(id)))
            .filter(|pane| live.contains(pane)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyClients {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_clients: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_panes: Option<Vec<u64>>,
    #[serde(default)]
    pub views: Vec<TopologyClientView>,
}

impl TopologyClients {
    fn into_client_view(self) -> ClientView {
        let human_clients = self.human_clients.map_or_else(
            || {
                self.views
                    .iter()
                    .map(|view| view.client_id)
                    .collect::<BTreeSet<_>>()
                    .len()
            },
            |count| count as usize,
        );
        let viewed_panes = self.viewed_panes.unwrap_or_else(|| {
            self.views
                .iter()
                .filter_map(|view| view.pane_id.terminal_id())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        });
        ClientView {
            clients: self
                .views
                .into_iter()
                .map(|view| ClientPaneView {
                    client_id: MuxClientId::Zellij(view.client_id),
                    pane_id: PaneId::from(view.pane_id),
                })
                .collect(),
            presence: crate::mux::ClientPresence {
                human_clients,
                last_input_ms: None,
            },
            viewed_panes: viewed_panes
                .into_iter()
                .map(|id| PaneId::from(ZellijPaneId::Terminal(id)))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyClientView {
    pub client_id: u32,
    pub pane_id: ZellijPaneId,
}

/// One native Zellij pane identity. Terminal and plugin ordinals occupy
/// separate namespaces even when their numeric values match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ZellijPaneId {
    Terminal(u64),
    Plugin(u64),
}

impl ZellijPaneId {
    pub const fn terminal_id(self) -> Option<u64> {
        match self {
            Self::Terminal(id) => Some(id),
            Self::Plugin(_) => None,
        }
    }

    /// Native target syntax accepted by Zellij actions.
    pub fn action_target(self) -> String {
        match self {
            Self::Terminal(id) => format!("terminal_{id}"),
            Self::Plugin(id) => format!("plugin_{id}"),
        }
    }
}

impl From<ZellijPaneId> for PaneId {
    fn from(value: ZellijPaneId) -> Self {
        Self::from_parts(MuxName::Zellij, value.action_target())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("pane `{0}` is not an exact Zellij terminal_<id> or plugin_<id>")]
pub struct InvalidZellijPaneId(PaneId);

impl TryFrom<&PaneId> for ZellijPaneId {
    type Error = InvalidZellijPaneId;

    fn try_from(value: &PaneId) -> Result<Self, Self::Error> {
        if value.mux() != MuxName::Zellij {
            return Err(InvalidZellijPaneId(value.clone()));
        }
        let parsed = value
            .raw()
            .strip_prefix("terminal_")
            .and_then(|raw| raw.parse::<u64>().ok())
            .map(Self::Terminal)
            .or_else(|| {
                value
                    .raw()
                    .strip_prefix("plugin_")
                    .and_then(|raw| raw.parse::<u64>().ok())
                    .map(Self::Plugin)
            });
        parsed.ok_or_else(|| InvalidZellijPaneId(value.clone()))
    }
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
    pub fn native_id(&self) -> ZellijPaneId {
        if self.is_plugin {
            ZellijPaneId::Plugin(self.id)
        } else {
            ZellijPaneId::Terminal(self.id)
        }
    }

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
    pub(crate) fn is_live_terminal(&self) -> bool {
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

    fn test_pane(id: u64, tab_position: u64, title: &str) -> PaneTopologyPane {
        PaneTopologyPane {
            id,
            is_plugin: false,
            is_held: false,
            exited: false,
            is_suppressed: false,
            is_floating: false,
            tab_position,
            tab_name: None,
            pane_columns: None,
            pane_x: None,
            title: Some(title.to_owned()),
            pane_command: None,
            pane_cwd: None,
            pane_pid: None,
            terminal_command: None,
        }
    }

    #[test]
    fn native_identity_enforces_exact_namespace_grammar() {
        let terminal = ZellijPaneId::Terminal(7);
        let plugin = ZellijPaneId::Plugin(7);
        assert_ne!(terminal, plugin);
        assert_eq!(PaneId::from(terminal).as_str(), "zellij:terminal_7");
        assert_eq!(PaneId::from(plugin).as_str(), "zellij:plugin_7");
        assert_eq!(terminal.action_target(), "terminal_7");
        assert_eq!(plugin.action_target(), "plugin_7");

        for invalid in [
            PaneId::from_parts(MuxName::Tmux, "%7"),
            PaneId::from_parts(MuxName::Zellij, "terminal_"),
            PaneId::from_parts(MuxName::Zellij, "terminal_7x"),
            PaneId::from_parts(MuxName::Zellij, "plugin_-1"),
            PaneId::from_parts(MuxName::Zellij, "terminal_18446744073709551616"),
        ] {
            assert!(ZellijPaneId::try_from(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn current_clients_override_conflicting_legacy_focus() {
        let mut cache = PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: 42,
            writer: None,
            focused_pane: Some(1),
            clients: Some(TopologyClients {
                human_clients: None,
                viewed_panes: None,
                views: vec![TopologyClientView {
                    client_id: 3,
                    pane_id: ZellijPaneId::Terminal(2),
                }],
            }),
            panes: vec![test_pane(1, 0, "one"), test_pane(2, 0, "two")],
        };
        assert_eq!(
            cache.projected_session_focus(),
            Some(PaneId::from(ZellijPaneId::Terminal(2)))
        );

        cache.clients = None;
        assert_eq!(
            cache.projected_session_focus(),
            Some(PaneId::from(ZellijPaneId::Terminal(1)))
        );
    }

    /// The topology cache is a wire format shared with the
    /// `rimz-presence-zellij` wasm plugin, which keeps its own deployment
    /// lifetime: a running older plugin writes payloads this host must read, and
    /// a newer plugin writes fields an older host must tolerate. Each row parses
    /// a payload, re-serializes it, and checks every pointer survives the trip —
    /// `None` meaning the key must not be emitted at all.
    #[test]
    fn topology_wire_format_survives_plugin_generation_skew() {
        use serde_json::{Value, json};

        /// Pointers into the re-serialized payload, each with the value it must
        /// carry — `None` when the key must not be emitted at all.
        type Pointers<'a> = &'a [(&'a str, Option<Value>)];

        let cases: &[(&str, &str, Pointers)] = &[
            (
                "a legacy payload leaves every optional absent",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"panes":[{"id":7,"tab_position":0}]}"#,
                &[
                    ("/focused_pane", None),
                    ("/writer", None),
                    ("/clients", None),
                    ("/panes/0/pane_pid", None),
                    ("/panes/0/pane_cwd", None),
                ],
            ),
            (
                "the focused pane round-trips",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"focused_pane":7,"panes":[]}"#,
                &[("/focused_pane", Some(json!(7)))],
            ),
            (
                "a writer without identity fields omits them",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"writer":{"plugin_id":9,"loaded_at_ms":1000},"panes":[]}"#,
                &[
                    ("/writer/plugin_id", Some(json!(9))),
                    ("/writer/loaded_at_ms", Some(json!(1000))),
                    ("/writer/build", None),
                    ("/writer/config", None),
                ],
            ),
            (
                "a writer carrying build and config round-trips both",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"writer":{"plugin_id":9,"loaded_at_ms":1000,"build":"wasm-build","config":"config-hash"},"panes":[]}"#,
                &[
                    ("/writer/build", Some(json!("wasm-build"))),
                    ("/writer/config", Some(json!("config-hash"))),
                ],
            ),
            (
                "clients round-trip, tagging each pane id with its namespace",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"clients":{"human_clients":2,"viewed_panes":[7],"views":[{"client_id":3,"pane_id":{"kind":"terminal","id":7}},{"client_id":4,"pane_id":{"kind":"plugin","id":9}}]},"panes":[]}"#,
                &[
                    ("/clients/human_clients", Some(json!(2))),
                    ("/clients/viewed_panes", Some(json!([7]))),
                    ("/clients/views/0/pane_id/kind", Some(json!("terminal"))),
                    ("/clients/views/1/pane_id/kind", Some(json!("plugin"))),
                    ("/clients/views/1/pane_id/id", Some(json!(9))),
                ],
            ),
            (
                "a pane pid round-trips",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"panes":[{"id":7,"tab_position":0,"pane_pid":707}]}"#,
                &[("/panes/0/pane_pid", Some(json!(707)))],
            ),
            (
                "a pane cwd round-trips",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"panes":[{"id":7,"tab_position":0,"pane_command":"zsh","pane_cwd":"/repo/main"}]}"#,
                &[("/panes/0/pane_cwd", Some(json!("/repo/main")))],
            ),
            (
                "a retired focus-resolution field is dropped rather than echoed",
                r#"{"session_name":"rimz-test","produced_at_ms":42,"active_panes":{"0":7,"1":11},"panes":[]}"#,
                &[("/active_panes", None)],
            ),
        ];

        for (skew, payload, pointers) in cases {
            let cache: PaneTopologyCache =
                serde_json::from_str(payload).unwrap_or_else(|err| panic!("{skew}: {err}"));
            let encoded = serde_json::to_value(&cache).expect("topology serializes");
            for (pointer, expected) in *pointers {
                assert_eq!(
                    encoded.pointer(pointer),
                    expected.as_ref(),
                    "{skew}: {pointer}"
                );
            }
        }
    }

    #[test]
    fn client_view_derives_presence_from_distinct_views() {
        // Neither `human_clients` nor `viewed_panes` is published, so both
        // derive from `views` — and the repeated client_id counts once.
        let cache: PaneTopologyCache = serde_json::from_str(
            r#"{"session_name":"rimz-test","produced_at_ms":42,"clients":{"views":[
                {"client_id":3,"pane_id":{"kind":"terminal","id":7}},
                {"client_id":3,"pane_id":{"kind":"terminal","id":7}},
                {"client_id":4,"pane_id":{"kind":"plugin","id":9}}
            ]},"panes":[]}"#,
        )
        .expect("views-only topology parses");

        let view = cache.clients.expect("client sample").into_client_view();

        assert_eq!(view.presence.human_clients, 2);
        assert_eq!(
            view.viewed_panes,
            vec![PaneId::from(ZellijPaneId::Terminal(7))]
        );
    }
}
