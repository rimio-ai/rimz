use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rimz::sidebar::consumer::{PublishedSnapshotReader, RollupCursor};
use rimz::sidebar::produce::{HostInputs, refresh_producer_caches};
use rimz::sidebar::refresh::ProducerRefreshState;
use rimz::testkit::{parse_cache_read_counts, read_cached_json};
use sha2::{Digest, Sha256};

#[test]
fn stamped_files_reparse_replacements_appends_rewrites_and_recovered_negatives() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value.json");
    let mut values = Vec::new();
    let mut parses = Vec::new();
    let before = parse_cache_read_counts();
    for step in 0..6 {
        match step {
            0 => {}
            1 => fs::write(&path, b"1").unwrap(),
            2 => {
                let old = File::open(&path).unwrap();
                let mtime = old.metadata().unwrap().modified().unwrap();
                let replacement = dir.path().join("replacement");
                fs::write(&replacement, b"2").unwrap();
                File::options()
                    .write(true)
                    .open(&replacement)
                    .unwrap()
                    .set_modified(mtime)
                    .unwrap();
                fs::rename(&replacement, &path).unwrap();
                drop(old);
            }
            3 => {
                OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(b"3")
                    .unwrap();
            }
            4 | 5 => {
                fs::write(&path, if step == 4 { b"{{" } else { b"24" }).unwrap();
                File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(
                        FileTimes::new()
                            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(100 + step)),
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let value = read_cached_json(&path).unwrap();
        values.push(value.as_deref().and_then(serde_json::Value::as_u64));
        let warm = read_cached_json(&path).unwrap();
        if let (Some(value), Some(warm)) = (value, warm) {
            assert!(
                Arc::ptr_eq(&value, &warm),
                "step {step} must share the parse"
            );
        }
        parses.push(parse_cache_read_counts().1 - before.1);
    }
    assert_eq!(values, [None, Some(1), Some(2), Some(23), None, Some(24)]);
    assert_eq!(parses, [0, 1, 2, 3, 4, 5]);
    assert_eq!(parse_cache_read_counts().0 - before.0, 12);
}

#[test]
fn warm_fold_and_enforcement_over_200_agents_parse_nothing() {
    let logs = tempfile::NamedTempFile::new().unwrap();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Arc::new(logs.reopen().unwrap()))
            .finish(),
    );
    let harness = crate::common::Harness::new();
    let paths = harness.store.paths();
    let runtime = &harness.runtime_paths;
    let frame = rimz::testkit::fleet::seed_scale_room(
        paths,
        runtime,
        rimz::testkit::fleet::ScaleRoom {
            agents: 200,
            panes: 200,
            ended_sessions: 0,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    let agents = harness.store.snapshot().unwrap().agents;
    let mut queue = Vec::new();
    let mut history = Vec::new();
    for (slot, agent) in agents.iter().enumerate() {
        let mut digest = Sha256::new();
        digest.update(agent.kind.as_str().as_bytes());
        digest.update([0]);
        digest.update(agent.agent_id.as_str().as_bytes());
        let digest = hex::encode(digest.finalize());
        let digest = &digest[..32];
        if slot % 3 != 0 {
            rimz::harness::budget::write_ledger(
                runtime,
                &agent.kind,
                &agent.agent_id,
                &rimz::harness::budget::BudgetLedger::new("10".parse().unwrap()),
            )
            .unwrap();
            if slot % 3 == 2 {
                fs::write(runtime.lane_path(format!("budget.{digest}.json")), b"{").unwrap();
            }
            let park = if slot % 3 == 1 {
                serde_json::to_vec(&serde_json::json!({
                    "kind": {"park": "rate_limit", "deadline": "2099-01-01T00:00:00Z"},
                    "parked_at_activity": agent.last_activity,
                    "retries": 0,
                }))
                .unwrap()
            } else {
                b"{".to_vec()
            };
            fs::write(
                runtime.lane_path(format!("auto-continue.{digest}.json")),
                park,
            )
            .unwrap();
        }
        let mut message = rimz::store::message::MessageRecord::new(
            paths.workspace_id.clone(),
            agent,
            "human input".to_owned(),
            rimz::store::message::DeliveryGate::Done,
        );
        serde_json::to_writer(&mut queue, &message).unwrap();
        queue.push(b'\n');
        message.status = rimz::store::message::MessageStatus::Delivered;
        serde_json::to_writer(&mut history, &message).unwrap();
        history.push(b'\n');
        rimz::store::agent_context::write(
            runtime,
            agent.kind.as_str(),
            agent.agent_id.as_str(),
            &rimz::agents::AgentContext::new("test", jiff::Timestamp::now()),
            None,
        )
        .unwrap();
    }
    queue.extend_from_slice(b"{\"message_id\"");
    fs::create_dir_all(&paths.messages_dir).unwrap();
    fs::write(paths.message_queue_file(), queue).unwrap();
    fs::create_dir_all(&paths.message_history_dir).unwrap();
    fs::write(paths.message_history_dir.join("2026-01-01.jsonl"), history).unwrap();
    rimz::disk::atomic::write_temp_then_rename_cache(
        &runtime.lane_path("budget.scopes.json"),
        &rimz::harness::budget::BudgetScopeState::default(),
    )
    .unwrap();
    for scope in [
        rimz::harness::budget::DailyBudgetScope::Fleet,
        rimz::harness::budget::DailyBudgetScope::Account("claude@default".parse().unwrap()),
    ] {
        scope
            .write_ledger(runtime, paths, &Default::default())
            .unwrap();
    }
    let mut park_config = rimz::config::MachineConfig::default();
    park_config.resume.auto_continue = true;
    let read_parks = || rimz::testkit::park_demotion(&harness.store, &park_config, &agents);
    read_parks();
    let mut reader =
        PublishedSnapshotReader::new(runtime.clone(), frame.session_name.clone(), None);
    reader.read(paths).unwrap();
    read_parks();
    let before = parse_cache_read_counts();
    reader.read(paths).unwrap();
    read_parks();
    let after = parse_cache_read_counts();
    assert!(
        after.0 - before.0 >= 200,
        "every ledger, including missing ones, is stat'd"
    );
    assert_eq!(after.1 - before.1, 0, "warm fold reparsed unchanged files");

    let inputs = HostInputs::from_frame(frame);
    let mut cursor = RollupCursor::new();
    let mut refresh = ProducerRefreshState::default();
    refresh_producer_caches(&mut cursor, paths, runtime, Some(&inputs), &mut refresh).unwrap();
    let before = parse_cache_read_counts();
    let sidecars = rimz::testkit::sidecar_read_counts();
    refresh_producer_caches(&mut cursor, paths, runtime, Some(&inputs), &mut refresh).unwrap();
    let after = parse_cache_read_counts();
    assert!(
        after.0 - before.0 >= 400,
        "both fold and enforcement must use stamped readers"
    );
    assert_eq!(
        after.1 - before.1,
        0,
        "warm enforcement reparsed unchanged files"
    );
    assert_eq!(
        rimz::testkit::sidecar_read_counts().2 - sidecars.2,
        0,
        "warm ctx reads must not parse"
    );
    let logs = fs::read_to_string(logs.path()).unwrap();
    assert_eq!(
        logs.matches("skipping torn trailing message queue record")
            .count(),
        1,
        "{logs}"
    );
}

#[test]
fn warm_auto_continue_over_200_agents_reads_no_unchanged_park_bytes() {
    use rimz::agents::{AgentContext, AgentStatus, AgentTurnError, TurnErrorClass};
    use rimz::testkit::host_pass::{Counter, thread_counters};

    let harness = crate::common::Harness::new();
    let runtime = &harness.runtime_paths;
    let now = jiff::Timestamp::from_second(10_000).unwrap();
    let mut agents = Vec::new();
    for slot in 0..200 {
        let mut agent = rimz::testkit::agent_state("claude", &format!("warm{slot}"), now);
        agent.status = AgentStatus::Idle;
        if slot % 3 == 1 {
            let mut digest = Sha256::new();
            digest.update(agent.kind.as_str().as_bytes());
            digest.update([0]);
            digest.update(agent.agent_id.as_str().as_bytes());
            let digest = hex::encode(digest.finalize());
            fs::write(
                runtime.lane_path(format!("auto-continue.{}.json", &digest[..32])),
                serde_json::to_vec(&serde_json::json!({
                    "kind": {"park": "rate_limit", "deadline": "2099-01-01T00:00:00Z"},
                    "parked_at_activity": now,
                    "retries": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        } else if slot % 3 == 2 {
            agent.status = AgentStatus::Running;
            agent.last_activity = jiff::Timestamp::from_second(9_999).unwrap();
            let mut context = AgentContext::new("claude", now);
            context.turn_error = Some(AgentTurnError {
                class: TurnErrorClass::PausedOverloaded,
                at: now,
                label: None,
            });
            agent.context = Some(context);
            assert!(agent.displayed_turn_error().is_some());
        }
        agents.push(agent);
    }
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        agents,
        now,
    );
    let mut config = rimz::config::MachineConfig::default();
    config.resume.auto_continue = true;
    rimz::testkit::resume_parked(&harness.store, &config, &snapshot);
    rimz::testkit::resume_parked(&harness.store, &config, &snapshot);
    let counters = thread_counters();
    let reads = counters.get(Counter::ParkRead);
    let parses = parse_cache_read_counts().1;
    rimz::testkit::resume_parked(&harness.store, &config, &snapshot);
    assert_eq!(counters.get(Counter::ParkRead) - reads, 0);
    assert_eq!(parse_cache_read_counts().1 - parses, 0);
}

#[test]
fn refresher_keeps_held_fold_context_cached() {
    let harness = crate::common::Harness::new();
    let paths = harness.store.paths();
    let runtime = &harness.runtime_paths;
    let frame = rimz::testkit::fleet::seed_scale_room(
        paths,
        runtime,
        rimz::testkit::fleet::ScaleRoom {
            agents: 2,
            panes: 2,
            ended_sessions: 0,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    let snapshot = harness.store.snapshot().unwrap();
    let held = &snapshot.agents[0];
    let mut context = rimz::agents::AgentContext::new("test", jiff::Timestamp::now());
    context.model_id = Some("retained-model".to_owned());
    rimz::store::agent_context::write(
        runtime,
        held.kind.as_str(),
        held.agent_id.as_str(),
        &context,
        None,
    )
    .unwrap();
    rimz::store::hibernate::record(
        paths,
        rimz::store::hibernate::Entry {
            kind: held.kind.clone(),
            agent_id: held.agent_id.clone(),
            pane_id: frame.to_pane_refs()[0].pane_id.clone(),
            scope: Default::default(),
            provider_pid: 1,
            frozen_at: jiff::Timestamp::now(),
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 0,
        },
    )
    .unwrap();
    let mut reader =
        PublishedSnapshotReader::new(runtime.clone(), frame.session_name.clone(), None);
    let first = reader.read(paths).unwrap();
    let row = first
        .agents
        .iter()
        .find(|agent| agent.agent_id == held.agent_id)
        .unwrap();
    assert!(
        row.hibernation.is_some(),
        "the test requires a held context read"
    );
    assert_eq!(
        row.context
            .as_ref()
            .and_then(|context| context.model_id.as_deref()),
        Some("retained-model")
    );
    let inputs = HostInputs::from_frame(frame);
    let mut cursor = RollupCursor::new();
    let mut refresh = ProducerRefreshState::default();
    for _ in 0..2 {
        refresh_producer_caches(&mut cursor, paths, runtime, Some(&inputs), &mut refresh).unwrap();
    }
    let after = reader.read(paths).unwrap();
    let row = after
        .agents
        .iter()
        .find(|agent| agent.agent_id == held.agent_id)
        .unwrap();
    assert_eq!(
        row.context
            .as_ref()
            .and_then(|context| context.model_id.as_deref()),
        Some("retained-model"),
        "a due-session scan must not evict another consumer's held context"
    );
}
