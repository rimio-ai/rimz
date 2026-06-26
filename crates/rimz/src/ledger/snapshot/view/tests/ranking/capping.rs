use super::*;
use crate::agents::ATTENTION_AGE_CEILING_SECS;

#[test]
fn calm_tail_cap_never_hides_attention_or_focused_rows() {
    for (label, protected_id, agents) in [
        ("attention", "failed", {
            let mut agents = idle_agents(8);
            agents.push(agent_in("failed", "/repo/main", AgentStatus::Failed, 2_000));
            agents
        }),
        (
            "focused",
            "sess-0",
            idle_agents(8)
                .into_iter()
                .enumerate()
                .map(|(i, mut agent)| {
                    if i == 0 {
                        agent.pane = Some(PaneRef {
                            is_focused: true,
                            ..pane("%99", "codex", "/repo/main")
                        });
                    }
                    agent
                })
                .collect(),
        ),
    ] {
        let snapshot = room_with_agent_panes(Vec::new(), agents);
        assert!(
            snapshot.worktree_groups[0]
                .rows
                .iter()
                .any(|row| row.id == protected_id),
            "{label} row remains visible past the calm-row cap"
        );
        assert!(snapshot.worktree_groups[0].hidden_count > 0, "{label}");
    }
}

#[test]
fn cap_keeps_active_and_finished_rows_for_unread_tracking() {
    let mut agents = Vec::new();
    for i in 0..8 {
        agents.push(agent_in(
            &format!("run-{i}"),
            "/repo/main",
            AgentStatus::Running,
            1_000 + i,
        ));
    }
    for i in 0..8 {
        agents.push(agent_in(
            &format!("done-{i}"),
            "/repo/main",
            AgentStatus::Success,
            2_000 + i,
        ));
    }
    for i in 0..8 {
        agents.push(agent_in(
            &format!("idle-{i}"),
            "/repo/main",
            AgentStatus::Idle,
            3_000 + i,
        ));
    }

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let visible = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert!(
        (0..8).all(|i| visible.contains(&format!("run-{i}"))),
        "running rows stay visible so their eventual completion can be observed: {visible:?}"
    );
    assert!(
        (0..8).all(|i| visible.contains(&format!("done-{i}"))),
        "finished rows stay visible so unread result receipts can propagate: {visible:?}"
    );
    assert!(
        snapshot.worktree_groups[0].hidden_count > 0,
        "the idle tail still trims behind +K more"
    );
}

#[test]
fn cap_keeps_sticky_unread_idle_rows() {
    let mut agents = idle_agents(8);
    let mut panes = Vec::new();
    for (idx, agent) in agents.iter_mut().enumerate() {
        let pane = pane(
            &format!("%agent-{idx}"),
            agent.kind.as_str(),
            agent.worktree_path.as_deref().unwrap_or("/repo/main"),
        );
        agent.pane = Some(pane.clone());
        panes.push(pane);
    }
    let unread = BTreeSet::from(["sess-7".to_owned()]);

    let snapshot = room(Vec::new(), agents).with_live_panes_and_unread(panes, None, &unread);
    let visible = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| (row.id.clone(), row.unread))
        .collect::<Vec<_>>();

    assert!(
        visible.contains(&("sess-7".to_owned(), true)),
        "a sticky unread idle row stays visible past the calm-row cap: {visible:?}"
    );
    assert!(
        snapshot.worktree_groups[0].hidden_count > 0,
        "the ordinary idle tail still trims behind +K more"
    );
}

#[test]
fn cap_keeps_process_liveness_anchor_for_inactive_agent_groups() {
    let inactive = ATTENTION_AGE_CEILING_SECS + 1;
    let mut agents = Vec::new();
    let mut panes = Vec::new();
    for (label, worktree) in [("a", "/repo/a"), ("b", "/repo/b")] {
        for i in 0..7 {
            let id = format!("{label}-{i}");
            let pane_id = format!("%{label}{i}");
            agents.push(
                agent_in(&id, worktree, AgentStatus::Idle, 1_000 + i)
                    .active_ago(inactive)
                    .in_pane(&pane_id),
            );
            panes.push(pane(&pane_id, "claude", worktree));
        }
    }
    agents.push(agent_in("fresh-c", "/repo/c", AgentStatus::Idle, 3_000).in_pane("%c0"));
    panes.push(pane("%c0", "claude", "/repo/c"));
    panes.push(pane("%99", "zsh", "/repo/b"));

    let snapshot = room(Vec::new(), agents).with_live_panes(panes, None);
    let groups = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        groups,
        vec!["c", "b", "a"],
        "a capped live process keeps its mixed group above inactive-only groups"
    );
    let mixed = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "b")
        .expect("group b present");
    assert!(
        mixed.rows.iter().any(|row| row.id == "tmux:%99"),
        "the process row remains visible as the group's live anchor"
    );
    assert!(
        mixed.hidden_count > 0,
        "ordinary inactive idle rows still trim behind +K more"
    );
}

#[test]
fn calm_groups_hold_order_through_member_status_churn() {
    // Calm worktree groups never leapfrog just because a member's calm status
    // flipped: the group tier collapses success/running/idle to one rank, so
    // the stable earliest-pane order decides until genuine attention arises.
    let build = |a_status: AgentStatus, b_status: AgentStatus| {
        let mut a = agent_in("sess-a", "/repo/a", a_status, 1_000);
        a.pane = Some(pane_started("%0", "/repo/a", ago(600)));
        let mut b = agent_in("sess-b", "/repo/b", b_status, 1_001);
        b.pane = Some(pane_started("%1", "/repo/b", ago(500)));
        room_with_agent_panes(Vec::new(), vec![a, b])
    };

    let groups = |snapshot: &SidebarSnapshot| {
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
    };

    let before = build(AgentStatus::Running, AgentStatus::Success);
    // b's agent finishing a turn while a's keeps working reorders nothing.
    let after = build(AgentStatus::Idle, AgentStatus::Running);
    assert_eq!(groups(&before), groups(&after));
    assert_eq!(groups(&before), vec!["a", "b"]);

    // Genuine attention still floats its group to the top.
    let blocked = build(AgentStatus::Running, AgentStatus::Waiting);
    assert_eq!(groups(&blocked), vec!["b", "a"]);
}

fn idle_agents(count: i64) -> Vec<AgentState> {
    (0..count)
        .map(|i| {
            agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Idle,
                1_000 + i,
            )
        })
        .collect()
}
