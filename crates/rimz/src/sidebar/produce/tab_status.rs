//! Pure tab-status projection from the producer's fused agent rows and pane
//! frame. Mux mutation stays with the elected producer; this module only
//! decides which observed names need a new suffix or release to the shell.

use std::collections::{HashMap, HashSet};

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::mux::tab_name::{TabNameIntent, is_named_after_panes};
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::TAB_SUCCESS_STATUS_TTL;
use crate::store::snapshot::SidebarSnapshot;
use crate::theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRename {
    pub(crate) anchor: PaneId,
    pub(crate) observed_name: String,
    pub(crate) desired_name: String,
    pub(crate) intent: TabNameIntent,
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
            AgentStatus::Success | AgentStatus::Idle | AgentStatus::Sleeping => None,
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

pub(crate) fn desired_tab_renames(
    snapshot: &SidebarSnapshot,
    frame: &PaneFrame,
    shell_name: &str,
) -> Vec<TabRename> {
    let live_agent_panes = snapshot
        .rows()
        .filter(|row| row.as_agent().is_some())
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect::<HashSet<_>>();
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
            if observed_name == crate::pane::VIEW_NAME {
                return None;
            }
            let work_panes = tab
                .panes
                .iter()
                .filter(|pane| {
                    !pane.current.command.as_deref().is_some_and(|command| {
                        crate::pane::command_is_sidebar_chrome(command)
                            || crate::pane::command_is_host(command)
                    }) && !pane
                        .current
                        .spawn_command
                        .as_deref()
                        .is_some_and(crate::pane::command_is_host)
                })
                .collect::<Vec<_>>();
            let anchor = work_panes.first()?.pane_id.clone();
            let status = work_panes
                .iter()
                .filter_map(|pane| status_by_pane.get(&pane.pane_id).copied())
                .max();
            let base = theme::strip_status_glyph_suffix(observed_name, &snapshot.theme);
            let has_agent = work_panes.iter().any(|pane| {
                live_agent_panes.contains(&pane.pane_id) || pane.current.hosted_agent_kind.is_some()
            });
            let names = work_panes
                .iter()
                .filter_map(|pane| pane.title.as_deref())
                .collect::<Vec<_>>();
            let (desired_name, intent) = if let Some(status) = status {
                (format!("{base} {}", status.glyph()), TabNameIntent::Status)
            } else if !has_agent && base != shell_name && is_named_after_panes(base, &names) {
                (shell_name.to_owned(), TabNameIntent::Release)
            } else {
                (base.to_owned(), TabNameIntent::Rest)
            };
            (desired_name != *observed_name).then(|| TabRename {
                anchor,
                observed_name: observed_name.clone(),
                desired_name,
                intent,
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
                        title: None,
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

        let renames = desired_tab_renames(&snapshot, &frame("#feat", &["%1", "%2", "%3"]), "zsh");

        assert_eq!(renames[0].desired_name, "#feat !");
        assert_eq!(renames[0].anchor.raw(), "%1");
        assert_eq!(renames[0].intent, TabNameIntent::Status);
    }

    #[test]
    fn success_expires_to_the_bare_manual_name() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let old = now - SignedDuration::from_mins(6);
        let snapshot = snapshot(vec![(AgentStatus::Success, "%1", old)], now);

        let renames = desired_tab_renames(&snapshot, &frame("my tab ✓", &["%1"]), "zsh");

        assert_eq!(renames[0].observed_name, "my tab ✓");
        assert_eq!(renames[0].desired_name, "my tab");
        assert_eq!(renames[0].intent, TabNameIntent::Rest);
    }

    #[test]
    fn status_change_replaces_both_catalog_variants() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(vec![(AgentStatus::Waiting, "%1", now)], now);
        let unicode = desired_tab_renames(&snapshot, &frame("manual ⏸\u{fe0e}", &["%1"]), "zsh");
        let nerd = desired_tab_renames(&snapshot, &frame("manual \u{f04c}", &["%1"]), "zsh");

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

            let renames = desired_tab_renames(&snapshot, &frame("#feat", &["%1"]), "zsh");

            assert_eq!(renames[0].desired_name, expected);
        }
    }

    #[test]
    fn unchanged_name_emits_no_mux_work() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(vec![(AgentStatus::Running, "%1", now)], now);

        assert!(desired_tab_renames(&snapshot, &frame("#feat ⢿", &["%1"]), "zsh").is_empty());
    }

    fn named_frame(name: &str, names: &[&str]) -> PaneFrame {
        let ids = (1..=names.len())
            .map(|id| format!("%{id}"))
            .collect::<Vec<_>>();
        let mut frame = frame(name, &ids.iter().map(String::as_str).collect::<Vec<_>>());
        for (pane, name) in frame.tabs[0].panes.iter_mut().zip(names) {
            pane.title = Some((*name).to_owned());
        }
        frame
    }

    #[test]
    fn pane_named_tab_releases_only_after_the_last_agent_leaves() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let frame = named_frame("opus+codex", &["codex", "opus"]);
        for remaining in [vec!["%1", "%2"], vec!["%2"]] {
            for status in [
                AgentStatus::Idle,
                AgentStatus::Sleeping,
                AgentStatus::Success,
            ] {
                let snapshot = snapshot(
                    remaining
                        .iter()
                        .map(|id| (status, *id, now - SignedDuration::from_mins(6)))
                        .collect(),
                    now,
                );
                assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
            }
        }
        let snapshot = snapshot(Vec::new(), now);
        let renames = desired_tab_renames(&snapshot, &frame, "zsh");
        assert_eq!(renames[0].desired_name, "zsh");
        assert_eq!(renames[0].intent, TabNameIntent::Release);
        let mut released = frame;
        released.tabs[0].name = Some("zsh".to_owned());
        assert!(desired_tab_renames(&snapshot, &released, "zsh").is_empty());
    }

    #[test]
    fn hosted_agent_without_a_row_blocks_release_even_when_floating() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(Vec::new(), now);
        let mut frame = named_frame("opus", &["opus"]);
        frame.tabs[0].panes[0].current.hosted_agent_kind =
            Some(crate::ids::AgentKind::new_unchecked("claude"));
        frame.tabs[0].panes[0].is_floating = true;
        assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
    }

    #[test]
    fn scoped_manual_and_unpinned_names_are_kept_but_stale_status_clears() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(Vec::new(), now);
        for name in ["#feat", "team:forge", "my tab", "zsh"] {
            assert!(
                desired_tab_renames(&snapshot, &named_frame(name, &["opus"]), "zsh").is_empty()
            );
        }
        assert!(desired_tab_renames(&snapshot, &frame("opus", &["%1"]), "zsh").is_empty());
        let renames = desired_tab_renames(&snapshot, &named_frame("#feat ?", &["opus"]), "zsh");
        assert_eq!(renames[0].desired_name, "#feat");
        assert_eq!(renames[0].intent, TabNameIntent::Rest);
        let renames = desired_tab_renames(
            &snapshot,
            &named_frame("opus+codex+pi+…", &["pi", "opus", "codex", "nvim"]),
            "zsh",
        );
        assert_eq!(renames[0].intent, TabNameIntent::Release);
    }

    #[test]
    fn chrome_and_daemon_panes_neither_claim_nor_hold_a_name() {
        let now = Timestamp::from_second(1_700_000_000).expect("time");
        let snapshot = snapshot(Vec::new(), now);
        for command in ["rimz-sidebar", "claude remote-control", "codex app-server"] {
            let mut frame = named_frame("opus", &["opus", "opus"]);
            frame.tabs[0].panes[0].current.command = Some(command.to_owned());
            frame.tabs[0].panes[0].current.hosted_agent_kind =
                Some(crate::ids::AgentKind::new_unchecked("claude"));
            let renames = desired_tab_renames(&snapshot, &frame, "zsh");
            assert_eq!(renames[0].intent, TabNameIntent::Release);
            assert_eq!(renames[0].anchor.raw(), "%2");
            frame.tabs[0].panes[1].title = None;
            assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
        }
    }
}
