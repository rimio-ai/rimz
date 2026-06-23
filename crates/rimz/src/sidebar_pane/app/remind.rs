use std::collections::{HashMap, HashSet};
use std::io;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tracing::debug;

use crate::SidebarSnapshot;
use crate::agents::AgentStatus;
use crate::config::NotificationsPrefs;
use crate::ids::PaneId;
use crate::sidebar::notify::{Notification, NotificationKind, spawn_notify_command};

use super::ServeConfig;
use super::notify::{BellNotice, emit_terminal_notification};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RemindState {
    armed_at_ms: Option<u64>,
    last_reminded_at_ms: Option<u64>,
}

impl RemindState {
    pub(super) fn note_ring(&mut self, now_ms: u64) {
        self.armed_at_ms = Some(now_ms);
        self.last_reminded_at_ms = None;
    }

    fn note_scope_visible(&mut self, now_ms: u64) {
        if self.armed_at_ms.is_none() {
            self.armed_at_ms = Some(now_ms);
            self.last_reminded_at_ms = None;
        }
    }

    pub(super) fn maybe_remind(
        &mut self,
        config: &ServeConfig,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        snapshot: &SidebarSnapshot,
        diag: Option<&crate::diag::DiagSink>,
    ) {
        let scope = unread_reminder_scope(snapshot, &config.notification_prefs);
        if scope.count == 0
            || !config.notification_prefs.enabled
            || config.notification_prefs.remind_secs == 0
        {
            self.clear();
            return;
        }
        let now_ms = crate::sidebar::cache::unix_now_ms();
        self.note_scope_visible(now_ms);
        if !self.due(now_ms, scope.count, config.notification_prefs.remind_secs) {
            return;
        }

        let notification = unread_reminder_notification(scope.count);
        // The reminder scope is already unread `waiting`/`failed` rows, and its
        // paneless path borrows non-unread sibling panes to reach a detached
        // ask — so ring directly rather than re-checking each borrowed pane's
        // row. The daemon exclusion in `bell_decision` still applies.
        if let Err(err) = emit_terminal_notification(
            config,
            terminal,
            snapshot,
            BellNotice {
                title: &notification.title,
                body: &notification.body,
                panes: &scope.panes,
                recheck_unread: false,
                kind: notification.kind_env(),
            },
            diag,
        ) {
            debug!(error = %err, "terminal unread reminder emit failed");
        }
        if let Some(command) = config.notification_prefs.command()
            && let Err(err) = spawn_notify_command(command, &notification)
        {
            debug!(error = %err, "unread reminder command spawn failed");
        }
        self.last_reminded_at_ms = Some(now_ms);
    }

    fn due(&self, now_ms: u64, count: usize, remind_secs: u64) -> bool {
        if count == 0 || remind_secs == 0 {
            return false;
        }
        let Some(armed_at_ms) = self.armed_at_ms else {
            return false;
        };
        let anchor = self.last_reminded_at_ms.unwrap_or(armed_at_ms);
        now_ms.saturating_sub(anchor) >= remind_secs.saturating_mul(1_000)
    }

    fn clear(&mut self) {
        self.armed_at_ms = None;
        self.last_reminded_at_ms = None;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReminderScope {
    count: usize,
    panes: Vec<PaneId>,
}

impl ReminderScope {
    fn add_pane_row(&mut self, pane: &PaneId) {
        self.count += 1;
        self.push_pane(pane);
    }

    fn add_paneless_row(&mut self, panes: &[PaneId]) {
        if panes.is_empty() {
            return;
        }
        self.count += 1;
        for pane in panes {
            self.push_pane(pane);
        }
    }

    fn push_pane(&mut self, pane: &PaneId) {
        if !self.panes.contains(pane) {
            self.panes.push(pane.clone());
        }
    }
}

fn unread_reminder_notification(count: usize) -> Notification {
    Notification {
        agents: Vec::new(),
        notification_kind: NotificationKind::Reminder,
        title: if count == 1 {
            "Rimz: 1 unread row needs you".to_owned()
        } else {
            format!("Rimz: {count} unread rows need you")
        },
        body: if count == 1 {
            "1 unread row still needs you.".to_owned()
        } else {
            format!("{count} unread rows still need you.")
        },
        unread_count: Some(count),
    }
}

fn unread_reminder_scope(snapshot: &SidebarSnapshot, prefs: &NotificationsPrefs) -> ReminderScope {
    let Some(own_view) = &snapshot.own_view else {
        return ReminderScope::default();
    };
    let working: HashSet<_> = own_view.working_pane_ids.iter().cloned().collect();
    if working.is_empty() {
        return ReminderScope::default();
    }
    let focused = prefs
        .suppress_focused
        .then_some(own_view.active_pane_id.as_ref())
        .flatten();
    let worktree_targets = worktree_target_panes(snapshot, &working, focused);
    let mut scope = ReminderScope::default();
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
    {
        if !row.unread || !row.status().is_some_and(is_reminder_status) {
            continue;
        }
        if let Some(pane) = row.pane.as_ref() {
            if working.contains(&pane.pane_id)
                && !focused.is_some_and(|active| active == &pane.pane_id)
            {
                scope.add_pane_row(&pane.pane_id);
            }
            continue;
        }
        if let Some(path) = row.worktree_path.as_deref().filter(|path| !path.is_empty())
            && let Some(panes) = worktree_targets.get(path)
        {
            scope.add_paneless_row(panes);
        }
    }
    scope
}

fn worktree_target_panes(
    snapshot: &SidebarSnapshot,
    working: &HashSet<PaneId>,
    focused: Option<&PaneId>,
) -> HashMap<String, Vec<PaneId>> {
    let mut targets: HashMap<String, Vec<PaneId>> = HashMap::new();
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
    {
        let Some(path) = row.worktree_path.as_deref().filter(|path| !path.is_empty()) else {
            continue;
        };
        let Some(pane) = row.pane.as_ref() else {
            continue;
        };
        if !working.contains(&pane.pane_id) || focused.is_some_and(|active| active == &pane.pane_id)
        {
            continue;
        }
        let panes = targets.entry(path.to_owned()).or_default();
        if !panes.contains(&pane.pane_id) {
            panes.push(pane.pane_id.clone());
        }
    }
    targets
}

fn is_reminder_status(status: AgentStatus) -> bool {
    status.is_actionable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::app::fixtures::{pane, snapshot, workspace};
    use crate::{
        AgentCard, RowCard, SidebarOwnView, SidebarRow, SidebarWorktreeGroup, SidebarWorktreeKind,
    };
    use jiff::Timestamp;

    fn row(raw: &str, status: AgentStatus, unread: bool) -> SidebarRow {
        SidebarRow {
            id: raw.to_owned(),
            name: "claude".to_owned(),
            pane: Some(pane(raw, "tab_0", false)),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            unread,
            inactive: false,
            last_activity: Timestamp::now(),
            card: RowCard::Agent(Box::new(AgentCard {
                status: Some(status),
                phase: crate::agents::TurnPhase::Idle,
                ..AgentCard::default()
            })),
        }
    }

    fn paneless_row(raw: &str, status: AgentStatus, unread: bool, worktree: &str) -> SidebarRow {
        let mut entry = row(raw, status, unread);
        entry.pane = None;
        entry.worktree_path = Some(worktree.to_owned());
        entry
    }

    fn snapshot_with(
        rows: Vec<SidebarRow>,
        active: Option<&str>,
        working: Vec<&str>,
    ) -> SidebarSnapshot {
        let mut snapshot = snapshot(&workspace());
        snapshot.own_view = Some(SidebarOwnView {
            sibling_count: working.len(),
            own_is_active: false,
            active_pane_id: active.map(|raw| PaneId::from_parts(crate::MuxName::Zellij, raw)),
            active_pane_is_viewed: false,
            working_pane_ids: working
                .into_iter()
                .map(|raw| PaneId::from_parts(crate::MuxName::Zellij, raw))
                .collect(),
            focus_contested: false,
            own_view_is_daemon: false,
        });
        snapshot.worktree_groups = vec![SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows,
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
        }];
        snapshot
    }

    #[test]
    fn reminder_waits_for_an_initial_ring() {
        let state = RemindState::default();
        assert!(!state.due(60_000, 1, 60));
    }

    #[test]
    fn reminder_interval_runs_from_ring_then_previous_reminder() {
        let mut state = RemindState::default();
        state.note_ring(1_000);
        assert!(!state.due(60_999, 1, 60));
        assert!(state.due(61_000, 1, 60));
        state.last_reminded_at_ms = Some(61_000);
        assert!(!state.due(120_999, 1, 60));
        assert!(state.due(121_000, 1, 60));
    }

    #[test]
    fn reminder_clear_stops_the_episode() {
        let mut state = RemindState::default();
        state.note_ring(1_000);
        state.clear();
        assert!(!state.due(120_000, 1, 60));
    }

    #[test]
    fn reminder_zero_interval_is_disabled() {
        let mut state = RemindState::default();
        state.note_ring(1_000);
        assert!(!state.due(120_000, 1, 0));
    }

    #[test]
    fn reminder_scope_appearance_arms_without_immediate_ring() {
        let mut state = RemindState::default();
        state.note_ring(1_000);
        state.clear();

        state.note_scope_visible(2_000);

        assert!(!state.due(61_999, 1, 60));
        assert!(state.due(62_000, 1, 60));
    }

    #[test]
    fn reminder_scope_counts_unread_waiting_and_failed_in_own_view() {
        let snapshot = snapshot_with(
            vec![
                row("terminal_1", AgentStatus::Waiting, true),
                row("terminal_2", AgentStatus::Failed, true),
                row("terminal_3", AgentStatus::Success, true),
                row("terminal_4", AgentStatus::Paused, true),
                row("terminal_5", AgentStatus::Waiting, false),
                row("terminal_6", AgentStatus::Waiting, true),
            ],
            None,
            vec![
                "terminal_1",
                "terminal_2",
                "terminal_3",
                "terminal_4",
                "terminal_5",
            ],
        );

        let scope = unread_reminder_scope(&snapshot, &NotificationsPrefs::default());
        assert_eq!(scope.count, 2);
        assert_eq!(
            scope.panes,
            vec![
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_2"),
            ]
        );
    }

    #[test]
    fn reminder_scope_suppresses_the_focused_pane_when_configured() {
        let snapshot = snapshot_with(
            vec![
                row("terminal_1", AgentStatus::Waiting, true),
                row("terminal_2", AgentStatus::Failed, true),
            ],
            Some("terminal_1"),
            vec!["terminal_1", "terminal_2"],
        );

        let scope = unread_reminder_scope(&snapshot, &NotificationsPrefs::default());
        assert_eq!(scope.count, 1);
        assert_eq!(
            scope.panes,
            vec![PaneId::from_parts(crate::MuxName::Zellij, "terminal_2")]
        );
        let prefs = NotificationsPrefs {
            suppress_focused: false,
            ..NotificationsPrefs::default()
        };
        let scope = unread_reminder_scope(&snapshot, &prefs);
        assert_eq!(scope.count, 2);
        assert_eq!(
            scope.panes,
            vec![
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_2"),
            ]
        );
    }

    #[test]
    fn reminder_scope_targets_paneless_rows_through_local_worktree_panes() {
        let snapshot = snapshot_with(
            vec![
                row("terminal_1", AgentStatus::Running, false),
                paneless_row("ask-1", AgentStatus::Waiting, true, "/repo/main"),
                paneless_row("ask-2", AgentStatus::Failed, true, "/repo/other"),
            ],
            None,
            vec!["terminal_1"],
        );

        let scope = unread_reminder_scope(&snapshot, &NotificationsPrefs::default());

        assert_eq!(scope.count, 1);
        assert_eq!(
            scope.panes,
            vec![PaneId::from_parts(crate::MuxName::Zellij, "terminal_1")]
        );
    }

    #[test]
    fn reminder_notification_carries_unread_count() {
        let notification = unread_reminder_notification(2);
        assert_eq!(notification.notification_kind, NotificationKind::Reminder);
        assert_eq!(notification.title, "Rimz: 2 unread rows need you");
        assert_eq!(notification.unread_count, Some(2));
        assert_eq!(notification.kind_env(), "reminder");
        assert_eq!(
            unread_reminder_notification(1).title,
            "Rimz: 1 unread row needs you"
        );
    }
}
