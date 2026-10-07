//! History-independence of the warm rollup fold.
//!
//! Every sidebar tab folds the event log on every wakeup, and tens of agents
//! push tens of events per second. The contract
//! (docs/internals/performance.md): a warm fold reads only the bytes
//! appended since its held base — one frame per event, never the log — so
//! per-tab work per event stays O(frame) while the log grows without bound.
//! Companion to `spending_incremental`, which proves the same shape for the
//! transcript walk.

use rimz::store::event_log::{self, testkit::bytes_read};
use rimz::store::snapshot::RollupCursor;
use rimz::testkit::carryover_bytes_parsed;
use rimz::testkit::fleet::{
    SESSION_NAME, registered_lifecycle, seed_ended_carryover, seed_fleet_store, synthetic_panes,
};

use crate::common::Harness;

const HISTORY_EVENTS: usize = 3_000;
const FLEET: usize = 30;

#[test]
fn delta_fold_is_o_new_bytes() {
    let h = Harness::new();
    let paths = h.store.paths();
    // Seed history through the raw log API: mutator tails would publish per
    // append, and the subject here is the reader's fold alone.
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let mut cursor = RollupCursor::new();
    let cold_before = bytes_read();
    let (cold_extent, _, _) = cursor.fold(paths).expect("cold fold");
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(cold_extent.offset, log_len, "the cold fold reaches the end");
    assert_eq!(cold_bytes, log_len, "a cold fold reads the whole history");

    // One event lands; the warm fold pays for that frame alone.
    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let warm_before = bytes_read();
    let (warm_extent, agents, _) = cursor.fold(paths).expect("warm fold");
    let warm_bytes = bytes_read() - warm_before;

    assert_eq!(warm_extent.offset, log_len + appended);
    assert_eq!(
        warm_bytes, appended,
        "a warm fold reads exactly the appended frame, independent of the \
         {cold_bytes}-byte history"
    );
    assert_eq!(agents.len(), FLEET, "the fold still lands the merged view");
}

#[test]
fn runtime_projection_uses_persisted_rollup_delta() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    h.store
        .append_event(&registered_lifecycle(&paths.workspace_id, 0))
        .expect("publish rollup");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let warm_before = bytes_read();
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("runtime projection");
    let warm_bytes = bytes_read() - warm_before;
    assert_eq!(projection.agents.len(), FLEET);
    assert_eq!(
        warm_bytes, 0,
        "a fresh persisted rollup keeps runtime projection off the {log_len}-byte history"
    );

    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let delta_before = bytes_read();
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("runtime projection after append");
    let delta_bytes = bytes_read() - delta_before;
    assert_eq!(projection.agents.len(), FLEET);
    assert_eq!(
        delta_bytes, appended,
        "runtime projection reads exactly the appended frame after its persisted base"
    );
}

/// The full produce pipeline inherits the cursor contract end to end: a
/// second [`rimz::sidebar::produce::produce_snapshot`] on one cursor reads
/// exactly the bytes appended since the first — the elder fetch worker's
/// steady state, where one warm cursor serves the fast lane and the produce
/// alike. Every fork-bearing enrichment input is pre-published fresh
/// ([`Harness::publish_fresh_produce_inputs`]), so the produce pays no mux
/// and no subprocess, and the byte counter isolates the rollup read.
#[test]
fn warm_produce_folds_o_new_bytes() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: rimz::diag::DiagSink::disabled(),
    };
    let mut cursor = RollupCursor::new();

    h.publish_fresh_produce_inputs(SESSION_NAME, synthetic_panes(1));
    let cold_before = bytes_read();
    let cold =
        rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
            .expect("cold produce");
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(
        cold_bytes, log_len,
        "a cold produce folds the whole history"
    );
    assert_eq!(cold.agents.len(), FLEET);

    // One event lands; the warm produce pays for that frame alone.
    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    h.publish_fresh_produce_inputs(SESSION_NAME, synthetic_panes(1));
    let warm_before = bytes_read();
    let warm =
        rimz::sidebar::produce::produce_snapshot(&mut cursor, paths, &h.runtime_paths, &opts)
            .expect("warm produce");
    let warm_bytes = bytes_read() - warm_before;
    assert_eq!(
        warm_bytes, appended,
        "a warm produce reads exactly the appended frame, independent of the \
         {cold_bytes}-byte history"
    );
    assert_eq!(
        warm.agents.len(),
        FLEET,
        "the produce lands the merged view"
    );
}

#[test]
fn warm_auto_continue_tick_reads_no_event_log_history() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();
    let mut cursor = RollupCursor::new();

    let cold_before = bytes_read();
    let cold = auto_continue_tick(&mut cursor, paths, &h.runtime_paths);
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(
        cold_bytes, log_len,
        "a cold auto-continue tick folds the log once through the rollup"
    );
    assert_eq!(cold.agents.len(), FLEET);

    for tick in 0..3 {
        let warm_before = bytes_read();
        let warm = auto_continue_tick(&mut cursor, paths, &h.runtime_paths);
        let warm_bytes = bytes_read() - warm_before;
        assert_eq!(
            warm_bytes, 0,
            "unchanged-log auto-continue tick {tick} must not read event-log history"
        );
        assert_eq!(warm.agents.len(), FLEET);
    }

    event_log::append(
        &paths.events_log,
        &registered_lifecycle(&paths.workspace_id, HISTORY_EVENTS % FLEET),
    )
    .expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let append_before = bytes_read();
    let warm = auto_continue_tick(&mut cursor, paths, &h.runtime_paths);
    let append_bytes = bytes_read() - append_before;
    assert_eq!(
        append_bytes, appended,
        "after one append, auto-continue tick reads only the appended frame"
    );
    assert_eq!(warm.agents.len(), FLEET);
}

fn auto_continue_tick(
    cursor: &mut RollupCursor,
    state: &rimz::StatePaths,
    runtime: &rimz::RuntimePaths,
) -> rimz::store::snapshot::SidebarSnapshot {
    let base = rimz::store::snapshot::build_with_cursor(state, cursor).expect("rollup");
    let mut config = rimz::config::MachineConfig::default();
    config.resume.auto_continue = true;
    let store = rimz::Store::open_existing(state.clone(), runtime.clone());
    rimz::sidebar::enrich::enrich(
        base,
        None,
        state,
        runtime,
        store.as_ref(),
        None,
        rimz::sidebar::enrich::FoldOpts {
            producing: true,
            fresh_roots: None,
            config: Some(std::sync::Arc::new(config)),
            lanes: None,
            agent_projection: Default::default(),
        },
        &rimz::diag::DiagSink::disabled(),
    )
}

/// Rotation preserves history in `agents.carryover.json`, and a busy room's
/// consumer folds against it on every wakeup. The contract: the carryover is
/// parsed once per file identity, so a warm or unchanged fold parses zero
/// carryover bytes however much history rotation kept, and an atomic
/// replacement re-parses exactly once. The folds run on one dedicated thread,
/// as a sidebar fetch worker does, so the seeding writer's own folds cannot
/// warm the parse this test measures.
#[test]
fn warm_fold_parses_unchanged_carryover_zero_times() {
    for history in [100, 800] {
        let h = Harness::new();
        let paths = h.store.paths().clone();
        seed_ended_carryover(&h.store, history).expect("stage carryover");
        seed_fleet_store(&paths, FLEET, HISTORY_EVENTS).expect("seed event");
        std::thread::spawn(move || assert_carryover_parsed_once(&paths, history))
            .join()
            .expect("fold thread");
    }
}

fn assert_carryover_parsed_once(paths: &rimz::StatePaths, history: usize) {
    let carryover_len = file_len(&paths.agents_carryover);
    let mut cursor = RollupCursor::new();

    let parsed_before = carryover_bytes_parsed();
    let (_, cold, _) = cursor.fold(paths).expect("cold fold");
    assert_eq!(
        carryover_bytes_parsed() - parsed_before,
        carryover_len,
        "a cold fold parses the carryover once"
    );
    assert_eq!(
        cold.len(),
        history + FLEET,
        "history merges beneath the fleet"
    );

    let fold_after_append = |cursor: &mut RollupCursor, slot: usize| {
        let log_len = file_len(&paths.events_log);
        event_log::append(
            &paths.events_log,
            &registered_lifecycle(&paths.workspace_id, slot),
        )
        .expect("append one");
        let appended = file_len(&paths.events_log) - log_len;
        let (read_before, parsed_before) = (bytes_read(), carryover_bytes_parsed());
        let (_, agents, _) = cursor.fold(paths).expect("warm fold");
        assert_eq!(
            bytes_read() - read_before,
            appended,
            "a warm fold reads the frame alone"
        );
        assert_eq!(agents.len(), history + FLEET);
        carryover_bytes_parsed() - parsed_before
    };

    assert_eq!(
        fold_after_append(&mut cursor, 0),
        0,
        "a warm fold parses none of the {carryover_len}-byte carryover ({history} rows)"
    );

    let (read_before, parsed_before) = (bytes_read(), carryover_bytes_parsed());
    let (_, unchanged, _) = cursor.fold(paths).expect("unchanged fold");
    assert_eq!(bytes_read() - read_before, 0);
    assert_eq!(
        carryover_bytes_parsed() - parsed_before,
        0,
        "an unchanged fold opens nothing"
    );
    assert_eq!(unchanged.len(), history + FLEET);

    // Rotation and prune republish by atomic rename; identical bytes under a
    // new inode still re-parse, exactly once.
    let staged = paths.agents_carryover.with_extension("json.staged");
    std::fs::copy(&paths.agents_carryover, &staged).expect("stage copy");
    std::fs::rename(&staged, &paths.agents_carryover).expect("replace carryover");
    assert_eq!(
        fold_after_append(&mut cursor, 1),
        carryover_len,
        "a replaced carryover re-parses once"
    );
    assert_eq!(
        fold_after_append(&mut cursor, 2),
        0,
        "then serves the new parse"
    );
}

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).expect("file meta").len()
}

#[test]
fn cold_carryover_trims_ended_text_and_shares_the_parse_with_audit_reads() {
    let h = Harness::new();
    seed_ended_carryover(&h.store, 3_300).expect("seed carryover");
    let paths = h.store.paths().clone();
    let carryover_len = file_len(&paths.agents_carryover);
    std::thread::spawn(move || {
        let before = carryover_bytes_parsed();
        let (_, agents, _) = RollupCursor::new().fold(&paths).expect("cold fold");
        assert_eq!(carryover_bytes_parsed() - before, carryover_len);
        for agent in agents.iter() {
            if agent.ended_at.is_none() {
                assert!(agent.first_prompt.is_some());
                assert!(!agent.recent_prompts.is_empty());
                if let Some(pane) = &agent.pane {
                    assert!(pane.foreground_cmdline.is_some());
                    assert!(pane.spawn_command.is_some());
                }
                continue;
            }
            let first_prompt = agent.first_prompt.as_deref().expect("ended prompt prefix");
            assert!(first_prompt.chars().count() <= 160, "bounded ended prefix");
            assert_eq!(first_prompt.lines().count(), 1);
            assert!(agent.prompt.is_none());
            assert!(agent.recent_prompts.is_empty());
            if let Some(pane) = &agent.pane {
                assert!(pane.foreground_cmdline.is_none());
                assert!(pane.spawn_command.is_none());
            }
        }
    })
    .join()
    .expect("fold thread");

    let before = carryover_bytes_parsed();
    for _ in 0..2 {
        let audit = h
            .store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit");
        assert_eq!(audit.agents.len(), 3_300);
    }
    assert_eq!(carryover_bytes_parsed() - before, carryover_len);
}

#[test]
fn delta_hydrates_an_ended_carried_row_with_one_raw_parse() {
    let h = Harness::new();
    seed_ended_carryover(&h.store, 100).expect("seed carryover");
    let paths = h.store.paths();
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&paths.agents_carryover).expect("carryover bytes"))
            .expect("carryover JSON");
    let mut cursor = RollupCursor::new();
    cursor.fold(paths).expect("cold fold");
    event_log::append(
        &paths.events_log,
        &rimz::store::event::EventEnvelope::new(
            paths.workspace_id.clone(),
            "session",
            "claude",
            "agent",
            "agent.launch_warnings",
            serde_json::json!({"agent_id": "history-0", "warnings": ["new warning"]}),
        ),
    )
    .expect("append delta");
    let before = carryover_bytes_parsed();
    let (_, agents, _) = cursor.fold(paths).expect("hydrate");
    assert_eq!(
        carryover_bytes_parsed() - before,
        file_len(&paths.agents_carryover)
    );
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "history-0")
        .expect("row");
    let row = serde_json::to_value(agent).expect("row JSON");
    assert_eq!(row["first_prompt"], raw["agents"][0]["first_prompt"]);
    assert_eq!(row["recent_prompts"], raw["agents"][0]["recent_prompts"]);
    assert_eq!(row["pane"], raw["agents"][0]["pane"]);
}

#[test]
fn explicit_agent_load_keeps_post_rotation_prompts() {
    for reuse_timestamp in [true, false] {
        let h = Harness::new();
        seed_ended_carryover(&h.store, 100).expect("seed carryover");
        let paths = h.store.paths();
        event_log::append(
            &paths.events_log,
            &rimz::store::event::EventEnvelope::new(
                paths.workspace_id.clone(),
                "session",
                "rimz",
                "cli",
                "test.noop",
                serde_json::json!({}),
            ),
        )
        .expect("append before rotation");
        h.store.rotate_event_log(1, None).expect("rotate");
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&paths.agents_carryover).expect("carryover bytes"),
        )
        .expect("carryover JSON");
        let carried: rimz::agents::AgentState = serde_json::from_value(
            raw["agents"]
                .as_array()
                .expect("rows")
                .iter()
                .find(|row| row["agent_id"] == "history-0")
                .expect("carried row")
                .clone(),
        )
        .expect("row");
        for (signal, seconds) in [
            (
                rimz::agents::LifecycleSignal::TurnStarted { turn_id: None },
                1,
            ),
            (rimz::agents::LifecycleSignal::Ended, 2),
        ] {
            let mut observation = rimz::agents::AgentLifecycleObservation::new(
                Some(carried.agent_id.clone()),
                signal,
            );
            observation.prompt =
                rimz::agents::SanitizedPrompt::new(Some("new post-rotation prompt"));
            let mut event = rimz::store::event::EventEnvelope::agent_lifecycle(
                paths.workspace_id.clone(),
                "session",
                "claude",
                "post-rotation",
                &observation,
            );
            event.timestamp = if reuse_timestamp {
                carried.last_seen
            } else {
                carried.last_seen + std::time::Duration::from_secs(seconds)
            };
            event_log::append(&paths.events_log, &event).expect("append lifecycle");
        }
        let audit = h
            .store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit");
        let advanced = audit
            .agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "history-0")
            .expect("advanced row");
        assert_eq!(advanced.prompt.as_deref(), Some("new post-rotation prompt"));
        assert!(advanced.ended_at.is_some());
        if reuse_timestamp {
            assert_eq!(advanced.ended_at, carried.ended_at);
            assert_eq!(advanced.last_seen, carried.last_seen);
        }
        let loaded = h.store.load_full_agent(advanced).expect("load");
        assert_eq!(
            loaded.prompt, advanced.prompt,
            "explicit loading must not overwrite an active-log prompt with carried text"
        );
        assert_eq!(loaded.recent_prompts, advanced.recent_prompts);
        assert_eq!(&loaded, advanced);
    }
}

#[test]
fn explicit_agent_load_restores_only_text_with_one_raw_parse() {
    let h = Harness::new();
    seed_ended_carryover(&h.store, 100).expect("seed carryover");
    let raw: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&h.store.paths().agents_carryover).expect("carryover bytes"),
    )
    .expect("carryover JSON");
    let full: rimz::agents::AgentState =
        serde_json::from_value(raw["agents"][0].clone()).expect("row");
    let mut trimmed = full.clone();
    trimmed.first_prompt = None;
    trimmed.prompt = None;
    trimmed.recent_prompts.clear();
    let pane = trimmed.pane.as_mut().expect("pane");
    pane.foreground_cmdline = None;
    pane.spawn_command = None;
    trimmed.launch_warnings = vec!["new warning".to_owned()];
    let before = carryover_bytes_parsed();
    let loaded = h.store.load_full_agent(&trimmed).expect("load");
    assert_eq!(
        loaded.first_prompt, full.first_prompt,
        "restore first prompt"
    );
    assert_eq!(loaded.prompt, full.prompt);
    assert_eq!(loaded.recent_prompts, full.recent_prompts);
    assert_eq!(loaded.pane, full.pane);
    assert_eq!(loaded.launch_warnings, trimmed.launch_warnings);
    assert_eq!(
        carryover_bytes_parsed() - before,
        file_len(&h.store.paths().agents_carryover)
    );
    trimmed.agent_id = rimz::ids::AgentSessionId::from("absent");
    assert_eq!(
        h.store.load_full_agent(&trimmed).expect("absent load"),
        trimmed
    );
}
