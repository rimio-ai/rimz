//! The in-process produce budget at fleet scale.
//!
//! The elder renderer runs [`rimz::sidebar::produce::produce_snapshot`] on its
//! fetch worker once per data tick (docs/internals/performance.md, the 2026-06
//! warm-producer pass). The contract: a warm steady-state produce — every
//! fork-bearing input pre-published fresh, the rollup folding O(new bytes)
//! through the worker's cursor — finishes far inside one data tick even with a
//! fleet-scale ledger and pane set, so the reconciling post never starves the
//! paint behind it. Companion to `compose_budget` in `sidebar_pane`,
//! which bounds the frame composition over the produced snapshot.

use std::time::{Duration, Instant};

use rimz::EventEnvelope;
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::AgentSessionId;
use rimz::ledger::event_log;
use rimz::sidebar::snapshot::RollupCursor;

use crate::common::Harness;

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
const ROUNDS: u32 = 20;

/// One registration per fleet slot, each in its own session and worktree
/// branch so the assembly's supersession keeps all `FLEET` agents. No
/// `worktree_path`, so the assembled groups carry no real roots and the
/// produce's git refresh has nothing to fork — the per-worktree git cost is
/// owned by the diff-stats cadence guards, not this budget.
fn lifecycle(h: &Harness, slot: usize) -> EventEnvelope {
    EventEnvelope::agent_lifecycle(
        h.workspace_id.clone(),
        format!("sess-{slot}"),
        "claude",
        "SessionStart",
        &registered_observation(slot),
    )
}

fn registered_observation(slot: usize) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(AgentSessionId::from(format!("agent-{slot}"))),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: None,
        worktree_branch: Some(format!("wt-{slot}")),
        task: None,
        prompt: None,
        transcript_path: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        pane_id: None,
        parent_agent_id: None,
    }
}

/// A live session pane, as `list-panes` would report it — no cwd, so the
/// grouping stays off the per-worktree git probes this budget does not own.
fn pane(i: usize) -> rimz::feed::PaneRef {
    rimz::feed::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::MuxName::Zellij, format!("terminal_{i}")),
        session_name: "rimz-perf".to_owned(),
        view_id: Some(format!("tab_{}", i % 8)),
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: None,
        is_focused: false,
        command: Some("zsh".to_owned()),
        spawn_command: None,
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[test]
fn warm_produce_stays_inside_the_data_tick_at_fleet_scale() {
    let h = Harness::new();
    let paths = h.ledger.paths();
    for i in 0..HISTORY_EVENTS {
        event_log::append(&paths.events_log, &lifecycle(&h, i % FLEET)).expect("seed event");
    }
    let panes: Vec<_> = (0..FLEET).map(pane).collect();
    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: "rimz-perf".to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: None,
    };
    let mut cursor = RollupCursor::new();

    // The cold produce pays the one-time history fold; uncounted, like the
    // first frame after attach.
    h.publish_fresh_produce_inputs("rimz-perf", panes.clone());
    rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
        .expect("cold produce");

    // Steady state: one delta per tick, every stamp young — the elder's
    // common case. Inputs re-publish outside the timed region.
    let mut elapsed = Duration::ZERO;
    for round in 0..ROUNDS {
        let event = lifecycle(&h, round as usize % FLEET);
        event_log::append(&paths.events_log, &event).expect("append delta");
        h.publish_fresh_produce_inputs("rimz-perf", panes.clone());
        let start = Instant::now();
        let snapshot =
            rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
                .expect("warm produce");
        elapsed += start.elapsed();
        assert_eq!(snapshot.agents.len(), FLEET);
    }

    let per_produce = elapsed / ROUNDS;
    assert!(
        per_produce < Duration::from_millis(50),
        "one warm fleet-scale produce took {per_produce:?}; the 1s data tick \
         leaves no room for an envelope that slow beside the paint it feeds"
    );
}
