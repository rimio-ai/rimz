use crate::agents::TurnPhase;
use crate::feed::{AgentStatus, PaneRef};
use crate::ids::{MuxName, PaneId};
use crate::sidebar::read_marks::ReadMarks;
use crate::{AgentCard, RowCard, SidebarRow};
use jiff::Timestamp;

use super::UnreadTracker;

fn fixed_now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn row(id: &str, status: AgentStatus) -> SidebarRow {
    row_at(id, status, fixed_now())
}

fn row_at(id: &str, status: AgentStatus, last_activity: Timestamp) -> SidebarRow {
    SidebarRow {
        id: id.to_owned(),
        name: "claude".to_owned(),
        pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        unread: false,
        last_activity,
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

fn observe(
    tracker: &mut UnreadTracker,
    rows: Vec<SidebarRow>,
    focused: Option<&str>,
) -> Vec<String> {
    observe_with_marks(tracker, rows, focused, &ReadMarks::empty())
}

fn observe_with_marks(
    tracker: &mut UnreadTracker,
    rows: Vec<SidebarRow>,
    focused: Option<&str>,
    marks: &ReadMarks,
) -> Vec<String> {
    tracker.observe(rows.iter(), focused, marks)
}

#[test]
fn unread_transitions_only_start_from_working_states() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(!tracker.is_unread("a"));

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

    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Idle)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Waiting)], None);
    assert!(!tracker.is_unread("a"));

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
fn focused_rows_clear_and_emit_receipts_once() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    let cleared = observe(
        &mut tracker,
        vec![row("a", AgentStatus::Success)],
        Some("a"),
    );
    assert!(!tracker.is_unread("a"));
    assert_eq!(cleared, vec!["a"]);
    let cleared_again = observe_with_marks(
        &mut tracker,
        vec![row("a", AgentStatus::Success)],
        Some("a"),
        &ReadMarks::from_entries([("a".to_owned(), fixed_now().as_millisecond())]),
    );
    assert!(
        cleared_again.is_empty(),
        "the folded-back receipt suppresses duplicate writes"
    );

    let stamp = Timestamp::from_second(1_700_000_100).unwrap();
    let mut tracker = UnreadTracker::default();

    let cleared = observe(
        &mut tracker,
        vec![row_at("a", AgentStatus::Success, stamp)],
        Some("a"),
    );

    assert_eq!(
        cleared,
        vec!["a"],
        "a newly attached focused renderer still publishes the cross-tab clear"
    );
    assert!(!tracker.is_unread("a"));

    let cleared_again = observe_with_marks(
        &mut tracker,
        vec![row_at("a", AgentStatus::Success, stamp)],
        Some("a"),
        &ReadMarks::from_entries([("a".to_owned(), stamp.as_millisecond())]),
    );
    assert!(
        cleared_again.is_empty(),
        "an existing receipt suppresses duplicate write churn"
    );
}

#[test]
fn departed_rows_and_receipts_only_clear_matching_current_episodes() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(tracker.is_unread("a"));
    observe(&mut tracker, vec![row("b", AgentStatus::Idle)], None);
    assert!(!tracker.is_unread("a"));

    let stamp = Timestamp::from_second(1_700_000_100).unwrap();
    let older = stamp.as_millisecond() - 1;
    let exact = stamp.as_millisecond();

    let mut tracker = UnreadTracker::default();
    observe(
        &mut tracker,
        vec![row_at("a", AgentStatus::Running, fixed_now())],
        None,
    );
    observe_with_marks(
        &mut tracker,
        vec![row_at("a", AgentStatus::Success, stamp)],
        None,
        &ReadMarks::from_entries([("a".to_owned(), older)]),
    );
    assert!(
        tracker.is_unread("a"),
        "an older receipt must not clear a newer episode"
    );

    observe_with_marks(
        &mut tracker,
        vec![row_at("a", AgentStatus::Success, stamp)],
        None,
        &ReadMarks::from_entries([("a".to_owned(), exact)]),
    );
    assert!(
        !tracker.is_unread("a"),
        "a receipt at the episode stamp clears it"
    );

    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Running)], None);
    observe_with_marks(
        &mut tracker,
        vec![row("a", AgentStatus::Success)],
        None,
        &ReadMarks::from_entries([("other".to_owned(), i64::MAX)]),
    );

    assert!(tracker.is_unread("a"));
}
