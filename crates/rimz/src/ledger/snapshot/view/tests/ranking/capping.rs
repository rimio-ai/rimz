use super::*;

#[test]
fn calm_tail_cap_never_hides_attention_rows() {
    let mut agents = (0..8)
        .map(|i| {
            agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Running,
                1_000 + i,
            )
        })
        .collect::<Vec<_>>();
    agents.push(agent_in("failed", "/repo/main", AgentStatus::Failed, 2_000));

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.status() == Some(AgentStatus::Failed)),
        "attention rows remain visible past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn calm_tail_cap_never_hides_focused_rows() {
    let agents = (0..8)
        .map(|i| {
            let mut agent = agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Running,
                1_000 + i,
            );
            if i == 0 {
                agent.pane = Some(PaneRef {
                    is_focused: true,
                    ..pane("%99", "codex", "/repo/main")
                });
            }
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.id == "sess-0"),
        "the focused running pane remains visible even past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn cap_trims_idle_before_running() {
    // Idle ranks last among agents, so the per-worktree cap's calm trim eats
    // the parked idle tail first and a working agent stays visible longer.
    let mut agents = Vec::new();
    for i in 0..4 {
        agents.push(agent_in(
            &format!("run-{i}"),
            "/repo/main",
            AgentStatus::Running,
            1_000 + i,
        ));
    }
    for i in 0..4 {
        agents.push(agent_in(
            &format!("idle-{i}"),
            "/repo/main",
            AgentStatus::Idle,
            2_000 + i,
        ));
    }

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let visible = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert!(
        (0..4).all(|i| visible.contains(&format!("run-{i}"))),
        "every running agent stays visible; only the idle tail trims: {visible:?}"
    );
    assert_eq!(snapshot.worktree_groups[0].hidden_count, 2);
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
