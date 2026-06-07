use super::*;
use crate::sidebar_renderer::app::fixtures::{snapshot, workspace};
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
    let frame = crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS);
    // Painted at the scheduled boundary: the next boundary is exactly one
    // frame later, holding the fixed cadence.
    assert_eq!(next_frame_after(base, base, frame), base + frame);
}

#[test]
fn frame_grid_snaps_forward_when_behind() {
    let base = Instant::now();
    let frame = crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS);
    // Scheduled several frames in the past relative to `now`: rather than
    // replaying every missed boundary, the grid snaps to one frame ahead of
    // `now`, so a slow paint never spirals into a burst of catch-up paints.
    let now = base + frame * 5;
    assert_eq!(next_frame_after(base, now, frame), now + frame);
}

#[test]
fn frame_interval_slows_cosmetic_animation_only() {
    let ws = workspace();
    let mut slow = snapshot(&ws);
    slow.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![crate::SidebarRow {
            id: "claude-1".to_owned(),
            name: "claude".to_owned(),
            pane: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            card: crate::RowCard::Agent(Box::new(crate::AgentCard {
                status: Some(crate::feed::AgentStatus::Waiting),
                phase: crate::agents::TurnPhase::Idle,
                task: Some("allow cargo fmt".to_owned()),
                ..crate::AgentCard::default()
            })),
        }],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];

    assert_eq!(
        frame_interval(&slow, &UiState::default()),
        crate::sidebar::timing::SLOW_ANIMATION_FRAME
    );

    slow.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = Some(crate::feed::AgentStatus::Running);
    assert_eq!(
        frame_interval(&slow, &UiState::default()),
        crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS)
    );
}

#[test]
fn heartbeat_write_due_on_first_or_aged_write_only() {
    assert!(heartbeat_write_due(None));
    assert!(!heartbeat_write_due(Some(Instant::now())));
    assert!(heartbeat_write_due(Some(
        Instant::now() - HEARTBEAT_WRITE_INTERVAL
    )));
}
