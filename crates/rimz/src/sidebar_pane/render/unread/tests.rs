use crate::agents::TurnPhase;
use crate::feed::{AgentStatus, PaneRef};
use crate::ids::{MuxName, PaneId};
use crate::{AgentCard, RowCard, SidebarRow};
use jiff::Timestamp;

use super::UnreadTracker;

fn fixed_now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn row(id: &str, status: AgentStatus) -> SidebarRow {
    SidebarRow {
        id: id.to_owned(),
        name: "claude".to_owned(),
        pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        unread: false,
        last_activity: fixed_now(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(status),
            phase: TurnPhase::Idle,
            request_id: None,
            surface: None,
            task: None,
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            context_severity: None,
            registered_at: None,
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            compacting: false,
            compaction_count: 0,
            turn_error_label: None,
        })),
    }
}

fn observe(tracker: &mut UnreadTracker, rows: Vec<SidebarRow>, focused: Option<&str>) {
    tracker.observe(rows.iter(), focused);
}

#[test]
fn startup_records_without_flagging() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(!tracker.is_unread("a"));
}

#[test]
fn running_to_needs_a_look_flags_unread() {
    for status in [
        AgentStatus::Success,
        AgentStatus::Failed,
        AgentStatus::Waiting,
        AgentStatus::Paused,
    ] {
        let mut tracker = UnreadTracker::default();
        observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
        observe(&mut tracker, vec![row("a", status)], None);
        assert!(tracker.is_unread("a"), "{status:?}");
    }
}

#[test]
fn idle_to_waiting_does_not_flag_unread() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Idle)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Waiting)], None);
    assert!(!tracker.is_unread("a"));
}

#[test]
fn paused_to_waiting_preserves_unread_but_does_not_create_it() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Paused)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Waiting)], None);
    assert!(!tracker.is_unread("a"));

    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Paused)], None);
    assert!(tracker.is_unread("a"));
    observe(&mut tracker, vec![row("a", AgentStatus::Waiting)], None);
    assert!(tracker.is_unread("a"));
}

#[test]
fn running_or_idle_clears_unread() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(tracker.is_unread("a"));
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    assert!(!tracker.is_unread("a"));
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(tracker.is_unread("a"));
    observe(&mut tracker, vec![row("a", AgentStatus::Idle)], None);
    assert!(!tracker.is_unread("a"));
}

#[test]
fn focused_row_clears_after_transition() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    observe(
        &mut tracker,
        vec![row("a", AgentStatus::Success)],
        Some("a"),
    );
    assert!(!tracker.is_unread("a"));
}

#[test]
fn departed_row_is_pruned() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(tracker.is_unread("a"));
    observe(&mut tracker, vec![row("b", AgentStatus::Idle)], None);
    assert!(!tracker.is_unread("a"));
}
