use super::*;
use crate::agents::ATTENTION_AGE_CEILING_SECS;

fn ranked_snapshot(mut agents: Vec<AgentState>) -> SidebarSnapshot {
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

    room(Vec::new(), agents).with_live_panes(panes, None)
}

fn labels(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect()
}

#[test]
fn group_tiering_floats_attention_and_always_tails_external() {
    let labels_for = |agents: Vec<AgentState>| labels(&ranked_snapshot(agents));
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

#[test]
fn git_rung_orders_calm_worktree_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("unknown", "/repo/unknown", AgentStatus::Idle, 1_000),
        agent_in("merged", "/repo/merged", AgentStatus::Idle, 1_000),
        agent_in("dirty", "/repo/dirty", AgentStatus::Idle, 1_000),
        agent_in("clean", "/repo/clean", AgentStatus::Idle, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        match group.label.as_str() {
            "dirty" => group.clean = Some(false),
            "clean" => {
                group.clean = Some(true);
                group.landed = Some(false);
            }
            "merged" => {
                group.clean = Some(true);
                group.landed = Some(true);
            }
            "unknown" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["dirty", "clean", "unknown", "merged"],
        "git rung decides among calm groups after band and urgency tie"
    );
}

#[test]
fn attention_member_overrides_git_rung() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("dirty-idle", "/repo/dirty", AgentStatus::Idle, 1_000),
        agent_in("merged-wait", "/repo/merged", AgentStatus::Waiting, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        match group.label.as_str() {
            "dirty" => group.clean = Some(false),
            "merged" => {
                group.clean = Some(true);
                group.landed = Some(true);
            }
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["merged", "dirty"],
        "attention urgency beats the merged git rung"
    );
}

#[test]
fn external_group_tails_even_merged_project_work() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged-idle", "/repo/merged", AgentStatus::Idle, 1_000),
        agent("claude", "external-fail", AgentStatus::Failed, 1_000),
    ]);
    let merged = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "merged")
        .expect("merged group present");
    merged.clean = Some(true);
    merged.landed = Some(true);
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["merged", "external"],
        "external stays the hard tail below even a calm merged project group"
    );
}
