use super::*;

#[test]
fn ended_labels_survive_lifecycle_and_launch_revival_in_every_fold() {
    use crate::agents::{AgentLifecycleObservation, SanitizedPrompt};

    for launch_revival in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = checkpoint_paths(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let first = format!("first\n {}", "é🦊".repeat(100));
        let latest = format!("latest\n {}", "different ".repeat(30));
        let first_label = format!("first {}", "é🦊".repeat(100))
            .chars()
            .take(160)
            .collect::<String>();
        let latest_label = format!("latest {}", "different ".repeat(30))
            .chars()
            .take(160)
            .collect::<String>()
            .trim_end()
            .to_owned();
        let lifecycle = |signal, prompt: Option<&str>, seconds| {
            let mut observation = AgentLifecycleObservation::new(Some("agent".into()), signal);
            observation.prompt = SanitizedPrompt::new(prompt);
            let mut event = EventEnvelope::agent_lifecycle(
                workspace.clone(),
                "session",
                "claude",
                "test.lifecycle",
                &observation,
            );
            event.timestamp = epoch() + Duration::from_secs(seconds);
            event
        };
        let mut revive = lifecycle(lifecycle::LifecycleSignal::Registered, None, 3);
        if launch_revival {
            revive = EventEnvelope::agent_launched(
                workspace.clone(),
                "session",
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    system_prompt_digest: None,
                    agent_id: "agent".into(),
                    launch_id: None,
                    agent_name: "lucid-atlas".to_owned(),
                    agent_name_explicit: false,
                    launch: LaunchParams::default(),
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: None,
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            );
            revive.timestamp = epoch() + Duration::from_secs(3);
        }
        let events = [
            lifecycle(
                lifecycle::LifecycleSignal::TurnStarted { turn_id: None },
                Some(&first),
                1,
            ),
            lifecycle(lifecycle::LifecycleSignal::Ended, Some(&latest), 2),
            revive,
            lifecycle(
                lifecycle::LifecycleSignal::TurnStarted { turn_id: None },
                Some("new\n prompt"),
                4,
            ),
            lifecycle(lifecycle::LifecycleSignal::Ended, None, 5),
        ];
        let mut cursor = RollupCursor::new();
        for (index, event) in events.iter().enumerate() {
            event_log::append(&paths.events_log, event).unwrap();
            let (warm_cache, warm, _) = catch_up_rollup(&paths).unwrap();
            let (cursor_extent, cursor_rows, _) = cursor.fold(&paths).unwrap();
            let (thread_cache, thread_rows, _) = thread_catch_up(&paths).unwrap();
            if paths.rollup_cache().unwrap().exists() {
                fs::remove_file(paths.rollup_cache().unwrap()).unwrap();
            }
            let (cold_cache, cold, _) = catch_up_rollup(&paths).unwrap();
            assert_eq!(warm, cold);
            assert_eq!(cursor_rows.to_vec(), cold);
            assert_eq!(thread_rows.to_vec(), cold);
            assert_eq!(warm_cache.raw_agents, cold_cache.raw_agents);
            assert_eq!(cursor_extent, cold_cache.extent);
            assert_eq!(thread_cache.extent, cold_cache.extent);
            let row = &cold[0];
            assert_eq!(row.first_prompt.as_deref(), Some(first_label.as_str()));
            let expected_prompt = match index {
                0 => first_label.as_str(),
                1 | 2 => latest_label.as_str(),
                _ => "new prompt",
            };
            assert_eq!(row.prompt.as_deref(), Some(expected_prompt));
            assert_eq!(row.ended_at.is_some(), matches!(index, 1 | 4));
            write_rollup_cache(&paths, &warm_cache).unwrap();
        }
    }
}

#[test]
fn catch_up_rollup_rejects_a_zeroed_middle_frame() {
    // Crash-sim for writeback reordering: an earlier frame's data pages
    // zeroed while a later frame survived. Reachable only when the
    // single-writer flock contract is broken (concurrent `O_APPEND`
    // appenders), so the fold must fail loudly rather than silently drop
    // the surviving frames behind the hole — this is the recovery
    // boundary the `O_APPEND` rejection in
    // `docs/internals/performance.md` leans on.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = checkpoint_paths(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    let mut frame_ends = Vec::new();
    for agent_id in ["a", "b", "c"] {
        event_log::append(
            &paths.events_log,
            &lifecycle_at(
                &workspace,
                "claude",
                "SessionStart",
                agent_id,
                lifecycle::LifecycleSignal::Registered,
            ),
        )
        .unwrap();
        frame_ends.push(std::fs::metadata(&paths.events_log).unwrap().len());
    }

    // Zero the middle frame's bytes but keep its newline terminator, so
    // the log still frames three entries with an unreadable second one.
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&paths.events_log)
        .unwrap();
    file.seek(SeekFrom::Start(frame_ends[0])).unwrap();
    let hole = usize::try_from(frame_ends[1] - frame_ends[0] - 1).unwrap();
    file.write_all(&vec![0u8; hole]).unwrap();
    drop(file);

    let err = catch_up_rollup(&paths).expect_err("a zeroed middle frame must fail the fold");
    assert!(
        matches!(
            err,
            SnapshotErr::EventLog(
                event_log::EventLogErr::Torn { .. } | event_log::EventLogErr::FrameLength { .. }
            )
        ),
        "torn middle frame surfaces as a hard event-log error, never a silent drop: {err:?}"
    );
}

/// One generated event. Actions 12-14 are a swap of `agent_id`'s launch at
/// `epoch` (its reservation starting, bound, failed) and 15 its session's
/// registration stamped at `epoch`; a lifecycle action is stamped one epoch
/// lower when `epoch` is above 0, and unstamped at 0.
fn sequence_event(
    workspace: &WorkspaceId,
    kind: &str,
    agent_id: &str,
    action: usize,
    epoch: u32,
    timestamp: Timestamp,
) -> EventEnvelope {
    let launch = format!("launch_{agent_id}");
    let mut event = if (12..15).contains(&action) {
        let key = if epoch == 0 {
            launch.clone()
        } else {
            format!("{launch}_{epoch}")
        };
        EventEnvelope::agent_launched(
            workspace.clone(),
            "session",
            &AgentKind::new_unchecked(kind),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: key.clone().into(),
                launch_id: Some(launch.into()),
                agent_name: format!("swap-{agent_id}"),
                agent_name_explicit: false,
                launch: LaunchParams {
                    epoch: (epoch > 0).then_some(epoch),
                    ..LaunchParams::default()
                },
                state: match action {
                    12 => AgentLaunchState::Starting,
                    13 => AgentLaunchState::Bound,
                    _ => AgentLaunchState::Failed,
                },
                run_id: None,
                pane_id: (action == 13).then(|| PaneId::parse(&format!("tmux:%{epoch}")).unwrap()),
                runtime_owner: (action == 13).then(|| RuntimeOwner {
                    ancestors: vec![100 + epoch, 7],
                    ..RuntimeOwner::new(RuntimeOwnerKind::Agent, key, 9 + epoch, None)
                }),
                worktree_path: None,
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        )
    } else if action == 15 {
        let mut event = lifecycle_at(
            workspace,
            kind,
            "SessionStart",
            agent_id,
            lifecycle::LifecycleSignal::Registered,
        );
        let mut params = event.params_value();
        params["launch_id"] = serde_json::json!(launch);
        params["pane_id"] = serde_json::json!(format!("tmux:%{epoch}"));
        params["runtime_owner"] = serde_json::json!({
            "kind": "agent",
            "subject_id": agent_id,
            "pid": 20 + epoch,
            "ancestors": [200 + epoch, 7],
        });
        if epoch > 0 {
            params["epoch"] = serde_json::json!(epoch);
        }
        event.params = serde_json::value::to_raw_value(&params).unwrap();
        event
    } else if action >= 10 {
        let mut params = serde_json::json!({"agent_id": agent_id, "setting": "keep-warm"});
        if action == 10 {
            params["value"] = serde_json::json!("2h");
        }
        EventEnvelope::new(
            workspace.clone(),
            "session",
            kind,
            "agent",
            "agent.tune",
            params,
        )
    } else if matches!(action, 4 | 5) {
        let (step, status, method) = if action == 4 {
            ("wrap_up", MessageStatus::Sent, MessageEventMethod::Sent)
        } else {
            (
                "continue",
                MessageStatus::Queued,
                MessageEventMethod::Queued,
            )
        };
        let mut record = MessageRecord::new(
            workspace.clone(),
            &agent(kind, agent_id, AgentStatus::Running, 0),
            "soft compaction".to_owned(),
            DeliveryGate::Any,
        );
        record.status = status;
        let mut event = EventEnvelope::message_event(&record, "session", method, None);
        let mut params = event.params_value();
        params["soft_compact"] = serde_json::json!(step);
        event.params = serde_json::value::to_raw_value(&params).unwrap();
        event
    } else {
        let (name, signal) = match action {
            1 => (
                "UserPromptSubmit",
                lifecycle::LifecycleSignal::TurnStarted { turn_id: None },
            ),
            2 => (
                "Stop",
                lifecycle::LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
            ),
            3 => ("SessionEnd", lifecycle::LifecycleSignal::Ended),
            6 => (
                "Interrupt",
                lifecycle::LifecycleSignal::TurnInterrupted { turn_id: None },
            ),
            7 => (
                "Error",
                lifecycle::LifecycleSignal::TurnEnded {
                    errored: true,
                    parked_on_background: false,
                    turn_id: None,
                },
            ),
            8 => ("PreCompact", lifecycle::LifecycleSignal::Compacting),
            9 => (
                "PostCompact",
                lifecycle::LifecycleSignal::CompactionEnded {
                    auto: Some(true),
                    failed: false,
                },
            ),
            _ => ("SessionStart", lifecycle::LifecycleSignal::Registered),
        };
        let mut event = lifecycle_at(workspace, kind, name, agent_id, signal);
        if epoch > 0 {
            let mut params = event.params_value();
            params["launch_id"] = serde_json::json!(launch);
            if epoch > 1 {
                params["epoch"] = serde_json::json!(epoch - 1);
            }
            event.params = serde_json::value::to_raw_value(&params).unwrap();
        }
        event
    };
    event.timestamp = timestamp;
    event
}

proptest::proptest! {
    /// The keystone fold invariant, generalized from the hand-built case
    /// above: over arbitrary lifecycle sequences and an arbitrary
    /// prefix/delta split, resuming from a persisted base equals folding
    /// the whole log cold — agents and extent alike, swaps and their stamped
    /// events included.
    #[test]
    fn fold_seed_delta_equals_fold_empty_all_over_arbitrary_sequences(
        seq in proptest::collection::vec((0usize..2, 0usize..16, 0usize..3, 0u32..3), 1..12),
        split in proptest::prelude::any::<proptest::sample::Index>(),
    ) {
        use proptest::prelude::prop_assert_eq;

        const KINDS: [&str; 2] = ["claude", "codex"];
        const AGENTS: [&str; 3] = ["a", "b", "c"];

        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = checkpoint_paths(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();

        let events: Vec<_> = seq
            .iter()
            .enumerate()
            .map(|(index, &(kind, action, agent_id, swap_epoch))| sequence_event(
                &workspace, KINDS[kind], AGENTS[agent_id], action, swap_epoch,
                epoch() + Duration::from_secs(index as u64),
            ))
            .collect();
        let at = split.index(events.len() + 1);

        for event in &events[..at] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (prefix_cache, _, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths, &prefix_cache).unwrap();
        let mut cursor = RollupCursor::new();
        cursor.fold(&paths).unwrap();
        // Cases share this thread, so the thread's held fold also crosses
        // from the previous case's workspace here.
        thread_catch_up(&paths).unwrap();
        for event in &events[at..] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (incremental_cache, incremental, _) = catch_up_rollup(&paths).unwrap();
        let (cursor_extent, cursor_merged, _) = cursor.fold(&paths).unwrap();
        let (thread_cache, thread_merged, _) = thread_catch_up(&paths).unwrap();

        std::fs::remove_file(paths.rollup_cache().unwrap()).unwrap();
        let (cold_cache, cold, _) = catch_up_rollup(&paths).unwrap();

        prop_assert_eq!(
            sorted_value(incremental),
            sorted_value(cold.clone()),
            "fold(seed, delta) == fold(empty, all)"
        );
        prop_assert_eq!(
            sorted_value(cursor_merged.to_vec()),
            sorted_value(cold.clone()),
            "the warm in-memory cursor fold equals the cold fold"
        );
        prop_assert_eq!(
            sorted_value(thread_merged.to_vec()),
            sorted_value(cold),
            "the thread-held fold equals the cold fold"
        );
        prop_assert_eq!(thread_cache.extent, cold_cache.extent);
        prop_assert_eq!(cursor_extent, cold_cache.extent);
        prop_assert_eq!(
            sorted_value(
                incremental_cache.raw_agents.into_iter().map(Arc::unwrap_or_clone).collect()
            ),
            sorted_value(
                cold_cache.raw_agents.into_iter().map(Arc::unwrap_or_clone).collect()
            )
        );
        prop_assert_eq!(incremental_cache.extent, cold_cache.extent);
    }
}

#[test]
fn rebirth_boundary_unstamps_carryover_agents() {
    // The carryover predates every event in the current log, so a rebirth
    // boundary anywhere in the window clears its stamps too — a rotated-out
    // session can no more own a reborn pane id than an in-log one.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let carried = agent("codex", "sess-old", AgentStatus::Success, 1_000).in_pane("terminal_6");
    let boundary = EventEnvelope::session_rebirth(workspace, "session");

    let merged = agent_rollup_with_carryover(&[boundary], vec![carried.clone()]);
    assert_eq!(
        merged.len(),
        1,
        "the boundary unstamps, it never ends a session"
    );
    assert!(
        merged[0].pane.is_none(),
        "a carryover stamp predates the boundary and clears"
    );

    let untouched = agent_rollup_with_carryover(&[], vec![carried]);
    assert!(
        untouched[0].pane.is_some(),
        "no boundary in the window keeps the carryover stamp"
    );
}

/// A launch event of the agent `launch_a` under row key `key` at `epoch`.
fn swap_launch(
    state: AgentLaunchState,
    key: &str,
    epoch: u32,
    pane: Option<&str>,
) -> EventEnvelope {
    let mut payload = AgentLaunchPayload {
        system_prompt_digest: None,
        agent_id: key.into(),
        launch_id: Some("launch_a".into()),
        agent_name: "coder".to_owned(),
        agent_name_explicit: false,
        launch: LaunchParams::default(),
        state,
        run_id: None,
        pane_id: pane.map(|raw| PaneId::parse(raw).unwrap()),
        runtime_owner: None,
        worktree_path: None,
        worktree_branch: None,
        prompt: None,
        description: None,
    };
    payload.launch.epoch = (epoch > 0).then_some(epoch);
    EventEnvelope::agent_launched(
        WorkspaceId::from_project_root(Path::new("/tmp/x")),
        "session",
        &AgentKind::new_unchecked("codex"),
        payload,
    )
}

/// A hook of `session` on `pane`, stamped with `launch_a` at `epoch`.
fn swap_hook(
    workspace: &WorkspaceId,
    session: &str,
    epoch: u32,
    signal: lifecycle::LifecycleSignal,
    name: &str,
    pane: &str,
) -> EventEnvelope {
    let mut event = lifecycle_at(workspace, "codex", name, session, signal);
    let mut params = event.params_value();
    params["launch_id"] = serde_json::json!("launch_a");
    params["pane_id"] = serde_json::json!(pane);
    if epoch > 0 {
        params["epoch"] = serde_json::json!(epoch);
    }
    event.params = serde_json::value::to_raw_value(&params).unwrap();
    event
}

fn swap_holder(agents: &[AgentState]) -> Vec<AgentState> {
    use crate::agents::state::{launch_holder, launch_members};

    let members = launch_members(
        agents,
        &AgentKind::new_unchecked("codex"),
        &"launch_a".into(),
    );
    launch_holder(&members).into_iter().cloned().collect()
}

#[test]
fn a_carryover_boundary_inside_a_swap_keeps_the_holder() {
    for boundary in [3, 4] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = checkpoint_paths(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let registered = lifecycle::LifecycleSignal::Registered;
        let mut events = [
            swap_launch(AgentLaunchState::Bound, "launch_a", 0, Some("tmux:%1")),
            swap_hook(
                &workspace,
                "s1",
                0,
                registered.clone(),
                "SessionStart",
                "tmux:%1",
            ),
            swap_launch(AgentLaunchState::Starting, "launch_b", 1, None),
            swap_launch(AgentLaunchState::Bound, "launch_b", 1, Some("tmux:%1")),
            swap_hook(
                &workspace,
                "s1",
                0,
                lifecycle::LifecycleSignal::Ended,
                "SessionEnd",
                "tmux:%1",
            ),
            swap_hook(&workspace, "s2", 1, registered, "SessionStart", "tmux:%1"),
        ];
        for (index, event) in events.iter_mut().enumerate() {
            event.workspace_id = workspace.clone();
            event.timestamp = recent(100 - index as u64);
        }
        for event in &events[..boundary] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (cache, _, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths, &cache).unwrap();
        stage_carryover_for_rotation(&paths, 1).unwrap();
        assert!(rotate(&paths).rotation.is_rotated());
        for event in &events[boundary..] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (_, warm, _) = catch_up_rollup(&paths).unwrap();
        let (_, cursor, _) = RollupCursor::new().fold(&paths).unwrap();
        for agents in [warm.to_vec(), cursor.to_vec()] {
            let holder = swap_holder(&agents);
            assert_eq!(holder.len(), 1, "boundary {boundary}: {agents:#?}");
            assert_eq!(holder[0].agent_id, "s2");
            assert_eq!(holder[0].epoch, 1);
            assert_eq!(holder[0].name.as_deref(), Some("coder"));
            assert!(
                agents.iter().all(|agent| agent.agent_id != "launch_b"),
                "boundary {boundary}: the carried reservation was taken: {agents:#?}"
            );
        }
    }
}

/// A bound reservation carried over a rotation, taken by its session in one
/// catch-up, stays taken in every later one: through a direct-key event, a
/// rebirth, and its session's stamped end, a warm fold equals a cold one.
#[test]
fn a_carried_reservation_once_taken_never_returns_to_a_later_catch_up() {
    use crate::agents::state::{AgentEnd, agent_end};

    for (resumed, rebirth) in [(false, false), (true, false), (false, true)] {
        let case = format!("resumed {resumed}, rebirth {rebirth}");
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = checkpoint_paths(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let session = if resumed { "s1" } else { "s2" };
        let mut events = vec![
            swap_launch(AgentLaunchState::Bound, "launch_a", 0, Some("tmux:%1")),
            swap_hook(
                &workspace,
                "s1",
                0,
                lifecycle::LifecycleSignal::Registered,
                "SessionStart",
                "tmux:%1",
            ),
            swap_launch(AgentLaunchState::Starting, "launch_b", 1, None),
            swap_launch(AgentLaunchState::Bound, "launch_b", 1, Some("tmux:%2")),
        ];
        let boundary = events.len();
        if resumed {
            events.push(EventEnvelope::agent_attached(
                workspace.clone(),
                "session",
                &AgentKind::new_unchecked("codex"),
                AgentAttachPayload {
                    epoch: Some(1),
                    login: None,
                    record: None,
                    tier: None,
                    mode: None,
                    effective_isolation: None,
                    agent_id: "s1".into(),
                    isolation: None,
                    launch_id: Some("launch_a".into()),
                    pane_id: PaneId::parse("tmux:%2").unwrap(),
                    pane_pid: None,
                    runtime_owner: RuntimeOwner::new(
                        RuntimeOwnerKind::Agent,
                        "s1",
                        9,
                        Some("start-9".into()),
                    ),
                },
            ));
        }
        events.push(swap_hook(
            &workspace,
            session,
            1,
            lifecycle::LifecycleSignal::Registered,
            "SessionStart",
            "tmux:%2",
        ));
        // A direct-key event of the taken reservation, as a late launch
        // write of it would be.
        events.push(swap_launch(AgentLaunchState::Failed, "launch_b", 1, None));
        if rebirth {
            events.push(EventEnvelope::session_rebirth(workspace.clone(), "session"));
        }
        events.push(swap_hook(
            &workspace,
            session,
            1,
            lifecycle::LifecycleSignal::Ended,
            "SessionEnd",
            "tmux:%2",
        ));
        for (index, event) in events.iter_mut().enumerate() {
            event.workspace_id = workspace.clone();
            event.timestamp = recent(100 - index as u64);
        }
        for event in &events[..boundary] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (cache, _, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths, &cache).unwrap();
        stage_carryover_for_rotation(&paths, 1).unwrap();
        assert!(rotate(&paths).rotation.is_rotated());
        let mut cursor = RollupCursor::new();
        cursor.fold(&paths).unwrap();
        for event in &events[boundary..] {
            event_log::append(&paths.events_log, event).unwrap();
            let (warm_cache, warm, _) = catch_up_rollup(&paths).unwrap();
            let (_, cursor_rows, _) = cursor.fold(&paths).unwrap();
            // A fresh cursor with no `rollup.json` folds the log and the
            // carryover from scratch; the thread's held fold would serve
            // the warm rows again.
            fs::remove_file(paths.rollup_cache().unwrap()).unwrap();
            let cold = RollupCursor::new().fold(&paths).unwrap().1.to_vec();
            assert_eq!(
                sorted_value(warm.clone()),
                sorted_value(cold.clone()),
                "{case}"
            );
            assert_eq!(
                sorted_value(cursor_rows.to_vec()),
                sorted_value(cold),
                "{case}"
            );
            assert!(
                warm.iter().all(|agent| agent.agent_id != "launch_b"),
                "{case}: the taken reservation came back: {warm:#?}"
            );
            write_rollup_cache(&paths, &warm_cache).unwrap();
        }
        let (_, agents, _) = catch_up_rollup(&paths).unwrap();
        assert_eq!(
            agent_end(
                &agents,
                &AgentKind::new_unchecked("codex"),
                &"launch_a".into()
            ),
            AgentEnd::Ended,
            "{case}: {agents:#?}"
        );
    }
}
