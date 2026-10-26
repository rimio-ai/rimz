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

fn paint(ui: &mut ViewState, pane_ui: &mut UiState, snapshot: &SidebarSnapshot, height: u16) {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(54, height)).unwrap();
    render::draw_to_terminal(&mut terminal, snapshot, None, ui, pane_ui).unwrap();
}

fn remember_roster(ui: &mut ViewState, pane_ui: &mut UiState, snapshot: &SidebarSnapshot) {
    pane_ui.painted_roster = ui
        .visible_roster(snapshot)
        .rows()
        .iter()
        .map(|row| row.id.clone())
        .collect();
}

#[test]
fn inbox_walk_targets_the_waiting_launched_child_only_for_lifted_attention() {
    use crate::agents::AgentStatus;
    for (native, child_pane, own_status, target_child) in [
        (
            false,
            Some("zellij:terminal_child"),
            AgentStatus::Idle,
            true,
        ),
        (true, None, AgentStatus::Idle, false),
        (false, None, AgentStatus::Idle, false),
        (
            false,
            Some("zellij:terminal_child"),
            AgentStatus::Waiting,
            false,
        ),
        (
            false,
            Some("zellij:terminal_child"),
            AgentStatus::Failed,
            false,
        ),
    ] {
        let mut snapshot = clickable_block_snapshot(&workspace());
        let now = snapshot.now;
        let row = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0];
        let card = row.as_agent_mut().unwrap();
        card.status = own_status;
        card.sub_agents = vec![
            serde_json::from_value(serde_json::json!({
                "id": "child", "kind": "claude", "name": "child", "status": "waiting",
                "last_activity": now, "provider_native": native, "pane": child_pane,
            }))
            .unwrap(),
        ];
        let expected = if target_child {
            child_pane.unwrap().parse().unwrap()
        } else {
            snapshot.worktree_groups[0].rows[0]
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .clone()
        };
        for action in [KeyAction::InboxNext, KeyAction::InboxPrev] {
            assert_eq!(
                handle_key(
                    action,
                    &mut ViewState::default(),
                    &mut UiState::default(),
                    &snapshot
                ),
                InputOutcome::focus(expected.clone()),
                "native={native}, pane={child_pane:?}, own={own_status:?}"
            );
        }
    }
}

#[test]
fn inbox_walk_advances_after_focusing_the_first_current_waiting_child() {
    let mut snapshot = clickable_block_snapshot(&workspace());
    let now = snapshot.now;
    let rows = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    );
    let mut next = rows[0].clone();
    next.id = "next-agent".into();
    next.pane = Some(pane("terminal_11", "tab_0", false));
    next.last_activity = now + std::time::Duration::from_secs(1);
    next.as_agent_mut().unwrap().status = crate::agents::AgentStatus::Waiting;
    let card = rows[0].as_agent_mut().unwrap();
    card.status = crate::agents::AgentStatus::Idle;
    card.sub_agents = [
        ("old", true),
        ("paneless", false),
        ("first", false),
        ("second", false),
    ]
    .into_iter()
    .map(|(id, prior)| {
        serde_json::from_value(serde_json::json!({
            "id": id, "kind": "claude", "name": id, "status": "waiting", "last_activity": now,
            "pane": (id != "paneless").then(|| format!("zellij:terminal_{id}")),
            "prior_turn": prior,
        }))
        .unwrap()
    })
    .collect();
    rows[0].last_activity = now;
    rows.push(next);
    let child: PaneId = "zellij:terminal_first".parse().unwrap();
    let mut ui = ViewState::default();
    let mut pane_ui = UiState::default();
    assert_eq!(
        handle_key(KeyAction::InboxNext, &mut ui, &mut pane_ui, &snapshot),
        InputOutcome::focus(child.clone())
    );
    snapshot.focused_pane = Some(child.clone());
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![child],
        own_view_is_daemon: false,
    });
    let sidebar = PaneId::from_parts(MuxName::Zellij, "sidebar_test");
    let baseline = crate::sidebar_pane::app::state::session_focus_baseline(&snapshot, &sidebar);
    reconcile_selection(&mut ui, &snapshot, baseline);
    assert_eq!(ui.selected_index, Some(0));
    assert_eq!(
        handle_key(KeyAction::InboxNext, &mut ui, &mut pane_ui, &snapshot),
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_11"))
    );
}

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
    filter: &BodyLens,
    selected: usize,
) -> Option<usize> {
    step_attention_index(snapshot, filter, &Default::default(), selected, true)
}

fn roster_len(
    snapshot: &SidebarSnapshot,
    filter: &BodyLens,
    expanded_groups: &std::collections::BTreeSet<String>,
) -> usize {
    VisibleRoster::new(snapshot, filter, expanded_groups, None).len()
}

/// A group whose first row is a multi-line agent card (model, effort, and
/// context% set so it carries identity + description + gauge, and selecting
/// it reveals its deeper budget-bar and stats lines), followed by a
/// single-line process row. The fixture for the whole-block clickability
/// regression guard.
fn clickable_block_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.panes_produced_at_ms = Some(1);
    let agent = crate::store::snapshot::SidebarRow {
        checkout: None,
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
        card: crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
            crate::store::snapshot::AgentCard {
                status: crate::agents::AgentStatus::Running,
                phase: crate::agents::TurnPhase::Idle,
                task: Some("inspect auth".to_owned()),
                model: Some("Opus".to_owned()),
                effort: Some("high".to_owned()),
                usage: crate::agents::AgentUsageSummary {
                    context_pct: Some(38),
                    total_tokens: Some(12_400),
                    ..crate::agents::AgentUsageSummary::default()
                },
                ..crate::store::snapshot::AgentCard::default()
            },
        )),
    };
    let process = crate::store::snapshot::SidebarRow {
        checkout: None,
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
        card: crate::store::snapshot::RowCard::Process(
            crate::store::snapshot::ProcessCard::default(),
        ),
    };
    snapshot.worktree_groups = vec![crate::store::snapshot::SidebarWorktreeGroup {
        pr_stack: Default::default(),
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        status_counts: vec![crate::store::snapshot::SidebarStatusCount {
            status: crate::agents::AgentStatus::Running,
            count: 1,
        }],
        rows: vec![agent, process].into(),
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

/// Lay out `snapshot` at a generous size through the real render path,
/// returning the freshly-composed typed interactions — the same projection
/// the live draw stores on `UiState`.
fn interactions_for(snapshot: &SidebarSnapshot, selected: usize) -> render::FrameInteractions {
    let ui = render::ViewState {
        selected_index: Some(selected),
        help_visible: false,
        animation_phase: 0,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let theme = pane_ui.theme(&snapshot.theme);
    render::compose_lines(snapshot, None, &ui, &pane_ui, theme.as_ref(), 54, 64).interactions
}

/// The screen row a content-line index maps to: borderless, the body fills
/// the frame from row 0, so map index `i` is screen row `i`.
fn screen_row_for(map_index: usize) -> u16 {
    u16::try_from(map_index).unwrap()
}

fn row_index_at_screen_position(ui: &UiState, row: u16) -> Option<usize> {
    match ui.interactions.target_at(0, row) {
        Some(HitTarget::Row(ordinal)) => Some(ordinal),
        _ => None,
    }
}

// ── The dashboard tab model ──────────────────────────────────────────────────

/// A minimal provider panel — only `kind` matters to the tab model.
fn provider(kind: &str) -> crate::store::snapshot::SidebarProviderPanel {
    crate::store::snapshot::SidebarProviderPanel {
        account: Default::default(),
        kind: kind.to_owned(),
        account_scope: Default::default(),
        entitlement: Default::default(),
        account_key: None,
        product_name: kind.to_owned(),
        art: Vec::new(),
        art_tints: Vec::new(),
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
        month_budget: None,
        extra_credits: None,
        reset_credits: None,
        redeem_forecast: None,
        window_placeholders: Vec::new(),
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
) -> crate::store::snapshot::SidebarRow {
    crate::store::snapshot::SidebarRow {
        checkout: None,
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
            crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
                crate::store::snapshot::AgentCard {
                    status: status.unwrap_or(crate::agents::AgentStatus::Idle),
                    phase: crate::agents::TurnPhase::Idle,
                    ..crate::store::snapshot::AgentCard::default()
                },
            ))
        } else {
            crate::store::snapshot::RowCard::Process(crate::store::snapshot::ProcessCard::default())
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
    snapshot.panes_produced_at_ms = Some(1);
    snapshot.worktree_groups = vec![
        crate::store::snapshot::SidebarWorktreeGroup {
            pr_stack: Default::default(),
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            label_qualifier: None,
            kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
            status_counts: vec![crate::store::snapshot::SidebarStatusCount {
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
            ]
            .into(),
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
        },
        crate::store::snapshot::SidebarWorktreeGroup {
            pr_stack: Default::default(),
            key: "/repo/feature".to_owned(),
            label: "feature".to_owned(),
            label_qualifier: None,
            kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
            status_counts: vec![crate::store::snapshot::SidebarStatusCount {
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
            )]
            .into(),
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
        },
    ]
    .into();
    snapshot
}
