use super::*;

use crate::agents::context::SubagentContext;

#[test]
fn with_subagent_context_enriches_matching_children_and_preserves_lifecycle_type() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
    fork.task = None;
    let mut typed = child_state("sess-root", "typed-1", AgentStatus::Running, 5);
    typed.task = Some("review".to_owned());
    let started = ago(100);

    let snapshot = room(Vec::new(), vec![parent, child, fork, typed]);
    let folded = snapshot.with_subagent_context(vec![
        record(
            "child-1",
            SubagentContext {
                agent_type: None,
                description: Some("locate the render path".to_owned()),
                token_count: Some(12_400),
                started_at: Some(started),
                observed_at: epoch(),
            },
        ),
        record(
            "fork-1",
            SubagentContext {
                agent_type: Some("Explore".to_owned()),
                description: Some("search the store".to_owned()),
                token_count: Some(5_000),
                started_at: None,
                observed_at: epoch(),
            },
        ),
        record(
            "typed-1",
            SubagentContext {
                agent_type: Some("SomethingElse".to_owned()),
                description: None,
                token_count: None,
                started_at: None,
                observed_at: epoch(),
            },
        ),
        record(
            "ghost",
            SubagentContext {
                agent_type: None,
                description: Some("nowhere".to_owned()),
                token_count: None,
                started_at: None,
                observed_at: epoch(),
            },
        ),
    ]);

    let child = rollup_agent(&folded, "child-1");
    assert_eq!(
        child.subagent_description.as_deref(),
        Some("locate the render path")
    );
    assert_eq!(child.total_tokens, Some(12_400));
    assert_eq!(child.subagent_started_at, Some(started));

    let fork = rollup_agent(&folded, "fork-1");
    assert_eq!(fork.task.as_deref(), Some("Explore"));
    assert_eq!(
        fork.subagent_description.as_deref(),
        Some("search the store")
    );

    let typed = rollup_agent(&folded, "typed-1");
    assert_eq!(
        typed.task.as_deref(),
        Some("review"),
        "lifecycle-established task is not overwritten by enrichment",
    );
    assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
}

fn record(agent_id: &str, context: SubagentContext) -> SubagentContextRecord {
    SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: agent_id.into(),
        context,
    }
}
