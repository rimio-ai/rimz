use std::collections::BTreeSet;
use std::path::Path;

use rimz::ids::{MuxName, PaneId, ViewKind};
use rimz::pane::PaneRef;
use serde::Deserialize;

use crate::common::CommandTimeoutExt;

use super::session::{
    LIST_PANES_JSON_ATTEMPTS, LIST_PANES_JSON_RETRY_DELAY, LIST_PANES_JSON_TIMEOUT, scoped_zellij,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::backend::zellij) struct PaneGeometry {
    pub(in crate::backend::zellij) id: u64,
    pub(in crate::backend::zellij) x: u64,
    pub(in crate::backend::zellij) y: u64,
    pub(in crate::backend::zellij) columns: u64,
    pub(in crate::backend::zellij) rows: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub(in crate::backend::zellij) struct ListedPane {
    pub(in crate::backend::zellij) id: u64,
    #[serde(default)]
    pub(in crate::backend::zellij) is_plugin: bool,
    #[serde(default)]
    pub(in crate::backend::zellij) is_held: bool,
    #[serde(default)]
    pub(in crate::backend::zellij) exited: bool,
    #[serde(default)]
    pub(in crate::backend::zellij) is_suppressed: bool,
    #[serde(default)]
    pub(in crate::backend::zellij) is_floating: bool,
    pub(in crate::backend::zellij) tab_id: u64,
    #[serde(default)]
    pub(in crate::backend::zellij) tab_position: Option<u64>,
    #[serde(default)]
    pub(in crate::backend::zellij) tab_name: Option<String>,
    #[serde(default)]
    pub(in crate::backend::zellij) title: Option<String>,
    pub(in crate::backend::zellij) pane_x: u64,
    pub(in crate::backend::zellij) pane_y: u64,
    pub(in crate::backend::zellij) pane_columns: u64,
    pub(in crate::backend::zellij) pane_rows: u64,
    #[serde(default, alias = "command")]
    pub(in crate::backend::zellij) pane_command: Option<String>,
    #[serde(default)]
    pub(in crate::backend::zellij) pane_cwd: Option<String>,
    #[serde(default)]
    pub(in crate::backend::zellij) terminal_command: Option<String>,
}

impl ListedPane {
    pub(in crate::backend::zellij) fn geometry(&self) -> PaneGeometry {
        PaneGeometry {
            id: self.id,
            x: self.pane_x,
            y: self.pane_y,
            columns: self.pane_columns,
            rows: self.pane_rows,
        }
    }

    pub(in crate::backend::zellij) fn is_sidebar(&self) -> bool {
        !self.is_plugin && self.title.as_deref() == Some("rimz-sidebar")
    }

    pub(in crate::backend::zellij) fn is_live_terminal(&self) -> bool {
        !self.is_plugin && !self.is_held && !self.exited && !self.is_suppressed
    }

    pub(in crate::backend::zellij) fn pane_ref(&self, session: &str) -> PaneRef {
        let tab = self.tab_position.unwrap_or(self.tab_id);
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", self.id)),
            session_name: session.to_owned(),
            view_id: Some(format!("tab_{tab}")),
            view_kind: Some(ViewKind::Tab),
            view_name: self.tab_name.clone(),
            title: self.title.clone(),
            is_floating: self.is_floating,
            command: self
                .pane_command
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            foreground_cmdline: None,
            spawn_command: self
                .terminal_command
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::backend::zellij) struct PaneSnapshot {
    pub(in crate::backend::zellij) session: String,
    pub(in crate::backend::zellij) panes: Vec<ListedPane>,
}

impl PaneSnapshot {
    pub(in crate::backend::zellij) fn load(xdg: &Path, session: &str) -> Result<Self, String> {
        let mut last_error = "list-panes was not run".to_owned();
        for attempt in 0..LIST_PANES_JSON_ATTEMPTS {
            match scoped_zellij(xdg)
                .args(["--session", session, "action", "list-panes", "-j", "-a"])
                .bounded_output_within(LIST_PANES_JSON_TIMEOUT)
            {
                Ok(output) if output.status.success() => {
                    match serde_json::from_slice::<Vec<ListedPane>>(&output.stdout) {
                        Ok(panes) => {
                            return Ok(Self {
                                session: session.to_owned(),
                                panes,
                            });
                        }
                        Err(err) => {
                            last_error = format!(
                                "parsing list-panes JSON for {session}: {err}; stdout: {}; stderr: {}",
                                String::from_utf8_lossy(&output.stdout),
                                String::from_utf8_lossy(&output.stderr),
                            );
                        }
                    }
                }
                Ok(output) => {
                    last_error = format!(
                        "list-panes failed for {session} with {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr),
                    );
                }
                Err(err) => last_error = format!("list-panes failed for {session}: {err}"),
            }
            if attempt + 1 < LIST_PANES_JSON_ATTEMPTS {
                std::thread::sleep(LIST_PANES_JSON_RETRY_DELAY);
            }
        }
        Err(last_error)
    }

    pub(in crate::backend::zellij) fn expect(xdg: &Path, session: &str) -> Self {
        Self::load(xdg, session).unwrap_or_else(|err| panic!("{err}"))
    }

    pub(in crate::backend::zellij) fn sidebar(&self) -> Option<&ListedPane> {
        self.panes.iter().find(|pane| pane.is_sidebar())
    }

    pub(in crate::backend::zellij) fn pane_refs(&self) -> Vec<PaneRef> {
        self.panes
            .iter()
            .filter(|pane| pane.is_live_terminal())
            .map(|pane| pane.pane_ref(&self.session))
            .collect()
    }

    pub(in crate::backend::zellij) fn tab_ids(&self) -> Vec<u64> {
        let ids: BTreeSet<u64> = self
            .panes
            .iter()
            .filter(|pane| !pane.is_plugin)
            .map(|pane| pane.tab_id)
            .collect();
        ids.into_iter().collect()
    }

    pub(in crate::backend::zellij) fn terminal_titles_in_tab(&self, tab_id: u64) -> Vec<String> {
        self.panes
            .iter()
            .filter(|pane| !pane.is_plugin && pane.tab_id == tab_id)
            .filter_map(|pane| pane.title.clone())
            .collect()
    }
}

pub(in crate::backend::zellij) fn list_panes(
    xdg: &Path,
    session: &str,
) -> Result<PaneSnapshot, String> {
    PaneSnapshot::load(xdg, session)
}

pub(in crate::backend::zellij) fn expect_list_panes(xdg: &Path, session: &str) -> PaneSnapshot {
    PaneSnapshot::expect(xdg, session)
}

pub(in crate::backend::zellij) fn raw_sidebar_pane(xdg: &Path, session: &str) -> ListedPane {
    PaneSnapshot::expect(xdg, session)
        .sidebar()
        .unwrap_or_else(|| panic!("rimz-sidebar pane missing in {session}"))
        .clone()
}

pub(in crate::backend::zellij) fn tab_ids(xdg: &Path, session: &str) -> Vec<u64> {
    PaneSnapshot::expect(xdg, session).tab_ids()
}

pub(in crate::backend::zellij) fn wait_for_pane_count(
    xdg: &Path,
    session: &str,
    want: usize,
) -> Vec<PaneRef> {
    super::actions::poll_until(
        std::time::Duration::from_secs(10),
        || list_panes(xdg, session).map(|snapshot| snapshot.pane_refs()),
        |panes| panes.len() >= want,
        &format!("{want} panes in {session}"),
    )
}
