//! Pure tab-status projection from the producer's fused agent rows and pane
//! frame. Mux mutation stays with the elected producer; this module only
//! decides which observed names need a new suffix.

use std::collections::HashMap;

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::TAB_SUCCESS_STATUS_TTL;
use crate::{store::snapshot::SidebarSnapshot, theme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRename {
    pub(crate) anchor: PaneId,
    pub(crate) observed_name: String,
    pub(crate) desired_name: String,
}

/// Declaration order is the product precedence ladder consumed by `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TabStatus {
    Success,
    Running,
    Paused,
    Waiting,
    Failed,
}

impl TabStatus {
    fn from_row(status: AgentStatus, fresh_success: bool) -> Option<Self> {
        match status {
            AgentStatus::Failed => Some(Self::Failed),
            AgentStatus::Waiting => Some(Self::Waiting),
            AgentStatus::Paused => Some(Self::Paused),
            AgentStatus::Running => Some(Self::Running),
            AgentStatus::Success if fresh_success => Some(Self::Success),
            AgentStatus::Success | AgentStatus::Idle => None,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Failed => "!",
            Self::Waiting => "?",
            Self::Paused => "⏸\u{FE0E}",
            Self::Running => "⢿",
            Self::Success => "✓",
        }
    }
}

pub(crate) fn desired_tab_renames(snapshot: &SidebarSnapshot, frame: &PaneFrame) -> Vec<TabRename> {
    let status_by_pane = snapshot
        .rows()
        .filter_map(|row| {
            let pane = row.pane.as_ref()?;
            let status = row.status()?;
            let success_age = snapshot.now.duration_since(row.last_activity);
            let fresh_success = success_age <= TAB_SUCCESS_STATUS_TTL;
            TabStatus::from_row(status, fresh_success).map(|status| (pane.pane_id.clone(), status))
        })
        .fold(
            HashMap::<PaneId, TabStatus>::new(),
            |mut statuses, (pane, status)| {
                statuses
                    .entry(pane)
                    .and_modify(|known| *known = (*known).max(status))
                    .or_insert(status);
                statuses
            },
        );
    frame
        .tabs
        .iter()
        .filter_map(|tab| {
            let observed_name = tab.name.as_ref()?;
            let anchor = tab.panes.first()?.pane_id.clone();
            let status = tab
                .panes
                .iter()
                .filter_map(|pane| status_by_pane.get(&pane.pane_id).copied())
                .max();
            let base = theme::strip_status_glyph_suffix(observed_name, &snapshot.theme);
            let desired_name = status.map_or_else(
                || base.to_owned(),
                |status| format!("{base} {}", status.glyph()),
            );
            (desired_name != *observed_name).then(|| TabRename {
                anchor,
                observed_name: observed_name.clone(),
                desired_name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::ids::{MuxName, ViewId, ViewKind, WorkspaceId};
    use crate::pane::PaneRef;
    use crate::sidebar::frame::{PaneProcess, PaneState, TabFrame};
    use crate::sidebar::test_support::{activity_row, worktree_group};

    fn pane(id: &str) -> PaneRef {
        PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))
    }

    fn frame(name: &str, panes: &[&str]) -> PaneFrame {
        PaneFrame {
            produced_at_ms: 1,
            observed_at_ms: 1,
            topology_stamp_ms: Some(1),
            metrics_stamp_ms: None,
            build: None,
            session_name: "room".to_owned(),
            tabs: vec![TabFrame {
                view_id: ViewId::new_unchecked("@1"),
                kind: ViewKind::Window,
                name: Some(name.to_owned()),
                panes: panes
                    .iter()
                    .map(|id| PaneState {
                        pane_id: PaneId::from_parts(MuxName::Tmux, id),
                        first_seen_at_ms: None,
                        hosted_carry_since_ms: None,
                        is_floating: false,
                        current: PaneProcess {
                            pid: None,
                            command: None,
                            foreground_cmdline: None,
                            spawn_command: None,
                            cwd: None,
                            started_at: None,
                            hosted_agent_kind: None,
                            hosted_agent_process_start: None,
                            resumed_session_id: None,
                            elevated_agent: None,
                        },
                        previous: None,
                        children: Vec::new(),
                        metrics: Default::default(),
                    })
                    .collect(),
            }],
            carried_panes: Vec::new(),
            viewed_panes: Vec::new(),
            client_views: Vec::new(),
            focused_pane: None,
            presence: None,
        }
    }

    fn snapshot(rows: Vec<(AgentStatus, &str, Timestamp)>, now: Timestamp) -> SidebarSnapshot {
        let workspace =
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id fixture");
        let mut snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), now);
        let rows = rows
            .into_iter()
            .map(|(status, pane_id, at)| {
                let mut row = activity_row(true, Some(status), at, std::path::Path::new("/repo"));
                row.pane = Some(pane(pane_id));
                row
            })
            .collect();
        snapshot.worktree_groups = vec![worktree_group(std::path::Path::new("/repo"), rows)];
        snapshot
    }

    #[test]
    fn worst_live_agent_status_wins_the_tab() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(
            vec![
                (AgentStatus::Running, "%1", now),
                (AgentStatus::Waiting, "%2", now),
                (AgentStatus::Failed, "%3", now),
            ],
            now,
        );

        let renames = desired_tab_renames(&snapshot, &frame("#feat", &["%1", "%2", "%3"]));

        assert_eq!(renames[0].desired_name, "#feat !");
        assert_eq!(renames[0].anchor.raw(), "%1");
    }

    #[test]
    fn success_expires_to_the_bare_manual_name() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let old = now - SignedDuration::from_mins(6);
        let snapshot = snapshot(vec![(AgentStatus::Success, "%1", old)], now);

        let renames = desired_tab_renames(&snapshot, &frame("my tab ✓", &["%1"]));

        assert_eq!(renames[0].observed_name, "my tab ✓");
        assert_eq!(renames[0].desired_name, "my tab");
    }

    #[test]
    fn status_change_replaces_both_catalog_variants() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(vec![(AgentStatus::Waiting, "%1", now)], now);
        let unicode = desired_tab_renames(&snapshot, &frame("manual ⏸\u{fe0e}", &["%1"]));
        let nerd = desired_tab_renames(&snapshot, &frame("manual \u{f04c}", &["%1"]));

        assert_eq!(unicode[0].desired_name, "manual ?");
        assert_eq!(nerd[0].desired_name, "manual ?");
    }

    #[test]
    fn tab_status_always_uses_unicode_glyphs() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        for (status, expected) in [
            (AgentStatus::Failed, "#feat !"),
            (AgentStatus::Waiting, "#feat ?"),
            (AgentStatus::Paused, "#feat ⏸\u{FE0E}"),
            (AgentStatus::Running, "#feat ⢿"),
            (AgentStatus::Success, "#feat ✓"),
        ] {
            assert_eq!(
                TabStatus::from_row(status, true)
                    .expect("tab status fixture")
                    .glyph(),
                theme::unicode_glyph(theme::agent_status_glyph_role(status)),
            );
            let mut snapshot = snapshot(vec![(status, "%1", now)], now);
            snapshot.theme.glyphs = toml::from_str(
                "set = \"nerd_font\"\n\
                 [nerd_font.status]\n\
                 waiting = \"W\"\n\
                 attention = \"A\"\n\
                 paused = \"P\"\n\
                 working = \"R\"\n\
                 done = \"D\"\n",
            )
            .expect("glyph config");

            let renames = desired_tab_renames(&snapshot, &frame("#feat", &["%1"]));

            assert_eq!(renames[0].desired_name, expected);
        }
    }

    #[test]
    fn unchanged_name_emits_no_mux_work() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(vec![(AgentStatus::Running, "%1", now)], now);

        assert!(desired_tab_renames(&snapshot, &frame("#feat ⢿", &["%1"])).is_empty());
    }
}
