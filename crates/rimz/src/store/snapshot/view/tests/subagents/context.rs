use super::*;

use crate::agents::context::SubagentContext;

#[test]
fn with_subagent_context_enriches_matching_children_and_preserves_lifecycle_type() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    child.model = None;
    child.usage.fresh_input_tokens = Some(1);
    child.usage.output_tokens = Some(9);
    let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
    fork.task = None;
    let mut typed = child_state("sess-root", "typed-1", AgentStatus::Running, 5);
    typed.task = Some("review".to_owned());
    typed.model = Some("lifecycle-model".to_owned());
    typed.usage.fresh_input_tokens = Some(42);
    let started = ago(100);

    let snapshot = room(vec![parent, child, fork, typed]);
    let folded = snapshot.with_subagent_context(vec![
        record(
            "child-1",
            SubagentContext {
                usage: Some(crate::agents::AgentUsageSummary {
                    fresh_input_tokens: Some(12_400),
                    ..Default::default()
                }),
                agent_type: None,
                model: Some("child-model".to_owned()),
                effort: Some("high".to_owned()),
                description: Some("locate the render path".to_owned()),
                cost_usd: Some(0.42),
                started_at: Some(started),
                observed_at: epoch(),
            },
        ),
        record(
            "fork-1",
            SubagentContext {
                usage: None,
                agent_type: Some("Explore".to_owned()),
                model: None,
                effort: None,
                description: Some("search the store".to_owned()),
                cost_usd: None,
                started_at: None,
                observed_at: epoch(),
            },
        ),
        record(
            "typed-1",
            SubagentContext {
                usage: None,
                agent_type: Some("SomethingElse".to_owned()),
                model: Some("sidecar-model".to_owned()),
                effort: Some("low".to_owned()),
                description: None,
                cost_usd: None,
                started_at: None,
                observed_at: epoch(),
            },
        ),
        record(
            "ghost",
            SubagentContext {
                usage: None,
                agent_type: None,
                model: None,
                effort: None,
                description: Some("nowhere".to_owned()),
                cost_usd: None,
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
    assert_eq!(child.context_used_tokens(), Some(12_400));
    assert_eq!(
        sub_agent_from_state(child, epoch(), false).tokens,
        Some(crate::store::snapshot::SubAgentTokens::Window(12_400))
    );
    assert_eq!(child.usage.output_tokens, Some(9));
    assert_eq!(child.subagent_cost_usd, Some(0.42));
    assert_eq!(child.subagent_started_at, Some(started));
    assert_eq!(child.model.as_deref(), Some("child-model"));
    assert_eq!(child.effort.as_deref(), Some("high"));

    let fork = rollup_agent(&folded, "fork-1");
    assert_eq!(fork.task.as_deref(), Some("Explore"));
    assert_eq!(fork.subagent_cost_usd, None);
    assert_eq!(
        fork.subagent_description.as_deref(),
        Some("search the store")
    );

    let typed = rollup_agent(&folded, "typed-1");
    assert_eq!(typed.context_used_tokens(), Some(42));
    assert_eq!(
        sub_agent_from_state(typed, epoch(), false).tokens,
        Some(crate::store::snapshot::SubAgentTokens::Window(42))
    );
    assert_eq!(
        typed.task.as_deref(),
        Some("review"),
        "lifecycle-established task is not overwritten by enrichment",
    );
    assert_eq!(
        typed.model.as_deref(),
        Some("lifecycle-model"),
        "lifecycle-established model is not overwritten by enrichment",
    );
    assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
}

fn record(agent_id: &str, context: SubagentContext) -> SubagentContextRecord {
    SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: agent_id.into(),
        context,
        usage_cursor: None,
    }
}
