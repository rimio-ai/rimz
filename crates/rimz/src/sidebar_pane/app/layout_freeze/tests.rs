use super::*;
use crate::sidebar::timing::LAYOUT_FREEZE;
use crate::sidebar_pane::render::ViewState;

#[test]
fn arrivals_take_their_live_rank_below_asks_and_failures() {
    let mut ui = ViewState::default();
    let mut current = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("ask", "terminal_1", AgentStatus::Waiting, 60),
            stamped_row("failed", "terminal_2", AgentStatus::Failed, 50),
            stamped_row("calm", "terminal_3", AgentStatus::Idle, 20),
        ],
    )]);
    arm_freeze(&mut ui, 1_000);
    ui.freeze.as_mut().unwrap().frozen = capture_order(&current, &ui);
    let rows = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut current.worktree_groups)[0].rows,
    );
    let mut shell = row("new-process", "terminal_4");
    shell.attention_at_ms = Some(40);
    rows.push(shell);
    rows.push(stamped_row(
        "new-agent",
        "terminal_5",
        AgentStatus::Idle,
        30,
    ));
    current.sort_groups_for_presentation();
    apply_freeze(&mut ui, &mut current, false, 1_001, false);
    assert_eq!(
        row_ids(&current, "a"),
        ["ask", "failed", "new-process", "new-agent", "calm"]
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
        let mut ui = ViewState::default();
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
        pane: Some(pane(raw_pane, "tab_0")),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        lane: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: jiff::Timestamp::from_second(1).expect("fixed timestamp"),
        card: RowCard::Process(ProcessCard::default()),
    }
}

fn group(key: &str, rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
    SidebarWorktreeGroup {
        stalled_since: None,
        pr_stack: Default::default(),
        key: key.to_owned(),
        label: key.to_owned(),
        label_qualifier: None,
        branch: None,
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        workflow: None,
        status_counts: Vec::<SidebarStatusCount>::new(),
        rows: rows.into(),
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        place: None,
        root_lane: false,
        inactive: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_queue: None,
        pr_conflicting: false,
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
        pane: Some(pane(raw_pane, "tab_0").pane_id.to_string()),
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
    let frozen = capture_order(&original, &ViewState::default());
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
    let mut frozen = capture_order(&original, &ViewState::default());
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
fn frozen_positions_preserve_interleaved_rows_and_stable_unknowns() {
    let mut current = snapshot_with_groups(vec![group(
        "a",
        vec![
            agent_row("arrival-agent", "terminal_1", AgentStatus::Running),
            row("known-process", "terminal_2"),
            agent_row("known-agent", "terminal_3", AgentStatus::Running),
            row("arrival-process", "terminal_4"),
        ],
    )]);
    let frozen = FrozenOrder {
        groups: vec!["a".to_owned()],
        rows: vec![
            frozen_paneless_row("known-process"),
            frozen_paneless_row("known-agent"),
        ],
        ..FrozenOrder::default()
    };
    reorder_to_frozen(&mut current, &frozen);
    assert_eq!(
        row_ids(&current, "a"),
        [
            "known-process",
            "known-agent",
            "arrival-agent",
            "arrival-process",
        ]
    );
}

#[test]
fn focused_status_changes_keep_the_captured_rank_without_timing_out() {
    for (before, after) in [
        (AgentStatus::Waiting, AgentStatus::Running),
        (AgentStatus::Idle, AgentStatus::Running),
        (AgentStatus::Running, AgentStatus::Waiting),
        (AgentStatus::Running, AgentStatus::Running),
        (AgentStatus::Running, AgentStatus::Success),
        (AgentStatus::Paused, AgentStatus::Running),
    ] {
        let mut previous = hold_snapshot(vec![group(
            "a",
            vec![
                stamped_row("focused", "terminal_1", before, 20),
                stamped_row("other", "terminal_2", AgentStatus::Waiting, 10),
            ],
        )]);
        let focused = pane("terminal_1", "tab_0").pane_id;
        let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
        rig.present(previous.clone(), std::slice::from_ref(&focused), false);
        let expected = row_ids(&rig.context.current, "a")
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        rig.clock.advance(std::time::Duration::from_secs(31));
        let row = std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut previous.worktree_groups)[0].rows,
        )
        .iter_mut()
        .find(|row| row.id == "focused")
        .unwrap();
        let agent = row.as_agent_mut().unwrap();
        agent.status = after;
        if before.is_actionable() != after.is_actionable() {
            row.attention_at_ms = Some(30);
        }
        rig.present(previous.clone(), std::slice::from_ref(&focused), false);
        assert!(
            rig.ui.freeze.is_none(),
            "{before:?} -> {after:?} is held, not timed"
        );
        assert_eq!(row_ids(&rig.context.current, "a"), expected);
        rig.clock.advance(std::time::Duration::from_secs(31));
        rig.present(previous, std::slice::from_ref(&focused), false);
        assert_eq!(row_ids(&rig.context.current, "a"), expected);
    }
}

#[test]
fn a_focus_entry_captures_the_previous_rank_before_this_folds_ask() {
    let previous = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("other", "terminal_2", AgentStatus::Running, 20),
            stamped_row("focused", "terminal_1", AgentStatus::Running, 10),
        ],
    )]);
    let focused = pane("terminal_1", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
    let mut next = previous;
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows,
    )[1];
    row.as_agent_mut().unwrap().status = AgentStatus::Waiting;
    row.attention_at_ms = Some(30);
    rig.present(next, &[focused], false);
    assert_eq!(row_ids(&rig.context.current, "a"), ["other", "focused"]);
}

#[test]
fn a_prompt_lifting_a_focused_cooling_card_does_not_move_it() {
    let mut cold = stamped_row("focused", "terminal_1", AgentStatus::Idle, 30);
    cold.inactive = true;
    let previous = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("other", "terminal_2", AgentStatus::Idle, 20),
            cold,
        ],
    )]);
    let focused = pane("terminal_1", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
    rig.present(previous.clone(), std::slice::from_ref(&focused), false);
    let mut next = previous;
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows,
    )[1];
    row.inactive = false;
    row.as_agent_mut().unwrap().status = AgentStatus::Running;
    rig.present(next.clone(), std::slice::from_ref(&focused), false);
    rig.clock.advance(std::time::Duration::from_secs(31));
    rig.present(next, &[focused], false);
    assert_eq!(row_ids(&rig.context.current, "a"), ["other", "focused"]);
}

#[test]
fn a_childs_ask_holds_its_focused_parent_without_freezing_other_arrivals() {
    let previous = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("other", "terminal_2", AgentStatus::Running, 20),
            stamped_row("focused", "terminal_1", AgentStatus::Running, 10),
        ],
    )]);
    let focused = pane("terminal_1", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
    rig.present(previous.clone(), std::slice::from_ref(&focused), false);
    let mut next = previous;
    let now = next.now;
    let rows =
        std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows);
    rows[1].attention_at_ms = Some(30);
    let parent = rows[1].as_agent_mut().unwrap();
    parent
        .sub_agents
        .push(crate::store::snapshot::SidebarSubAgent {
            id: "child".into(),
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            pane: None,
            prior_turn: false,
            name: "child".into(),
            petname: None,
            provider_native: true,
            status: AgentStatus::Waiting,
            stalled: false,
            compacting: false,
            phase: crate::agents::TurnPhase::Idle,
            turn_error_label: None,
            task: None,
            first_prompt: None,
            profile: None,
            model: None,
            fast: false,
            effort: None,
            description: None,
            tokens: None,
            context_window: None,
            compaction_count: 0,
            cost_usd: None,
            elapsed_secs: None,
            started_at: None,
            finished_at: None,
            last_activity: now,
            registered_at: Some(now),
        });
    rows.push(stamped_row(
        "arrival",
        "terminal_3",
        AgentStatus::Waiting,
        40,
    ));
    rig.present(next, &[focused], false);
    assert_eq!(
        row_ids(&rig.context.current, "a"),
        ["arrival", "other", "focused"]
    );
}

#[test]
fn a_focused_old_shell_becoming_an_agent_keeps_its_rank_even_during_a_freeze() {
    let previous = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("agent", "terminal_3", AgentStatus::Running, 10),
            {
                let mut row = row("shell", "terminal_1");
                row.attention_at_ms = Some(5);
                row
            },
            {
                let mut row = row("focused", "terminal_2");
                row.attention_at_ms = Some(1);
                row
            },
        ],
    )]);
    let focused = pane("terminal_2", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
    rig.present(previous.clone(), std::slice::from_ref(&focused), false);
    arm_freeze(&mut rig.ui, rig.clock.now_ms() as i64);
    let mut next = previous;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows)[2] =
        stamped_row("new-id", "terminal_2", AgentStatus::Waiting, 30);
    rig.present(next.clone(), std::slice::from_ref(&focused), false);
    assert_eq!(
        row_ids(&rig.context.current, "a"),
        ["agent", "shell", "new-id"]
    );
    rig.clock.advance(std::time::Duration::from_secs(31));
    rig.present(next, &[focused], false);
    assert_eq!(
        row_ids(&rig.context.current, "a"),
        ["agent", "shell", "new-id"]
    );
}

#[test]
fn a_fresh_shell_keeps_its_painted_index_through_launch_release_and_expiry() {
    for first_fetch in [true, false] {
        let previous = hold_snapshot(vec![group(
            "a",
            vec![
                stamped_row("ask", "terminal_1", AgentStatus::Waiting, 40),
                stamped_row("failed", "terminal_2", AgentStatus::Failed, 30),
                stamped_row("calm", "terminal_3", AgentStatus::Idle, 10),
            ],
        )]);
        let old_focus = pane("terminal_3", "tab_0").pane_id;
        let mut rig = if first_fetch {
            HoldRig::new(hold_snapshot(Vec::new()), None)
        } else {
            let mut rig = HoldRig::new(previous.clone(), Some(old_focus.clone()));
            rig.present(previous.clone(), &[old_focus], false);
            rig
        };
        let mut fresh = previous;
        let mut shell = row("shell", "terminal_4");
        shell.attention_at_ms = Some(20);
        let focused = shell.pane.as_ref().unwrap().pane_id.clone();
        std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut fresh.worktree_groups)[0].rows)
            .push(shell);
        fresh.sort_groups_for_presentation();
        rig.present(fresh.clone(), std::slice::from_ref(&focused), false);
        assert!(rig.ui.freeze.is_some(), "first fetch={first_fetch}");
        let painted_index = |rig: &HoldRig| {
            rig.ui
                .visible_roster(&rig.context.current)
                .rows()
                .iter()
                .position(|row| {
                    row.pane
                        .as_ref()
                        .is_some_and(|pane| pane.pane_id == focused)
                })
        };
        let mut positions = vec![painted_index(&rig)];
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut fresh.worktree_groups)[0].rows,
        )[2] = stamped_row("launched", "terminal_4", AgentStatus::Running, 21);
        rig.present(fresh.clone(), std::slice::from_ref(&focused), false);
        positions.push(painted_index(&rig));
        rig.present(fresh.clone(), &[], false);
        positions.push(painted_index(&rig));
        assert!(rig.ui.freeze.is_some());
        rig.clock.advance(LAYOUT_FREEZE);
        rig.present(fresh, &[], false);
        positions.push(painted_index(&rig));
        assert!(rig.ui.freeze.is_none());
        assert_eq!(positions, [Some(2); 4], "first fetch={first_fetch}");
    }
}

#[test]
fn reconciled_shell_slot_survives_launch_focus_release_and_freeze_expiry() {
    for first_fetch in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = crate::RuntimePaths::under(workspace(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let mut previous = hold_snapshot(vec![group(
            "a",
            vec![
                agent_row("newer", "terminal_1", AgentStatus::Idle),
                row("shell", "terminal_2"),
                agent_row("older", "terminal_3", AgentStatus::Idle),
            ],
        )]);
        let now = previous.now;
        for row in previous.rows_mut() {
            let age = match row.id.as_str() {
                "newer" => 1,
                "shell" => 2,
                "older" => 3,
                _ => unreachable!("the fixture has only these three rows"),
            };
            let birth = now - jiff::SignedDuration::from_secs(age);
            row.last_activity = birth;
            if let Some(agent) = row.as_agent_mut() {
                agent.registered_at = Some(birth);
            }
            row.pane.as_mut().unwrap().first_seen_at_ms =
                Some(row.last_activity.as_millisecond() as u64);
        }
        crate::sidebar::engine::reconcile_final_fold(&runtime, &mut previous);
        assert_eq!(row_ids(&previous, "a"), ["newer", "shell", "older"]);
        let focused = previous
            .rows()
            .find(|row| row.id == "shell")
            .unwrap()
            .pane
            .clone()
            .unwrap();
        let mut rig = HoldRig::new(
            if first_fetch {
                hold_snapshot(Vec::new())
            } else {
                previous.clone()
            },
            if first_fetch {
                None
            } else {
                Some(focused.pane_id.clone())
            },
        );
        rig.present(
            previous.clone(),
            std::slice::from_ref(&focused.pane_id),
            false,
        );
        let painted_index = |rig: &HoldRig| {
            rig.ui
                .visible_roster(&rig.context.current)
                .rows()
                .iter()
                .position(|row| {
                    row.pane
                        .as_ref()
                        .is_some_and(|pane| pane.pane_id == focused.pane_id)
                })
        };
        let mut positions = vec![painted_index(&rig)];
        let mut launched = agent_row("launched", "terminal_2", AgentStatus::Idle);
        launched.pane = Some(focused.clone());
        launched.last_activity = now;
        launched.as_agent_mut().unwrap().registered_at = Some(now);
        *previous.rows_mut().find(|row| row.id == "shell").unwrap() = launched;
        crate::sidebar::engine::reconcile_final_fold(&runtime, &mut previous);
        rig.present(
            previous.clone(),
            std::slice::from_ref(&focused.pane_id),
            false,
        );
        positions.push(painted_index(&rig));
        rig.present(previous.clone(), &[], false);
        positions.push(painted_index(&rig));
        assert!(rig.ui.freeze.is_some());
        rig.clock.advance(LAYOUT_FREEZE);
        rig.present(previous, &[], false);
        positions.push(painted_index(&rig));
        assert!(rig.ui.freeze.is_none());
        assert_eq!(positions, [Some(1); 4], "first fetch={first_fetch}");
    }
}

#[test]
fn a_launch_in_a_focused_group_keeps_its_group_rank_through_deferred_regrouping() {
    let mut previous = inactivity_boundary();
    let focused = pane("terminal_3", "tab_0").pane_id;
    let cooling = pane("terminal_2", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), None);
    rig.present(previous.clone(), &[focused.clone(), cooling.clone()], false);
    assert_eq!(
        group_keys(&rig.context.current),
        ["a", "c", "b", "external"]
    );
    rig.clock.advance(std::time::Duration::from_secs(1));
    previous.now = rig.clock.timestamp();
    let mut arrival = stamped_row("new", "terminal_5", AgentStatus::Running, 30);
    arrival.as_agent_mut().unwrap().registered_at = Some(previous.now);
    let focused_group = std::sync::Arc::make_mut(&mut previous.worktree_groups)
        .iter_mut()
        .find(|group| group.key == "c")
        .unwrap();
    std::sync::Arc::make_mut(&mut focused_group.rows).push(arrival);
    rig.present(previous, &[focused, cooling], false);
    assert!(
        !inactive_group(&rig.context.current, "b"),
        "every co-viewer's focus defers folding"
    );
    assert_eq!(
        group_keys(&rig.context.current),
        ["a", "c", "b", "external"]
    );
}

#[test]
fn releasing_focus_arms_the_existing_deadline_then_settles_the_card() {
    for (parked, seconds) in [(false, 5), (true, 30)] {
        let previous = hold_snapshot(vec![group(
            "a",
            vec![
                stamped_row("other", "terminal_2", AgentStatus::Running, 20),
                stamped_row("focused", "terminal_1", AgentStatus::Running, 10),
            ],
        )]);
        let focused = pane("terminal_1", "tab_0").pane_id;
        let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
        rig.present(previous.clone(), std::slice::from_ref(&focused), false);
        let mut next = previous;
        let row = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows,
        )[1];
        row.as_agent_mut().unwrap().status = AgentStatus::Waiting;
        row.attention_at_ms = Some(30);
        rig.present(next.clone(), std::slice::from_ref(&focused), false);
        rig.clock.advance(std::time::Duration::from_secs(40));
        rig.present(next.clone(), &[], parked);
        assert_eq!(
            rig.ui.freeze.as_ref().map(|hold| hold.armed_ms),
            Some(rig.clock.now_ms() as i64)
        );
        rig.clock
            .advance(std::time::Duration::from_millis(seconds * 1_000 - 1));
        rig.present(next.clone(), &[], parked);
        assert_eq!(row_ids(&rig.context.current, "a"), ["other", "focused"]);
        rig.clock.advance(std::time::Duration::from_millis(1));
        rig.present(next, &[], parked);
        assert!(rig.ui.freeze.is_none());
        assert_eq!(row_ids(&rig.context.current, "a"), ["focused", "other"]);
    }
}

#[test]
fn refocusing_before_the_release_freeze_settles_keeps_the_held_rank() {
    let mut cold = stamped_row("focused", "terminal_1", AgentStatus::Idle, 30);
    cold.inactive = true;
    let previous = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("other", "terminal_2", AgentStatus::Idle, 20),
            cold,
        ],
    )]);
    let focused = pane("terminal_1", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), Some(focused.clone()));
    rig.present(previous.clone(), std::slice::from_ref(&focused), false);
    let mut next = previous;
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows,
    )[1];
    row.inactive = false;
    row.as_agent_mut().unwrap().status = AgentStatus::Running;
    rig.present(next.clone(), std::slice::from_ref(&focused), false);
    rig.clock.advance(std::time::Duration::from_secs(1));
    rig.present(next.clone(), &[], false);
    rig.clock.advance(std::time::Duration::from_secs(1));
    rig.present(next.clone(), std::slice::from_ref(&focused), false);
    rig.clock.advance(std::time::Duration::from_secs(10));
    rig.present(next, &[focused], false);
    assert!(rig.ui.freeze.is_none());
    assert_eq!(row_ids(&rig.context.current, "a"), ["other", "focused"]);
}

#[test]
fn first_fetch_and_new_panes_capture_from_the_current_snapshot() {
    for empty in [true, false] {
        let previous = hold_snapshot(if empty {
            Vec::new()
        } else {
            vec![group(
                "a",
                vec![stamped_row("other", "terminal_2", AgentStatus::Running, 20)],
            )]
        });
        let focused = pane("terminal_1", "tab_0").pane_id;
        let mut rig = HoldRig::new(previous, Some(focused.clone()));
        let mut next = hold_snapshot(vec![group(
            "a",
            vec![
                stamped_row("other", "terminal_2", AgentStatus::Running, 20),
                stamped_row("focused", "terminal_1", AgentStatus::Running, 10),
            ],
        )]);
        rig.present(next.clone(), std::slice::from_ref(&focused), false);
        let row = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows,
        )[1];
        row.as_agent_mut().unwrap().status = AgentStatus::Waiting;
        row.attention_at_ms = Some(30);
        rig.clock.advance(std::time::Duration::from_secs(31));
        rig.present(next, &[focused], false);
        assert_eq!(
            row_ids(&rig.context.current, "a"),
            ["other", "focused"],
            "first fetch={empty}"
        );
    }
}

#[test]
fn only_focused_rows_hold_and_a_disappearing_row_releases_its_hold() {
    let previous = hold_snapshot(vec![group(
        "a",
        vec![
            stamped_row("focused", "terminal_1", AgentStatus::Waiting, 20),
            stamped_row("other", "terminal_2", AgentStatus::Idle, 10),
        ],
    )]);
    let focused = pane("terminal_1", "tab_0").pane_id;
    let mut rig = HoldRig::new(previous.clone(), None);
    rig.present(previous.clone(), std::slice::from_ref(&focused), false);
    let mut next = previous;
    let rows =
        std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0].rows);
    rows.remove(0);
    rows[0].as_agent_mut().unwrap().status = AgentStatus::Running;
    rig.clock.advance(std::time::Duration::from_secs(40));
    rig.present(next, &[focused], false);
    assert_eq!(
        rig.ui.freeze.as_ref().map(|hold| hold.armed_ms),
        Some(rig.clock.now_ms() as i64)
    );
    assert_eq!(row_ids(&rig.context.current, "a"), ["other"]);
}

fn stamped_row(id: &str, raw_pane: &str, status: AgentStatus, stamp: i64) -> SidebarRow {
    let mut row = agent_row(id, raw_pane, status);
    row.last_activity = jiff::Timestamp::from_second(100_000).unwrap();
    let birth = row.last_activity;
    let card = row.as_agent_mut().unwrap();
    card.registered_at = Some(birth);
    row.attention_at_ms = Some(stamp);
    row
}

fn hold_snapshot(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
    let mut snapshot = snapshot_with_groups(groups);
    snapshot.now = jiff::Timestamp::from_second(100_000).unwrap();
    snapshot.sort_groups_for_presentation();
    snapshot
}

struct HoldRig {
    clock: super::super::clock::LoopClock,
    ui: ViewState,
    context: super::super::view_state::ViewContext,
}

impl HoldRig {
    fn new(previous: SidebarSnapshot, selected: Option<PaneId>) -> Self {
        let clock = super::super::clock::LoopClock::play(previous.now);
        Self {
            context: super::super::view_state::ViewContext::new(previous, clock.now()),
            clock,
            ui: ViewState {
                selected_pane: selected.clone(),
                baseline_pane: selected,
                ..ViewState::default()
            },
        }
    }

    fn present(&mut self, next: SidebarSnapshot, focused: &[PaneId], parked: bool) {
        let previous = std::mem::replace(&mut self.context.current, next);
        let selected = self.ui.selected_pane.clone();
        self.context.reconcile_selection_and_order(
            &mut self.ui,
            super::super::view_state::SelectionFold {
                previous: &previous,
                selected,
                focused: focused
                    .first()
                    .filter(|first| focused.iter().all(|pane| pane == *first))
                    .cloned(),
                held_panes: focused.to_vec(),
                cleared: false,
                filter: None,
                mux: crate::MuxName::Zellij,
                clock: &self.clock,
                now: self.clock.now(),
                parked,
            },
        );
        self.ui.last_order = capture_order(&self.context.current, &self.ui);
    }
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
fn new_row_takes_its_live_place_between_frozen_neighbors() {
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
fn new_first_ranked_row_leads_frozen_rows() {
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
    let ui = ViewState::default();

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
    let mut ui = crate::sidebar_pane::render::ViewState {
        last_order: FrozenOrder {
            groups: vec!["a".to_owned()],
            rows: vec![frozen_paneless_row("a2"), frozen_paneless_row("a1")],
            visible: HashSet::from(["a2".to_owned(), "a1".to_owned()]),
        },
        ..Default::default()
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
fn first_fetch_admits_inactive_groups_while_initial_focus_arms_freeze() {
    let previous = snapshot_with_groups(Vec::new());
    let mut next = inactivity_boundary();
    next.now += std::time::Duration::from_secs(1);
    next.sort_groups_for_presentation();
    assert!(inactive_group(&next, "b"));
    let clock = super::super::clock::LoopClock::play(next.now);
    let focused = pane("terminal_1", "tab_0").pane_id;
    let mut ui = ViewState::default();

    let (presented, _, _) = reconcile_fold(next, &previous, &mut ui, &clock, Some(focused.clone()));

    assert_eq!(ui.selected_pane, Some(focused));
    assert!(ui.freeze.is_some(), "initial focus must arm the freeze");
    assert!(
        inactive_group(&presented, "b"),
        "the first fetch must preserve producer-folded groups during the initial freeze"
    );
}

#[test]
fn inactive_admission_waits_for_freeze_release_but_revival_is_immediate() {
    let previous = inactivity_boundary();
    let clock = super::super::clock::LoopClock::play(previous.now);
    let mut ui = ViewState::default();
    ui.last_order = capture_order(&previous, &ui);
    arm_freeze(&mut ui, clock.now_ms() as i64);
    clock.advance(std::time::Duration::from_secs(1));
    let mut next = previous.clone();
    next.now = clock.timestamp();
    let (held, _, expired) = reconcile_fold(next, &previous, &mut ui, &clock, None);
    assert_eq!(
        (group_keys(&held), inactive_group(&held, "b"), expired),
        (vec!["a", "c", "b", "external"], false, false),
        "time alone must not admit a group during the hold"
    );

    clock.advance(LAYOUT_FREEZE - std::time::Duration::from_secs(1));
    let mut next = held.clone();
    next.now = clock.timestamp();
    let (admitted, _, expired) = reconcile_fold(next, &held, &mut ui, &clock, None);
    assert_eq!(
        (
            group_keys(&admitted),
            inactive_group(&admitted, "b"),
            expired
        ),
        (vec!["a", "c", "external", "b"], true, true),
        "the releasing pass must admit the previously deferred group"
    );

    ui.last_order = capture_order(&admitted, &ui);
    arm_freeze(&mut ui, clock.now_ms() as i64);
    let mut next = admitted.clone();
    let group = std::sync::Arc::make_mut(&mut next.worktree_groups)
        .iter_mut()
        .find(|group| group.key == "b")
        .unwrap();
    std::sync::Arc::make_mut(&mut group.rows)[0]
        .as_agent_mut()
        .unwrap()
        .engaged_at = Some(next.now);
    let (revived, _, _) = reconcile_fold(next, &admitted, &mut ui, &clock, None);
    assert!(!inactive_group(&revived, "b"));
    assert_eq!(group_keys(&revived), ["a", "c", "b", "external"]);
    assert!(ui.freeze.is_some(), "revival does not wait for the hold");
}

#[test]
fn success_lens_survives_inactive_admission_deferred_by_the_hold() {
    let previous = inactivity_boundary();
    let clock = super::super::clock::LoopClock::play(previous.now);
    let filter = crate::sidebar::body_filter::BodyFilter::Status(AgentStatus::Success);
    let mut ui = ViewState::default();
    ui.toggles.make_up_filter.filter = Some(filter);
    ui.last_order = capture_order(&previous, &ui);
    arm_freeze(&mut ui, clock.now_ms() as i64);
    clock.advance(std::time::Duration::from_secs(1));
    let mut next = previous.clone();
    next.now = clock.timestamp();

    let (presented, persist_filter, _) = reconcile_fold(next, &previous, &mut ui, &clock, None);

    assert!(ui.freeze.is_some());
    assert!(!inactive_group(&presented, "b"));
    assert_eq!(ui.toggles.make_up_filter.filter, Some(filter));
    assert!(!persist_filter);

    clock.advance(LAYOUT_FREEZE - std::time::Duration::from_secs(1));
    let mut next = presented.clone();
    next.now = clock.timestamp();
    let (released, persist_filter, expired) =
        reconcile_fold(next, &presented, &mut ui, &clock, None);
    assert!(expired);
    assert!(inactive_group(&released, "b"));
    assert_eq!(ui.toggles.make_up_filter.filter, None);
    assert!(persist_filter);
    assert!(ui.freeze.is_none(), "clearing must not arm another hold");
}

#[test]
fn unread_lens_clears_and_persists_while_freeze_is_held() {
    let mut unread = agent_row("a2", "terminal_2", AgentStatus::Running);
    unread.unread = true;
    let previous = snapshot_with_groups(vec![group(
        "a",
        vec![agent_row("a1", "terminal_1", AgentStatus::Running), unread],
    )]);
    let clock = super::super::clock::LoopClock::play(previous.now);
    let focused = pane("terminal_2", "tab_0").pane_id;
    let filter = crate::sidebar::body_filter::BodyFilter::Unread;
    let mut ui = ViewState::default();
    ui.toggles.make_up_filter.filter = Some(filter);
    ui.selected_pane = Some(focused.clone());
    ui.selected_index = Some(0);
    ui.baseline_pane = Some(focused.clone());
    ui.last_order = capture_order(&previous, &ui);
    arm_freeze(&mut ui, clock.now_ms() as i64);
    clock.advance(std::time::Duration::from_secs(1));
    let mut next = previous.clone();
    next.now = clock.timestamp();
    let group = &mut std::sync::Arc::make_mut(&mut next.worktree_groups)[0];
    std::sync::Arc::make_mut(&mut group.rows)[1].unread = false;

    let (held, persist_filter, expired) =
        reconcile_fold(next, &previous, &mut ui, &clock, Some(focused.clone()));
    assert!(ui.freeze.is_some());
    assert_eq!(filter.total(&held.worktree_groups), 0);
    assert_eq!(ui.toggles.make_up_filter.filter, None);
    assert!(persist_filter);
    assert!(!expired);
    assert_eq!(ui.selected_pane, Some(focused.clone()));
    assert_eq!(ui.selected_index, Some(1));
    assert_eq!(ui.visible_roster(&held).rows().len(), 2);

    clock.advance(LAYOUT_FREEZE);
    let mut next = held.clone();
    next.now = clock.timestamp();
    let (_, persist_filter, expired) =
        reconcile_fold(next, &held, &mut ui, &clock, Some(focused.clone()));
    assert!(expired);
    assert_eq!(ui.toggles.make_up_filter.filter, None);
    assert!(!persist_filter);
    assert!(ui.freeze.is_none(), "clearing must not arm another hold");
    assert_eq!(ui.selected_pane, Some(focused));
    assert_eq!(ui.selected_index, Some(1));
}

#[test]
fn viewed_active_group_waits_until_selection_moves_and_the_hold_releases() {
    for focused in [false, true] {
        let previous = inactivity_boundary();
        let clock = super::super::clock::LoopClock::play(previous.now);
        let viewed = pane("terminal_2", "tab_0").pane_id;
        let mut ui = ViewState::default();
        if !focused {
            ui.selected_pane = Some(viewed.clone());
            ui.baseline_pane = Some(viewed.clone());
        }
        clock.advance(std::time::Duration::from_secs(1));
        let mut next = previous.clone();
        next.now = clock.timestamp();
        let (held, _, _) = reconcile_fold(
            next,
            &previous,
            &mut ui,
            &clock,
            focused.then_some(viewed.clone()),
        );
        assert!(!inactive_group(&held, "b"), "focused={focused}");
        assert_eq!(ui.selected_pane, Some(viewed));

        ui.last_order = capture_order(&held, &ui);
        let other = pane("terminal_1", "tab_0").pane_id;
        ui.selected_pane = Some(other.clone());
        ui.baseline_pane = Some(other.clone());
        arm_freeze(&mut ui, clock.now_ms() as i64);
        clock.advance(std::time::Duration::from_secs(1));
        let mut next = held.clone();
        next.now = clock.timestamp();
        let (deferred, _, _) = reconcile_fold(next, &held, &mut ui, &clock, Some(other.clone()));
        assert!(!inactive_group(&deferred, "b"));
        clock.advance(LAYOUT_FREEZE - std::time::Duration::from_secs(1));
        let mut next = deferred.clone();
        next.now = clock.timestamp();
        let (admitted, _, expired) =
            reconcile_fold(next, &deferred, &mut ui, &clock, Some(other.clone()));
        assert!(inactive_group(&admitted, "b"));
        assert!(expired);
        assert_eq!(ui.selected_pane, Some(other));
    }
}

#[test]
fn viewing_an_existing_inactive_group_keeps_membership_and_order() {
    let mut previous = inactivity_boundary();
    previous.now += std::time::Duration::from_secs(1);
    previous.sort_groups_for_presentation();
    let clock = super::super::clock::LoopClock::play(previous.now);
    let focused = pane("terminal_2", "tab_0").pane_id;
    let mut next = previous.clone();
    next.focused_pane = Some(focused.clone());
    let mut ui = ViewState::default();
    let (viewed, _, _) = reconcile_fold(next, &previous, &mut ui, &clock, Some(focused));
    assert!(inactive_group(&viewed, "b"));
    assert_eq!(group_keys(&viewed), group_keys(&previous));
    assert!(
        ui.visible_roster(&viewed)
            .rows()
            .iter()
            .any(|row| row.id == "b1")
    );
}

fn inactivity_boundary() -> SidebarSnapshot {
    let mut current = snapshot_with_groups(vec![
        group("a", vec![agent_row("a1", "terminal_1", AgentStatus::Idle)]),
        group(
            "b",
            vec![agent_row("b1", "terminal_2", AgentStatus::Success)],
        ),
        group("c", vec![agent_row("c1", "terminal_3", AgentStatus::Idle)]),
        group("external", vec![row("e1", "terminal_4")]),
    ]);
    current.now = jiff::Timestamp::from_second(100_000).unwrap();
    for (group, age) in std::sync::Arc::make_mut(&mut current.worktree_groups)
        .iter_mut()
        .zip([0, 3600, 8000, 0])
    {
        let row = &mut std::sync::Arc::make_mut(&mut group.rows)[0];
        row.last_activity = current.now;
        if let Some(agent) = row.as_agent_mut() {
            let registered = current.now - std::time::Duration::from_secs(age);
            agent.registered_at = Some(registered);
            agent.engaged_at = Some(registered);
        }
        if group.key == "b" {
            group.clean = Some(true);
            group.pr_state = Some(crate::store::snapshot::WorktreePrState::Merged);
        }
        if group.key == "external" {
            group.kind = SidebarWorktreeKind::External;
        }
    }
    current.sort_groups_for_presentation();
    current
}

fn inactive_group(current: &SidebarSnapshot, key: &str) -> bool {
    current
        .worktree_groups
        .iter()
        .find(|group| group.key == key)
        .unwrap()
        .inactive
        .is_some()
}

fn reconcile_fold(
    current: SidebarSnapshot,
    previous: &SidebarSnapshot,
    ui: &mut ViewState,
    clock: &super::super::clock::LoopClock,
    focused: Option<PaneId>,
) -> (SidebarSnapshot, bool, bool) {
    use super::super::view_state::{SelectionFold, ViewContext};
    let mut context = ViewContext::new(current, clock.now());
    let selected = ui.selected_pane.clone();
    let filter = ui.toggles.make_up_filter.clone();
    let (persist_filter, expired) = context.reconcile_selection_and_order(
        ui,
        SelectionFold {
            previous,
            selected,
            held_panes: focused.iter().cloned().collect(),
            focused,
            cleared: false,
            filter: Some(filter),
            mux: crate::MuxName::Zellij,
            clock,
            now: clock.now(),
            parked: false,
        },
    );
    (context.current, persist_filter, expired)
}

#[test]
fn freeze_keeps_external_above_inactive_even_in_old_frozen_order() {
    let mut finished = group("finished", vec![]);
    finished.inactive = Some(crate::store::snapshot::SidebarInactive::Landed);
    let mut external = group("external", vec![]);
    external.kind = SidebarWorktreeKind::External;
    let mut current = snapshot_with_groups(vec![finished, external, group("active", vec![])]);
    let mut ui = ViewState {
        last_order: capture_order(&current, &ViewState::default()),
        ..Default::default()
    };
    apply_freeze(&mut ui, &mut current, true, 1_000, false);
    assert!(ui.freeze.is_some());
    assert_eq!(group_keys(&current), ["active", "external", "finished"]);
}

#[test]
fn new_group_appends_during_freeze_then_takes_its_rank_after_expiry() {
    let mut ui = crate::sidebar_pane::render::ViewState {
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
    };
    let groups = || {
        let mut finished = group("finished", vec![]);
        finished.inactive = Some(crate::store::snapshot::SidebarInactive::Landed);
        let mut external = group("external", vec![]);
        external.kind = SidebarWorktreeKind::External;
        vec![
            group(
                "team",
                vec![agent_row("coder", "terminal_2", AgentStatus::Idle)],
            ),
            group("main", vec![row("zsh", "terminal_1")]),
            external,
            finished,
        ]
    };
    let now_ms = 1_000;
    let mut current = snapshot_with_groups(groups());

    apply_freeze(&mut ui, &mut current, true, now_ms, false);

    assert_eq!(
        group_keys(&current),
        vec!["main", "team", "external", "finished"]
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
        vec!["team", "main", "external", "finished"]
    );
}

#[test]
fn adopt_shared_order_installs_hold_reorders_and_recaptures() {
    let mut ui = ViewState::default();
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
        1,
        FrozenRow {
            id: "a-new".to_owned(),
            pane: Some("zellij:terminal_3".to_owned()),
        },
    );
    let stamp_ms = 2_000;

    adopt_shared_order(&mut ui, &mut current, order, Some(stamp_ms));

    assert_eq!(row_ids(&current, "a"), vec!["a2", "a-new", "a1"]);
    assert_eq!(
        ui.freeze.as_ref().map(|hold| hold.deadline_ms(false)),
        Some(stamp_ms + LAYOUT_FREEZE.as_millis() as i64)
    );
    assert_eq!(
        ui.freeze.as_ref().map(|hold| &hold.frozen),
        Some(&expected_order)
    );
    assert_eq!(frozen_row_ids(&ui.last_order), vec!["a2", "a-new", "a1"]);
    assert_eq!(
        ui.last_order.visible,
        HashSet::from(["a-new".to_owned(), "a2".to_owned(), "a1".to_owned()])
    );
}

#[test]
fn freeze_migrates_rekeyed_rows_by_pane_and_keeps_visible_slots() {
    let mut ui = crate::sidebar_pane::render::ViewState {
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
