use super::*;

#[test]
fn fleet_room_tiering_floats_the_attention_child_repo() {
    // Directory-room ordering rides the same tier ladder: the waiting child
    // repo leads, calm pods follow by label, external tails.
    let q_pane = pane("%q1", "claude", "/srv/agents/query-engine");
    let r_pane = pane("%r1", "claude", "/srv/agents");
    let b_pane = pane("%b1", "claude", "/srv/agents/billing");
    let e_pane = pane("%e1", "claude", "/tmp/outside");
    let mut q1 = agent_in(
        "q1",
        "/srv/agents/query-engine",
        AgentStatus::Waiting,
        1_000,
    );
    q1.pane = Some(q_pane.clone());
    let mut r1 = agent_in("r1", "/srv/agents", AgentStatus::Idle, 1_000);
    r1.pane = Some(r_pane.clone());
    let mut b1 = agent_in("b1", "/srv/agents/billing", AgentStatus::Idle, 1_000);
    b1.pane = Some(b_pane.clone());
    let mut e1 = agent("claude", "e1", AgentStatus::Idle, 1_000);
    e1.pane = Some(e_pane.clone());

    let snapshot = room(Vec::new(), vec![q1, r1, b1, e1])
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![
            PathBuf::from("/srv/agents/billing"),
            PathBuf::from("/srv/agents/query-engine"),
        ])
        .with_live_panes(vec![q_pane, r_pane, b_pane, e_pane], None);

    let labels: Vec<&str> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["query-engine", "agents", "billing", "external"]
    );
}

#[test]
fn group_tiering_floats_attention_and_tails_external() {
    let labels_for = |mut agents: Vec<AgentState>| {
        let mut panes = Vec::new();
        for (idx, agent) in agents.iter_mut().enumerate() {
            let raw = format!("%tier-{idx}");
            let mut live = pane(
                &raw,
                agent.kind.as_str(),
                agent.worktree_path.as_deref().unwrap_or("/repo/main"),
            );
            if agent.worktree_path.is_none() {
                live.cwd = None;
            }
            agent.pane = Some(live.clone());
            panes.push(live);
        }

        room(Vec::new(), agents)
            .with_live_panes(panes, None)
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
    };
    let external = |id: &str, status: AgentStatus| agent("claude", id, status, 1_000);

    // A calm external sinks below calm project worktrees; an attention
    // worktree leads regardless of its name.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Failed, 1_000),
            agent_in("a2", "/repo/alpha", AgentStatus::Idle, 1_000),
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Idle),
        ]),
        vec!["alpha", "beta", "external"]
    );

    // The external catch-all rises out of the tail only when it holds an
    // attention agent (waiting or failed).
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["external", "beta"]
    );
}
