use super::*;

#[test]
fn with_subagent_context_folds_onto_child_by_key() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let started = ago(100);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("locate the render seam".to_owned()),
            token_count: Some(12_400),
            started_at: Some(started),
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let child = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child in rollup");
    assert_eq!(
        child.subagent_description.as_deref(),
        Some("locate the render seam")
    );
    assert_eq!(child.total_tokens, Some(12_400));
    assert_eq!(child.subagent_started_at, Some(started));

    // A record whose child is absent from the rollup is dropped — the key it
    // is filed under is authority.
    let absent = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "ghost".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("nowhere".to_owned()),
            token_count: None,
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = folded.with_subagent_context(vec![absent]);
    assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
}

#[test]
fn with_subagent_context_back_fills_task_from_agent_type() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // A fork child: parent_agent_id set, task None (no agent_type in SubagentStart).
    let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
    fork.task = None;
    let snapshot = room(Vec::new(), vec![parent, fork]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "fork-1".into(),
        context: SubagentContext {
            agent_type: Some("Explore".to_owned()),
            description: Some("search the ledger".to_owned()),
            token_count: Some(5_000),
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let fork = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "fork-1")
        .expect("fork in rollup");
    assert_eq!(
        fork.task.as_deref(),
        Some("Explore"),
        "agent_type back-fills task"
    );
    assert_eq!(
        fork.subagent_description.as_deref(),
        Some("search the ledger")
    );
}

#[test]
fn with_subagent_context_does_not_overwrite_existing_task() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // Typed child: task already set by SubagentStart.
    let mut typed = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    typed.task = Some("review".to_owned());
    let snapshot = room(Vec::new(), vec![parent, typed]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: Some("SomethingElse".to_owned()),
            description: None,
            token_count: None,
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let typed = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child in rollup");
    assert_eq!(
        typed.task.as_deref(),
        Some("review"),
        "lifecycle-established task must not be overwritten by enrichment",
    );
}
