use super::*;
use crate::app::fixtures::{snapshot, workspace};
use jiff::Timestamp;

#[test]
fn tick_for_honours_above_two_seconds() {
    assert_eq!(tick_for(5), Duration::from_secs(5));
}

#[test]
fn tick_for_clamps_zero_to_one() {
    assert_eq!(tick_for(0), Duration::from_secs(1));
}

#[test]
fn frame_grid_advances_one_frame_when_on_time() {
    let base = Instant::now();
    // Painted at the scheduled boundary: the next boundary is exactly one
    // frame later, holding the fixed cadence.
    assert_eq!(
        next_frame_after(base, base, ANIMATION_FRAME),
        base + ANIMATION_FRAME
    );
}

#[test]
fn frame_grid_snaps_forward_when_behind() {
    let base = Instant::now();
    // Scheduled several frames in the past relative to `now`: rather than
    // replaying every missed boundary, the grid snaps to one frame ahead of
    // `now`, so a slow paint never spirals into a burst of catch-up paints.
    let now = base + ANIMATION_FRAME * 5;
    assert_eq!(
        next_frame_after(base, now, ANIMATION_FRAME),
        now + ANIMATION_FRAME
    );
}

#[test]
fn frame_interval_slows_cosmetic_animation_only() {
    let ws = workspace();
    let mut slow = snapshot(&ws);
    slow.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Agent,
            id: "claude-1".to_owned(),
            name: "claude".to_owned(),
            status: Some(rimz::feed::AgentStatus::Waiting),
            phase: rimz::agents::TurnPhase::Idle,
            pane: None,
            request_id: None,
            surface: None,
            task: Some("allow cargo fmt".to_owned()),
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
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            registered_at: None,
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            process_active: false,
            command_detail: None,
            compacting: false,
            turn_error_label: None,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];

    assert_eq!(
        frame_interval(&slow, &UiState::default()),
        SLOW_ANIMATION_FRAME
    );

    slow.worktree_groups[0].rows[0].status = Some(rimz::feed::AgentStatus::Running);
    assert_eq!(frame_interval(&slow, &UiState::default()), ANIMATION_FRAME);
}

#[test]
fn heartbeat_write_due_on_first_or_aged_write_only() {
    assert!(heartbeat_write_due(None));
    assert!(!heartbeat_write_due(Some(Instant::now())));
    assert!(heartbeat_write_due(Some(
        Instant::now() - HEARTBEAT_WRITE_INTERVAL
    )));
}
