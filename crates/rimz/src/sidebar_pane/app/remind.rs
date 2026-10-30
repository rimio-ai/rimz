//! Room-wide unread reminder timing and actionable attention scope for the fetch worker.

use crate::agents::AgentStatus;
use crate::config::NotificationsPrefs;
use crate::ids::PaneId;
use crate::sidebar::notify::{Notification, NotificationKind};
use crate::store::snapshot::SidebarSnapshot;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RemindState {
    armed_at_ms: Option<u64>,
    last_reminded_at_ms: Option<u64>,
}

impl RemindState {
    pub(super) fn next_due_ms(&self, prefs: &NotificationsPrefs) -> Option<u64> {
        if !prefs.enabled || prefs.remind_secs == 0 {
            return None;
        }
        let armed_at_ms = self.armed_at_ms?;
        let anchor = self.last_reminded_at_ms.unwrap_or(armed_at_ms);
        Some(anchor.saturating_add(prefs.remind_secs.saturating_mul(1_000)))
    }

    pub(super) fn evaluate(
        &mut self,
        now_ms: u64,
        snapshot: &SidebarSnapshot,
        prefs: &NotificationsPrefs,
    ) -> Option<(Notification, Vec<PaneId>)> {
        let scope = unread_reminder_scope(snapshot, prefs);
        if scope.count == 0 || !prefs.enabled || prefs.remind_secs == 0 {
            self.clear();
            return None;
        }
        self.note_scope_visible(now_ms);
        if !self.due(now_ms, scope.count, prefs.remind_secs) {
            return None;
        }
        self.last_reminded_at_ms = Some(now_ms);
        Some((unread_reminder_notification(scope.count), scope.panes))
    }
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
            "RimZ: 1 unread row needs you".to_owned()
        } else {
            format!("RimZ: {count} unread rows need you")
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
    let focused = prefs.suppress_focused.then_some(&snapshot.viewed_panes[..]);
    let mut scope = ReminderScope::default();
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
    {
        if !row.unread || !row.status().is_some_and(AgentStatus::is_actionable) {
            continue;
        }
        let Some(pane) = row.pane.as_ref() else {
            continue;
        };
        if !focused.is_some_and(|viewed| viewed.contains(&pane.pane_id)) {
            scope.add_pane_row(&pane.pane_id);
        }
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::app::fixtures::{pane, snapshot, workspace};
    use crate::store::snapshot::{
        AgentCard, RowCard, SidebarOwnView, SidebarRow, SidebarWorktreeGroup, SidebarWorktreeKind,
    };
    use jiff::Timestamp;

    fn row(raw: &str, status: AgentStatus, unread: bool) -> SidebarRow {
        SidebarRow {
            checkout: None,
            id: raw.to_owned(),
            name: "claude".to_owned(),
            pane: Some(pane(raw, "tab_0", false)),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            channel: None,
            unread,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: Timestamp::now(),
            card: RowCard::Agent(std::sync::Arc::new(AgentCard {
                status,
                phase: crate::agents::TurnPhase::Idle,
                ..AgentCard::default()
            })),
        }
    }

    fn snapshot_with(
        rows: Vec<SidebarRow>,
        active: Option<&str>,
        working: Vec<&str>,
    ) -> SidebarSnapshot {
        let mut snapshot = snapshot(&workspace());
        snapshot.panes_produced_at_ms = Some(1);
        snapshot.own_view = Some(SidebarOwnView {
            sibling_count: working.len(),
            working_pane_ids: working
                .into_iter()
                .map(|raw| PaneId::from_parts(crate::MuxName::Zellij, raw))
                .collect(),
            own_view_is_daemon: false,
        });
        if let Some(active) = active {
            snapshot
                .viewed_panes
                .push(PaneId::from_parts(crate::MuxName::Zellij, active));
        }
        snapshot.worktree_groups = vec![SidebarWorktreeGroup {
            pr_stack: Default::default(),
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            label_qualifier: None,
            kind: SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
            status_counts: Vec::new(),
            rows: rows.into(),
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            worktree_backed: false,
            finished: false,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
            pr_queue: None,
            ci: None,
            pr_number: None,
            pr_url: None,
        }]
        .into();
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
    fn reminder_scope_counts_unread_waiting_and_failed_across_the_room() {
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
        assert_eq!(scope.count, 3);
        assert_eq!(
            scope.panes,
            vec![
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_2"),
                PaneId::from_parts(crate::MuxName::Zellij, "terminal_6"),
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
    fn reminder_notification_carries_unread_count() {
        let notification = unread_reminder_notification(2);
        assert_eq!(notification.notification_kind, NotificationKind::Reminder);
        assert_eq!(notification.title, "RimZ: 2 unread rows need you");
        assert_eq!(notification.unread_count, Some(2));
        assert_eq!(notification.kind_env(), "reminder");
        assert_eq!(
            unread_reminder_notification(1).title,
            "RimZ: 1 unread row needs you"
        );
    }

    #[test]
    fn room_timer_arms_from_scope_and_clears_when_disabled_or_empty() {
        let mut state = RemindState::default();
        let mut snapshot = snapshot_with(
            vec![row("terminal_1", AgentStatus::Waiting, true)],
            None,
            vec![],
        );
        let mut prefs = NotificationsPrefs {
            remind_secs: 1,
            ..NotificationsPrefs::default()
        };
        assert!(state.evaluate(1_000, &snapshot, &prefs).is_none());
        assert_eq!(state.next_due_ms(&prefs), Some(2_000));
        assert!(state.evaluate(1_999, &snapshot, &prefs).is_none());
        let reminder = state.evaluate(2_000, &snapshot, &prefs);
        assert!(
            reminder.is_some(),
            "room scope arms a reminder without an agent notification"
        );
        assert_eq!(reminder.unwrap().0.unread_count, Some(1));
        assert_eq!(state.next_due_ms(&prefs), Some(3_000));
        assert!(state.evaluate(2_001, &snapshot, &prefs).is_none());
        assert!(state.evaluate(3_000, &snapshot, &prefs).is_some());
        prefs.enabled = false;
        assert_eq!(state.next_due_ms(&prefs), None);
        assert!(state.evaluate(4_000, &snapshot, &prefs).is_none());
        prefs.enabled = true;
        assert!(state.evaluate(5_000, &snapshot, &prefs).is_none());
        snapshot.worktree_groups = Default::default();
        assert!(state.evaluate(6_000, &snapshot, &prefs).is_none());
        assert!(!state.due(7_000, 1, 1));
    }
}
