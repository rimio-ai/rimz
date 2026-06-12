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
        inactive: false,
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
fn transitions_into_needs_a_look_stamp_unread() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Success)], None);
    assert!(!tracker.is_unread("a"));

    for (from, status) in [
        (AgentStatus::Running, AgentStatus::Success),
        (AgentStatus::Running, AgentStatus::Failed),
        (AgentStatus::Running, AgentStatus::Waiting),
        (AgentStatus::Running, AgentStatus::Paused),
    ] {
        let mut tracker = UnreadTracker::default();
        observe(&mut tracker, vec![row("a", from)], None);
        observe(&mut tracker, vec![row("a", status)], None);
        assert!(tracker.is_unread("a"), "{from:?} -> {status:?}");
    }
}

#[test]
fn idle_to_waiting_stamps() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Idle)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Waiting)], None);
    assert!(tracker.is_unread("a"));
}

#[test]
fn success_to_waiting_restamps_new_episode() {
    let first = Timestamp::from_second(1_700_000_100).unwrap();
    let second = Timestamp::from_second(1_700_000_200).unwrap();
    let mut tracker = UnreadTracker::default();
    observe(
        &mut tracker,
        vec![row_at("a", AgentStatus::Running, fixed_now())],
        None,
    );
    observe(
        &mut tracker,
        vec![row_at("a", AgentStatus::Success, first)],
        None,
    );
    observe_with_marks(
        &mut tracker,
        vec![row_at("a", AgentStatus::Waiting, second)],
        None,
        &ReadMarks::from_entries([("a".to_owned(), first.as_millisecond())]),
    );
    assert!(
        tracker.is_unread("a"),
        "the newer waiting episode survives the older success receipt"
    );
}

#[test]
fn paused_to_waiting_restamps() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("a", AgentStatus::Paused)], None);
    observe(&mut tracker, vec![row("a", AgentStatus::Waiting)], None);
    assert!(tracker.is_unread("a"));
}

#[test]
fn same_status_frame_keeps_episode_stamp() {
    let first = Timestamp::from_second(1_700_000_100).unwrap();
    let second = Timestamp::from_second(1_700_000_200).unwrap();
    let mut tracker = UnreadTracker::default();
    observe(
        &mut tracker,
        vec![row_at("a", AgentStatus::Running, fixed_now())],
        None,
    );
    observe(
        &mut tracker,
        vec![row_at("a", AgentStatus::Waiting, first)],
        None,
    );
    observe_with_marks(
        &mut tracker,
        vec![row_at("a", AgentStatus::Waiting, second)],
        None,
        &ReadMarks::from_entries([("a".to_owned(), first.as_millisecond())]),
    );
    assert!(
        !tracker.is_unread("a"),
        "same-status frames do not restamp an existing episode"
    );
}

#[test]
fn fresh_tracker_baselines_attention_rows_without_stamping() {
    for status in [
        AgentStatus::Success,
        AgentStatus::Failed,
        AgentStatus::Waiting,
        AgentStatus::Paused,
    ] {
        let mut tracker = UnreadTracker::default();
        observe(&mut tracker, vec![row("a", status)], None);
        assert!(!tracker.is_unread("a"), "{status:?}");
    }
}

#[test]
fn first_seen_attention_after_seed_stamps_unread() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("seed", AgentStatus::Idle)], None);
    observe(
        &mut tracker,
        vec![
            row("seed", AgentStatus::Idle),
            row("ask", AgentStatus::Waiting),
        ],
        None,
    );
    assert!(tracker.is_unread("ask"));
}

#[test]
fn first_seen_attention_after_empty_seed_stamps_unread() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, Vec::new(), None);
    observe(&mut tracker, vec![row("ask", AgentStatus::Waiting)], None);
    assert!(tracker.is_unread("ask"));
}

#[test]
fn first_seen_calm_after_seed_stays_read() {
    let mut tracker = UnreadTracker::default();
    observe(&mut tracker, vec![row("seed", AgentStatus::Idle)], None);
    observe(
        &mut tracker,
        vec![
            row("seed", AgentStatus::Idle),
            row("new", AgentStatus::Idle),
        ],
        None,
    );
    assert!(!tracker.is_unread("new"));
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
