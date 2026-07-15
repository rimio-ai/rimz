use super::*;
use crate::sidebar_pane::app::fixtures::{pane, snapshot, snapshot_with_panes, workspace};
use crate::sidebar_pane::render;
use crate::{MuxName, WorkspaceId};
use jiff::Timestamp;

mod browse;
mod filter;
mod input;
mod reconcile;
mod tabs;

/// A browse pick of `pane`, begun while the derived baseline was `baseline`.
fn browse(pane: &PaneId, baseline: Option<&PaneId>) -> Browse {
    Browse {
        pane: pane.clone(),
        baseline_at_start: baseline.cloned(),
    }
}

/// Forward inbox step from `selected` — the `Space`/`n` walk. A readable test
/// seam over the directional [`step_attention_index`].
fn next_attention_index(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    selected: usize,
) -> Option<usize> {
    step_attention_index(snapshot, filter, &Default::default(), selected, true)
}

/// A group whose first row is a multi-line agent card (model, effort, and
/// context% set so it carries identity + description + gauge, and selecting
/// it reveals its deeper budget-bar and stats lines), followed by a
/// single-line process row. The fixture for the whole-block clickability
/// regression guard.
fn clickable_block_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    let agent = crate::SidebarRow {
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        pane: Some(pane("terminal_9", "tab_0", false)),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: Timestamp::now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status: crate::agents::AgentStatus::Running,
            phase: crate::agents::TurnPhase::Idle,
            task: Some("inspect auth".to_owned()),
            model: Some("Opus".to_owned()),
            effort: Some("high".to_owned()),
            context_pct: Some(38),
            total_tokens: Some(12_400),
            ..crate::AgentCard::default()
        })),
    };
    let process = crate::SidebarRow {
        id: "terminal_10".to_owned(),
        name: "zsh".to_owned(),
        pane: Some(pane("terminal_10", "tab_0", false)),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: Timestamp::now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    };
    snapshot.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: vec![crate::SidebarStatusCount {
            status: crate::agents::AgentStatus::Running,
            count: 1,
        }],
        rows: vec![agent, process],
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
        pr_number: None,
    }];
    snapshot
}

/// Lay out `snapshot` at a generous size through the real render path,
/// returning the freshly-composed hit-test map — the same map the live draw
/// stores on `UiState`. Width/height are wide and tall enough that nothing
/// the tests probe is clipped.
fn line_map_for(snapshot: &SidebarSnapshot, selected: usize) -> Vec<Option<usize>> {
    let mut ui = UiState {
        selected_index: selected,
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };
    let theme = ui.theme(&snapshot.theme);
    render::compose_lines(snapshot, None, &ui, theme.as_ref(), 54, 64).line_map
}

/// The screen row a content-line index maps to: borderless, the body fills
/// the frame from row 0, so map index `i` is screen row `i`.
fn screen_row_for(map_index: usize) -> u16 {
    u16::try_from(map_index).unwrap()
}

// ── The dashboard tab model ──────────────────────────────────────────────────

/// A minimal provider panel — only `kind` matters to the tab model.
fn provider(kind: &str) -> crate::SidebarProviderPanel {
    crate::SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        color: 7,
        color_rgb: None,
        color_role: None,
        version: None,
        plan: None,
        metered: false,
        remote_control: Default::default(),
        active_sessions: 0,
        spending: None,
        day_budget: None,
        extra_credits: None,
        reset_credits: None,
        windows: Vec::new(),
    }
}

/// The clickable-block room (a claude agent row, then a process row) with a
/// three-account dashboard — the tab-model fixture.
fn tabbed_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = clickable_block_snapshot(ws);
    snapshot.providers = vec![provider("claude"), provider("codex"), provider("pi")];
    snapshot
}

/// One agent or process row bound to `pane_name` in `worktree` — the make-up
/// filter tests' row builder.
fn filter_row(
    is_agent: bool,
    id: &str,
    name: &str,
    status: Option<crate::agents::AgentStatus>,
    pane_name: &str,
    worktree: &str,
) -> crate::SidebarRow {
    crate::SidebarRow {
        id: id.to_owned(),
        name: name.to_owned(),
        pane: Some(pane(pane_name, "tab_0", false)),
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: Timestamp::now(),
        card: if is_agent {
            crate::RowCard::Agent(Box::new(crate::AgentCard {
                status: status.unwrap_or(crate::agents::AgentStatus::Idle),
                phase: crate::agents::TurnPhase::Idle,
                ..crate::AgentCard::default()
            }))
        } else {
            crate::RowCard::Process(crate::ProcessCard::default())
        },
    }
}

/// Two worktrees and a process tail — the make-up filter fixture: a running
/// agent (`terminal_1`) and a `zsh` process row (`terminal_2`) in `main`, a
/// failed agent (`terminal_3`) in `feature`. Failed and running each read 1
/// in the cockpit; waiting reads 0.
fn filterable_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    use crate::agents::AgentStatus;
    let mut snapshot = snapshot(ws);
    snapshot.worktree_groups = vec![
        crate::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: crate::SidebarWorktreeKind::Worktree,
            status_counts: vec![crate::SidebarStatusCount {
                status: AgentStatus::Running,
                count: 1,
            }],
            rows: vec![
                filter_row(
                    true,
                    "agent-1",
                    "claude",
                    Some(AgentStatus::Running),
                    "terminal_1",
                    "/repo/main",
                ),
                filter_row(false, "terminal_2", "zsh", None, "terminal_2", "/repo/main"),
            ],
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
            pr_number: None,
        },
        crate::SidebarWorktreeGroup {
            key: "/repo/feature".to_owned(),
            label: "feature".to_owned(),
            kind: crate::SidebarWorktreeKind::Worktree,
            status_counts: vec![crate::SidebarStatusCount {
                status: AgentStatus::Failed,
                count: 1,
            }],
            rows: vec![filter_row(
                true,
                "agent-2",
                "codex",
                Some(AgentStatus::Failed),
                "terminal_3",
                "/repo/feature",
            )],
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
            pr_number: None,
        },
    ];
    snapshot
}
