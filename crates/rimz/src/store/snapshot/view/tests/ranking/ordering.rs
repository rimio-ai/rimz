use super::*;

#[test]
fn group_order_is_newest_registration_not_working_state_or_pane_creation() {
    let actual = [AgentStatus::Idle, AgentStatus::Running, AgentStatus::Paused].map(|status| {
        let mut old = agent_in("old", "/repo/old", status, 0).in_pane("%99");
        old.registered_at = Some(ago(100));
        let mut new = agent_in("new", "/repo/new", AgentStatus::Idle, 0).in_pane("%1");
        new.registered_at = Some(ago(1));
        let mut snapshot = room_with_agent_panes(vec![old, new]);
        let before = snapshot
            .worktree_groups
            .iter()
            .map(|g| g.label.clone())
            .collect::<Vec<_>>();
        let mut arrival = snapshot.worktree_groups[1].rows[0].clone();
        arrival.id = "arrival".to_owned();
        arrival.as_agent_mut().unwrap().registered_at = Some(epoch());
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].rows,
        )
        .push(arrival);
        snapshot.sort_groups_for_presentation();
        let after = snapshot
            .worktree_groups
            .iter()
            .map(|g| g.label.clone())
            .collect::<Vec<_>>();
        (before, after)
    });
    assert_eq!(
        actual,
        std::array::from_fn(|_| (
            vec!["new".to_owned(), "old".to_owned()],
            vec!["old".to_owned(), "new".to_owned()]
        ))
    );
}

#[test]
fn inactive_groups_order_by_latest_agent_activity_not_creation_or_process_activity() {
    let mut old = agent_in("old", "/repo/old", AgentStatus::Idle, 0);
    old.registered_at = Some(ago(90000));
    old.last_activity = ago(100);
    let mut new = agent_in("new", "/repo/new", AgentStatus::Success, 0);
    new.registered_at = Some(ago(88000));
    new.last_activity = ago(200);
    let mut snapshot = room_with_agent_panes(vec![old, new]);
    let new_group = std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .find(|g| g.label == "new")
        .unwrap();
    let mut process = new_group.rows[0].clone();
    process.id = "process".to_owned();
    process.last_activity = epoch();
    process.card = crate::store::snapshot::RowCard::Process(Default::default());
    std::sync::Arc::make_mut(&mut new_group.rows).push(process);
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|g| (g.label.as_str(), g.inactive.is_some()))
            .collect::<Vec<_>>(),
        [("old", true), ("new", true)]
    );
}
use crate::agents::ATTENTION_AGE_CEILING_SECS;
use crate::ids::{MuxName, PaneId};
use crate::store::snapshot::SidebarWorktreeGroup;
use crate::store::snapshot::group_live_agents_by_worktree;

#[test]
fn closed_permissions_do_not_rank_as_waiting() {
    for proof in ["completed cell", "turn ended", "answered"] {
        let mut closed = agent("codex", "closed", AgentStatus::Waiting, 0)
            .worktree("/repo/main")
            .active_ago(90);
        closed.mode = Some(crate::agents::PermissionMode::Auto);
        closed.waiting_since = Some(ago(90));
        closed.open_ask = Some(crate::agents::OpenAsk {
            extra_questions: 0,
            id: crate::ids::AskId::new(),
            kind: crate::agents::AskKind::Permission,
            detail: None,
            native_key: None,
            since: ago(90),
        });
        closed.context = Some(crate::agents::AgentContext::default());
        match proof {
            "completed cell" => {
                closed.context.as_mut().unwrap().permission_settle =
                    Some(crate::agents::context::PermissionSettle {
                        ask_since: ago(90),
                        at: ago(88),
                    })
            }
            "turn ended" => {
                closed.context.as_mut().unwrap().turn_error = Some(crate::agents::AgentTurnError {
                    class: crate::agents::TurnErrorClass::PausedOverloaded,
                    at: ago(88),
                    label: None,
                    ended_turn: true,
                })
            }
            _ => closed.last_activity = ago(89),
        }
        let waiting = agent("claude", "waiting", AgentStatus::Waiting, 0)
            .worktree("/repo/main")
            .active_ago(30);
        let snapshot = room_with_agent_panes(vec![closed, waiting]);
        let agents = snapshot.agents.iter().collect::<Vec<_>>();
        let groups = group_live_agents_by_worktree(&agents, &snapshot);
        assert_eq!(groups[0].agents[0].agent_id.as_str(), "waiting", "{proof}");
    }
}

#[test]
fn auto_review_permission_listing_ranks_with_running_until_visible() {
    for age in [59, 60] {
        let mut review = agent("codex", "review", AgentStatus::Waiting, 0)
            .worktree("/repo/main")
            .active_ago(age);
        review.mode = Some(crate::agents::PermissionMode::Auto);
        review.waiting_since = Some(ago(age));
        review.open_ask = Some(crate::agents::OpenAsk {
            extra_questions: 0,
            id: crate::ids::AskId::new(),
            kind: crate::agents::AskKind::Permission,
            detail: None,
            native_key: None,
            since: ago(age),
        });
        review.pane = Some(pane("%9", "codex", "/repo/main"));
        let mut running = agent("claude", "running", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .active_ago(age);
        running.pane = Some(pane("%1", "claude", "/repo/main"));
        let waiting = agent("claude", "waiting", AgentStatus::Waiting, 0)
            .worktree("/repo/main")
            .active_ago(30);
        let snapshot = room_with_agent_panes(vec![review, running, waiting]);
        let agents = snapshot.agents.iter().collect::<Vec<_>>();
        let groups = group_live_agents_by_worktree(&agents, &snapshot);
        let order = groups[0]
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            if age == 59 {
                vec!["waiting", "running", "review"]
            } else {
                vec!["waiting", "review", "running"]
            },
            "at {age}s"
        );
    }
}

#[test]
fn delegated_park_counts_as_paused_without_leading_calm_cards() {
    let parent = agent("claude", "root", AgentStatus::Idle, 0).worktree("/repo/main");
    let mut child =
        child_state("root", "child", AgentStatus::Running, 1).paused_turn_error(0, "limit");
    child.launch_depth = Some(1);
    let running = agent("claude", "running", AgentStatus::Running, 0).worktree("/repo/main");
    let snapshot = room_with_agent_panes(vec![running, parent, child]);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.rows[0].id, "running");
    let parent = group.rows.iter().find(|row| row.id == "root").unwrap();
    assert_eq!(parent.attention_status(), Some(AgentStatus::Paused));
    assert_eq!(group.status_counts[0].status, AgentStatus::Paused);
    assert_eq!(group.status_counts[0].count, 1);
}

#[test]
fn row_order_puts_actionable_first_then_calm_by_stamp() {
    // Scrambled input proves the sort, not the insertion order.
    let agents = [
        AgentStatus::Running,
        AgentStatus::Sleeping,
        AgentStatus::Success,
        AgentStatus::Idle,
        AgentStatus::Paused,
        AgentStatus::Failed,
        AgentStatus::Waiting,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| agent_in(&format!("sess-{i}"), "/repo/main", status, 1_000 + i as i64))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.status())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Failed),
            Some(AgentStatus::Paused),
            Some(AgentStatus::Idle),
            Some(AgentStatus::Success),
            Some(AgentStatus::Sleeping),
            Some(AgentStatus::Running),
        ],
        "asks and failures lead; calm cards rank by birth, not status"
    );

    let counts = snapshot.worktree_groups[0]
        .status_counts
        .iter()
        .map(|count| count.status)
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        vec![
            AgentStatus::Waiting,
            AgentStatus::Failed,
            AgentStatus::Paused,
            AgentStatus::Success,
            AgentStatus::Running,
            AgentStatus::Sleeping,
            AgentStatus::Idle,
        ],
        "status tallies stay in cockpit make-up order"
    );
}

#[test]
fn calm_rows_and_groups_use_newest_registration() {
    let mut older = agent("codex", "older", AgentStatus::Idle, 1_000).worktree("/repo/main");
    older.pane = Some(pane("%0", "codex", "/repo/main"));
    let mut newer = agent("claude", "newer", AgentStatus::Idle, 9_000).worktree("/repo/main");
    newer.pane = Some(pane("%1", "claude", "/repo/main"));
    let mut snapshot = room_with_agent_panes(vec![newer, older]);
    let row_order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(row_order, vec!["newer", "older"]);

    // A row without a birth stamp tails the dated calm cards.
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(idle_agent_row("paneless", None));
    snapshot.sort_groups_for_presentation();
    let row_order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(row_order, vec!["newer", "older", "paneless"]);

    let mut older = agent_in("sess-b", "/repo/b", AgentStatus::Idle, 1_000);
    older.pane = Some(pane("%0", "node", "/repo/b"));
    let mut newer = agent_in("sess-a", "/repo/a", AgentStatus::Idle, 9_000);
    newer.pane = Some(pane("%1", "node", "/repo/a"));

    let snapshot = room_with_agent_panes(vec![newer, older]);

    let groups = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        vec!["a", "b"],
        "the newer registration leads even though its pane was created later"
    );
}

#[test]
fn attention_zone_sorts_newest_stamp_first_without_status_tiers() {
    // Scrambled input; a higher rank means more recent activity.
    let agents = vec![
        ("wait-new", AgentStatus::Waiting, 9_000),
        ("wait-old", AgentStatus::Waiting, 1_000),
        ("fail-new", AgentStatus::Failed, 8_000),
        ("fail-old", AgentStatus::Failed, 2_000),
    ]
    .into_iter()
    .map(|(id, status, rank)| agent_in(id, "/repo/main", status, rank))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["wait-new", "fail-new", "fail-old", "wait-old"]);
}

#[test]
fn fresh_ask_leads_an_hour_old_failure() {
    let snapshot = room_with_agent_panes(vec![
        agent_in("fresh-wait", "/repo/main", AgentStatus::Waiting, 4_000).active_ago(120),
        agent_in("old-fail", "/repo/main", AgentStatus::Failed, 2_000).active_ago(50 * 60),
        agent_in("calm", "/repo/main", AgentStatus::Success, 3_000).active_ago(60),
    ]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["fresh-wait", "old-fail", "calm"],
        "newest attention first, regardless of the group-urgency score"
    );
}

#[test]
fn old_attention_holds_above_recent_calm() {
    let snapshot = room_with_agent_panes(vec![
        agent_in("old-ask", "/repo/main", AgentStatus::Waiting, 1_000).active_ago(20 * 60 * 60),
        agent_in("recent-done", "/repo/main", AgentStatus::Success, 2_000).active_ago(2 * 60 * 60),
    ]);

    assert!(!row(&snapshot, "old-ask").inactive);
    assert!(row(&snapshot, "recent-done").inactive);
    assert!(!row(&snapshot, "old-ask").archived);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["old-ask", "recent-done"],
        "an unanswered ask never cools"
    );
}

#[test]
fn archive_band_parks_calm_rows_but_not_attention() {
    let snapshot = room_with_agent_panes(vec![
        agent_in("warm-idle", "/repo/main", AgentStatus::Idle, 1_000).active_ago(23 * 60 * 60),
        agent_in("archived-idle", "/repo/main", AgentStatus::Idle, 2_000).active_ago(25 * 60 * 60),
        agent_in("archived-ask", "/repo/main", AgentStatus::Waiting, 3_000)
            .active_ago(25 * 60 * 60),
    ]);

    assert!(!row(&snapshot, "warm-idle").archived);
    assert!(!row(&snapshot, "archived-ask").archived);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["archived-ask", "warm-idle", "archived-idle"],
        "an unanswered ask stays hot past archive"
    );
}

#[test]
fn read_state_never_changes_the_band() {
    let mut snapshot = room_with_agent_panes(vec![
        agent_in("seen-wait", "/repo/a", AgentStatus::Waiting, 1_000),
        agent_in("new-done", "/repo/b", AgentStatus::Success, 2_000),
    ]);
    row_mut(&mut snapshot, "new-done").unread = true;
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        group_labels(&snapshot),
        vec!["a", "b"],
        "read ask outranks unread result; read state never changes the band"
    );

    let mut snapshot = room_with_agent_panes(vec![
        agent_in("read-old", "/repo/main", AgentStatus::Waiting, 1_000),
        agent_in("new-wait", "/repo/main", AgentStatus::Waiting, 9_000),
    ]);
    row_mut(&mut snapshot, "new-wait").unread = true;
    snapshot.sort_groups_for_presentation();
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["new-wait", "read-old"],
        "among asks, the newest stamp leads regardless of read state"
    );
}

#[test]
fn stale_attention_leads_live_work_while_parks_cool() {
    let snapshot = room_with_agent_panes(vec![
        agent_in("fresh-idle", "/repo/main", AgentStatus::Idle, 3_000),
        agent_in("fresh-run", "/repo/main", AgentStatus::Running, 4_000),
        agent_in("old-wait", "/repo/main", AgentStatus::Waiting, 1_000).active_ago(22 * 3_600),
        agent_in("older-wait", "/repo/main", AgentStatus::Waiting, 500).active_ago(23 * 3_600),
        agent_in("old-park", "/repo/main", AgentStatus::Paused, 5_000).active_ago(22 * 3_600),
        agent_in("old-done", "/repo/main", AgentStatus::Success, 2_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
    ]);

    assert!(
        !row(&snapshot, "fresh-idle").inactive,
        "fresh work stays live"
    );
    assert!(
        !row(&snapshot, "old-wait").inactive,
        "a stale ask stays hot even after being read"
    );
    assert!(row(&snapshot, "old-done").inactive);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            "old-wait",
            "older-wait",
            "fresh-run",
            "fresh-idle",
            "old-park",
            "old-done"
        ],
        "asks stay hot, newest birth first; calm and parked cards still cool"
    );
}

#[test]
fn process_rows_interleave_calm_cards_by_stamp_and_break_ties_after_cards() {
    let actual =
        [(10, None), (30, None), (20, None), (20, Some(7))].map(|(process_stamp, ordinal)| {
            let mut snapshot = room_with_agent_panes(vec![
                agent_in("calm", "/repo/main", AgentStatus::Idle, 0).in_pane("%99"),
            ]);
            let calm = row_mut(&mut snapshot, "calm");
            calm.attention_at_ms = Some(20);
            calm.as_agent_mut().unwrap().launch_ordinal = ordinal;
            let mut shell = process_row("%1", "/repo/main");
            shell.attention_at_ms = Some(process_stamp);
            std::sync::Arc::make_mut(
                &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
            )
            .push(shell);
            snapshot.sort_groups_for_presentation();
            snapshot
                .rows()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>()
        });
    assert_eq!(
        actual,
        [
            vec!["calm", "%1"],
            vec!["%1", "calm"],
            vec!["calm", "%1"],
            vec!["calm", "%1"]
        ]
    );
}

#[test]
fn process_rows_follow_asks_and_failures_but_lead_older_calm_cards() {
    let mut snapshot = room_with_agent_panes(vec![
        agent_in("ask", "/repo/main", AgentStatus::Waiting, 0),
        agent_in("failure", "/repo/main", AgentStatus::Failed, 0),
        agent_in("calm", "/repo/main", AgentStatus::Idle, 0),
    ]);
    for (id, stamp) in [("ask", 1), ("failure", 2), ("calm", 3)] {
        row_mut(&mut snapshot, id).attention_at_ms = Some(stamp);
    }
    let mut shell = process_row("shell", "/repo/main");
    shell.attention_at_ms = Some(100);
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(shell);
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        snapshot
            .rows()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["failure", "ask", "shell", "calm"]
    );
}

#[test]
fn a_newer_process_sinks_below_an_older_hot_card_when_archived() {
    let mut snapshot =
        room_with_agent_panes(vec![agent_in("calm", "/repo/main", AgentStatus::Idle, 0)]);
    row_mut(&mut snapshot, "calm").attention_at_ms = Some(10);
    let mut shell = process_row("shell", "/repo/main");
    shell.attention_at_ms = Some(20);
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(shell);
    snapshot.sort_groups_for_presentation();
    let hot = snapshot
        .rows()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let shell = row_mut(&mut snapshot, "shell");
    shell.inactive = true;
    shell.archived = true;
    snapshot.sort_groups_for_presentation();
    let archived = snapshot
        .rows()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        (hot, archived),
        (
            ["shell", "calm"].map(String::from).to_vec(),
            ["calm", "shell"].map(String::from).to_vec()
        )
    );
}

#[test]
fn inactive_idle_uses_the_hour_boundary_strictly() {
    let snapshot = room_with_agent_panes(vec![
        agent_in("fresh-idle", "/repo/main", AgentStatus::Idle, 1_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS),
        agent_in("old-idle", "/repo/main", AgentStatus::Idle, 2_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
    ]);

    assert!(!row(&snapshot, "fresh-idle").inactive);
    assert!(row(&snapshot, "old-idle").inactive);
}

#[test]
fn inactive_groups_sink_below_process_groups() {
    let mut old = agent_in("old-done", "/repo/a", AgentStatus::Success, 1_000)
        .active_ago(ATTENTION_AGE_CEILING_SECS + 1);
    old.registered_at = Some(ago(86401));
    let mut snapshot = room_with_agent_panes(vec![
        old,
        agent_in("fresh-idle", "/repo/c", AgentStatus::Idle, 2_000),
    ]);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups).push(SidebarWorktreeGroup {
        stalled_since: None,
        pr_stack: Default::default(),
        key: "/repo/b".to_owned(),
        label: "b".to_owned(),
        label_qualifier: None,
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        workflow: None,
        status_counts: Vec::new(),
        rows: vec![process_row("zsh", "/repo/b")].into(),
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
    });
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        group_labels(&snapshot),
        vec!["c", "b", "a"],
        "registered active groups lead process groups, and quiet groups sink below both"
    );
}

#[test]
fn live_process_keeps_mixed_group_above_inactive_groups() {
    let mut snapshot = room_with_agent_panes(vec![
        agent_in("old-a", "/repo/a", AgentStatus::Success, 1_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        agent_in("old-b", "/repo/b", AgentStatus::Success, 2_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        agent_in("fresh-c", "/repo/c", AgentStatus::Idle, 3_000),
    ]);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
            .iter_mut()
            .find(|group| group.label == "b")
            .expect("group b present")
            .rows,
    )
    .push(process_row("zsh", "/repo/b"));
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        group_labels(&snapshot),
        vec!["c", "b", "a"],
        "a live process keeps its mixed group above inactive-only groups"
    );
    let mixed_order = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "b")
        .expect("group b present")
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        mixed_order,
        vec!["zsh", "old-b"],
        "the hot process leads the archived card inside the mixed group"
    );
}

#[test]
fn snapshot_keeps_all_inactive_rows_for_renderer_capping() {
    let mut agents = vec![
        agent_in("old-done", "/repo/main", AgentStatus::Success, 1_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
    ];
    agents.extend((0..10).map(|i| {
        agent_in(
            &format!("old-idle-{i}"),
            "/repo/main",
            AgentStatus::Idle,
            2_000 + i,
        )
        .active_ago(ATTENTION_AGE_CEILING_SECS + 1)
    }));
    let snapshot = room_with_agent_panes(agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.id == "old-done"),
        "inactive success remains visible even when the inactive idle tail is capped"
    );
    assert_eq!(snapshot.worktree_groups[0].rows.len(), 11);
}

#[test]
fn team_birth_ties_keep_layout_order_across_calm_status_and_read_churn() {
    let mut snapshot = room_with_agent_panes(vec![
        cohort_agent(
            "cohort-second",
            AgentStatus::Success,
            1_000,
            "launch_group_1",
            Some(1),
            "%1",
        ),
        cohort_agent(
            "cohort-tail",
            AgentStatus::Running,
            2_000,
            "launch_group_1",
            None,
            "%2",
        ),
        cohort_agent(
            "cohort-first",
            AgentStatus::Idle,
            3_000,
            "launch_group_1",
            Some(0),
            "%3",
        ),
        agent_in("loose-unread", "/repo/main", AgentStatus::Success, 4_000).in_pane("%0"),
    ]);
    for id in ["cohort-first", "cohort-second", "cohort-tail"] {
        row_mut(&mut snapshot, id)
            .as_agent_mut()
            .unwrap()
            .registered_at = Some(ago(1));
    }
    row_mut(&mut snapshot, "loose-unread").unread = true;
    snapshot.sort_groups_for_presentation();

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            "cohort-first",
            "cohort-second",
            "cohort-tail",
            "loose-unread",
        ],
        "one team birth ties on layout ordinal, with ordinal-less legacy members last"
    );
    row_mut(&mut snapshot, "cohort-first")
        .as_agent_mut()
        .unwrap()
        .status = AgentStatus::Running;
    row_mut(&mut snapshot, "cohort-tail")
        .as_agent_mut()
        .unwrap()
        .status = AgentStatus::Paused;
    row_mut(&mut snapshot, "cohort-second").unread = true;
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        [
            "cohort-first",
            "cohort-second",
            "cohort-tail",
            "loose-unread"
        ]
    );
}

#[test]
fn inline_members_rank_independently_by_birth() {
    let snapshot = room_with_agent_panes(vec![
        cohort_agent(
            "g1-a",
            AgentStatus::Success,
            1_000,
            "launch_g1",
            Some(0),
            "%1",
        ),
        cohort_agent(
            "g2-a",
            AgentStatus::Success,
            2_000,
            "launch_g2",
            Some(0),
            "%2",
        ),
        cohort_agent(
            "g2-b",
            AgentStatus::Success,
            3_000,
            "launch_g2",
            Some(1),
            "%3",
        ),
        cohort_agent(
            "g1-b",
            AgentStatus::Success,
            4_000,
            "launch_g1",
            Some(1),
            "%4",
        ),
    ]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["g1-b", "g2-b", "g2-a", "g1-a"],
        "launch groups do not override each card's own stamp"
    );
}

#[test]
fn team_members_rank_by_their_own_zone_and_birth() {
    let blocked_a = cohort_agent(
        "blocked-a",
        AgentStatus::Waiting,
        1_000,
        "blocked",
        Some(0),
        "%4",
    );
    let blocked_b = cohort_agent(
        "blocked-b",
        AgentStatus::Success,
        2_000,
        "blocked",
        Some(1),
        "%5",
    );
    let success_a = cohort_agent(
        "success-a",
        AgentStatus::Success,
        3_000,
        "success",
        Some(0),
        "%6",
    );
    let working_a = cohort_agent(
        "working-a",
        AgentStatus::Running,
        5_000,
        "working",
        Some(0),
        "%0",
    );
    let idle_a = cohort_agent("idle-a", AgentStatus::Idle, 7_000, "idle", Some(0), "%2");

    let snapshot = room_with_agent_panes(vec![blocked_b, idle_a, working_a, success_a, blocked_a]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["blocked-a", "idle-a", "working-a", "success-a", "blocked-b"],
        "only the actionable team member rises above the calm zone"
    );
}

#[test]
fn only_the_asking_member_lifts_out_of_its_cohorts_cooling_band() {
    let mut snapshot = room_with_agent_panes(vec![
        agent_in("fresh-run", "/repo/main", AgentStatus::Running, 1_000),
        cohort_agent(
            "old-a",
            AgentStatus::Waiting,
            2_000,
            "launch_group_1",
            Some(0),
            "%1",
        )
        .active_ago(22 * 3_600),
        cohort_agent(
            "old-b",
            AgentStatus::Idle,
            3_000,
            "launch_group_1",
            Some(1),
            "%2",
        )
        .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        cohort_agent(
            "old-c",
            AgentStatus::Idle,
            4_000,
            "launch_group_1",
            Some(2),
            "%3",
        )
        .active_ago(22 * 3_600),
    ]);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["old-a", "fresh-run", "old-c", "old-b"]);

    row_mut(&mut snapshot, "old-b").unread = true;
    snapshot.sort_groups_for_presentation();
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["old-a", "fresh-run", "old-c", "old-b"],
        "read state never changes the band"
    );
}

#[test]
fn listing_roster_order_matches_row_order_when_rows_have_no_sidebar_state() {
    let mut cohort_first =
        agent_in("cohort-first", "/repo/main", AgentStatus::Waiting, 3_000).in_pane("%2");
    cohort_first.launch_group = Some("launch_group_1".to_owned());
    cohort_first.launch_ordinal = Some(0);
    let mut cohort_second =
        agent_in("cohort-second", "/repo/main", AgentStatus::Running, 2_000).in_pane("%3");
    cohort_second.launch_group = Some("launch_group_1".to_owned());
    cohort_second.launch_ordinal = Some(1);
    let agents = vec![
        agent_in("done-late", "/repo/main", AgentStatus::Success, 6_000).in_pane("%5"),
        agent_in(
            "running-early-pane",
            "/repo/main",
            AgentStatus::Running,
            5_000,
        )
        .in_pane("%0"),
        agent_in("wait", "/repo/main", AgentStatus::Waiting, 1_000).in_pane("%9"),
        agent_in("done-early", "/repo/main", AgentStatus::Success, 4_000).in_pane("%1"),
        cohort_second,
        agent_in("paneless", "/repo/main", AgentStatus::Idle, 7_000),
        cohort_first,
        agent_in("warm", "/repo/main", AgentStatus::Idle, 8_000)
            .in_pane("%6")
            .active_ago(101),
        agent_in("archived", "/repo/main", AgentStatus::Idle, 9_000)
            .in_pane("%7")
            .active_ago(102),
    ];

    let mut row_snapshot = room(Vec::new());
    row_snapshot.worktree_groups = vec![SidebarWorktreeGroup {
        stalled_since: None,
        pr_stack: Default::default(),
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        workflow: None,
        status_counts: Vec::new(),
        rows: agents
            .iter()
            .map(|agent| {
                let mut row = row_from_agent(agent, epoch());
                stamp_lifted_attention(&mut row);
                row
            })
            .collect::<Vec<_>>()
            .into(),
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
    }]
    .into();
    row_snapshot.sort_groups_for_presentation();
    let row_order = row_snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect::<Vec<_>>();

    let mut listing_snapshot = room(agents);
    listing_snapshot.attention.inactive_after_secs = std::num::NonZeroU32::new(100).unwrap();
    listing_snapshot.attention.archive_after_secs = std::num::NonZeroU32::new(50).unwrap();
    let refs = listing_snapshot.agents.iter().collect::<Vec<_>>();
    let groups = group_live_agents_by_worktree(&refs, &listing_snapshot);
    let listing_order = groups
        .iter()
        .flat_map(|group| &group.agents)
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(row_order, listing_order);
    assert_eq!(&row_order[row_order.len() - 2..], ["warm", "archived"]);
}

#[test]
fn listing_roster_uses_the_same_group_activity_rung() {
    let agents = vec![
        agent_in("success", "/repo/a-success", AgentStatus::Success, 1_000).in_pane("%1"),
        agent_in("running", "/repo/z-working", AgentStatus::Running, 2_000).in_pane("%2"),
        agent_in("idle", "/repo/z-working", AgentStatus::Idle, 3_000).in_pane("%3"),
    ];
    let snapshot = list_agent_checkouts(room(agents));
    let refs = snapshot.agents.iter().collect::<Vec<_>>();
    let groups = group_live_agents_by_worktree(&refs, &snapshot);

    assert_eq!(
        groups
            .iter()
            .map(|group| group.label.as_str())
            .collect::<Vec<_>>(),
        vec!["z-working", "a-success"]
    );
}

fn stamp_lifted_attention(row: &mut SidebarRow) {
    let age_secs = super::super::super::score::age_secs(epoch(), row.last_activity);
    (row.inactive, row.archived, row.attention_score) = super::super::super::score::attention_facts(
        row.attention_status(),
        age_secs,
        super::super::super::aggregate::AttentionWindows {
            stalled_after_secs: 1,
            tool_repeat_attention_after: 20,
            inactive_after_secs: 100,
            archive_after_secs: 101,
        },
    );
}

fn row_mut<'a>(snapshot: &'a mut SidebarSnapshot, id: &str) -> &'a mut SidebarRow {
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .flat_map(|group| std::sync::Arc::make_mut(&mut group.rows).iter_mut())
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("row {id} present"))
}

fn cohort_agent(
    id: &str,
    status: AgentStatus,
    rank: i64,
    launch_group: &str,
    launch_ordinal: Option<u32>,
    pane_raw: &str,
) -> AgentState {
    let mut agent = agent_in(id, "/repo/main", status, rank).in_pane(pane_raw);
    agent.launch_group = Some(launch_group.to_owned());
    agent.launch_ordinal = launch_ordinal;
    agent
}

fn process_row(id: &str, worktree: &str) -> SidebarRow {
    SidebarRow {
        checkout: None,
        id: id.to_owned(),
        name: id.to_owned(),
        pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))),
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: epoch(),
        card: crate::store::snapshot::RowCard::Process(
            crate::store::snapshot::ProcessCard::default(),
        ),
    }
}

fn idle_agent_row(id: &str, pane_raw: Option<&str>) -> SidebarRow {
    SidebarRow {
        checkout: None,
        id: id.to_owned(),
        name: id.to_owned(),
        pane: pane_raw.map(|raw| PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, raw))),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: epoch(),
        card: crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
            crate::store::snapshot::row::AgentCard {
                status: AgentStatus::Idle,
                ..Default::default()
            },
        )),
    }
}

fn group_labels(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect()
}
