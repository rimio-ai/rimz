use super::*;
use std::time::{Duration, Instant};

#[test]
fn held_sidecars_keep_values_without_stats_until_the_boundary_or_thaw() {
    let (_dir, runtime, _) = runtime();
    let at = Timestamp::from_second(100).unwrap();
    let mut agents: Vec<_> = ["held", "live"]
        .into_iter()
        .map(|id| crate::testkit::agent_state("claude", id, at))
        .collect();
    agents[0].hibernation = Some(crate::agents::state::Hibernation {
        frozen_at: at,
        reclaimed_bytes: 0,
        step: 0,
        wake_error: None,
    });
    let base = SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), agents, at);
    for id in ["held", "live"] {
        crate::store::agent_context::write(
            &runtime,
            "claude",
            id,
            &crate::agents::AgentContext::new("claude", at),
            None,
        )
        .unwrap();
        crate::store::subagent_context::update(&runtime, "claude", id, |_| {
            (
                serde_json::from_value(serde_json::json!({
                    "description": "child", "observed_at": at.to_string(),
                }))
                .unwrap(),
                None,
            )
        })
        .unwrap();
        crate::agent_activity::touch(
            &runtime,
            "claude",
            id,
            crate::agent_activity::ToolRun::Reset,
            true,
            at,
        )
        .unwrap();
    }
    let now = Instant::now();
    let mut observations = Vec::new();
    for seconds in [0, 30, 60, 61] {
        if seconds == 30 {
            crate::agent_activity::touch(
                &runtime,
                "claude",
                "held",
                crate::agent_activity::ToolRun::Reset,
                true,
                at + Duration::from_secs(1),
            )
            .unwrap();
        }
        let mut snapshot = base.clone();
        if seconds == 61 {
            Arc::make_mut(&mut snapshot.agents)[0].hibernation = None;
        }
        let before = crate::testkit::sidecar_read_counts();
        let enriched =
            enrich_agent_sidecars(snapshot, &runtime, &|| now + Duration::from_secs(seconds));
        let after = crate::testkit::sidecar_read_counts();
        assert_eq!(after.1 - before.1, 0);
        observations.push((
            after.0 - before.0,
            after.2 - before.2,
            enriched.agents[0].last_tool_at,
        ));
    }
    assert_eq!(
        observations,
        [
            (4, 4, Some(at)),
            (2, 0, Some(at)),
            (4, 0, Some(at + Duration::from_secs(1))),
            (4, 0, Some(at + Duration::from_secs(1))),
        ]
    );
}
