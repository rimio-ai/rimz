//! Stored row byte budgets over a fleet with full launch argv and prompts.

use rimz::EventEnvelope;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, SanitizedPrompt};
use rimz::ids::AgentSessionId;
use rimz::store::event_log;
use rimz::store::snapshot::RollupCursor;
use rimz::testkit::fleet::synthetic_panes;

use crate::common::Harness;

const AGENTS: usize = 200;
const ROLLUP_ROW_BYTES_CEILING: usize = 2816;

#[test]
fn ended_rows_keep_rollup_and_carryover_within_byte_budget() {
    let h = Harness::new();
    let paths = h.store.paths();
    let prompt = "p".repeat(6 * 1024);
    let argv = "rimz agents exec claude --request ".repeat(48);
    for (slot, mut pane) in synthetic_panes(AGENTS).into_iter().enumerate() {
        let id = AgentSessionId::from(format!("history-{slot}"));
        pane.spawn_command = Some(argv.clone());
        pane.foreground_cmdline = Some(argv.clone());
        let mut observation =
            AgentLifecycleObservation::new(Some(id.clone()), LifecycleSignal::Registered);
        observation.pane_stamp = Some(pane);
        observation.prompt = SanitizedPrompt::new(Some(&prompt));
        event_log::append(
            &paths.events_log,
            &EventEnvelope::agent_lifecycle(
                paths.workspace_id.clone(),
                "rimz-perf",
                "claude",
                "SessionStart",
                &observation,
            ),
        )
        .expect("register history row");
        let ended = AgentLifecycleObservation::new(Some(id), LifecycleSignal::Ended);
        event_log::append(
            &paths.events_log,
            &EventEnvelope::agent_lifecycle(
                paths.workspace_id.clone(),
                "rimz-perf",
                "claude",
                "SessionEnd",
                &ended,
            ),
        )
        .expect("end history row");
    }

    let (_, agents, _) = RollupCursor::new().fold(paths).expect("cold fold");
    assert_eq!(agents.len(), AGENTS);
    assert!(agents.iter().all(|agent| agent.ended_at.is_some()));
    h.store
        .append_event(&EventEnvelope::new(
            paths.workspace_id.clone(),
            "rimz-perf",
            "rimz",
            "cli",
            "test.publish",
            serde_json::json!({}),
        ))
        .expect("publish cold fold");
    let rollup = std::fs::read_to_string(&paths.rollup_cache).expect("rollup cache");
    h.store.rotate_event_log(1, None).expect("rotate history");
    let carryover = std::fs::read_to_string(&paths.agents_carryover).expect("carryover");
    let rollup_bytes = rollup.len();
    let carryover_bytes = carryover.len();
    for (label, contents, row_key) in [
        ("rollup", rollup, "raw_agents"),
        ("carryover", carryover, "agents"),
    ] {
        let value: serde_json::Value = serde_json::from_str(&contents).expect("stored JSON");
        assert_eq!(value[row_key].as_array().unwrap().len(), AGENTS);
        assert!(
            contents.len() <= ROLLUP_ROW_BYTES_CEILING * AGENTS,
            "{label}: {} bytes for {AGENTS} rows exceeds {ROLLUP_ROW_BYTES_CEILING} bytes per row (rollup {rollup_bytes}, carryover {carryover_bytes})",
            contents.len()
        );
        for field in ["spawn_command", "foreground_cmdline", "recent_prompts"] {
            assert!(!contents.contains(field), "{label} still stores {field}");
        }
    }
}
