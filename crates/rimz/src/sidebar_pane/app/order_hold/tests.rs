use super::*;
use crate::sidebar_pane::app::fixtures::{pane, snapshot, workspace};
use crate::{
    AgentCard, ProcessCard, RowCard, SidebarRow, SidebarStatusCount, SidebarWorktreeGroup,
    SidebarWorktreeKind, agents::AgentStatus,
};

fn row(id: &str, raw_pane: &str) -> SidebarRow {
    SidebarRow {
        id: id.to_owned(),
        name: id.to_owned(),
        pane: Some(pane(raw_pane, "tab_0", false)),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: jiff::Timestamp::from_second(1).expect("fixed timestamp"),
        card: RowCard::Process(ProcessCard::default()),
    }
}

fn group(key: &str, rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
    SidebarWorktreeGroup {
        key: key.to_owned(),
        label: key.to_owned(),
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        status_counts: Vec::<SidebarStatusCount>::new(),
        rows,
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
        pr_ci: None,
        pr_number: None,
        pr_url: None,
    }
}

fn agent_row(id: &str, raw_pane: &str, status: AgentStatus) -> SidebarRow {
    SidebarRow {
        card: RowCard::Agent(Box::new(AgentCard {
            status,
            ..AgentCard::default()
        })),
        ..row(id, raw_pane)
    }
}

fn snapshot_with_groups(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
    let ws = workspace();
    let mut current = snapshot(&ws);
    current.worktree_groups = groups;
    current
}

fn group_keys(current: &SidebarSnapshot) -> Vec<&str> {
    current
        .worktree_groups
        .iter()
        .map(|group| group.key.as_str())
        .collect()
}

fn row_ids<'a>(current: &'a SidebarSnapshot, group_key: &str) -> Vec<&'a str> {
    current
        .worktree_groups
        .iter()
        .find(|group| group.key == group_key)
        .expect("group exists")
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect()
}

fn frozen_row(id: &str, raw_pane: &str) -> FrozenRow {
    FrozenRow {
        id: id.to_owned(),
        pane: Some(pane(raw_pane, "tab_0", false).pane_id.to_string()),
    }
}

fn frozen_paneless_row(id: &str) -> FrozenRow {
    FrozenRow {
        id: id.to_owned(),
        pane: None,
    }
}

fn frozen_row_ids(order: &FrozenOrder) -> Vec<&str> {
    order.rows.iter().map(|row| row.id.as_str()).collect()
}

#[test]
fn focused_waiting_row_becoming_running_is_an_interaction() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Waiting)],
    )]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Running)],
    )]);

    assert!(focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn focused_running_row_staying_running_is_not_an_interaction() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Running)],
    )]);
    let current = prev.clone();

    assert!(!focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn attention_drop_on_an_unselected_row_is_ignored() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![
            agent_row("selected", "terminal_1", AgentStatus::Running),
            agent_row("other", "terminal_2", AgentStatus::Waiting),
        ],
    )]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![
            agent_row("selected", "terminal_1", AgentStatus::Running),
            agent_row("other", "terminal_2", AgentStatus::Running),
        ],
    )]);

    assert!(!focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn attention_drop_without_a_selection_is_ignored() {
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Waiting)],
    )]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Running)],
    )]);

    assert!(!focused_interaction(&prev, &current, None));
}

#[test]
fn focused_attention_row_leaving_the_snapshot_drops_attention() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Waiting)],
    )]);
    let current = snapshot_with_groups(vec![group("a", Vec::new())]);

    assert!(focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn focused_idle_row_becoming_running_is_a_prompt() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Idle)],
    )]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Running)],
    )]);

    assert!(focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn focused_running_row_materializing_is_a_prompt() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group("a", Vec::new())]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Running)],
    )]);

    assert!(focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn focused_running_row_finishing_is_not_an_interaction() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Running)],
    )]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("selected", "terminal_1", AgentStatus::Success)],
    )]);

    assert!(!focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn prompt_on_an_unselected_row_is_ignored() {
    let selected = pane("terminal_1", "tab_0", false).pane_id;
    let prev = snapshot_with_groups(vec![group(
        "a",
        vec![
            agent_row("selected", "terminal_1", AgentStatus::Idle),
            agent_row("other", "terminal_2", AgentStatus::Idle),
        ],
    )]);
    let current = snapshot_with_groups(vec![group(
        "a",
        vec![
            agent_row("selected", "terminal_1", AgentStatus::Idle),
            agent_row("other", "terminal_2", AgentStatus::Running),
        ],
    )]);

    assert!(!focused_interaction(&prev, &current, Some(&selected)));
}

#[test]
fn reorder_pipeline_splices_unknowns_at_ranked_positions() {
    let mut current = snapshot_with_groups(vec![
        group("new", vec![row("x", "terminal_9")]),
        group(
            "b",
            vec![
                row("b2", "terminal_4"),
                row("b1", "terminal_3"),
                row("b-new", "terminal_5"),
            ],
        ),
        group("a", vec![row("a2", "terminal_2"), row("a1", "terminal_1")]),
    ]);
    let mut frozen = FrozenOrder {
        groups: vec!["a".to_owned(), "b".to_owned()],
        rows: vec![
            frozen_paneless_row("a1"),
            frozen_paneless_row("a2"),
            frozen_paneless_row("b1"),
            frozen_paneless_row("b2"),
        ],
        visible: HashSet::new(),
    };

    migrate_frozen_order(&current, &mut frozen);
    admit_new_items(&current, &mut frozen);
    reorder_to_frozen(&mut current, &frozen);

    assert_eq!(group_keys(&current), vec!["new", "a", "b"]);
    assert_eq!(row_ids(&current, "a"), vec!["a1", "a2"]);
    assert_eq!(row_ids(&current, "b"), vec!["b1", "b2", "b-new"]);
    assert_eq!(row_ids(&current, "new"), vec!["x"]);
    assert_eq!(frozen.groups, vec!["new", "a", "b"]);
    assert_eq!(
        frozen_row_ids(&frozen),
        vec!["x", "a1", "a2", "b1", "b2", "b-new"]
    );
}

#[test]
fn mid_hold_group_churn_keeps_new_last_ranked_group_last() {
    let mut current = snapshot_with_groups(vec![
        group("b", vec![row("b1", "terminal_2")]),
        group("a", vec![row("a1", "terminal_1")]),
        group("new", vec![row("new1", "terminal_3")]),
    ]);
    let mut frozen = FrozenOrder {
        groups: vec!["a".to_owned(), "b".to_owned()],
        rows: Vec::new(),
        visible: HashSet::new(),
    };

    admit_new_items(&current, &mut frozen);
    reorder_to_frozen(&mut current, &frozen);

    assert_eq!(group_keys(&current), vec!["a", "b", "new"]);
    assert_eq!(frozen.groups, vec!["a", "b", "new"]);
}

#[test]
fn new_row_splices_between_ranked_neighbors() {
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![
            row("a1", "terminal_1"),
            row("a2", "terminal_2"),
            row("a3", "terminal_3"),
        ],
    )]);
    let mut frozen = FrozenOrder {
        groups: vec!["a".to_owned()],
        rows: vec![frozen_paneless_row("a1"), frozen_paneless_row("a3")],
        visible: HashSet::new(),
    };

    migrate_frozen_order(&current, &mut frozen);
    admit_new_items(&current, &mut frozen);
    reorder_to_frozen(&mut current, &frozen);

    assert_eq!(row_ids(&current, "a"), vec!["a1", "a2", "a3"]);
    assert_eq!(frozen_row_ids(&frozen), vec!["a1", "a2", "a3"]);
}

#[test]
fn new_first_ranked_row_splices_at_front() {
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![row("a-new", "terminal_2"), row("a1", "terminal_1")],
    )]);
    let mut frozen = FrozenOrder {
        groups: vec!["a".to_owned()],
        rows: vec![frozen_paneless_row("a1")],
        visible: HashSet::new(),
    };

    migrate_frozen_order(&current, &mut frozen);
    admit_new_items(&current, &mut frozen);
    reorder_to_frozen(&mut current, &frozen);

    assert_eq!(row_ids(&current, "a"), vec!["a-new", "a1"]);
    assert_eq!(frozen_row_ids(&frozen), vec!["a-new", "a1"]);
}

#[test]
fn capture_order_collects_group_keys_flat_row_ids_and_visible_ids() {
    let current = snapshot_with_groups(vec![group(
        "a",
        (0..8)
            .map(|index| row(&format!("a{index}"), &format!("terminal_{index}")))
            .collect(),
    )]);
    let ui = UiState::default();

    let order = capture_order(&current, &ui);

    assert_eq!(order.groups, vec!["a"]);
    assert_eq!(
        frozen_row_ids(&order),
        ["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"]
    );
    assert_eq!(order.rows[0].pane, Some("zellij:terminal_0".to_owned()));
    assert_eq!(
        order.visible,
        HashSet::from([
            "a0".to_owned(),
            "a1".to_owned(),
            "a2".to_owned(),
            "a3".to_owned(),
            "a4".to_owned(),
            "a5".to_owned(),
        ])
    );
}

#[test]
fn apply_order_hold_arms_holds_and_expires() {
    let mut ui = UiState {
        last_order: FrozenOrder {
            groups: vec!["a".to_owned()],
            rows: vec![frozen_paneless_row("a2"), frozen_paneless_row("a1")],
            visible: HashSet::from(["a2".to_owned(), "a1".to_owned()]),
        },
        ..UiState::default()
    };
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![row("a1", "terminal_1"), row("a2", "terminal_2")],
    )]);
    let now_ms = 1_000;

    apply_order_hold(&mut ui, &mut current, true, now_ms);

    let expires_ms = now_ms + REORDER_HOLD.as_millis() as i64;
    assert_eq!(
        ui.order_hold.as_ref().map(|hold| hold.expires_ms),
        Some(expires_ms)
    );
    assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);

    current.worktree_groups[0].rows.reverse();
    apply_order_hold(&mut ui, &mut current, false, expires_ms - 1);
    assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);

    current.worktree_groups[0].rows.reverse();
    apply_order_hold(&mut ui, &mut current, false, expires_ms);
    assert!(ui.order_hold.is_none());
    assert_eq!(row_ids(&current, "a"), vec!["a1", "a2"]);
}

#[test]
fn new_group_during_hold_stays_at_its_ranked_position_after_expiry() {
    let mut ui = UiState {
        last_order: FrozenOrder {
            groups: vec!["main".to_owned()],
            rows: vec![frozen_paneless_row("zsh")],
            visible: HashSet::from(["zsh".to_owned()]),
        },
        ..UiState::default()
    };
    let groups = || {
        vec![
            group(
                "team",
                vec![agent_row("coder", "terminal_2", AgentStatus::Idle)],
            ),
            group("main", vec![row("zsh", "terminal_1")]),
        ]
    };
    let now_ms = 1_000;
    let mut current = snapshot_with_groups(groups());

    apply_order_hold(&mut ui, &mut current, true, now_ms);

    assert_eq!(group_keys(&current), vec!["team", "main"]);
    assert_eq!(
        ui.order_hold
            .as_ref()
            .map(|hold| hold.frozen.groups.clone()),
        Some(vec!["team".to_owned(), "main".to_owned()])
    );

    let expires_ms = now_ms + REORDER_HOLD.as_millis() as i64;
    let mut current = snapshot_with_groups(groups());
    apply_order_hold(&mut ui, &mut current, false, expires_ms);

    assert!(ui.order_hold.is_none());
    assert_eq!(group_keys(&current), vec!["team", "main"]);
}

#[test]
fn adopt_shared_hold_installs_reorders_and_recaptures() {
    let mut ui = UiState::default();
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![
            row("a-new", "terminal_3"),
            row("a1", "terminal_1"),
            row("a2", "terminal_2"),
        ],
    )]);
    let order = FrozenOrder {
        groups: vec!["a".to_owned()],
        rows: vec![frozen_paneless_row("a2"), frozen_paneless_row("a1")],
        visible: HashSet::from(["a2".to_owned()]),
    };
    let mut expected_order = order.clone();
    expected_order.rows.insert(
        0,
        FrozenRow {
            id: "a-new".to_owned(),
            pane: Some("zellij:terminal_3".to_owned()),
        },
    );
    let stamp_ms = 2_000;

    adopt_shared_hold(&mut ui, &mut current, order, stamp_ms);

    assert_eq!(row_ids(&current, "a"), vec!["a-new", "a2", "a1"]);
    assert_eq!(
        ui.order_hold.as_ref().map(|hold| hold.expires_ms),
        Some(stamp_ms + REORDER_HOLD.as_millis() as i64)
    );
    assert_eq!(
        ui.order_hold.as_ref().map(|hold| &hold.frozen),
        Some(&expected_order)
    );
    assert_eq!(frozen_row_ids(&ui.last_order), vec!["a-new", "a2", "a1"]);
    assert_eq!(
        ui.last_order.visible,
        HashSet::from(["a-new".to_owned(), "a2".to_owned(), "a1".to_owned()])
    );
}

#[test]
fn order_hold_migrates_rekeyed_rows_by_pane_and_keeps_visible_slots() {
    let mut ui = UiState {
        last_order: FrozenOrder {
            groups: vec!["team".to_owned()],
            rows: vec![
                frozen_row("launch-planner", "terminal_1"),
                frozen_row("launch-coder", "terminal_2"),
                frozen_row("launch-reviewer", "terminal_3"),
            ],
            visible: HashSet::from([
                "launch-planner".to_owned(),
                "launch-coder".to_owned(),
                "launch-reviewer".to_owned(),
            ]),
        },
        ..UiState::default()
    };
    let mut current = snapshot_with_groups(vec![group(
        "team",
        vec![
            row("session-coder", "terminal_2"),
            row("session-reviewer", "terminal_3"),
            row("session-planner", "terminal_1"),
            row("new-agent", "terminal_4"),
        ],
    )]);

    apply_order_hold(&mut ui, &mut current, true, 1_000);

    assert_eq!(
        row_ids(&current, "team"),
        vec![
            "session-planner",
            "session-coder",
            "session-reviewer",
            "new-agent"
        ],
    );
    let frozen = &ui.order_hold.as_ref().expect("hold").frozen;
    assert_eq!(
        frozen_row_ids(frozen),
        vec![
            "session-planner",
            "session-coder",
            "session-reviewer",
            "new-agent"
        ],
    );
    assert_eq!(
        frozen.visible,
        HashSet::from([
            "session-planner".to_owned(),
            "session-coder".to_owned(),
            "session-reviewer".to_owned(),
        ]),
    );
}
