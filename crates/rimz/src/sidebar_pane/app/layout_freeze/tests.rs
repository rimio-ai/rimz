use super::*;
use crate::sidebar::timing::LAYOUT_FREEZE;

#[test]
fn arrivals_append_within_the_agent_and_process_partitions() {
    let mut ui = UiState::default();
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![
            agent_row("agent-a", "terminal_1", AgentStatus::Running),
            row("shell", "terminal_2"),
            row("build", "terminal_3"),
        ],
    )]);
    arm_freeze(&mut ui, 1_000);
    ui.freeze.as_mut().unwrap().frozen = capture_order(&current, &ui);
    std::sync::Arc::make_mut(&mut current.worktree_groups)[0].rows = vec![
        agent_row("agent-b", "terminal_4", AgentStatus::Waiting),
        agent_row("agent-a", "terminal_1", AgentStatus::Running),
        row("new-process", "terminal_5"),
        row("shell", "terminal_2"),
        row("build", "terminal_3"),
    ]
    .into();
    apply_freeze(&mut ui, &mut current, false, 1_001, false);
    assert_eq!(
        row_ids(&current, "a"),
        ["agent-a", "agent-b", "shell", "build", "new-process"]
    );
}

#[test]
fn freeze_deadline_counts_from_arming_for_both_focus_states() {
    let freeze = LayoutFreeze {
        frozen: FrozenOrder::default(),
        armed_ms: 1_000,
    };
    assert_eq!(freeze.deadline_ms(false), 6_000);
    assert_eq!(freeze.deadline_ms(true), 31_000);
}

#[test]
fn freeze_releases_at_the_deadline_including_parked_ties() {
    for (parked, deadline) in [(false, 6_000), (true, 31_000)] {
        let mut ui = UiState::default();
        let mut current = snapshot_with_groups(vec![group("a", vec![row("a1", "terminal_1")])]);
        arm_freeze(&mut ui, 1_000);
        apply_freeze(&mut ui, &mut current, false, deadline - 1, parked);
        assert!(
            ui.freeze.is_some(),
            "holds through deadline - 1 (parked={parked})"
        );
        apply_freeze(&mut ui, &mut current, false, deadline, parked);
        assert!(
            ui.freeze.is_none(),
            "releases at the exact deadline (parked={parked})"
        );
    }
}
use crate::agents::AgentStatus;
use crate::sidebar_pane::app::fixtures::{pane, snapshot, workspace};
use crate::store::snapshot::{
    AgentCard, ProcessCard, RowCard, SidebarRow, SidebarStatusCount, SidebarWorktreeGroup,
    SidebarWorktreeKind,
};

fn row(id: &str, raw_pane: &str) -> SidebarRow {
    SidebarRow {
        checkout: None,
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
        pr_stack: Default::default(),
        key: key.to_owned(),
        label: key.to_owned(),
        label_qualifier: None,
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        status_counts: Vec::<SidebarStatusCount>::new(),
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
    }
}

fn agent_row(id: &str, raw_pane: &str, status: AgentStatus) -> SidebarRow {
    SidebarRow {
        card: RowCard::Agent(std::sync::Arc::new(AgentCard {
            status,
            ..AgentCard::default()
        })),
        ..row(id, raw_pane)
    }
}

fn snapshot_with_groups(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
    let ws = workspace();
    let mut current = snapshot(&ws);
    current.panes_produced_at_ms = Some(1);
    current.worktree_groups = groups.into();
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
fn already_frozen_order_keeps_groups_and_rows_shared() {
    let original = snapshot_with_groups(vec![
        group("a", vec![row("a1", "terminal_1"), row("a2", "terminal_2")]),
        group("b", vec![row("b1", "terminal_3")]),
    ]);
    let frozen = capture_order(&original, &UiState::default());
    let mut current = original.clone();
    reorder_to_frozen(&mut current, &frozen);
    assert!(std::sync::Arc::ptr_eq(
        &current.worktree_groups,
        &original.worktree_groups
    ));
    for (current, original) in current
        .worktree_groups
        .iter()
        .zip(original.worktree_groups.iter())
    {
        assert!(std::sync::Arc::ptr_eq(&current.rows, &original.rows));
    }
}

#[test]
fn frozen_reorder_detaches_only_changed_rows() {
    let original = snapshot_with_groups(vec![
        group("a", vec![row("a2", "terminal_2"), row("a1", "terminal_1")]),
        group("b", vec![row("b1", "terminal_3")]),
    ]);
    let mut frozen = capture_order(&original, &UiState::default());
    frozen.rows.swap(0, 1);
    let mut current = original.clone();
    reorder_to_frozen(&mut current, &frozen);
    assert_eq!(row_ids(&current, "a"), ["a1", "a2"]);
    assert!(!std::sync::Arc::ptr_eq(
        &current.worktree_groups,
        &original.worktree_groups
    ));
    assert!(!std::sync::Arc::ptr_eq(
        &current.worktree_groups[0].rows,
        &original.worktree_groups[0].rows
    ));
    assert!(std::sync::Arc::ptr_eq(
        &current.worktree_groups[1].rows,
        &original.worktree_groups[1].rows
    ));
}

#[test]
fn frozen_reorder_matches_stable_sort_across_partitions_and_arrivals() {
    for reverse_groups in [false, true] {
        for reverse_rows in [false, true] {
            let mut groups = Vec::new();
            for (key, finished, external) in [
                ("live", false, false),
                ("finished", true, false),
                ("external", false, true),
                ("external-finished", true, true),
                ("arrival-1", false, false),
                ("arrival-2", false, false),
            ] {
                let mut rows = vec![
                    agent_row("known-agent", "terminal_1", AgentStatus::Running),
                    agent_row("arrival-a", "terminal_2", AgentStatus::Running),
                    agent_row("arrival-b", "terminal_3", AgentStatus::Running),
                    row("known-process", "terminal_4"),
                    row("arrival-c", "terminal_5"),
                    row("arrival-d", "terminal_6"),
                ];
                if reverse_rows {
                    rows.reverse();
                }
                let mut group = group(key, rows);
                group.finished = finished;
                if external {
                    group.kind = SidebarWorktreeKind::External;
                }
                groups.push(group);
            }
            if reverse_groups {
                groups.reverse();
            }
            let mut current = snapshot_with_groups(groups);
            let frozen = FrozenOrder {
                groups: ["external-finished", "external", "finished", "live"]
                    .map(str::to_owned)
                    .into(),
                rows: vec![
                    frozen_paneless_row("known-process"),
                    frozen_paneless_row("known-agent"),
                ],
                ..FrozenOrder::default()
            };
            let mut expected = current.clone();
            std::sync::Arc::make_mut(&mut expected.worktree_groups).sort_by_key(|group| {
                (
                    group.kind == SidebarWorktreeKind::External,
                    group.finished,
                    frozen
                        .groups
                        .iter()
                        .position(|key| key == &group.key)
                        .unwrap_or(usize::MAX),
                )
            });
            for group in std::sync::Arc::make_mut(&mut expected.worktree_groups) {
                std::sync::Arc::make_mut(&mut group.rows).sort_by_key(|row| {
                    (
                        row.is_process(),
                        frozen
                            .rows
                            .iter()
                            .position(|known| known.id == row.id)
                            .unwrap_or(usize::MAX),
                    )
                });
            }
            reorder_to_frozen(&mut current, &frozen);
            assert_eq!(current.worktree_groups, expected.worktree_groups);
        }
    }
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
fn reorder_pipeline_appends_unknowns_after_frozen_items() {
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

    assert_eq!(group_keys(&current), vec!["a", "b", "new"]);
    assert_eq!(row_ids(&current, "a"), vec!["a1", "a2"]);
    assert_eq!(row_ids(&current, "b"), vec!["b1", "b2", "b-new"]);
    assert_eq!(row_ids(&current, "new"), vec!["x"]);
    assert_eq!(frozen.groups, vec!["a", "b", "new"]);
    assert_eq!(
        frozen_row_ids(&frozen),
        vec!["a1", "a2", "b1", "b2", "x", "b-new"]
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
fn new_row_appends_after_frozen_neighbors() {
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

    assert_eq!(row_ids(&current, "a"), vec!["a1", "a3", "a2"]);
    assert_eq!(frozen_row_ids(&frozen), vec!["a1", "a3", "a2"]);
}

#[test]
fn new_first_ranked_row_appends_at_tail() {
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

    assert_eq!(row_ids(&current, "a"), vec!["a1", "a-new"]);
    assert_eq!(frozen_row_ids(&frozen), vec!["a1", "a-new"]);
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
fn apply_freeze_arms_holds_and_expires() {
    let mut ui = UiState {
        view: crate::sidebar_pane::render::ViewState {
            last_order: FrozenOrder {
                groups: vec!["a".to_owned()],
                rows: vec![frozen_paneless_row("a2"), frozen_paneless_row("a1")],
                visible: HashSet::from(["a2".to_owned(), "a1".to_owned()]),
            },
            ..Default::default()
        },
        ..UiState::default()
    };
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![row("a1", "terminal_1"), row("a2", "terminal_2")],
    )]);
    let now_ms = 1_000;

    apply_freeze(&mut ui, &mut current, true, now_ms, false);

    let expires_ms = now_ms + LAYOUT_FREEZE.as_millis() as i64;
    assert_eq!(
        ui.freeze.as_ref().map(|hold| hold.deadline_ms(false)),
        Some(expires_ms)
    );
    assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);

    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut current.worktree_groups)[0].rows)
        .reverse();
    apply_freeze(&mut ui, &mut current, false, expires_ms - 1, false);
    assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);

    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut current.worktree_groups)[0].rows)
        .reverse();
    apply_freeze(&mut ui, &mut current, false, expires_ms, false);
    assert!(ui.freeze.is_none());
    assert_eq!(row_ids(&current, "a"), vec!["a1", "a2"]);
}

#[test]
fn freeze_preserves_partitions_when_a_middle_group_lands_or_revives() {
    for was_finished in [false, true] {
        let mut groups = vec![
            group("a", vec![row("a1", "terminal_1")]),
            group("b", vec![row("b1", "terminal_2")]),
            group("c", vec![row("c1", "terminal_3")]),
            group("external", vec![row("e1", "terminal_4")]),
        ];
        groups[1].finished = was_finished;
        groups[2].finished = was_finished;
        groups[3].kind = SidebarWorktreeKind::External;
        let mut current = snapshot_with_groups(groups);
        let mut ui = UiState::default();
        ui.last_order = capture_order(&current, &ui);
        apply_freeze(&mut ui, &mut current, true, 1_000, false);

        let changed = if was_finished { 2 } else { 1 };
        std::sync::Arc::make_mut(&mut current.worktree_groups)[changed].finished = !was_finished;
        apply_freeze(&mut ui, &mut current, false, 1_001, false);

        assert_eq!(group_keys(&current), vec!["a", "c", "b", "external"]);
        assert!(ui.freeze.is_some(), "the hold remains armed");
    }
}

#[test]
fn new_group_appends_during_freeze_then_takes_its_rank_after_expiry() {
    let mut ui = UiState {
        view: crate::sidebar_pane::render::ViewState {
            last_order: FrozenOrder {
                groups: vec![
                    "main".to_owned(),
                    "finished".to_owned(),
                    "external".to_owned(),
                ],
                rows: vec![frozen_paneless_row("zsh")],
                visible: HashSet::from(["zsh".to_owned()]),
            },
            ..Default::default()
        },
        ..UiState::default()
    };
    let groups = || {
        let mut finished = group("finished", vec![]);
        finished.finished = true;
        let mut external = group("external", vec![]);
        external.kind = SidebarWorktreeKind::External;
        vec![
            group(
                "team",
                vec![agent_row("coder", "terminal_2", AgentStatus::Idle)],
            ),
            group("main", vec![row("zsh", "terminal_1")]),
            finished,
            external,
        ]
    };
    let now_ms = 1_000;
    let mut current = snapshot_with_groups(groups());

    apply_freeze(&mut ui, &mut current, true, now_ms, false);

    assert_eq!(
        group_keys(&current),
        vec!["main", "team", "finished", "external"]
    );
    assert_eq!(
        ui.freeze.as_ref().map(|hold| hold.frozen.groups.clone()),
        Some(
            ["main", "finished", "external", "team"]
                .map(str::to_owned)
                .to_vec()
        )
    );

    let expires_ms = now_ms + LAYOUT_FREEZE.as_millis() as i64;
    let mut current = snapshot_with_groups(groups());
    apply_freeze(&mut ui, &mut current, false, expires_ms, false);

    assert!(ui.freeze.is_none());
    assert_eq!(
        group_keys(&current),
        vec!["team", "main", "finished", "external"]
    );
}

#[test]
fn adopt_shared_order_installs_hold_reorders_and_recaptures() {
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
    expected_order.rows.push(FrozenRow {
        id: "a-new".to_owned(),
        pane: Some("zellij:terminal_3".to_owned()),
    });
    let stamp_ms = 2_000;

    adopt_shared_order(&mut ui, &mut current, order, Some(stamp_ms));

    assert_eq!(row_ids(&current, "a"), vec!["a2", "a1", "a-new"]);
    assert_eq!(
        ui.freeze.as_ref().map(|hold| hold.deadline_ms(false)),
        Some(stamp_ms + LAYOUT_FREEZE.as_millis() as i64)
    );
    assert_eq!(
        ui.freeze.as_ref().map(|hold| &hold.frozen),
        Some(&expected_order)
    );
    assert_eq!(frozen_row_ids(&ui.last_order), vec!["a2", "a1", "a-new"]);
    assert_eq!(
        ui.last_order.visible,
        HashSet::from(["a-new".to_owned(), "a2".to_owned(), "a1".to_owned()])
    );
}

#[test]
fn freeze_migrates_rekeyed_rows_by_pane_and_keeps_visible_slots() {
    let mut ui = UiState {
        view: crate::sidebar_pane::render::ViewState {
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
            ..Default::default()
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

    apply_freeze(&mut ui, &mut current, true, 1_000, false);

    assert_eq!(
        row_ids(&current, "team"),
        vec![
            "session-planner",
            "session-coder",
            "session-reviewer",
            "new-agent"
        ],
    );
    let frozen = &ui.freeze.as_ref().expect("hold").frozen;
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
