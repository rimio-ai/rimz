use super::*;
use crate::agents::ATTENTION_AGE_CEILING_SECS;

#[test]
fn group_tiering_floats_attention_and_always_tails_external() {
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

    // The external catch-all always tails, even when it holds an attention
    // agent: out-of-project work never displaces a project worktree.
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["beta", "external"]
    );

    // It tails below even an inactive project group — a stale, calm worktree
    // still outranks an out-of-project agent holding `failed`.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Idle, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["alpha", "external"]
    );
}
