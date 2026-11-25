use super::lifecycle::handle_lifecycle_hook;
use super::lifecycle::{fill_root_launch_identity, root_identity_rollup};
use super::proctree::matches_agent_kind;
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::AgentSessionId;
use rimz::ids::{MuxName, PaneId};
use rimz::pane::{PaneRef, RuntimeOwnerKind};
use rimz::store::runtime::process_owner;
use std::sync::atomic::{AtomicUsize, Ordering};

fn id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn pane(raw: &str, command: &str, cwd: &str, _focused: bool) -> PaneRef {
    PaneRef {
        pane_id: id(raw),
        session_name: "rimz-test".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: None,
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn candidate(raw: &str, focused: bool) -> PaneRef {
    pane(raw, "codex", "/repo/main", focused)
}

fn root_observation() -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::Registered,
    )
}

fn hooks_test_store() -> (tempfile::TempDir, rimz::Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let workspace_id =
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
    let paths = rimz::disk::paths::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    (dir, store)
}

fn apply_raised_frame(
    event: &str,
    replay: bool,
    state: rimz::agents::state::RaisedAskState,
) -> (
    rimz::store::raised::RaisedRecord,
    Vec<rimz::store::event::EventEnvelope>,
) {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    let dir = tempfile::tempdir().unwrap();
    let workspace = rimz::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
    let paths =
        rimz::StatePaths::under(workspace.workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    store.record_workspace(&workspace).unwrap();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let at = jiff::Timestamp::now();
    rimz::store::raised::raise(
        store.paths(),
        rimz::store::raised::Entry {
            kind: agent.spec().kind_id(),
            agent_id: "session".into(),
            value: rimz::agents::state::RaisedAsk {
                ask_id: rimz::ids::AskId::new(),
                state,
                raised_at: at - std::time::Duration::from_secs(1),
                basis_at: at - std::time::Duration::from_secs(1),
                opened_at: None,
                text: None,
                options: vec![],
                raised_by: None,
            },
        },
    )
    .unwrap();
    let mut payload = serde_json::json!({"session_id": "session", "prompt": "headerless notice",
        "last_assistant_message": "Ship?"});
    if event == "StopFailure" {
        payload["error"] = serde_json::json!("failed");
    }
    let mut decoded = agent.decode_hook(event, &payload).unwrap();
    let frame = rimz::store::ingress::HookIngress {
        boot_id: None,
        schema_version: "1".into(),
        event_id: rimz::EventId::new(),
        ts: at,
        source: agent.spec().kind_id(),
        event: Some(event.into()),
        payload: payload.to_string(),
        cwd: workspace.project_root.clone(),
        hook_pid: std::process::id(),
        env: std::collections::BTreeMap::from([
            ("RIMZ_AGENT_KIND".into(), "claude".into()),
            ("RIMZ_AGENT_ID".into(), "session".into()),
            ("RIMZ_AGENT_NAME".into(), "worker".into()),
        ]),
    };
    let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
    if replay {
        scoped
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: agent.spec().kind_id(),
                event_name: event,
                observation: decoded.lifecycle().unwrap(),
                spawned_subagents: &[],
            })
            .unwrap();
    }
    let recovered = store.read_events().unwrap();
    with_frame_env(
        &frame,
        if replay {
            FrameAttempt::Replay
        } else {
            FrameAttempt::First
        },
        || {
            super::lifecycle::handle_lifecycle_frame(
                &workspace,
                &scoped,
                agent,
                &mut decoded,
                &payload,
                rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                &crate::cli::GlobalFlags {
                    root: None,
                    mux: None,
                    zellij: false,
                    tmux: false,
                    color: crate::cli::ColorWhen::Never,
                },
                &recovered,
            )
        },
    )
    .unwrap();
    (
        rimz::store::raised::read(store.paths()),
        store.read_events().unwrap(),
    )
}

#[test]
fn raised_attention_replay_does_not_stamp_human_turns_but_still_opens_armed_asks() {
    use rimz::agents::state::RaisedAskState;
    let (record, _) = apply_raised_frame("UserPromptSubmit", true, RaisedAskState::Open);
    assert!(
        record.human_turns.is_empty(),
        "a replay cannot reclassify a prompt as human"
    );
    assert!(record.is_open(&record.asks[0]));
    let (record, _) = apply_raised_frame("Stop", true, RaisedAskState::Armed);
    assert_eq!(record.asks[0].value.state, RaisedAskState::Open);
}

#[test]
fn opencode_snapshot_frames_leave_edge_consumers_untouched_on_apply_and_replay() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageStatus};
    let mut actual = Vec::new();
    for event in ["client_idle", "client_running"] {
        for repeated in [false, true] {
            for replay in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let workspace =
                    rimz::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
                let paths = rimz::StatePaths::for_project_root(&workspace.project_root).unwrap();
                let runtime = rimz::disk::paths::RuntimePaths::under(
                    workspace.workspace_id.clone(),
                    dir.path(),
                )
                .unwrap();
                let store = rimz::Store::open(paths, runtime).unwrap();
                store.record_workspace(&workspace).unwrap();
                let agent = rimz::agents::definition_by_kind("opencode").unwrap();
                let mut state =
                    rimz::testkit::agent_state("opencode", "session", jiff::Timestamp::now());
                state.launch_id = Some("launch".into());
                state.launched_by = Some(rimz::agents::LaunchedBy {
                    kind: rimz::ids::AgentKind::new_unchecked("codex"),
                    agent_id: "launcher".into(),
                });
                let run = rimz::harness::run::create_peer_prompt(
                    store.paths(),
                    &state,
                    None,
                    agent,
                    "original task",
                    &workspace.worktree_root,
                    rimz::store::run::ReportTo::Launcher,
                )
                .unwrap()
                .unwrap();
                let run_before = serde_json::to_value(&run).unwrap();
                let mut message = MessageRecord::new(
                    workspace.workspace_id.clone(),
                    &state,
                    "pending sent prompt".into(),
                    DeliveryGate::Done,
                );
                message.status = MessageStatus::Sent;
                store
                    .queue_message(&message, &workspace.session_name)
                    .unwrap();
                // A one-shot signal delivery row, as an older `loop add --wait` stored it.
                let subscriptions = store.paths().root.join("records/loop-instances.json");
                let row = rimz::config::TaskEntry {
                    root: workspace.project_root.clone(),
                    signal: Some("agent.started".into()),
                    matches: Some(std::collections::BTreeMap::from([(
                        "session".into(),
                        "session".into(),
                    )])),
                    wait: Some(rimz::config::TaskTarget {
                        kind: rimz::ids::AgentKind::new_unchecked("opencode"),
                        session: "recipient".into(),
                        handle: "@recipient".into(),
                    }),
                    ..rimz::config::TaskEntry::default()
                };
                let rows = rimz::config::Tasks(std::collections::BTreeMap::from([(
                    "arrival".to_owned(),
                    row,
                )]));
                std::fs::create_dir_all(subscriptions.parent().unwrap()).unwrap();
                std::fs::write(&subscriptions, serde_json::to_vec(&rows).unwrap()).unwrap();
                let subscriptions_before = std::fs::read(&subscriptions).unwrap();
                let payload = serde_json::json!({"session_id": "session", "on_screen": true,
                    "session_name": "screen title", "model": "gpt-5.5"});
                let mut decoded = agent.decode_hook(event, &payload).unwrap();
                let Some(observation) = decoded.lifecycle() else {
                    actual.push((event, repeated, replay, None));
                    continue;
                };
                if repeated {
                    store
                        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
                            workspace.workspace_id.clone(),
                            &workspace.session_name,
                            "opencode",
                            event,
                            observation,
                        ))
                        .unwrap();
                }
                let frame = rimz::store::ingress::HookIngress {
                    boot_id: None,
                    schema_version: "1".into(),
                    event_id: rimz::EventId::new(),
                    ts: jiff::Timestamp::now(),
                    source: state.kind.clone(),
                    event: Some(event.into()),
                    payload: payload.to_string(),
                    cwd: workspace.project_root.clone(),
                    hook_pid: std::process::id(),
                    env: std::collections::BTreeMap::from([(
                        "RIMZ_RUN_ID".into(),
                        run.run_id.to_string(),
                    )]),
                };
                let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
                if replay {
                    scoped
                        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                            session_name: &workspace.session_name,
                            agent_kind: state.kind.clone(),
                            event_name: event,
                            observation: decoded.lifecycle().unwrap(),
                            spawned_subagents: &[],
                        })
                        .unwrap();
                }
                let recovered = store.read_events().unwrap();
                let non_lifecycle_before = recovered
                    .iter()
                    .filter(|row| row.method != "agent.lifecycle")
                    .map(|row| row.event_id.clone())
                    .collect::<Vec<_>>();
                with_frame_env(
                    &frame,
                    if replay {
                        FrameAttempt::Replay
                    } else {
                        FrameAttempt::First
                    },
                    || {
                        super::lifecycle::handle_lifecycle_frame(
                            &workspace,
                            &scoped,
                            agent,
                            &mut decoded,
                            &payload,
                            rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                            &crate::cli::GlobalFlags {
                                root: None,
                                mux: None,
                                zellij: false,
                                tmux: false,
                                color: crate::cli::ColorWhen::Never,
                            },
                            &recovered,
                        )
                    },
                )
                .unwrap();
                let events = store.read_events().unwrap();
                let run_after = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
                actual.push((
                    event,
                    repeated,
                    replay,
                    Some([
                        subscriptions_before == std::fs::read(&subscriptions).unwrap_or_default(),
                        run_before == serde_json::to_value(run_after).unwrap(),
                        store.list_messages().unwrap().iter().any(|row| {
                            row.message_id == message.message_id
                                && row.status == MessageStatus::Sent
                        }),
                        non_lifecycle_before
                            == events
                                .iter()
                                .filter(|row| row.method != "agent.lifecycle")
                                .map(|row| row.event_id.clone())
                                .collect::<Vec<_>>(),
                        !store.paths().transcript_dir.exists()
                            || std::fs::read_dir(&store.paths().transcript_dir)
                                .unwrap()
                                .next()
                                .is_none(),
                        rimz::store::active_time::read_for_keys(
                            store.runtime_paths(),
                            [("opencode", "session")],
                        )
                        .is_empty(),
                        std::fs::read_dir(&store.runtime_paths().agent_activity_dir)
                            .unwrap()
                            .next()
                            .is_none(),
                    ]),
                ));
            }
        }
    }
    let expected = ["client_idle", "client_running"]
        .into_iter()
        .flat_map(|event| {
            [false, true].into_iter().flat_map(move |repeated| {
                [false, true].map(move |replay| (event, repeated, replay, Some([true; 7])))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "snapshots must never reach lifecycle edge consumers"
    );
}

#[test]
fn cold_apply_frame_reads_one_carryover_and_resolves_git_once() {
    for published_base in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let mut workspace =
            rimz::workspace::WorkspaceResolver::resolve_under(&repo, None, dir.path()).unwrap();
        let paths = rimz::StatePaths::for_project_root_under(&repo, dir.path()).unwrap();
        let runtime =
            rimz::disk::paths::RuntimePaths::under(paths.workspace_id.clone(), dir.path()).unwrap();
        let store = rimz::Store::open(paths.clone(), runtime).unwrap();
        store.record_workspace(&workspace).unwrap();
        rimz::testkit::fleet::seed_ended_carryover(&store, 3_300).unwrap();
        let registration = rimz::testkit::fleet::registered_lifecycle(&paths.workspace_id, 0);
        if published_base {
            store.append_event(&registration).unwrap();
            // Changing snapshot identity forces a persisted base before the cold frame.
            workspace.session_name.push_str("-seeded");
            store.record_workspace(&workspace).unwrap();
        } else {
            rimz::store::event_log::append(&paths.events_log, &registration).unwrap();
        }
        let raw_len = std::fs::metadata(&paths.agents_carryover).unwrap().len();
        let mut env = rimz::workspace::pin_env(&paths.workspace_id, &repo);
        env.insert("HOME".into(), dir.path().display().to_string());
        env.insert("RIMZ_HOME".into(), dir.path().display().to_string());
        env.insert("RIMZ_AGENT_PID".into(), std::process::id().to_string());
        let frame = rimz::store::ingress::HookIngress {
            boot_id: None,
            schema_version: "1".into(),
            event_id: rimz::EventId::new(),
            ts: jiff::Timestamp::now(),
            source: rimz::ids::AgentKind::new_unchecked("claude"),
            event: Some("UserPromptSubmit".into()),
            payload: serde_json::json!({"session_id": "agent-0", "cwd": repo, "prompt": "work"})
                .to_string(),
            cwd: repo,
            hook_pid: std::process::id(),
            env,
        };
        std::thread::spawn(move || {
            let raw_before = rimz::testkit::carryover_bytes_parsed();
            let spawns_before = rimz::testkit::spawn_count();
            rimz::harness::hook_drain::with_frame_env(
                &frame,
                rimz::harness::hook_drain::FrameAttempt::First,
                || {
                    super::process_frame(
                        &store.for_hook_ingress(frame.event_id.clone(), frame.ts),
                        &frame,
                        &[],
                    )
                },
            )
            .unwrap();
            let projection = store
                .runtime_projection(rimz::store::runtime::RuntimeScope::Audit)
                .unwrap();
            let agent = projection
                .agents
                .iter()
                .find(|agent| agent.agent_id == "agent-0")
                .unwrap();
            assert_eq!(agent.status, rimz::agents::AgentStatus::Running);
            assert_eq!(agent.prompt.as_deref(), Some("work"));
            assert_eq!(rimz::testkit::spawn_count() - spawns_before, 1);
            assert_eq!(
                rimz::testkit::carryover_bytes_parsed() - raw_before,
                raw_len
            );
        })
        .join()
        .unwrap();
    }
}

#[test]
fn cold_plain_frames_read_a_constant_number_of_sidecars() {
    let mut budgets = Vec::new();
    for event in ["UserPromptSubmit", "Stop"] {
        let mut counts = Vec::new();
        for population in [8, 216] {
            let dir = tempfile::tempdir().unwrap();
            let repo = dir.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec![
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.test",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ],
            ] {
                assert!(
                    std::process::Command::new("git")
                        .current_dir(&repo)
                        .args(args)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let workspace =
                rimz::WorkspaceResolver::resolve_under(&repo, None, dir.path()).unwrap();
            let paths = rimz::StatePaths::for_project_root_under(&repo, dir.path()).unwrap();
            let runtime =
                rimz::RuntimePaths::under(paths.workspace_id.clone(), dir.path()).unwrap();
            let store = rimz::Store::open(paths.clone(), runtime).unwrap();
            store.record_workspace(&workspace).unwrap();
            for slot in 0..population {
                rimz::store::event_log::append(
                    &paths.events_log,
                    &rimz::testkit::fleet::registered_lifecycle(&paths.workspace_id, slot),
                )
                .unwrap();
                rimz::store::agent_context::write(
                    store.runtime_paths(),
                    "claude",
                    &format!("agent-{slot}"),
                    &rimz::agents::AgentContext::new("claude", jiff::Timestamp::now()),
                    None,
                )
                .unwrap();
            }
            let mut env = rimz::workspace::pin_env(&paths.workspace_id, &repo);
            env.insert("HOME".into(), dir.path().display().to_string());
            env.insert("RIMZ_HOME".into(), dir.path().display().to_string());
            env.insert("RIMZ_AGENT_PID".into(), std::process::id().to_string());
            let frame = rimz::store::ingress::HookIngress {
                boot_id: None,
                schema_version: "1".into(),
                event_id: rimz::EventId::new(),
                ts: jiff::Timestamp::now(),
                source: rimz::ids::AgentKind::new_unchecked("claude"),
                event: Some(event.into()),
                payload: serde_json::json!({
                    "session_id": "agent-0", "cwd": repo, "prompt": "work",
                    "last_assistant_message": "done"
                })
                .to_string(),
                cwd: repo,
                hook_pid: std::process::id(),
                env,
            };
            counts.push(
                std::thread::spawn(move || {
                    let before = rimz::testkit::sidecar_read_counts();
                    rimz::harness::hook_drain::with_frame_env(
                        &frame,
                        rimz::harness::hook_drain::FrameAttempt::First,
                        || {
                            super::process_frame(
                                &store.for_hook_ingress(frame.event_id.clone(), frame.ts),
                                &frame,
                                &[],
                            )
                        },
                    )
                    .unwrap();
                    let after = rimz::testkit::sidecar_read_counts();
                    let projection = store
                        .runtime_projection(rimz::store::RuntimeScope::Audit)
                        .unwrap();
                    let agent = projection
                        .agents
                        .iter()
                        .find(|agent| agent.agent_id == "agent-0")
                        .unwrap();
                    assert_eq!(
                        agent.status,
                        if event == "UserPromptSubmit" {
                            rimz::agents::AgentStatus::Running
                        } else {
                            rimz::agents::AgentStatus::Success
                        }
                    );
                    let entries = rimz::transcript::read_all(store.paths()).unwrap();
                    assert!(entries.iter().any(|entry| entry.ingress.as_ref()
                        == Some(&frame.event_id)
                        && entry.text
                            == if event == "UserPromptSubmit" {
                                "work"
                            } else {
                                "done"
                            }));
                    (after.0 - before.0, after.1 - before.1, after.2 - before.2)
                })
                .join()
                .unwrap(),
            );
        }
        budgets.push(counts);
    }
    assert_eq!(
        budgets.iter().map(|counts| counts[0]).collect::<Vec<_>>(),
        budgets.iter().map(|counts| counts[1]).collect::<Vec<_>>(),
        "prompt and Stop sidecar work must not grow with the fleet"
    );
    // Parses include the direct reads that locked sidecar updates make; no frame
    // enumerates the context directory.
    for counts in budgets {
        assert_eq!((counts[0].1, counts[0].2), (0, 3), "{counts:?}");
    }
}

/// A team member's tool use can raise no team signal, so its frame reads no
/// room-wide sidecars for the team address view.
#[test]
fn cold_team_tool_frames_read_a_constant_number_of_sidecars() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::harness::launch::ENV_TEAM;

    let mut counts = Vec::new();
    for population in [8, 216] {
        let (_dir, store) = hooks_test_store();
        let store = std::sync::Arc::new(store);
        for slot in 0..population {
            rimz::store::event_log::append(
                &store.paths().events_log,
                &rimz::testkit::fleet::registered_lifecycle(&store.paths().workspace_id, slot),
            )
            .unwrap();
            rimz::store::agent_context::write(
                store.runtime_paths(),
                "claude",
                &format!("agent-{slot}"),
                &rimz::agents::AgentContext::new("claude", jiff::Timestamp::now()),
                None,
            )
            .unwrap();
        }
        let adapter = rimz::agents::definition_by_kind("claude").unwrap();
        let env = std::collections::BTreeMap::from([(ENV_TEAM.to_owned(), "forge".to_owned())]);
        let apply = |event: &'static str, payload: serde_json::Value| {
            let store = std::sync::Arc::clone(&store);
            let env = env.clone();
            std::thread::spawn(move || {
                let frame = rimz::store::ingress::HookIngress {
                    boot_id: None,
                    schema_version: "1".into(),
                    event_id: rimz::EventId::new(),
                    ts: jiff::Timestamp::now(),
                    source: adapter.spec().kind_id(),
                    event: Some(event.into()),
                    payload: payload.to_string(),
                    cwd: "/tmp/hooks-test".into(),
                    hook_pid: std::process::id(),
                    env,
                };
                let before = rimz::testkit::sidecar_read_counts();
                with_frame_env(&frame, FrameAttempt::First, || {
                    let mut decoded = adapter.decode_hook(event, &payload).unwrap();
                    handle_lifecycle_hook(
                        &hooks_test_workspace(Some("main")),
                        &store,
                        adapter,
                        &mut decoded,
                        &payload,
                        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                        &hooks_test_globals(),
                    )
                    .unwrap()
                });
                let after = rimz::testkit::sidecar_read_counts();
                (after.0 - before.0, after.1 - before.1, after.2 - before.2)
            })
            .join()
            .unwrap()
        };
        apply("SessionStart", serde_json::json!({"session_id": "leader"}));
        apply(
            "UserPromptSubmit",
            serde_json::json!({"session_id": "leader", "prompt": "work"}),
        );
        let events = std::fs::metadata(&store.paths().events_log).unwrap().len();
        let tool = apply(
            "PostToolUse",
            serde_json::json!({
                "session_id": "leader", "tool_name": "Bash",
                "tool_input": {"command": "true"}, "tool_response": {}
            }),
        );
        assert!(
            std::fs::metadata(&store.paths().events_log).unwrap().len() > events,
            "the tool use appended an event"
        );
        counts.push(tool);
    }
    assert_eq!(
        counts[0], counts[1],
        "a team member's tool frame must not read sidecars per room row: {counts:?}"
    );
}

#[test]
fn cold_team_frames_read_a_constant_number_of_run_records() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::harness::launch::ENV_TEAM;
    use rimz::store::run::{RunRecord, RunStatus, TeamRun};

    let mut reads = Vec::new();
    for terminal in [4, 40] {
        let (_dir, store) = hooks_test_store();
        let store = std::sync::Arc::new(store);
        let adapter = rimz::agents::definition_by_kind("claude").unwrap();
        let instance = rimz::ids::TeamInstanceId::new("forge", None).unwrap();
        let team_run = |launch: &str, status: RunStatus| {
            let mut run = RunRecord::new(
                store.paths().workspace_id.clone(),
                adapter.spec().kind_id(),
                rimz::agents::PermissionMode::Auto,
                "task".into(),
                "/tmp/hooks-test".into(),
            );
            run.status = status;
            run.team = Some(TeamRun {
                launch_id: launch.into(),
                instance: instance.clone(),
            });
            rimz::harness::run::create(store.paths(), &run).unwrap();
            run
        };
        for _ in 0..terminal {
            team_run("leader", RunStatus::Completed);
        }
        let open = team_run("leader", RunStatus::Running);
        let env = std::collections::BTreeMap::from([(ENV_TEAM.to_owned(), "forge".to_owned())]);
        let apply = |event: &'static str, payload: serde_json::Value| {
            let store = std::sync::Arc::clone(&store);
            let env = env.clone();
            std::thread::spawn(move || {
                let frame = rimz::store::ingress::HookIngress {
                    boot_id: None,
                    schema_version: "1".into(),
                    event_id: rimz::EventId::new(),
                    ts: jiff::Timestamp::now(),
                    source: adapter.spec().kind_id(),
                    event: Some(event.into()),
                    payload: payload.to_string(),
                    cwd: "/tmp/hooks-test".into(),
                    hook_pid: std::process::id(),
                    env,
                };
                let before = rimz::testkit::run_record_reads();
                with_frame_env(&frame, FrameAttempt::First, || {
                    let mut decoded = adapter.decode_hook(event, &payload).unwrap();
                    handle_lifecycle_hook(
                        &hooks_test_workspace(Some("main")),
                        &store,
                        adapter,
                        &mut decoded,
                        &payload,
                        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                        &hooks_test_globals(),
                    )
                    .unwrap()
                });
                rimz::testkit::run_record_reads() - before
            })
            .join()
            .unwrap()
        };
        apply("SessionStart", serde_json::json!({"session_id": "leader"}));
        let stop = apply(
            "Stop",
            serde_json::json!({"session_id": "leader", "last_assistant_message": "done"}),
        );
        let answered = rimz::harness::run::load(store.paths(), &open.run_id).unwrap();
        assert_eq!(
            answered.last_message.as_deref(),
            Some("done"),
            "the frame found the open team run"
        );
        reads.push(stop);
    }
    assert_eq!(
        reads[0], reads[1],
        "a cold team frame's run-record reads must not grow with terminal runs: {reads:?}"
    );
}

#[test]
fn delayed_progress_hook_preserves_credit_limit_park_instead_of_stall() {
    use rimz::agents::{
        AgentContext, AgentState, AgentStatus, AgentTurnError, PermissionMode, TurnErrorClass,
    };
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::store::run::{RunRecord, RunStatus};
    use rimz::store::snapshot::SidebarSnapshot;
    use std::time::Duration;

    let (dir, store) = hooks_test_store();
    let now = jiff::Timestamp::now();
    let ingress_at = now - Duration::from_secs(10);
    let marker_at = now - Duration::from_secs(5);
    let label = "Your workspace is out of credits. Ask your workspace owner to refill in order to continue.";
    let parent = AgentState::stub("claude", "parent", AgentStatus::Sleeping);
    let mut child = AgentState::stub("codex", "child", AgentStatus::Running);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.last_activity = ingress_at - Duration::from_secs(1);
    let mut context = AgentContext::new("codex", marker_at);
    context.turn_error = Some(AgentTurnError {
        model: None,
        ended_turn: false,
        class: TurnErrorClass::PausedRateLimit,
        at: marker_at,
        label: Some(label.to_owned()),
    });
    child.context = Some(context);
    let mut run = RunRecord::new(
        store.paths().workspace_id.clone(),
        child.kind.clone(),
        PermissionMode::Auto,
        "task".into(),
        "/tmp/hooks-test".into(),
    );
    run.subagent = true;
    run.status = RunStatus::Running;
    run.agent_id = Some(child.agent_id.clone());
    let payload = serde_json::json!({"session_id": "child", "tool_name": "Bash", "tool_input": {"command": "true"}});
    let frame = rimz::store::ingress::HookIngress {
        boot_id: None,
        schema_version: "1".into(),
        event_id: rimz::EventId::new(),
        ts: ingress_at,
        source: child.kind.clone(),
        event: Some("PostToolUse".into()),
        payload: payload.to_string(),
        cwd: "/tmp/hooks-test".into(),
        hook_pid: std::process::id(),
        env: std::collections::BTreeMap::from([(
            "HOME".into(),
            dir.path().to_string_lossy().into_owned(),
        )]),
    };
    let adapter = rimz::agents::definition_by_kind("codex").unwrap();
    with_frame_env(&frame, FrameAttempt::First, || {
        let mut decoded = adapter.decode_hook("PostToolUse", &payload).unwrap();
        handle_lifecycle_hook(
            &hooks_test_workspace(None),
            &store.for_hook_ingress(frame.event_id.clone(), frame.ts),
            adapter,
            &mut decoded,
            &payload,
            rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
            &hooks_test_globals(),
        )
        .unwrap();
    });
    let activity = rimz::agent_activity::read_for_keys(store.runtime_paths(), [("codex", "child")]);
    let snapshot = SidebarSnapshot::build_with_agents(
        store.paths().workspace_id.clone(),
        vec![parent, child],
        now,
    )
    .with_agent_activity(&activity);
    let park = rimz::harness::park_notice::unnoticed_park(&run, &snapshot.agents, snapshot.now)
        .expect("delayed hook apply must not hide the child's credit-limit park");
    let (class, error) = park.child.displayed_turn_error(snapshot.now).unwrap();
    assert_eq!(class, TurnErrorClass::PausedRateLimit);
    assert_eq!(error.label.as_deref(), Some(label));
    assert_eq!(park.child.last_activity, ingress_at);
    assert_eq!(activity[0].tool_at, Some(ingress_at));
    assert!(
        rimz::harness::stall_notice::unnoticed_stall(
            &run,
            &snapshot.agents,
            now + Duration::from_secs(1_800),
            1_800,
            |_| rimz::harness::park_notice::Continuation::Off,
        )
        .is_none(),
        "the limit marker suppresses silent_child_for even past the stall window"
    );
}

#[test]
fn lifecycle_records_pane_roles_for_roots_and_native_children() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::harness::launch::{ENV_AGENT_PROFILE, ENV_AGENT_ROLE, ENV_RUN_ID, ENV_TEAM};
    use rimz::store::run::RunRecord;
    for (subagent, profile, native_child, team, role) in [
        (Some(true), Some("fast"), false, None, None),
        (Some(true), None, false, None, None),
        (Some(false), Some("peer"), false, None, None),
        (None, None, false, None, None),
        (Some(true), Some("fast"), true, None, None),
        (
            None,
            Some("plain-profile"),
            false,
            Some("forge"),
            Some("coder"),
        ),
        (
            None,
            Some("plain-profile"),
            true,
            Some("forge"),
            Some("coder"),
        ),
        (None, Some("solo.dotted"), false, None, None),
        (None, Some("peer"), false, Some("forge"), None),
        (None, None, false, Some("forge"), None),
        (None, Some("peer"), false, None, Some("inline")),
        (
            Some(true),
            Some("fast"),
            false,
            Some("forge"),
            Some("coder"),
        ),
    ] {
        let (_dir, store) = hooks_test_store();
        let adapter = rimz::agents::definition_by_kind("claude").unwrap();
        let mut env = std::collections::BTreeMap::new();
        if let Some(subagent) = subagent {
            let mut run = RunRecord::new(
                store.paths().workspace_id.clone(),
                adapter.spec().kind_id(),
                rimz::agents::PermissionMode::Auto,
                "task".into(),
                "/tmp/hooks-test".into(),
            );
            run.subagent = subagent;
            env.insert(ENV_RUN_ID.into(), run.run_id.to_string());
            rimz::harness::run::create(store.paths(), &run).unwrap();
        }
        if let Some(profile) = profile {
            env.insert(ENV_AGENT_PROFILE.into(), profile.into());
        }
        if let Some(team) = team {
            env.insert(ENV_TEAM.into(), team.into());
        }
        if let Some(role) = role {
            env.insert(ENV_AGENT_ROLE.into(), role.into());
        }
        let payload = if native_child {
            serde_json::json!({"session_id":"parent", "agent_id":"child", "agent_type":"review"})
        } else {
            serde_json::json!({"session_id":"parent"})
        };
        let event = if native_child {
            "SubagentStart"
        } else {
            "SessionStart"
        };
        let frame_for =
            |event: &str, payload: &serde_json::Value| rimz::store::ingress::HookIngress {
                boot_id: None,
                schema_version: "1".into(),
                event_id: rimz::EventId::new(),
                ts: jiff::Timestamp::now(),
                source: adapter.spec().kind_id(),
                event: Some(event.into()),
                payload: payload.to_string(),
                cwd: "/tmp/hooks-test".into(),
                hook_pid: std::process::id(),
                env: env.clone(),
            };
        let frame = frame_for(event, &payload);
        let feed_event = |event: &str, payload: &serde_json::Value| {
            with_frame_env(&frame_for(event, payload), FrameAttempt::First, || {
                let mut decoded = adapter.decode_hook(event, payload).unwrap();
                handle_lifecycle_hook(
                    &hooks_test_workspace(Some("main")),
                    &store,
                    adapter,
                    &mut decoded,
                    payload,
                    rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                    &hooks_test_globals(),
                )
                .unwrap()
            })
        };
        let feed = || feed_event(event, &payload);
        let reply = feed();
        let path = store
            .runtime_paths()
            .spending_history
            .roles(frame.ts.to_zoned_datetime(jiff::tz::TimeZone::UTC).year());
        assert!(path.exists(), "hook must persist session provenance");
        let before = std::fs::read(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(
            saved["session"],
            if native_child { "child" } else { "parent" }
        );
        assert_eq!(
            saved["role"],
            if subagent == Some(true) {
                "subagent"
            } else {
                "agent"
            }
        );
        assert_eq!(
            saved["profile"].as_str(),
            profile.or((subagent == Some(true)).then_some("claude"))
        );
        if subagent == Some(true) {
            assert!(saved.get("seat").is_none());
        } else {
            assert_eq!(
                saved["seat"],
                match team {
                    Some(team) => serde_json::json!({
                        "team": team,
                        "role": role.or(profile).unwrap_or("claude"),
                    }),
                    None => serde_json::json!("solo"),
                }
            );
        }
        feed();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        if !native_child {
            std::fs::remove_file(&path).unwrap();
            feed_event(
                "UserPromptSubmit",
                &serde_json::json!({"session_id":"parent", "prompt":"go"}),
            );
            assert!(
                !path.exists(),
                "a frame for a session already in the room must not reach the index"
            );
            feed();
            let again: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                (&again["session"], &again["role"]),
                (&saved["session"], &saved["role"]),
                "a registering frame indexes a session the room already knows"
            );
        }
        let directory = store.runtime_paths().spending_history.roles_dir();
        std::fs::remove_dir_all(&directory).unwrap();
        std::fs::write(&directory, b"not a directory").unwrap();
        assert_eq!(
            feed(),
            reply,
            "provenance write failure must not fail or alter the hook reply"
        );
    }
}

#[test]
fn lifecycle_records_subagent_owners_for_the_pane_and_native_children() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::harness::launch::{ENV_AGENT_ID, ENV_RUN_ID};
    use serde_json::json;

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (subagent, parent_exists, team) in [
        (true, true, Some("forge")),
        (true, true, None),
        (true, false, None),
        (false, true, Some("forge")),
    ] {
        let (_dir, store) = hooks_test_store();
        let adapter = rimz::agents::definition_by_kind("claude").unwrap();
        let mut run = rimz::store::run::RunRecord::new(
            store.paths().workspace_id.clone(),
            adapter.spec().kind_id(),
            rimz::agents::PermissionMode::Auto,
            "work".into(),
            "/tmp/hooks-test".into(),
        );
        run.subagent = subagent;
        rimz::harness::run::create(store.paths(), &run).unwrap();
        for (session, kind) in [("owner-launch", "codex"), ("pane-launch", "claude")] {
            if session == "owner-launch" && !parent_exists {
                continue;
            }
            let mut observation =
                AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered);
            if session == "owner-launch" {
                observation.launch.profile = Some("planner".into());
                observation.launch.team = team.map(Into::into);
                observation.launch.role = Some("coder".into());
            } else {
                observation.launch.parent_agent_id = Some("owner-launch".into());
                observation.launch.parent_agent_kind =
                    Some(rimz::ids::AgentKind::new_unchecked("codex"));
            }
            store
                .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                    session_name: "hooks-test",
                    agent_kind: rimz::ids::AgentKind::new_unchecked(kind),
                    event_name: "seed",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        for (event, payload) in [
            ("SessionStart", json!({"session_id":"pane-launch"})),
            (
                "SubagentStart",
                json!({"session_id":"pane-launch", "agent_id":"native-child"}),
            ),
        ] {
            let frame = rimz::store::ingress::HookIngress {
                boot_id: None,
                schema_version: "1".into(),
                event_id: rimz::EventId::new(),
                ts: jiff::Timestamp::now(),
                source: adapter.spec().kind_id(),
                event: Some(event.into()),
                payload: payload.to_string(),
                cwd: "/tmp/hooks-test".into(),
                hook_pid: std::process::id(),
                env: std::collections::BTreeMap::from([
                    (ENV_RUN_ID.into(), run.run_id.to_string()),
                    (ENV_AGENT_ID.into(), "pane-launch".into()),
                ]),
            };
            with_frame_env(&frame, FrameAttempt::First, || {
                let mut decoded = adapter.decode_hook(event, &payload).unwrap();
                handle_lifecycle_hook(
                    &hooks_test_workspace(Some("main")),
                    &store,
                    adapter,
                    &mut decoded,
                    &payload,
                    rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                    &hooks_test_globals(),
                )
                .unwrap();
            });
        }
        let path = store.runtime_paths().spending_history.roles(
            jiff::Timestamp::now()
                .to_zoned_datetime(jiff::tz::TimeZone::UTC)
                .year(),
        );
        let records = std::fs::read_to_string(path).unwrap();
        for line in records.lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            actual.push((row["session"].clone(), row["owner"].clone()));
            expected.push((
                row["session"].clone(),
                if !subagent || !parent_exists {
                    json!(null)
                } else if let Some(team) = team {
                    json!({"team":team, "role":"coder"})
                } else {
                    json!({"profile":"planner"})
                },
            ));
        }
        assert_eq!(records.lines().count(), 2);
    }
    assert_eq!(actual, expected);
}

#[test]
fn sandbox_path_mapping_preserves_rebound_roots_and_maps_native_path_lists() {
    let (_dir, store) = hooks_test_store();
    let home = store.paths().root.join("home");
    let unit = store.paths().temp_unit_dir(Some("scout"));
    let mut frame = rimz::store::ingress::HookIngress {
        boot_id: None,
        schema_version: "1".into(),
        event_id: rimz::EventId::new(),
        ts: jiff::Timestamp::now(),
        source: rimz::ids::AgentKind::new_unchecked("claude"),
        event: Some("SessionStart".into()),
        payload: serde_json::json!({"cwd": "/tmp/work", "transcript_path": "/var/tmp/tail.jsonl", "prompt": "/tmp/user text"}).to_string(),
        cwd: home.clone(),
        hook_pid: std::process::id(),
        env: std::collections::BTreeMap::from([
            ("RIMZ_AGENT_NAME".into(), "scout".into()),
            ("HOME".into(), home.display().to_string()),
            ("TMPDIR".into(), "/tmp".into()),
            ("ZELLIJ_SOCKET_DIR".into(), "/tmp/hook-zellij".into()),
            ("TMUX".into(), "/tmp/hook-tmux/default,123,0".into()),
            ("PATH".into(), "/tmp/bin:/var/tmp/tools:/usr/bin".into()),
            ("PI_AGENT_DIR".into(), "/tmp/one,/var/tmp/two".into()),
            ("CLAUDE_CONFIG_DIR".into(), "/tmp/account,/var/tmp/other".into()),
        ]),
    };
    let mounts = rimz::sandbox::MountPlan {
        mounts: vec![
            rimz::sandbox::Mount::Bind {
                source: home.clone(),
                target: home.clone(),
            },
            rimz::sandbox::Mount::Bind {
                source: "/tmp/hook-zellij".into(),
                target: "/tmp/hook-zellij".into(),
            },
        ],
    };
    frame.env.insert(
        rimz::config::Isolation::SCOPED_HOST_PATHS_ENV.into(),
        serde_json::to_string(&mounts.host_bound_paths()).unwrap(),
    );
    for isolation in ["sandbox", "scoped"] {
        let mut frame = frame.clone();
        frame.env.insert("RIMZ_ISOLATION".into(), isolation.into());
        super::map_sandbox_paths(&store, &mut frame).unwrap();
        assert_eq!(frame.cwd, home);
        assert_eq!(frame.env["HOME"], home.display().to_string());
        assert_eq!(frame.env["TMPDIR"], unit.display().to_string());
        assert_eq!(frame.env["ZELLIJ_SOCKET_DIR"], "/tmp/hook-zellij");
        assert_eq!(frame.env["TMUX"], "/tmp/hook-tmux/default,123,0");
        assert_eq!(
            frame.env["PATH"],
            format!("{}/bin:{}/tools:/usr/bin", unit.display(), unit.display())
        );
        assert_eq!(
            frame.env["PI_AGENT_DIR"],
            format!("{}/one,{}/two", unit.display(), unit.display())
        );
        assert_eq!(
            frame.env["CLAUDE_CONFIG_DIR"],
            format!("{}/account,{}/other", unit.display(), unit.display())
        );
        let payload: serde_json::Value = serde_json::from_str(&frame.payload).unwrap();
        assert_eq!(payload["cwd"], unit.join("work").display().to_string());
        assert_eq!(
            payload["transcript_path"],
            unit.join("tail.jsonl").display().to_string()
        );
        assert_eq!(payload["prompt"], "/tmp/user text");
    }
}

#[test]
fn runtime_env_context_reaches_the_first_root_prompt_of_each_session() {
    use rimz::agents::HookReply;
    use rimz::harness::launch::ENV_RUNTIME_ENV;

    if std::env::var_os("RIMZ_TEST_RUNTIME_ENV_FEED").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(
                    module_path!(),
                    "::runtime_env_context_reaches_the_first_root_prompt_of_each_session"
                )
                .split_once("::")
                .unwrap()
                .1,
                "--nocapture",
            ])
            .env("RIMZ_TEST_RUNTIME_ENV_FEED", "1")
            .env(ENV_RUNTIME_ENV, "1")
            .env_remove(rimz::harness::launch::ENV_RUN_ID)
            .env_remove(rimz::harness::launch::ENV_AGENT_ID)
            .env_remove(rimz::harness::launch::ENV_AGENT_NAME)
            .env_remove(rimz::harness::launch::ENV_TEAM)
            .env("RIMZ_BIN", "/nonexistent/rimz")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_dir, store) = hooks_test_store();
    let repo = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "initial"],
    ] {
        let status = std::process::Command::new("git")
            .current_dir(repo.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let adapter = rimz::agents::definition_by_kind("claude").unwrap();
    let submit =
        |session: &str, prompt: &str, native_child: bool, owner: rimz::pane::RuntimeOwnerKind| {
            let mut payload =
                serde_json::json!({"session_id":session, "cwd":repo.path(), "prompt":prompt});
            if native_child {
                payload["agent_id"] = serde_json::json!("native-child");
            }
            let mut decoded = adapter.decode_hook("UserPromptSubmit", &payload).unwrap();
            handle_lifecycle_hook(
                &rimz::ResolvedWorkspace {
                    worktree_root: repo.path().into(),
                    ..hooks_test_workspace(Some("main"))
                },
                &store,
                adapter,
                &mut decoded,
                &payload,
                rimz::agents::HookIngressOwner {
                    pid: Some(std::process::id()),
                    kind: owner,
                },
                &hooks_test_globals(),
            )
            .unwrap();
            match decoded.reply() {
                HookReply::Silent => None,
                HookReply::Json(reply) => {
                    assert_eq!(
                        reply["hookSpecificOutput"]["hookEventName"],
                        "UserPromptSubmit"
                    );
                    reply["hookSpecificOutput"]["additionalContext"]
                        .as_str()
                        .map(ToOwned::to_owned)
                }
            }
        };
    use rimz::pane::RuntimeOwnerKind::{Agent, Daemon};
    let feed = |session, native_child, owner| submit(session, "work", native_child, owner);
    assert_eq!(feed("daemon-owned", false, Daemon), None);
    assert_eq!(feed("root", true, Agent), None, "a native child's prompt");
    let first = feed("root", false, Agent).expect("the first root prompt");
    assert!(
        first.starts_with(
            "<system_reminder>\n### Environment\n\nSampled as this prompt was submitted.\n\n```\n$ git status --short --branch\n## main\n$ git log -5 --oneline\n"
        ),
        "{first}"
    );
    assert_eq!(feed("root", false, Agent), None, "the same session again");
    assert!(feed("resumed", false, Agent).is_some(), "a new session id");

    use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
    let root = rimz::testkit::agent_state("claude", "root", jiff::Timestamp::now());
    let notice = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &root,
        "Implement is yours.".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Stage,
    });
    store.queue_message(&notice, "hooks-test").unwrap();
    store
        .record_sent_batch(std::slice::from_ref(&notice), "hooks-test")
        .unwrap();
    let staged = submit(
        "root",
        "Type: STAGE\nFrom: @rimz\nContent:\nImplement is yours.",
        false,
        Agent,
    );
    assert!(
        store.list_messages().unwrap().is_empty(),
        "the prompt confirmed the stage notice as delivered"
    );
    assert_eq!(
        staged.as_deref(),
        Some(first.as_str()),
        "a delivered stage notice"
    );
    assert_eq!(feed("root", false, Agent), None, "the next plain prompt");
}

#[test]
fn deadline_context_reaches_only_the_root_post_tool_consumer_once() {
    use rimz::agents::HookReply;
    use rimz::harness::launch::ENV_RUN_ID;
    use rimz::store::run::{RunRecord, RunStatus};
    use std::time::Duration;

    if std::env::var_os("RIMZ_TEST_DEADLINE_FEED").is_none() {
        let run_id = rimz::RunId::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(
                    module_path!(),
                    "::deadline_context_reaches_only_the_root_post_tool_consumer_once"
                )
                .split_once("::")
                .unwrap()
                .1,
                "--nocapture",
            ])
            .env("RIMZ_TEST_DEADLINE_FEED", "1")
            .env(ENV_RUN_ID, run_id.as_str())
            .env_remove(rimz::harness::launch::ENV_AGENT_ID)
            .env_remove(rimz::harness::launch::ENV_AGENT_NAME)
            .env("RIMZ_BIN", "/nonexistent/rimz")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_dir, store) = hooks_test_store();
    let adapter = rimz::agents::definition_by_kind("claude").unwrap();
    let mut record = RunRecord::new(
        store.paths().workspace_id.clone(),
        adapter.spec().kind_id(),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        "/tmp/hooks-test".into(),
    );
    record.run_id = rimz::RunId::parse(&std::env::var(ENV_RUN_ID).unwrap()).unwrap();
    record.agent_id = Some("deadline-child".into());
    record.subagent = true;
    record.status = RunStatus::Running;
    record.timeout = Some(Duration::from_secs(1800));
    record.warn = vec![Duration::from_secs(360), Duration::from_secs(180)];
    record.grace = Some(Duration::from_secs(180));
    record.deadline_at = Some(jiff::Timestamp::now() + Duration::from_secs(299));
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let feed = |event, native_child: bool| {
        let mut payload = serde_json::json!({"session_id":"deadline-child", "cwd":"/tmp/hooks-test", "tool_name":"Read", "tool_input":{}, "tool_response":{}});
        if native_child {
            payload["agent_id"] = serde_json::json!("native-child");
        }
        let mut decoded = adapter.decode_hook(event, &payload).unwrap();
        assert_eq!(
            decoded.lifecycle().unwrap().parent_agent_id.is_some(),
            native_child
        );
        handle_lifecycle_hook(
            &hooks_test_workspace(Some("main")),
            &store,
            adapter,
            &mut decoded,
            &payload,
            rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
            &hooks_test_globals(),
        )
        .unwrap();
        super::attach_deadline_context(&store, adapter, event, &payload)
    };
    assert_eq!(feed("PreToolUse", false), HookReply::Silent);
    assert_eq!(feed("PostToolUse", true), HookReply::Silent);
    let root_context =
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "deadline-child");
    assert_eq!(feed("SubagentStop", true), HookReply::Silent);
    assert_eq!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "deadline-child"),
        root_context
    );
    let mut activity = rimz::agent_activity::read_for_keys(
        store.runtime_paths(),
        [("claude", "native-child"), ("claude", "deadline-child")],
    );
    activity.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    assert_eq!(activity.len(), 2);
    assert_eq!(activity[0].agent_id.as_str(), "deadline-child");
    assert_eq!(activity[1].agent_id.as_str(), "native-child");
    assert_eq!(activity[1].tool_at, None);
    assert!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "native-child")
            .is_none()
    );
    assert_eq!(
        rimz::harness::run::load(store.paths(), &record.run_id)
            .unwrap()
            .deadline_notice_at,
        None
    );
    let reply = |text| {
        HookReply::Json(
            serde_json::json!({"hookSpecificOutput":{"hookEventName":"PostToolUse", "additionalContext":text}}),
        )
    };
    assert_eq!(
        feed("PostToolUse", false),
        reply(
            "4m of 30m left. Don't start new investigation; if the task can be finished in a few more steps, finish it, otherwise prepare to report."
        )
    );
    assert_eq!(feed("PostToolUse", false), HookReply::Silent);
    record = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(
        record.deadline_notice_at,
        record.deadline_at.map(|at| at - Duration::from_secs(360))
    );
    record.deadline_at = Some(jiff::Timestamp::now() + Duration::from_secs(150));
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert_eq!(
        feed("PostToolUse", false),
        reply(
            "2m left. Wrap up now: finish only what is in flight, then report what is done, what is unverified, and what remains."
        )
    );
    record = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    record.deadline_at = Some(jiff::Timestamp::now() - Duration::from_secs(1));
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert_eq!(
        feed("PostToolUse", false),
        reply(
            "Time is up. Stop now and report what is done, what is unverified, and what remains. End your turn."
        )
    );
    assert_eq!(feed("PostToolUse", false), HookReply::Silent);
    assert_eq!(
        rimz::harness::run::load(store.paths(), &record.run_id)
            .unwrap()
            .deadline_notice_at,
        record.deadline_at
    );
}

fn hooks_test_workspace(worktree_branch: Option<&str>) -> rimz::ResolvedWorkspace {
    rimz::ResolvedWorkspace {
        workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new(
            "/tmp/hooks-test",
        )),
        project_root: std::path::PathBuf::from("/tmp/hooks-test"),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
        worktree_branch: worktree_branch.map(ToOwned::to_owned),
        session_name: "hooks-test".to_owned(),
        mux_hint: None,
    }
}

fn hooks_test_globals() -> crate::cli::GlobalFlags {
    crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    }
}

#[derive(Default)]
struct CorrelationTestAdapter {
    correlation_calls: AtomicUsize,
}

fn hook_definition<T>(provider: &'static T) -> rimz::agents::AgentDefinition
where
    T: rimz::agents::capabilities::AgentIntegration,
{
    rimz::agents::AgentDefinition::new(provider)
}

impl rimz::agents::capabilities::CoreCapability for CorrelationTestAdapter {
    fn spec(&self) -> &'static rimz::agents::AgentSpec {
        rimz::agents::definition_by_kind("antigravity")
            .unwrap()
            .spec()
    }
}

impl rimz::agents::capabilities::HookCapability for CorrelationTestAdapter {
    fn decode_hook(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> rimz::agents::Result<rimz::agents::HookOutput> {
        let mut decoded = rimz::agents::definition_by_kind("antigravity")
            .unwrap()
            .decode_hook(event_name, payload)?;
        decoded.update_lifecycle(|observation| {
            observation.pane_id = Some(id("terminal_77"));
        });
        Ok(decoded)
    }

    fn correlate_subagent(
        &self,
        input: rimz::agents::SubagentCorrelationInput<'_>,
    ) -> Option<rimz::agents::SubagentCorrelation> {
        self.correlation_calls.fetch_add(1, Ordering::Relaxed);
        let child = input.child_agent_id.as_str();
        let parent = input.parent_agent_id.as_str();
        let matches = match child {
            "child-clean" | "child-failed" | "child-parked" | "child-recovered" => parent == "root",
            "nested-child" => parent == "child-clean",
            "ambiguous-child" => {
                matches!(parent, "ambiguous-parent-a" | "ambiguous-parent-b")
            }
            "cycle-child" => parent == "cycle-parent",
            _ => false,
        };
        matches.then(|| rimz::agents::SubagentCorrelation {
            agent_name: Some(format!("name-{child}")),
            role: Some(format!("role-{child}")),
            task: Some(format!("task-{child}")),
            prompt: Some(format!("prompt-{child}")),
            model: None,
        })
    }

    fn spawned_subagents(
        &self,
        input: rimz::agents::SubagentSpawnInput<'_>,
    ) -> Vec<rimz::agents::SpawnedSubagent> {
        let child =
            |id: &'static str, name: &'static str, role: &'static str, prompt: &'static str| {
                rimz::agents::SpawnedSubagent {
                    child_agent_id: AgentSessionId::from(id),
                    agent_name: Some(name.to_owned()),
                    role: Some(role.to_owned()),
                    prompt: Some(prompt.to_owned()),
                    model: None,
                    usage: Default::default(),
                }
            };
        match input.parent_agent_id.as_str() {
            "late-root" => vec![
                child(
                    "late-child-clean",
                    "clean-name",
                    "clean-role",
                    "clean prompt",
                ),
                child(
                    "late-child-failed",
                    "failed-name",
                    "failed-role",
                    "failed prompt",
                ),
            ],
            "reaped-root" => vec![child(
                "late-child-reaped",
                "reaped-name",
                "reaped-role",
                "reaped prompt",
            )],
            _ => Vec::new(),
        }
    }
}

fn antigravity_payload(agent_id: &str) -> serde_json::Value {
    serde_json::json!({
        "conversationId": agent_id,
        "workspacePaths": ["/tmp/hooks-test"],
        "transcriptPath": format!("/tmp/{agent_id}.jsonl"),
    })
}

fn feed_antigravity(
    store: &rimz::Store,
    adapter: &rimz::agents::AgentDefinition,
    event_name: &str,
    payload: serde_json::Value,
) {
    let mut decoded = adapter.decode_hook(event_name, &payload).unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        store,
        adapter,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
}

fn feed_antigravity_statusline(
    store: &rimz::Store,
    agent_id: &str,
    fields: serde_json::Value,
) -> Vec<rimz::store::ingress::HookIngress> {
    feed_antigravity_statusline_in(store, agent_id, fields, &std::collections::BTreeMap::new())
}

/// Feed one statusline payload, then apply the hook frames it appended in log order, as the
/// drainer does, and return them.
fn feed_antigravity_statusline_in(
    store: &rimz::Store,
    agent_id: &str,
    fields: serde_json::Value,
    env: &std::collections::BTreeMap<String, String>,
) -> Vec<rimz::store::ingress::HookIngress> {
    let (payload, observation) = observe_antigravity_statusline(agent_id, fields);
    // The hook and the statusline stamp their own clocks; keep them apart.
    std::thread::sleep(std::time::Duration::from_millis(2));
    let from = ingress_len(store);
    persist_antigravity_statusline(store, &payload, observation, env);
    apply_antigravity_frames_from(store, from)
}

fn feed_antigravity_statusline_at(
    store: &rimz::Store,
    agent_id: &str,
    fields: serde_json::Value,
    env: &std::collections::BTreeMap<String, String>,
    observed_at: jiff::Timestamp,
) -> Vec<rimz::store::ingress::HookIngress> {
    let (payload, mut observation) = observe_antigravity_statusline(agent_id, fields);
    observation.context.observed_at = observed_at;
    let from = ingress_len(store);
    persist_antigravity_statusline(store, &payload, observation, env);
    apply_antigravity_frames_from(store, from)
}

fn observe_antigravity_statusline(
    agent_id: &str,
    fields: serde_json::Value,
) -> (serde_json::Value, rimz::agents::context::ContextObservation) {
    let mut payload = fields;
    payload["conversation_id"] = serde_json::json!(agent_id);
    let observation = rimz::agents::definition_by_kind("antigravity")
        .unwrap()
        .observe_context("antigravity", &payload)
        .unwrap();
    (payload, observation)
}

fn persist_antigravity_statusline(
    store: &rimz::Store,
    payload: &serde_json::Value,
    observation: rimz::agents::context::ContextObservation,
    env: &std::collections::BTreeMap<String, String>,
) {
    let agent = rimz::agents::definition_by_kind("antigravity").unwrap();
    crate::cli::statusline::persist_observation(store, agent, payload, observation, env).unwrap();
}

fn ingress_len(store: &rimz::Store) -> u64 {
    std::fs::metadata(&store.paths().hook_ingress_log).map_or(0, |metadata| metadata.len())
}

fn apply_antigravity_frames_from(
    store: &rimz::Store,
    from: u64,
) -> Vec<rimz::store::ingress::HookIngress> {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    if ingress_len(store) == from {
        return Vec::new();
    }
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    let (frames, _) = rimz::store::ingress::read_from_offset(store.paths(), from).unwrap();
    let frames: Vec<_> = frames.into_iter().map(|(frame, _)| frame).collect();
    for frame in &frames {
        let payload: serde_json::Value = serde_json::from_str(&frame.payload).unwrap();
        let event = frame.event.as_deref().unwrap();
        with_frame_env(frame, FrameAttempt::First, || {
            let mut decoded = adapter.decode_hook_in(event, &payload, &frame.env).unwrap();
            handle_lifecycle_hook(
                &hooks_test_workspace(Some("main")),
                &store.for_hook_ingress(frame.event_id.clone(), frame.ts),
                adapter,
                &mut decoded,
                &payload,
                rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                &hooks_test_globals(),
            )
            .unwrap()
        });
    }
    frames
}

/// The lifecycle signals the log holds for `agent_id`, in log order.
fn antigravity_signals(store: &rimz::Store, agent_id: &str) -> Vec<LifecycleSignal> {
    store
        .read_events()
        .unwrap()
        .into_iter()
        .filter_map(|envelope| match envelope.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload
                    .observation
                    .agent_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == agent_id) =>
            {
                Some(payload.observation.signal)
            }
            _ => None,
        })
        .collect()
}

fn queue_antigravity_continue(store: &rimz::Store, agent_id: &str) {
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageSender, SoftCompactStep};
    let mut message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &antigravity_row(store, agent_id),
        "continue".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::System);
    message.soft_compact = Some(SoftCompactStep::Continue);
    store.queue_message(&message, "main").unwrap();
}

fn antigravity_continue_owed(store: &rimz::Store, agent_id: &str) -> bool {
    antigravity_row(store, agent_id)
        .soft_compact
        .is_some_and(|mark| mark.continue_owed.is_some())
}

fn antigravity_row(store: &rimz::Store, agent_id: &str) -> rimz::agents::AgentState {
    store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == agent_id)
        .cloned()
        .unwrap()
}

fn stored_settle(store: &rimz::Store, agent_id: &str) -> Option<rimz::agents::TurnSettle> {
    rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", agent_id)
        .unwrap()
        .context
        .settle
}

fn start_antigravity_turn(store: &rimz::Store, agent_id: &str) {
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    let mut start = antigravity_payload(agent_id);
    start["invocationNum"] = serde_json::json!(0);
    feed_antigravity(store, adapter, "PreInvocation", start);
}

fn stop_antigravity_turn(store: &rimz::Store, agent_id: &str, fully_idle: bool) {
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    let mut stop = antigravity_payload(agent_id);
    stop["fullyIdle"] = serde_json::json!(fully_idle);
    stop["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(store, adapter, "Stop", stop);
}

fn park_antigravity_with_shell(store: &rimz::Store, agent_id: &str, shell_id: &str, command: &str) {
    let workspace = hooks_test_workspace(Some("main"));
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
            turn_id: None,
        },
    );
    observation.background_shells = Some(rimz::agents::BackgroundShellReport::Snapshot {
        shells: vec![rimz::agents::BackgroundShell {
            id: shell_id.to_owned(),
            command: Some(command.to_owned()),
            description: None,
            started_at: jiff::Timestamp::now(),
        }],
    });
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event_name: "Stop",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

fn create_antigravity_child_run(
    store: &rimz::Store,
    agent_id: &str,
) -> rimz::store::run::RunRecord {
    let mut run = rimz::store::run::RunRecord::new(
        store.paths().workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("antigravity"),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        "/tmp/hooks-test".into(),
    );
    run.subagent = true;
    run.status = rimz::store::run::RunStatus::Running;
    run.agent_id = Some(agent_id.into());
    rimz::harness::run::create(store.paths(), &run).unwrap();
    run
}

fn write_antigravity_transcript(
    home: &std::path::Path,
    agent_id: &str,
    reply: &str,
) -> std::path::PathBuf {
    let path = home
        .join(".gemini/antigravity-cli/brain")
        .join(agent_id)
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let user = serde_json::json!({
        "step_index": 0,
        "source": "USER_EXPLICIT",
        "type": "USER_INPUT",
        "status": "DONE",
        "created_at": "2026-07-13T23:23:08Z",
        "content": "run task",
    });
    let assistant = serde_json::json!({
        "step_index": 1,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "DONE",
        "created_at": "2026-07-13T23:23:09Z",
        "content": reply,
    });
    std::fs::write(&path, format!("{user}\n{assistant}\n")).unwrap();
    path
}

struct CopilotCorrelationAdapter;

impl rimz::agents::capabilities::CoreCapability for CopilotCorrelationAdapter {
    fn spec(&self) -> &'static rimz::agents::AgentSpec {
        rimz::agents::definition_by_kind("copilot").unwrap().spec()
    }
}

impl rimz::agents::capabilities::HookCapability for CopilotCorrelationAdapter {
    fn decode_hook(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> rimz::agents::Result<rimz::agents::HookOutput> {
        let mut decoded = rimz::agents::definition_by_kind("copilot")
            .unwrap()
            .decode_hook(event_name, payload)?;
        decoded.update_lifecycle(|observation| {
            observation.pane_id = Some(id("terminal_88"));
        });
        Ok(decoded)
    }

    fn correlate_subagent(
        &self,
        input: rimz::agents::SubagentCorrelationInput<'_>,
    ) -> Option<rimz::agents::SubagentCorrelation> {
        rimz::agents::definition_by_kind("copilot")
            .unwrap()
            .correlate_subagent(input)
    }

    fn spawned_subagents(
        &self,
        input: rimz::agents::SubagentSpawnInput<'_>,
    ) -> Vec<rimz::agents::SpawnedSubagent> {
        rimz::agents::definition_by_kind("copilot")
            .unwrap()
            .spawned_subagents(input)
    }
}

fn feed_copilot(store: &rimz::Store, event_name: &str, payload: serde_json::Value) {
    let adapter = hook_definition(Box::leak(Box::new(CopilotCorrelationAdapter)));
    let mut decoded = adapter.decode_hook(event_name, &payload).unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        store,
        &adapter,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
}

struct PiAdoptionTestAdapter {
    pane_id: PaneId,
}

impl rimz::agents::capabilities::CoreCapability for PiAdoptionTestAdapter {
    fn spec(&self) -> &'static rimz::agents::AgentSpec {
        rimz::agents::definition_by_kind("pi").unwrap().spec()
    }
}

impl rimz::agents::capabilities::HookCapability for PiAdoptionTestAdapter {
    fn decode_hook(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> rimz::agents::Result<rimz::agents::HookOutput> {
        let mut decoded = rimz::agents::definition_by_kind("pi")
            .unwrap()
            .decode_hook(event_name, payload)?;
        decoded.update_lifecycle(|observation| {
            observation.pane_id = Some(self.pane_id.clone());
        });
        Ok(decoded)
    }
}

fn feed_pi(
    store: &rimz::Store,
    adapter: &rimz::agents::AgentDefinition,
    event_name: &str,
    payload: serde_json::Value,
) {
    let mut decoded = adapter.decode_hook(event_name, &payload).unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        store,
        adapter,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
}

fn pi_session_payload(session_id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp/hooks-test",
        "model": "gpt-5.6-sol",
        "effort": "xhigh",
        "context_pct": 12,
        "context_window": 200_000,
        "total_tokens": 24_000,
    })
}

fn pi_bridge_payload(parent_id: &str, child_id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": parent_id,
        "cwd": "/tmp/hooks-test",
        "subagent_id": child_id,
        "subagent_label": "general-purpose: inspect the bridge",
        "subagent_source": "pi-session",
    })
}

fn adoption_signals(store: &rimz::Store) -> Vec<LifecycleSignal> {
    store
        .read_events()
        .unwrap()
        .iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload.event_name.as_deref() == Some("SubagentAdopted") =>
            {
                Some(payload.observation.signal.clone())
            }
            _ => None,
        })
        .collect()
}

fn seed_subagent_candidate(store: &rimz::Store, agent_id: &str, parent_id: &str) {
    let mut candidate = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::SubagentStarted,
    );
    candidate.parent_agent_id = Some(AgentSessionId::from(parent_id));
    candidate.agent_name = Some(agent_id.to_owned());
    candidate.task = Some("seeded candidate".to_owned());
    candidate.pane_id = Some(id("terminal_77"));
    candidate.worktree_path = Some("/tmp/hooks-test".to_owned());
    candidate.transcript_path = Some(format!("/tmp/{agent_id}.jsonl"));
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "hooks-test",
            agent_kind: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event_name: "seed-candidate",
            observation: &candidate,
            spawned_subagents: &[],
        })
        .unwrap();
}

fn launch_identity_env(_agent_pid: Option<u32>, var: &'static str) -> Option<String> {
    match var {
        rimz::harness::launch::ENV_AGENT_ROLE => Some("coder".to_owned()),
        rimz::harness::launch::ENV_TEAM => Some("forge".to_owned()),
        rimz::harness::launch::ENV_LAUNCH_GROUP => Some("launch_group_1".to_owned()),
        rimz::harness::launch::ENV_LAUNCH_ORDINAL => Some("2".to_owned()),
        rimz::harness::launch::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
        rimz::harness::launch::ENV_AGENT_MODEL => Some("env-model".to_owned()),
        rimz::harness::launch::ENV_AGENT_EFFORT => Some("env-effort".to_owned()),
        rimz::harness::launch::ENV_AGENT_ID => Some("launch_coder".to_owned()),
        rimz::harness::launch::ENV_AGENT_EPOCH => Some("2".to_owned()),
        _ => None,
    }
}

#[test]
fn agent_kind_matches_known_launch_shapes() {
    for (comm, source, expected) in [
        ("claude", "claude", true),
        ("codex", "codex", true),
        ("codex-aarch64-a", "codex", true),
        ("node", "codex", true),
        ("node", "claude", false),
        ("kiro-cli", "kiro", true),
        ("kiro-cli-chat", "kiro", true),
        ("kiro-cli-term", "kiro", false),
        ("zsh", "claude", false),
        ("bash", "codex", false),
    ] {
        assert_eq!(
            matches_agent_kind(comm, source),
            expected,
            "{comm}/{source}"
        );
    }
}

#[test]
fn stop_failure_records_turn_error_transcript_entry() {
    let (_dir, store) = hooks_test_store();
    let workspace = hooks_test_workspace(Some("main"));
    let globals = hooks_test_globals();

    let payload = serde_json::json!({
        "session_id": "sess-1",
        "error": "overloaded",
        "last_assistant_message": "API Error: Response stalled mid-stream. The response above may be incomplete."
    });
    let mut decoded = rimz::agents::definition_by_kind("claude")
        .unwrap()
        .decode_hook("StopFailure", &payload)
        .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &globals,
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let [entry] = entries.as_slice() else {
        panic!("expected one transcript entry, got {entries:?}");
    };
    assert_eq!(entry.entry, rimz::transcript::TranscriptKind::Error);
    assert_eq!(entry.kind.as_str(), "claude");
    assert_eq!(entry.agent_id.as_str(), "sess-1");
    assert_eq!(
        entry.channel.as_ref().map(rimz::ids::LaneName::as_str),
        Some("main")
    );
    assert_eq!(
        entry.text,
        "API Error: Response stalled mid-stream. The response above may be incomplete."
    );
}

#[test]
fn canonical_droid_prompt_and_worker_stop_record_one_conversation() {
    let (dir, store) = hooks_test_store();
    let workspace = hooks_test_workspace(Some("main"));
    let globals = hooks_test_globals();
    let transcript_path = dir.path().join("droid-session.jsonl");
    std::fs::write(
        &transcript_path,
        concat!(
            "{\"type\":\"session_start\",\"version\":2}\n",
            "{\"type\":\"message\",\"id\":\"user\",\"timestamp\":\"2026-07-13T20:19:51.315Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ping\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"assistant\",\"parentId\":\"user\",\"timestamp\":\"2026-07-13T20:19:54.616Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"pong\"}]}}\n",
        ),
    )
    .unwrap();
    let path = transcript_path.to_string_lossy();
    let owner_pid = std::process::id();

    let prompt_payload = serde_json::json!({
        "session_id": "droid-session",
        "transcript_path": path,
        "prompt": "ping"
    });
    let mut prompt_decoded = rimz::agents::definition_by_kind("droid")
        .unwrap()
        .decode_hook("UserPromptSubmit", &prompt_payload)
        .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("droid").unwrap(),
        &mut prompt_decoded,
        &prompt_payload,
        rimz::agents::HookIngressOwner::agent(Some(owner_pid)),
        &globals,
    )
    .unwrap();
    let stop_payload = serde_json::json!({
        "session_id": "droid-session",
        "transcript_path": path
    });
    let mut stop_decoded = rimz::agents::definition_by_kind("droid")
        .unwrap()
        .decode_hook("Stop", &stop_payload)
        .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("droid").unwrap(),
        &mut stop_decoded,
        &stop_payload,
        rimz::agents::HookIngressOwner::agent(Some(owner_pid)),
        &globals,
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[0].text, "ping");
    assert_eq!(
        entries[1].entry,
        rimz::transcript::TranscriptKind::Assistant
    );
    assert_eq!(entries[1].text, "pong");
    let user_inputs = rimz::agents::spending::user_input::load_in(dir.path());
    assert_eq!(user_inputs.len(), 1);
    assert_eq!(user_inputs[0].kind.as_str(), "droid");
    assert_eq!(
        user_inputs[0].origin.as_deref(),
        Some(std::path::Path::new("/tmp/hooks-test"))
    );
}

#[test]
fn non_pricing_context_merge_does_not_load_a_price_book() {
    let (_dir, store) = hooks_test_store();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let payload = serde_json::json!({"session_id": "no-pricing", "cwd": "/tmp/hooks-test"});
    let mut decoded = agent.decode_hook("SessionStart", &payload).unwrap();
    decoded.take_lifecycle();
    assert!(decoded.context_agent_id().is_some());
    let before = rimz::agents::pricing::testkit::book_loads();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        &store,
        agent,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
    assert_eq!(rimz::agents::pricing::testkit::book_loads(), before);
}

#[test]
fn context_only_session_end_cleans_up_sidecar() {
    let (_dir, store) = hooks_test_store();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let payload = serde_json::json!({"session_id": "context-only", "cwd": "/tmp/hooks-test"});
    let mut decoded = agent.decode_hook("SessionEnd", &payload).unwrap();
    decoded.take_lifecycle();
    assert!(decoded.lifecycle().is_none());
    assert_eq!(decoded.context_agent_id().unwrap().as_str(), "context-only");
    let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
    context.model_id = Some("stale-model".into());
    rimz::store::agent_context::merge_observed(
        store.runtime_paths(),
        "claude",
        "context-only",
        context,
    )
    .unwrap();
    assert!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "context-only")
            .is_some()
    );
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        &store,
        agent,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
    assert!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "context-only")
            .is_none()
    );
}

#[test]
fn root_launch_identity_fills_from_env_then_config_without_clobbering_payload() {
    let mut observed = root_observation();
    observed.agent_pid = Some(123);
    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        (true, true),
        |agent_pid, var| {
            assert_eq!(agent_pid, Some(123));
            launch_identity_env(agent_pid, var)
        },
    );
    assert_eq!(observed.launch.role.as_deref(), Some("coder"));
    assert_eq!(observed.launch.team.as_deref(), Some("forge"));
    assert_eq!(
        observed.launch.launch_group.as_deref(),
        Some("launch_group_1")
    );
    assert_eq!(observed.launch.launch_ordinal, Some(2));
    assert_eq!(observed.launch.profile.as_deref(), Some("codex-coder"));
    assert_eq!(observed.launch.model.as_deref(), Some("env-model"));
    assert_eq!(observed.launch.effort.as_deref(), Some("env-effort"));
    assert_eq!(
        (
            observed.launch_id.as_ref().map(|id| id.as_str()),
            observed.launch.epoch
        ),
        (Some("launch_coder"), Some(2)),
        "the hook stamps the launch and epoch it ran under"
    );

    let mut payload = root_observation();
    payload.launch.role = Some("payload-role".to_owned());
    payload.launch.team = Some("payload-team".to_owned());
    payload.launch.launch_group = Some("payload-group".to_owned());
    payload.launch.launch_ordinal = Some(7);
    payload.launch.profile = Some("payload-profile".to_owned());
    payload.launch.model = Some("payload-model".to_owned());
    payload.launch.effort = Some("payload-effort".to_owned());
    fill_root_launch_identity(
        &mut payload,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        (true, true),
        launch_identity_env,
    );
    assert_eq!(payload.launch.role.as_deref(), Some("payload-role"));
    assert_eq!(payload.launch.team.as_deref(), Some("payload-team"));
    assert_eq!(
        payload.launch.launch_group.as_deref(),
        Some("payload-group")
    );
    assert_eq!(payload.launch.launch_ordinal, Some(7));
    assert_eq!(payload.launch.profile.as_deref(), Some("payload-profile"));
    assert_eq!(payload.launch.model.as_deref(), Some("payload-model"));
    assert_eq!(payload.launch.effort.as_deref(), Some("payload-effort"));

    let mut configured = root_observation();
    fill_root_launch_identity(
        &mut configured,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        (true, true),
        |_agent_pid, var| match var {
            rimz::harness::launch::ENV_AGENT_ROLE => Some("coder".to_owned()),
            rimz::harness::launch::ENV_TEAM => Some("forge".to_owned()),
            rimz::harness::launch::ENV_LAUNCH_ORDINAL => Some("not-a-number".to_owned()),
            rimz::harness::launch::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
            _ => None,
        },
    );
    assert_eq!(configured.launch.model.as_deref(), Some("cfg-model"));
    assert_eq!(configured.launch.effort.as_deref(), Some("cfg-effort"));
    assert_eq!(configured.launch.launch_ordinal, None);
}

#[test]
fn root_launch_identity_seeds_model_and_effort_independently() {
    for seed in [(false, false), (true, false), (false, true)] {
        let mut observed = root_observation();
        fill_root_launch_identity(
            &mut observed,
            (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
            seed,
            launch_identity_env,
        );
        assert_eq!(
            observed.launch.model.as_deref(),
            seed.0.then_some("env-model")
        );
        assert_eq!(
            observed.launch.effort.as_deref(),
            seed.1.then_some("env-effort")
        );
        assert_eq!(observed.launch.role.as_deref(), Some("coder"));
        assert_eq!(observed.launch.team.as_deref(), Some("forge"));
        assert_eq!(
            observed.launch.launch_group.as_deref(),
            Some("launch_group_1")
        );
        assert_eq!(observed.launch.launch_ordinal, Some(2));
        assert_eq!(observed.launch.profile.as_deref(), Some("codex-coder"));
    }
}

#[test]
fn root_identity_rollup_seeds_until_observed_and_hints_the_shown_model() {
    let (_dir, store) = hooks_test_store();
    let claude = rimz::agents::definition_by_kind("claude").unwrap();
    assert_eq!(
        root_identity_rollup(&store, "claude", &root_observation()),
        ((true, true), None)
    );

    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-1",
        "cwd": "/tmp/hooks-test",
        "source": "startup",
        "model": "claude-fable-5-1",
    });
    let mut decoded = claude.decode_hook("SessionStart", &payload).unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        &store,
        claude,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();

    assert_eq!(
        root_identity_rollup(&store, "claude", &root_observation()),
        ((false, true), Some("claude-fable-5-1".to_owned()))
    );
}

#[test]
fn subagent_launch_identity_is_not_inherited_from_parent_env() {
    let mut observed = root_observation();
    observed.parent_agent_id = Some(AgentSessionId::from("parent-1"));

    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        (true, true),
        |_, _| panic!("child identity must not read parent env"),
    );

    assert_eq!(observed.launch.role, None);
    assert_eq!(observed.launch.team, None);
    assert_eq!(observed.launch.launch_group, None);
    assert_eq!(observed.launch.launch_ordinal, None);
    assert_eq!(observed.launch.profile, None);
    assert_eq!(observed.launch.model, None);
    assert_eq!(observed.launch.effort, None);
}

#[test]
fn grok_child_session_hooks_resolve_children_under_the_spawning_parent() {
    let (_dir, store) = hooks_test_store();
    let grok = rimz::agents::definition_by_kind("grok").unwrap();
    let feed = |event: &str, payload: serde_json::Value| feed_pi(&store, grok, event, payload);
    let agent = |agent_id: &str| {
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == agent_id)
            .cloned()
            .unwrap()
    };
    let root = |extra: serde_json::Value| {
        let mut payload = serde_json::json!({"sessionId": "grok-root", "cwd": "/tmp/hooks-test"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        payload
    };
    let child = |child_id: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "sessionId": child_id,
            "cwd": "/tmp/hooks-test",
            "subagentType": "explore",
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        payload
    };

    feed("session_start", root(serde_json::json!({"source": "new"})));
    feed(
        "user_prompt_submit",
        root(serde_json::json!({"prompt": "go"})),
    );
    for child_id in ["grok-child-ok", "grok-child-failed"] {
        feed(
            "subagent_start",
            root(serde_json::json!({"subagentId": child_id, "subagentType": "explore"})),
        );
        feed(
            "user_prompt_submit",
            child(child_id, serde_json::json!({"prompt": "map it"})),
        );
    }
    feed(
        "subagent_stop",
        child(
            "grok-child-ok",
            serde_json::json!({"subagentId": "grok-child-ok", "phase": "gate"}),
        ),
    );
    feed(
        "session_end",
        child("grok-child-ok", serde_json::json!({"reason": "shutdown"})),
    );
    feed(
        "stop_failure",
        child(
            "grok-child-failed",
            serde_json::json!({"error": "server_error"}),
        ),
    );

    let ok = agent("grok-child-ok");
    assert_eq!(ok.parent_agent_id.as_deref(), Some("grok-root"));
    assert_eq!(ok.status, rimz::agents::AgentStatus::Success);
    let failed = agent("grok-child-failed");
    assert_eq!(failed.parent_agent_id.as_deref(), Some("grok-root"));
    assert_eq!(failed.status, rimz::agents::AgentStatus::Failed);
    assert_eq!(
        agent("grok-root").status,
        rimz::agents::AgentStatus::Running
    );

    feed(
        "stop_cancelled",
        root(serde_json::json!({"reason": "user_interrupt", "cancelledBy": "user"})),
    );
    assert_eq!(agent("grok-root").status, rimz::agents::AgentStatus::Idle);
    feed(
        "user_prompt_submit",
        root(serde_json::json!({"prompt": "again"})),
    );
    feed(
        "stop_failure",
        root(serde_json::json!({"error": "invalid_request", "errorDetails": "HTTP 400"})),
    );
    assert_eq!(agent("grok-root").status, rimz::agents::AgentStatus::Failed);
}

#[test]
fn an_antigravity_rest_report_interrupts_only_a_turn_its_hooks_hold_open() {
    let (_dir, store) = hooks_test_store();
    let idle = || serde_json::json!({"agent_state": "idle"});
    let interrupted = LifecycleSignal::TurnInterrupted { turn_id: None };

    start_antigravity_turn(&store, "escaped");
    let frames = feed_antigravity_statusline(&store, "escaped", idle());
    assert_eq!(frames.len(), 1);
    assert_eq!(
        antigravity_signals(&store, "escaped").last(),
        Some(&interrupted),
        "an Escape records the interrupt"
    );
    let escaped = antigravity_row(&store, "escaped");
    assert_eq!(escaped.status, rimz::agents::AgentStatus::Idle);
    assert_eq!(
        stored_settle(&store, "escaped"),
        None,
        "no settle is written"
    );
    let recorded = antigravity_signals(&store, "escaped").len();
    assert_eq!(
        feed_antigravity_statusline(&store, "escaped", idle()).len(),
        1
    );
    assert_eq!(
        antigravity_signals(&store, "escaped").len(),
        recorded,
        "a re-emitted idle on a rested row records nothing"
    );
    assert_eq!(
        antigravity_row(&store, "escaped").last_activity,
        escaped.last_activity
    );
}

#[test]
fn an_antigravity_rest_after_a_turn_end_changes_nothing_and_leaves_an_owed_continue_owed() {
    let (_dir, store) = hooks_test_store();
    start_antigravity_turn(&store, "finished");
    queue_antigravity_continue(&store, "finished");
    stop_antigravity_turn(&store, "finished", true);
    assert!(antigravity_continue_owed(&store, "finished"));
    let (payload, observation) =
        observe_antigravity_statusline("finished", serde_json::json!({"agent_state": "idle"}));
    let from = ingress_len(&store);
    persist_antigravity_statusline(
        &store,
        &payload,
        observation,
        &std::collections::BTreeMap::new(),
    );
    let before = antigravity_row(&store, "finished");
    let signals = antigravity_signals(&store, "finished");
    let sidecar =
        rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", "finished");

    assert_eq!(apply_antigravity_frames_from(&store, from).len(), 1);
    let after = antigravity_row(&store, "finished");
    assert_eq!(antigravity_signals(&store, "finished"), signals);
    assert_eq!(
        (after.status, after.last_activity, after.context.clone()),
        (before.status, before.last_activity, before.context.clone())
    );
    assert_eq!(after.status, rimz::agents::AgentStatus::Success);
    assert_eq!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", "finished"),
        sidecar
    );
    assert!(antigravity_continue_owed(&store, "finished"));
}

#[test]
fn an_antigravity_rest_report_rests_a_waiting_row_and_closes_its_ask() {
    let (_dir, store) = hooks_test_store();
    start_antigravity_turn(&store, "asking");
    let workspace = hooks_test_workspace(Some("main"));
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("asking")),
        LifecycleSignal::AwaitingInput {
            extra_questions: 0,
            kind: rimz::agents::AskKind::Permission,
            ask_id: Some(rimz::ids::AskId::new()),
            detail: None,
            native_key: None,
        },
    );
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event_name: "Ask",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    let waiting = antigravity_row(&store, "asking");
    assert_eq!(waiting.status, rimz::agents::AgentStatus::Waiting);
    assert!(waiting.open_ask.is_some());

    feed_antigravity_statusline(&store, "asking", serde_json::json!({"agent_state": "idle"}));
    let rested = antigravity_row(&store, "asking");
    assert_eq!(
        (rested.status, rested.open_ask.is_some()),
        (rimz::agents::AgentStatus::Idle, false)
    );
}

#[test]
fn an_antigravity_rest_report_leaves_a_busy_or_newer_turn_alone() {
    let (_dir, store) = hooks_test_store();
    let idle = || serde_json::json!({"agent_state": "idle"});
    let interrupts = |agent_id: &str| {
        antigravity_signals(&store, agent_id)
            .iter()
            .filter(|signal| matches!(signal, LifecycleSignal::TurnInterrupted { .. }))
            .count()
    };

    // Pending task_count (command, timer, or recurring job) or working state changes nothing.
    start_antigravity_turn(&store, "parked");
    park_antigravity_with_shell(&store, "parked", "task-1", "sleep 60");
    let before = antigravity_row(&store, "parked");
    let signals_before = antigravity_signals(&store, "parked");
    for payload in [
        serde_json::json!({"agent_state": "idle", "task_count": 1}),
        serde_json::json!({"agent_state": "working"}),
    ] {
        assert_eq!(
            feed_antigravity_statusline(&store, "parked", payload).len(),
            0
        );
    }
    let after = antigravity_row(&store, "parked");
    assert_eq!(
        (after.status, after.phase, after.background_shells.len()),
        (
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Parked,
            1
        )
    );
    assert_eq!(after.last_activity, before.last_activity);
    assert_eq!(antigravity_signals(&store, "parked"), signals_before);

    // agy reads idle before a new turn's first hook; the report lands after that hook.
    start_antigravity_turn(&store, "newer");
    stop_antigravity_turn(&store, "newer", true);
    let (payload, observation) = observe_antigravity_statusline("newer", idle());
    std::thread::sleep(std::time::Duration::from_millis(2));
    start_antigravity_turn(&store, "newer");
    let from = ingress_len(&store);
    persist_antigravity_statusline(
        &store,
        &payload,
        observation,
        &std::collections::BTreeMap::new(),
    );
    assert_eq!(apply_antigravity_frames_from(&store, from).len(), 1);
    assert_eq!(
        interrupts("newer"),
        0,
        "a report observed before the new turn"
    );
    assert_eq!(
        antigravity_row(&store, "newer").status,
        rimz::agents::AgentStatus::Running
    );
}

#[test]
fn an_antigravity_escape_after_a_self_compaction_cancels_its_continue_for_good() {
    let (_dir, store) = hooks_test_store();
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    start_antigravity_turn(&store, "compacted");
    queue_antigravity_continue(&store, "compacted");
    assert!(antigravity_continue_owed(&store, "compacted"));

    feed_antigravity_statusline(
        &store,
        "compacted",
        serde_json::json!({"agent_state": "idle"}),
    );
    let mut owed = vec![antigravity_continue_owed(&store, "compacted")];
    feed_antigravity_statusline(
        &store,
        "compacted",
        serde_json::json!({"agent_state": "working"}),
    );
    owed.push(antigravity_continue_owed(&store, "compacted"));
    let mut compact = antigravity_payload("compacted");
    compact["lastUserInput"] = serde_json::json!("/compact");
    compact["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, adapter, "PreInvocation", compact.clone());
    owed.push(antigravity_continue_owed(&store, "compacted"));
    compact["fullyIdle"] = serde_json::json!(true);
    compact["terminationReason"] = serde_json::json!("TERMINAL_CUSTOM_HOOK");
    feed_antigravity(&store, adapter, "Stop", compact);
    assert!(
        antigravity_signals(&store, "compacted")
            .iter()
            .any(|signal| matches!(signal, LifecycleSignal::CompactionEnded { .. }))
    );
    owed.push(antigravity_continue_owed(&store, "compacted"));
    assert_eq!(
        owed, [false; 4],
        "escape, working, compacting, compaction ended"
    );
}

#[test]
fn an_antigravity_rest_report_opens_the_interrupt_hold_of_a_supervised_child() {
    let (_dir, store) = hooks_test_store();
    let mut run = rimz::store::run::RunRecord::new(
        store.paths().workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("antigravity"),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        "/tmp/hooks-test".into(),
    );
    run.subagent = true;
    run.status = rimz::store::run::RunStatus::Running;
    run.agent_id = Some("child".into());
    rimz::harness::run::create(store.paths(), &run).unwrap();
    start_antigravity_turn(&store, "child");

    let env =
        std::collections::BTreeMap::from([("RIMZ_RUN_ID".to_owned(), run.run_id.to_string())]);
    let frames = feed_antigravity_statusline_in(
        &store,
        "child",
        serde_json::json!({"agent_state": "idle"}),
        &env,
    );
    assert_eq!(frames.len(), 1);
    let held = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (held.status, held.interrupted_at),
        (rimz::store::run::RunStatus::Running, Some(frames[0].ts))
    );
}

#[test]
fn an_antigravity_statusline_appends_one_rest_frame_only_when_it_reports_rest() {
    let (_dir, store) = hooks_test_store();
    start_antigravity_turn(&store, "working");
    let rows = rimz::testkit::rollup_rows_materialized();
    let feed = |fields| {
        let (payload, observation) = observe_antigravity_statusline("working", fields);
        let observed_at = observation.context.observed_at;
        let from = ingress_len(&store);
        persist_antigravity_statusline(
            &store,
            &payload,
            observation,
            &std::collections::BTreeMap::new(),
        );
        let (frames, _) = rimz::store::ingress::read_from_offset(store.paths(), from).unwrap();
        (
            frames
                .into_iter()
                .map(|(frame, _)| frame)
                .collect::<Vec<_>>(),
            observed_at,
        )
    };
    let (working, _) = feed(serde_json::json!({"agent_state": "working"}));
    let (background, _) = feed(serde_json::json!({"agent_state": "idle", "task_count": 1}));
    let (rest, observed_at) = feed(serde_json::json!({"agent_state": "idle"}));
    assert_eq!([working.len(), background.len()], [0, 0]);
    assert_eq!(
        rest.iter()
            .map(|frame| (frame.source.as_str(), frame.event.as_deref(), frame.ts))
            .collect::<Vec<_>>(),
        [(
            "antigravity",
            Some(rimz::agents::PROVIDER_REST_EVENT),
            observed_at
        )]
    );
    assert_eq!(stored_settle(&store, "working"), None);
    assert_eq!(
        rimz::testkit::rollup_rows_materialized(),
        rows,
        "the statusline reads no row"
    );

    // A failed append still stores the context.
    let (_dir, store) = hooks_test_store();
    std::fs::create_dir_all(&store.paths().hook_ingress_log).unwrap();
    let (payload, observation) =
        observe_antigravity_statusline("unlogged", serde_json::json!({"agent_state": "idle"}));
    persist_antigravity_statusline(
        &store,
        &payload,
        observation,
        &std::collections::BTreeMap::new(),
    );
    assert!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", "unlogged")
            .is_some()
    );
}

#[test]
fn a_scoped_antigravity_rest_report_with_no_host_drainer_is_dropped() {
    for host_paths in [false, true] {
        let (_dir, store) = hooks_test_store();
        start_antigravity_turn(&store, "scoped");
        let env = std::collections::BTreeMap::from([if host_paths {
            (
                rimz::config::Isolation::SCOPED_HOST_PATHS_ENV.to_owned(),
                "[]".to_owned(),
            )
        } else {
            (rimz::config::Isolation::ENV.to_owned(), "scoped".to_owned())
        }]);
        let frames = feed_antigravity_statusline_in(
            &store,
            "scoped",
            serde_json::json!({"agent_state": "idle"}),
            &env,
        );
        assert_eq!(
            (
                frames.len(),
                store.paths().hook_drain_cursor.exists(),
                store.runtime_paths().hook_drainer_spawn_lock().exists(),
            ),
            (0, false, false),
            "host paths: {host_paths}: nothing appended, drained, or spawned"
        );
        assert!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", "scoped")
                .is_some(),
            "host paths: {host_paths}"
        );
        assert_eq!(
            antigravity_row(&store, "scoped").status,
            rimz::agents::AgentStatus::Running,
            "host paths: {host_paths}"
        );
    }
}

#[test]
fn an_antigravity_parked_turn_reads_success_whichever_of_stop_and_idle_lands_first() {
    let parked_idle = || serde_json::json!({"agent_state": "idle", "task_count": 1});
    for stop_first in [true, false] {
        let (_dir, store) = hooks_test_store();
        let agent_id = "parked";
        start_antigravity_turn(&store, agent_id);
        if stop_first {
            stop_antigravity_turn(&store, agent_id, false);
            feed_antigravity_statusline(&store, agent_id, parked_idle());
        } else {
            feed_antigravity_statusline(&store, agent_id, parked_idle());
            stop_antigravity_turn(&store, agent_id, false);
        }
        assert_eq!(
            stored_settle(&store, agent_id),
            None,
            "stop first: {stop_first}"
        );
        assert_eq!(
            antigravity_row(&store, agent_id).effective_status(jiff::Timestamp::now()),
            rimz::agents::AgentStatus::Success,
            "stop first: {stop_first}"
        );
    }
}

#[test]
fn an_antigravity_rest_report_finishes_a_parked_turn_and_completes_its_run() {
    // Parked with a background shell, a completed native subagent, and a readable transcript.
    let (_dir, store) = hooks_test_store();
    let home = tempfile::tempdir().unwrap();
    let transcript = write_antigravity_transcript(home.path(), "parked-shell", "parked reply");
    let run = create_antigravity_child_run(&store, "parked-shell");

    start_antigravity_turn(&store, "parked-shell");
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    let mut stop = antigravity_payload("parked-shell");
    stop["fullyIdle"] = serde_json::json!(false);
    stop["terminationReason"] = serde_json::json!("model_stop");
    stop["transcriptPath"] = serde_json::json!(transcript.display().to_string());
    feed_antigravity(&store, adapter, "Stop", stop);
    park_antigravity_with_shell(&store, "parked-shell", "task-1", "sleep 60");
    let before = antigravity_row(&store, "parked-shell");
    assert_eq!(
        (before.status, before.phase, before.background_shells.len()),
        (
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Parked,
            1
        )
    );

    let env = std::collections::BTreeMap::from([
        ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);
    let frames = feed_antigravity_statusline_at(
        &store,
        "parked-shell",
        serde_json::json!({
            "agent_state": "idle",
            "subagents": [{"name": "self", "role": "worker", "status": "completed"}],
        }),
        &env,
        before.last_activity + jiff::SignedDuration::from_secs(3),
    );
    assert_eq!(frames.len(), 1);
    let after = antigravity_row(&store, "parked-shell");
    let settled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    let assistant_entries =
        rimz::transcript::read_since(store.paths(), jiff::Timestamp::from_second(0).unwrap())
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| {
                entry.entry == rimz::transcript::TranscriptKind::Assistant
                    && entry.agent_id == "parked-shell"
            })
            .count();
    assert_eq!(
        (
            after.status,
            after.phase,
            after.background_shells.is_empty(),
            settled.status,
            settled.interrupted_at,
            settled.last_message.as_deref(),
            assistant_entries,
        ),
        (
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            true,
            rimz::store::run::RunStatus::Completed,
            None,
            Some("parked reply"),
            1,
        )
    );
    assert_eq!(
        antigravity_signals(&store, "parked-shell").last(),
        Some(&LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        })
    );

    // A repeat rest report on the finished row records nothing.
    let recorded = antigravity_signals(&store, "parked-shell").len();
    assert_eq!(
        feed_antigravity_statusline_at(
            &store,
            "parked-shell",
            serde_json::json!({"agent_state": "idle"}),
            &env,
            after.last_activity + jiff::SignedDuration::from_secs(4),
        )
        .len(),
        1
    );
    assert_eq!(antigravity_signals(&store, "parked-shell").len(), recorded);
    assert_eq!(
        antigravity_row(&store, "parked-shell").last_activity,
        after.last_activity
    );

    // Parked with no listed shell (natural-finish stuck park), and with an unreadable transcript.
    for (agent_id, write_reply) in [
        ("parked-no-shell", Some("no-shell reply")),
        ("parked-no-transcript", None),
    ] {
        let (_dir, store) = hooks_test_store();
        let home = tempfile::tempdir().unwrap();
        if let Some(reply) = write_reply {
            write_antigravity_transcript(home.path(), agent_id, reply);
        }
        let run = create_antigravity_child_run(&store, agent_id);
        start_antigravity_turn(&store, agent_id);
        stop_antigravity_turn(&store, agent_id, false);
        let parked = antigravity_row(&store, agent_id);
        assert!(parked.background_shells.is_empty());

        let env = std::collections::BTreeMap::from([
            ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
            (
                "HOME".to_owned(),
                home.path().to_string_lossy().into_owned(),
            ),
        ]);
        let frames = feed_antigravity_statusline_at(
            &store,
            agent_id,
            serde_json::json!({"agent_state": "idle"}),
            &env,
            parked.last_activity + jiff::SignedDuration::from_secs(3),
        );
        assert_eq!(frames.len(), 1, "{agent_id}");
        let row = antigravity_row(&store, agent_id);
        let settled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        assert_eq!(
            (
                row.status,
                row.phase,
                row.background_shells.is_empty(),
                settled.status,
                settled.interrupted_at,
                settled.last_message.as_deref(),
            ),
            (
                rimz::agents::AgentStatus::Success,
                rimz::agents::TurnPhase::Idle,
                true,
                rimz::store::run::RunStatus::Completed,
                None,
                write_reply,
            ),
            "{agent_id}"
        );
    }

    // Mid-turn `/compact <message>` + `CHECKPOINT` followed by the parked turn's reply,
    // and a bare `/compact` after a parked turn's reply, both harvest through `rest_turn_end_assistant_message`.
    for (agent_id, lines, expected) in [
        (
            "parked-steered-compact",
            include_str!("../../agents/adapters/antigravity/tests/fixtures/compaction/steered-bg-continue-transcript.jsonl")
                .lines()
                .take(22)
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
            "DONE_FG",
        ),
        (
            "parked-bare-compact",
            include_str!("../../agents/adapters/antigravity/tests/fixtures/compaction/parked-transcript.jsonl")
                .lines()
                .take(6)
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
            "STARTED",
        ),
    ] {
        let (_dir, store) = hooks_test_store();
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(".gemini/antigravity-cli/brain")
            .join(agent_id)
            .join(".system_generated/logs/transcript.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, lines).unwrap();
        let run = create_antigravity_child_run(&store, agent_id);
        start_antigravity_turn(&store, agent_id);
        stop_antigravity_turn(&store, agent_id, false);
        let parked = antigravity_row(&store, agent_id);
        let env = std::collections::BTreeMap::from([
            ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
            (
                "HOME".to_owned(),
                home.path().to_string_lossy().into_owned(),
            ),
        ]);
        feed_antigravity_statusline_at(
            &store,
            agent_id,
            serde_json::json!({"agent_state": "idle"}),
            &env,
            parked.last_activity + jiff::SignedDuration::from_secs(3),
        );
        let settled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        assert_eq!(
            (settled.status, settled.last_message.as_deref()),
            (rimz::store::run::RunStatus::Completed, Some(expected)),
            "{agent_id}"
        );
    }
}

#[test]
fn an_antigravity_rest_report_ends_a_parked_turn_shortly_after_stop_and_after_shell_clear() {
    // Captured natural-finish sequence (probe records 453 -> 456): a parked `Stop`,
    // one Complete report 0.9 s later, and no further payload.
    let (_dir, store) = hooks_test_store();
    let home = tempfile::tempdir().unwrap();
    write_antigravity_transcript(home.path(), "short-bare", "natural finish");
    let run = create_antigravity_child_run(&store, "short-bare");
    start_antigravity_turn(&store, "short-bare");
    stop_antigravity_turn(&store, "short-bare", false);
    let before = antigravity_row(&store, "short-bare");
    let env = std::collections::BTreeMap::from([
        ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);

    let frames = feed_antigravity_statusline_at(
        &store,
        "short-bare",
        serde_json::json!({"agent_state": "idle"}),
        &env,
        before.last_activity + jiff::SignedDuration::from_millis(900),
    );
    assert_eq!(frames.len(), 1);
    let done = antigravity_row(&store, "short-bare");
    let done_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            done.status,
            done.phase,
            done_run.status,
            done_run.last_message.as_deref(),
        ),
        (
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            rimz::store::run::RunStatus::Completed,
            Some("natural finish"),
        )
    );

    // An earlier Foreground report clears shells (advancing `last_activity`) while keeping
    // the row parked; a Complete report shortly after it finishes the turn at once.
    let (_dir, store) = hooks_test_store();
    let home = tempfile::tempdir().unwrap();
    write_antigravity_transcript(home.path(), "after-clear", "after shell clear");
    let run = create_antigravity_child_run(&store, "after-clear");
    start_antigravity_turn(&store, "after-clear");
    park_antigravity_with_shell(&store, "after-clear", "task-1", "sleep 60");
    let before = antigravity_row(&store, "after-clear");
    let env = std::collections::BTreeMap::from([
        ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);

    feed_antigravity_statusline_at(
        &store,
        "after-clear",
        serde_json::json!({
            "agent_state": "idle",
            "subagents": [{"name": "self", "role": "worker", "status": "running"}],
        }),
        &env,
        before.last_activity + jiff::SignedDuration::from_millis(500),
    );
    let mid = antigravity_row(&store, "after-clear");
    let mid_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            mid.status,
            mid.phase,
            mid.background_shells.is_empty(),
            mid_run.status,
            mid_run.last_message.as_deref(),
        ),
        (
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Parked,
            true,
            rimz::store::run::RunStatus::Running,
            None,
        )
    );

    feed_antigravity_statusline_at(
        &store,
        "after-clear",
        serde_json::json!({
            "agent_state": "idle",
            "subagents": [{"name": "self", "role": "worker", "status": "completed"}],
        }),
        &env,
        mid.last_activity + jiff::SignedDuration::from_millis(500),
    );
    let done = antigravity_row(&store, "after-clear");
    let done_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            done.status,
            done.phase,
            done_run.status,
            done_run.last_message.as_deref(),
        ),
        (
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            rimz::store::run::RunStatus::Completed,
            Some("after shell clear"),
        )
    );

    // A bare `/compact` on the parked turn closes with `CompactionEnded`; a Complete report
    // shortly after finishes the turn and preserves the pre-compaction reply as the answer.
    let (_dir, store) = hooks_test_store();
    let home = tempfile::tempdir().unwrap();
    let fixture = include_str!(
        "../../agents/adapters/antigravity/tests/fixtures/compaction/parked-transcript.jsonl"
    );
    let through_compact: String = fixture
        .lines()
        .take(6)
        .map(|line| format!("{line}\n"))
        .collect();
    let transcript = home
        .path()
        .join(".gemini/antigravity-cli/brain")
        .join("after-compact")
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, &through_compact).unwrap();
    let run = create_antigravity_child_run(&store, "after-compact");
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    start_antigravity_turn(&store, "after-compact");
    park_antigravity_with_shell(&store, "after-compact", "task-2", "sleep 150 && echo SLEPT");
    let mut compact = antigravity_payload("after-compact");
    compact["lastUserInput"] = serde_json::json!("/compact");
    compact["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, adapter, "PreInvocation", compact.clone());
    compact["fullyIdle"] = serde_json::json!(false);
    compact["terminationReason"] = serde_json::json!("TERMINAL_CUSTOM_HOOK");
    feed_antigravity(&store, adapter, "Stop", compact);
    let compacted = antigravity_row(&store, "after-compact");
    assert_eq!(
        (compacted.status, compacted.phase),
        (
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Parked
        )
    );
    let env = std::collections::BTreeMap::from([
        ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);
    feed_antigravity_statusline_at(
        &store,
        "after-compact",
        serde_json::json!({"agent_state": "idle"}),
        &env,
        compacted.last_activity + jiff::SignedDuration::from_millis(500),
    );
    let done = antigravity_row(&store, "after-compact");
    let done_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            done.status,
            done.phase,
            done.background_shells.is_empty(),
            done_run.status,
            done_run.last_message.as_deref(),
        ),
        (
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            true,
            rimz::store::run::RunStatus::Completed,
            Some("STARTED"),
        )
    );
}

#[test]
fn an_antigravity_rest_interrupt_leaves_supervised_run_last_message_unset() {
    let (_dir, store) = hooks_test_store();
    let home = tempfile::tempdir().unwrap();
    write_antigravity_transcript(home.path(), "esc-child", "partial reply");
    let run = create_antigravity_child_run(&store, "esc-child");
    start_antigravity_turn(&store, "esc-child");

    let env = std::collections::BTreeMap::from([
        ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);
    let frames = feed_antigravity_statusline_in(
        &store,
        "esc-child",
        serde_json::json!({"agent_state": "idle"}),
        &env,
    );
    assert_eq!(frames.len(), 1);
    let row = antigravity_row(&store, "esc-child");
    let held = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            row.status,
            held.status,
            held.interrupted_at,
            held.last_message.as_deref(),
        ),
        (
            rimz::agents::AgentStatus::Idle,
            rimz::store::run::RunStatus::Running,
            Some(frames[0].ts),
            None,
        ),
        "a Complete rest report interrupting an open turn must not store a transcript reply"
    );
}

#[test]
fn an_antigravity_rest_report_keeps_a_parked_turn_open_while_a_native_subagent_is_live() {
    for status in ["running", "pending"] {
        let (_dir, store) = hooks_test_store();
        let run = create_antigravity_child_run(&store, "parked-live-sub");
        start_antigravity_turn(&store, "parked-live-sub");
        park_antigravity_with_shell(&store, "parked-live-sub", "task-1", "sleep 60");
        let before = antigravity_row(&store, "parked-live-sub");
        let env =
            std::collections::BTreeMap::from([("RIMZ_RUN_ID".to_owned(), run.run_id.to_string())]);
        let payload = serde_json::json!({
            "agent_state": "idle",
            "subagents": [{"name": "self", "role": "worker", "status": status}],
        });

        let frames = feed_antigravity_statusline_at(
            &store,
            "parked-live-sub",
            payload.clone(),
            &env,
            before.last_activity + jiff::SignedDuration::from_secs(4),
        );
        assert_eq!(frames.len(), 1, "{status}");
        let after = antigravity_row(&store, "parked-live-sub");
        let open_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        assert_eq!(
            (
                after.status,
                after.phase,
                after.background_shells.is_empty(),
                open_run.status,
                open_run.interrupted_at,
            ),
            (
                rimz::agents::AgentStatus::Running,
                rimz::agents::TurnPhase::Parked,
                true,
                rimz::store::run::RunStatus::Running,
                None,
            ),
            "{status}: listed shells clear while parked row and run stay open"
        );

        // Once shells are empty, a repeat payload with a live subagent records nothing.
        let recorded = antigravity_signals(&store, "parked-live-sub").len();
        assert_eq!(
            feed_antigravity_statusline_at(
                &store,
                "parked-live-sub",
                payload,
                &env,
                after.last_activity + jiff::SignedDuration::from_secs(4),
            )
            .len(),
            1,
            "{status}"
        );
        assert_eq!(
            antigravity_signals(&store, "parked-live-sub").len(),
            recorded,
            "{status}"
        );
        assert_eq!(
            antigravity_row(&store, "parked-live-sub").last_activity,
            after.last_activity,
            "{status}"
        );
    }

    // Parked with no shell and a running subagent records nothing at all.
    let (_dir, store) = hooks_test_store();
    let run = create_antigravity_child_run(&store, "parked-only-sub");
    start_antigravity_turn(&store, "parked-only-sub");
    stop_antigravity_turn(&store, "parked-only-sub", false);
    let before = antigravity_row(&store, "parked-only-sub");
    let recorded = antigravity_signals(&store, "parked-only-sub").len();
    let env =
        std::collections::BTreeMap::from([("RIMZ_RUN_ID".to_owned(), run.run_id.to_string())]);
    feed_antigravity_statusline_at(
        &store,
        "parked-only-sub",
        serde_json::json!({
            "agent_state": "idle",
            "subagents": [{"name": "self", "role": "worker", "status": "running"}],
        }),
        &env,
        before.last_activity + jiff::SignedDuration::from_secs(4),
    );
    let after = antigravity_row(&store, "parked-only-sub");
    let open_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            after.status,
            after.phase,
            open_run.status,
            antigravity_signals(&store, "parked-only-sub").len(),
        ),
        (
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Parked,
            rimz::store::run::RunStatus::Running,
            recorded,
        )
    );
}

#[test]
fn an_antigravity_rest_report_keeps_an_owed_continue_owed_when_finishing_a_parked_turn() {
    let (_dir, store) = hooks_test_store();
    let home = tempfile::tempdir().unwrap();
    write_antigravity_transcript(home.path(), "parked-owed", "before compact");
    let run = create_antigravity_child_run(&store, "parked-owed");

    start_antigravity_turn(&store, "parked-owed");
    queue_antigravity_continue(&store, "parked-owed");
    park_antigravity_with_shell(&store, "parked-owed", "task-1", "sleep 60");
    let before = antigravity_row(&store, "parked-owed");
    assert!(antigravity_continue_owed(&store, "parked-owed"));

    let env = std::collections::BTreeMap::from([
        ("RIMZ_RUN_ID".to_owned(), run.run_id.to_string()),
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);
    let frames = feed_antigravity_statusline_at(
        &store,
        "parked-owed",
        serde_json::json!({"agent_state": "idle"}),
        &env,
        before.last_activity + jiff::SignedDuration::from_secs(4),
    );
    assert_eq!(frames.len(), 1);
    let after = antigravity_row(&store, "parked-owed");
    let parked_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (
            after.status,
            after.phase,
            after.background_shells.is_empty(),
            antigravity_continue_owed(&store, "parked-owed"),
            parked_run.status,
            parked_run.parked_on,
            parked_run.interrupted_at,
            parked_run.last_message.as_deref(),
        ),
        (
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            true,
            true,
            rimz::store::run::RunStatus::Running,
            Some(rimz::store::run::ParkCause::SoftCompact),
            None,
            Some("before compact"),
        )
    );
    assert!(
        !antigravity_signals(&store, "parked-owed")
            .iter()
            .any(|signal| matches!(signal, LifecycleSignal::TurnInterrupted { .. })),
        "parked rest records no TurnInterrupted"
    );
}

#[test]
fn an_antigravity_rest_report_interrupts_an_open_turn_even_with_a_running_subagent() {
    let (_dir, store) = hooks_test_store();
    let run = create_antigravity_child_run(&store, "open-sub");
    start_antigravity_turn(&store, "open-sub");

    let env =
        std::collections::BTreeMap::from([("RIMZ_RUN_ID".to_owned(), run.run_id.to_string())]);
    let frames = feed_antigravity_statusline_in(
        &store,
        "open-sub",
        serde_json::json!({
            "agent_state": "idle",
            "subagents": [{"name": "self", "role": "worker", "status": "running"}],
        }),
        &env,
    );
    assert_eq!(frames.len(), 1);
    let row = antigravity_row(&store, "open-sub");
    let held = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(
        (row.status, held.status, held.interrupted_at),
        (
            rimz::agents::AgentStatus::Idle,
            rimz::store::run::RunStatus::Running,
            Some(frames[0].ts),
        )
    );
}

#[test]
fn an_antigravity_rest_report_for_a_newer_conversation_leaves_an_older_parked_turn_alone() {
    let (_dir, store) = hooks_test_store();
    let run = create_antigravity_child_run(&store, "older-parked");
    start_antigravity_turn(&store, "older-parked");
    park_antigravity_with_shell(&store, "older-parked", "task-1", "sleep 60");
    let before = antigravity_row(&store, "older-parked");

    // A newer conversation in the same pane (first unseen, then started) receives idle.
    for started in [false, true] {
        let newer = if started {
            "newer-started"
        } else {
            "newer-unseen"
        };
        if started {
            start_antigravity_turn(&store, newer);
        }
        feed_antigravity_statusline_at(
            &store,
            newer,
            serde_json::json!({"agent_state": "idle"}),
            &std::collections::BTreeMap::new(),
            before.last_activity + jiff::SignedDuration::from_secs(4),
        );
        let older = store
            .agent_with_context(
                &rimz::ids::AgentKind::new_unchecked("antigravity"),
                &AgentSessionId::from("older-parked"),
                rimz::RuntimeScope::Audit,
            )
            .unwrap()
            .unwrap();
        let open_run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        assert_eq!(
            (
                older.status,
                older.phase,
                older.background_shells.len(),
                older.last_activity,
                open_run.status,
            ),
            (
                rimz::agents::AgentStatus::Running,
                rimz::agents::TurnPhase::Parked,
                1,
                before.last_activity,
                rimz::store::run::RunStatus::Running,
            ),
            "started: {started}"
        );
    }
}

#[test]
fn a_stale_antigravity_rest_report_leaves_background_shells_intact() {
    let (_dir, store) = hooks_test_store();
    start_antigravity_turn(&store, "stale-shell");
    let (payload, observation) =
        observe_antigravity_statusline("stale-shell", serde_json::json!({"agent_state": "idle"}));
    std::thread::sleep(std::time::Duration::from_millis(2));
    park_antigravity_with_shell(&store, "stale-shell", "task-1", "sleep 60");
    let before = antigravity_row(&store, "stale-shell");
    let signals = antigravity_signals(&store, "stale-shell");

    let from = ingress_len(&store);
    persist_antigravity_statusline(
        &store,
        &payload,
        observation,
        &std::collections::BTreeMap::new(),
    );
    assert_eq!(apply_antigravity_frames_from(&store, from).len(), 1);
    let after = antigravity_row(&store, "stale-shell");
    assert_eq!(after.background_shells, before.background_shells);
    assert_eq!(antigravity_signals(&store, "stale-shell"), signals);
    assert_eq!(
        (after.status, after.phase, after.last_activity),
        (before.status, before.phase, before.last_activity)
    );
}

#[test]
fn an_antigravity_rest_report_interrupts_an_open_turn_and_empties_its_background_shells() {
    let (_dir, store) = hooks_test_store();
    start_antigravity_turn(&store, "open-with-shell");
    park_antigravity_with_shell(&store, "open-with-shell", "task-1", "sleep 60");
    start_antigravity_turn(&store, "open-with-shell");
    let before = antigravity_row(&store, "open-with-shell");
    assert_eq!(
        (before.status, before.phase, before.background_shells.len()),
        (
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            1
        )
    );

    let frames = feed_antigravity_statusline(
        &store,
        "open-with-shell",
        serde_json::json!({"agent_state": "idle"}),
    );
    assert_eq!(frames.len(), 1);
    let after = antigravity_row(&store, "open-with-shell");
    assert_eq!(after.status, rimz::agents::AgentStatus::Idle);
    assert!(
        after.background_shells.is_empty(),
        "interrupting rest also empties listed background shells"
    );
    assert_eq!(
        antigravity_signals(&store, "open-with-shell").last(),
        Some(&LifecycleSignal::TurnInterrupted { turn_id: None })
    );
}

#[test]
fn correlated_antigravity_children_keep_independent_lifecycle_and_root_parent() {
    let (_dir, store) = hooks_test_store();
    let provider = Box::leak(Box::new(CorrelationTestAdapter::default()));
    let adapter = hook_definition(&*provider);
    let mut root = antigravity_payload("root");
    root["invocationNum"] = serde_json::json!(0);
    root["modelName"] = serde_json::json!("parent-model");
    feed_antigravity(&store, &adapter, "PreInvocation", root);

    let mut clean = antigravity_payload("child-clean");
    clean["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", clean.clone());
    let calls_after_start = provider.correlation_calls.load(Ordering::Relaxed);
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == "child-clean")
        .cloned()
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("root"));
    assert_eq!(child.name.as_deref(), Some("name-child-clean"));
    assert_eq!(child.role.as_deref(), Some("role-child-clean"));
    assert_eq!(child.task.as_deref(), Some("task-child-clean"));
    assert_eq!(child.prompt.as_deref(), Some("prompt-child-clean"));
    assert_eq!(child.status, rimz::agents::AgentStatus::Running);
    assert!(child.profile.is_none() && child.team.is_none());
    assert!(child.model.is_none() && child.effort.is_none());

    feed_antigravity(&store, &adapter, "PostToolUse:observed", clean.clone());
    let mut clean_stop = clean;
    clean_stop["fullyIdle"] = serde_json::json!(true);
    clean_stop["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", clean_stop);
    assert_eq!(
        provider.correlation_calls.load(Ordering::Relaxed),
        calls_after_start,
        "a persisted child relation must skip later transcript correlation"
    );
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == "child-clean")
        .cloned()
        .unwrap();
    assert_eq!(child.status, rimz::agents::AgentStatus::Success);
    assert_eq!(child.parent_agent_id.as_deref(), Some("root"));

    let mut failed = antigravity_payload("child-failed");
    failed["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", failed.clone());
    failed["fullyIdle"] = serde_json::json!(true);
    failed["terminationReason"] = serde_json::json!("max_steps_exceeded");
    feed_antigravity(&store, &adapter, "Stop", failed);

    let mut parked = antigravity_payload("child-parked");
    parked["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", parked.clone());
    parked["fullyIdle"] = serde_json::json!(false);
    parked["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", parked.clone());
    let parked_state = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == "child-parked")
        .cloned()
        .unwrap();
    assert_eq!(parked_state.status, rimz::agents::AgentStatus::Running);
    assert_eq!(parked_state.phase, rimz::agents::TurnPhase::Parked);
    parked["fullyIdle"] = serde_json::json!(true);
    feed_antigravity(&store, &adapter, "Stop", parked);

    let mut recovered = antigravity_payload("child-recovered");
    recovered["fullyIdle"] = serde_json::json!(true);
    recovered["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", recovered);

    let mut nested = antigravity_payload("nested-child");
    nested["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", nested);

    let mut uncorrelated = antigravity_payload("unrelated-root");
    uncorrelated["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", uncorrelated);

    let agents = store.snapshot_cached().unwrap().agents;
    let state = |id: &str| agents.iter().find(|state| state.agent_id == id).unwrap();
    assert_eq!(
        state("child-failed").status,
        rimz::agents::AgentStatus::Failed
    );
    assert_eq!(
        state("child-parked").status,
        rimz::agents::AgentStatus::Success
    );
    assert_eq!(
        state("child-recovered").status,
        rimz::agents::AgentStatus::Success
    );
    assert_eq!(
        state("child-recovered").parent_agent_id.as_deref(),
        Some("root")
    );
    assert_eq!(
        state("nested-child").parent_agent_id.as_deref(),
        Some("root")
    );
    assert_eq!(state("unrelated-root").parent_agent_id, None);
}

#[test]
fn a_prompt_taken_in_mid_turn_confirms_its_steer_without_opening_a_turn() {
    use rimz::store::message::{DeliveryGate, MessageRecord};
    let (_dir, store) = hooks_test_store();
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    let mut opener = antigravity_payload("steered");
    opener["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, adapter, "PreInvocation", opener.clone());
    let row = || {
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "steered")
            .cloned()
            .unwrap()
    };
    let opened = row();
    let steer = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &opened,
        "Also include STEERAPPLE.".to_owned(),
        DeliveryGate::Done,
    );
    store.queue_message(&steer, "hooks-test").unwrap();
    store
        .record_sent_batch(std::slice::from_ref(&steer), "hooks-test")
        .unwrap();

    // What the adapter reports from the PostInvocation of the invocation that took it in.
    let mut decoded = adapter.decode_hook("PreInvocation", &opener).unwrap();
    decoded.update_lifecycle(|observation| {
        observation.state_snapshot = Some(true);
        observation.prompt = rimz::agents::SanitizedPrompt::new(Some("Also include STEERAPPLE."));
    });
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        &store,
        adapter,
        &mut decoded,
        &opener,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();

    assert!(
        store.list_messages().unwrap().is_empty(),
        "the steer is confirmed delivered"
    );
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let recorded: Vec<_> = entries
        .iter()
        .filter(|entry| entry.message_id.as_ref() == Some(&steer.message_id))
        .collect();
    assert_eq!(recorded.len(), 1, "{entries:?}");
    assert_eq!(recorded[0].text, "Also include STEERAPPLE.");
    let steered = row();
    assert_eq!(steered.status, rimz::agents::AgentStatus::Running);
    assert_eq!(
        steered.turn_started_at, opened.turn_started_at,
        "a steer joins the running turn"
    );
}

/// Live run 1, case 3: agy took two steers in during one invocation, and the person typed a
/// third prompt among them. Each RimZ record is confirmed and recorded once, in the order agy
/// took them in, and the person's text is recorded as direct input.
#[test]
fn prompts_taken_in_by_one_invocation_each_confirm_their_steer() {
    use rimz::store::message::{DeliveryGate, MessageRecord};
    let (_dir, store) = hooks_test_store();
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    let mut opener = antigravity_payload("steered");
    opener["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, adapter, "PreInvocation", opener.clone());
    let opened = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == "steered")
        .cloned()
        .unwrap();
    let steers = [
        "Also include RZLIVE3TA.",
        "Also include RZLIVE3TB.",
        "Also include RZLIVE3TC.",
    ]
    .map(|text| {
        let steer = MessageRecord::new(
            store.paths().workspace_id.clone(),
            &opened,
            text.to_owned(),
            DeliveryGate::Done,
        );
        store.queue_message(&steer, "hooks-test").unwrap();
        store
            .record_sent_batch(std::slice::from_ref(&steer), "hooks-test")
            .unwrap();
        steer
    });
    let mut decoded = adapter.decode_hook("PreInvocation", &opener).unwrap();
    decoded.update_lifecycle(|observation| {
        observation.state_snapshot = Some(true);
        observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
            "Also include RZLIVE3TA.\n\nAnd keep it short.\n\nAlso include RZLIVE3TB.\n\nAlso include RZLIVE3TC.",
        ));
    });
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        &store,
        adapter,
        &mut decoded,
        &opener,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();

    assert!(
        store.list_messages().unwrap().is_empty(),
        "every steer is confirmed delivered: {:?}",
        store.list_messages().unwrap()
    );
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let inputs = entries
        .iter()
        .filter(|entry| entry.agent_id == "steered" && entry.text != "steered")
        .map(|entry| (entry.text.as_str(), entry.message_id.as_ref()))
        .collect::<Vec<_>>();
    assert_eq!(
        inputs,
        [
            ("Also include RZLIVE3TA.", Some(&steers[0].message_id)),
            ("And keep it short.", None),
            ("Also include RZLIVE3TB.", Some(&steers[1].message_id)),
            ("Also include RZLIVE3TC.", Some(&steers[2].message_id)),
        ],
        "{entries:?}"
    );
}

#[test]
fn copilot_child_metadata_reconciles_at_the_parent_checkpoint() {
    let (_store_dir, store) = hooks_test_store();
    let transcript_dir = tempfile::tempdir().unwrap();
    let parent_dir = transcript_dir.path().join("parent-session");
    std::fs::create_dir(&parent_dir).unwrap();
    let transcript = parent_dir.join("events.jsonl");
    let records = include_str!("../../agents/adapters/copilot/tests/fixtures/subagents.jsonl")
        .lines()
        .collect::<Vec<_>>();
    std::fs::write(&transcript, format!("{}\n{}\n", records[0], records[1])).unwrap();
    feed_copilot(
        &store,
        "sessionStart",
        serde_json::json!({
            "sessionId":"parent-session",
            "source":"startup",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        }),
    );
    feed_copilot(
        &store,
        "userPromptSubmitted",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "prompt":"delegate this",
            "transcriptPath":transcript,
        }),
    );
    feed_copilot(
        &store,
        "userPromptSubmitted",
        serde_json::json!({
            "sessionId":"toolu_alpha",
            "cwd":"/tmp/hooks-test",
            "prompt":"Trace the retry flow",
            "transcriptPath":"",
        }),
    );
    feed_copilot(
        &store,
        "agentStop",
        serde_json::json!({
            "sessionId":"toolu_alpha",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":"",
        }),
    );

    let child_before_completion = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == "toolu_alpha")
        .cloned()
        .unwrap();
    assert_eq!(
        child_before_completion.parent_agent_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(child_before_completion.name.as_deref(), Some("researcher"));
    assert_eq!(
        child_before_completion.task.as_deref(),
        Some("Inspect auth retry")
    );
    assert_eq!(
        child_before_completion.model.as_deref(),
        Some("claude-haiku-4.5")
    );
    assert_eq!(child_before_completion.usage.total_tokens, None);
    assert_eq!(
        child_before_completion.status,
        rimz::agents::AgentStatus::Success
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    use std::io::Write as _;
    writeln!(file, "{}", records[2]).unwrap();
    feed_copilot(
        &store,
        "postToolUse",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
            "toolName":"task",
        }),
    );

    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|state| state.agent_id == "toolu_alpha")
        .cloned()
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent-session"));
    assert_eq!(child.name.as_deref(), Some("researcher"));
    assert_eq!(child.task.as_deref(), Some("Inspect auth retry"));
    assert_eq!(child.model.as_deref(), Some("claude-haiku-4.5"));
    assert_eq!(child.usage.run_total_tokens, Some(22_116));
    assert_eq!(child.usage.total_tokens, None);
    assert_eq!(child.status, rimz::agents::AgentStatus::Success);

    let reconciliation_count = || {
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    rimz::store::event::EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("SubagentReconciled")
                )
            })
            .count()
    };
    assert_eq!(reconciliation_count(), 1);
    feed_copilot(
        &store,
        "postToolUse",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        }),
    );
    feed_copilot(
        &store,
        "agentStop",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        }),
    );
    assert_eq!(
        reconciliation_count(),
        1,
        "repeated root checkpoints are idempotent"
    );

    let signals = store
        .read_events()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload.observation.agent_id.as_deref() == Some("toolu_alpha") =>
            {
                Some(payload.observation.signal)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        signals,
        vec![
            LifecycleSignal::SubagentStarted,
            LifecycleSignal::SubagentStopped { errored: false },
            LifecycleSignal::SubagentStopped { errored: false },
        ]
    );
}

#[test]
fn copilot_uuid_child_hooks_join_through_the_start_record_agent_id() {
    let (_store_dir, store) = hooks_test_store();
    let transcript_dir = tempfile::tempdir().unwrap();
    let parent_dir = transcript_dir.path().join("parent-session");
    std::fs::create_dir(&parent_dir).unwrap();
    let transcript = parent_dir.join("events.jsonl");
    std::fs::write(
        &transcript,
        include_str!("../../agents/adapters/copilot/tests/fixtures/subagents-agent-id.jsonl"),
    )
    .unwrap();
    let child_id = "6f1c2e0a-1111-4a5b-9c3d-000000000001";
    let parent = |event: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        feed_copilot(&store, event, payload);
    };
    let child = |event: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "sessionId":child_id,
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        feed_copilot(&store, event, payload);
    };
    let state = |agent_id: &str| {
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == agent_id)
            .cloned()
    };

    parent("sessionStart", serde_json::json!({"source":"new"}));
    parent(
        "userPromptSubmitted",
        serde_json::json!({"prompt":"delegate"}),
    );
    child(
        "userPromptSubmitted",
        serde_json::json!({"prompt":"View README.md and report its first line"}),
    );
    // Copilot 1.0.83 raises the child's permission prompt on the parent
    // session, narrowing the call's arguments to `toolInput.command`.
    parent(
        "permissionRequest",
        serde_json::json!({"hookName":"permissionRequest","toolName":"bash","toolInput":{"command":"ls"},"permissionSuggestions":[]}),
    );
    assert_eq!(
        state("parent-session").unwrap().status,
        rimz::agents::AgentStatus::Waiting
    );
    child(
        "postToolUse",
        serde_json::json!({"toolName":"bash","toolArgs":{"command":"pwd","mode":"sync"}}),
    );
    assert_eq!(
        state("parent-session").unwrap().status,
        rimz::agents::AgentStatus::Waiting,
        "a different child call must not clear the parent's ask"
    );
    child(
        "postToolUse",
        serde_json::json!({"toolName":"bash","toolArgs":{"command":"ls","description":"List files","initial_wait":30,"mode":"sync"}}),
    );
    let answered = state("parent-session").unwrap();
    assert_eq!(answered.status, rimz::agents::AgentStatus::Running);
    assert!(answered.open_ask.is_none());
    assert!(!answered.is_awaiting_input());
    child("agentStop", serde_json::json!({"stopReason":"end_turn"}));

    let joined = state(child_id).expect("the UUID child joins its parent");
    assert_eq!(joined.parent_agent_id.as_deref(), Some("parent-session"));
    assert_eq!(joined.name.as_deref(), Some("readme-first-line"));
    assert_eq!(joined.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(joined.status, rimz::agents::AgentStatus::Success);
    assert!(
        state("call_alpha").is_none(),
        "the task call id must not surface as a phantom child"
    );

    parent("postToolUse", serde_json::json!({"toolName":"task"}));
    let reconciled = state(child_id).unwrap();
    assert_eq!(reconciled.usage.run_total_tokens, Some(11_444));
    assert_eq!(reconciled.usage.total_tokens, None);
    assert!(state("call_alpha").is_none());
    assert_eq!(
        state("parent-session").unwrap().status,
        rimz::agents::AgentStatus::Running
    );
}

#[test]
fn pi_bridge_adopts_an_existing_rich_root_with_the_live_signal() {
    let (_dir, store) = hooks_test_store();
    let adapter = hook_definition(Box::leak(Box::new(PiAdoptionTestAdapter {
        pane_id: id("terminal_pi"),
    })));
    feed_pi(
        &store,
        &adapter,
        "session_start",
        pi_session_payload("parent-session"),
    );
    feed_pi(
        &store,
        &adapter,
        "session_start",
        pi_session_payload("child-session"),
    );
    feed_pi(
        &store,
        &adapter,
        "subagent_started",
        pi_bridge_payload("parent-session", "child-session"),
    );

    assert_eq!(
        adoption_signals(&store),
        vec![LifecycleSignal::SubagentStarted]
    );
    let snapshot = store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "child-session")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent-session"));
    assert_eq!(child.status, rimz::agents::AgentStatus::Running);
    assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(child.effort.as_deref(), Some("xhigh"));
    assert_eq!(child.usage.context_pct, Some(12));
    assert_eq!(child.usage.total_tokens, Some(24_000));
}

#[test]
fn pi_compaction_failure_without_open_bracket_preserves_the_turn() {
    let (_dir, store) = hooks_test_store();
    let adapter = rimz::agents::definition_by_kind("pi").unwrap();
    feed_pi(
        &store,
        adapter,
        "session_start",
        pi_session_payload("compact-session"),
    );
    feed_pi(
        &store,
        adapter,
        "before_agent_start",
        serde_json::json!({
            "session_id": "compact-session", "prompt": "continue"
        }),
    );
    let running = store.snapshot_cached().unwrap();
    let before = running
        .agents
        .iter()
        .find(|state| state.agent_id == "compact-session")
        .unwrap();
    assert_eq!(before.status, rimz::agents::AgentStatus::Running);
    assert!(before.turn_started_at.is_some());

    feed_pi(
        &store,
        adapter,
        "session_compact_failed",
        serde_json::json!({
            "session_id": "compact-session", "compaction_reason": "overflow"
        }),
    );
    let failed = store.snapshot_cached().unwrap();
    let after = failed
        .agents
        .iter()
        .find(|state| state.agent_id == "compact-session")
        .unwrap();
    assert_eq!(after.status, rimz::agents::AgentStatus::Running);
    assert!(after.compacting_since.is_none());
    assert_eq!(after.compaction_count, 0);
    assert_eq!(after.turn_started_at, before.turn_started_at);
    assert_eq!(after.turn_ended_at, before.turn_ended_at);
}

#[test]
fn pi_compaction_failure_closes_the_active_bracket() {
    let (_dir, store) = hooks_test_store();
    let adapter = rimz::agents::definition_by_kind("pi").unwrap();
    feed_pi(
        &store,
        adapter,
        "session_start",
        pi_session_payload("compact-session"),
    );
    feed_pi(
        &store,
        adapter,
        "before_agent_start",
        serde_json::json!({ "session_id": "compact-session", "prompt": "continue" }),
    );
    feed_pi(
        &store,
        adapter,
        "session_before_compact",
        serde_json::json!({ "session_id": "compact-session", "compaction_reason": "manual" }),
    );

    let compacting = store.snapshot_cached().unwrap();
    let agent = compacting
        .agents
        .iter()
        .find(|state| state.agent_id == "compact-session")
        .unwrap();
    assert_eq!(agent.status, rimz::agents::AgentStatus::Running);
    assert!(agent.compacting_since.is_some());

    feed_pi(
        &store,
        adapter,
        "session_compact_failed",
        serde_json::json!({
            "session_id": "compact-session",
            "compaction_reason": "manual",
        }),
    );

    let failed = store.snapshot_cached().unwrap();
    let agent = failed
        .agents
        .iter()
        .find(|state| state.agent_id == "compact-session")
        .unwrap();
    assert_eq!(agent.status, rimz::agents::AgentStatus::Running);
    assert!(agent.compacting_since.is_none());
    assert_eq!(agent.compaction_count, 0);
}

#[test]
fn pi_bridge_adoption_preserves_parented_and_foreign_pane_roots() {
    let parent_adapter = hook_definition(Box::leak(Box::new(PiAdoptionTestAdapter {
        pane_id: id("terminal_parent"),
    })));

    let (_dir, parented_store) = hooks_test_store();
    let mut parented = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("parented-child")),
        LifecycleSignal::SubagentStarted,
    );
    parented.parent_agent_id = Some(AgentSessionId::from("original-parent"));
    parented.task = Some("existing child".to_owned());
    parented.pane_id = Some(id("terminal_parent"));
    parented_store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "hooks-test",
            agent_kind: rimz::ids::AgentKind::new_unchecked("pi"),
            event_name: "seed-parented",
            observation: &parented,
            spawned_subagents: &[],
        })
        .unwrap();
    feed_pi(
        &parented_store,
        &parent_adapter,
        "subagent_started",
        pi_bridge_payload("new-parent", "parented-child"),
    );
    let snapshot = parented_store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "parented-child")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("original-parent"));
    assert!(adoption_signals(&parented_store).is_empty());

    let (_dir, foreign_store) = hooks_test_store();
    let foreign_adapter = hook_definition(Box::leak(Box::new(PiAdoptionTestAdapter {
        pane_id: id("terminal_foreign"),
    })));
    feed_pi(
        &foreign_store,
        &parent_adapter,
        "session_start",
        pi_session_payload("parent-session"),
    );
    feed_pi(
        &foreign_store,
        &foreign_adapter,
        "session_start",
        pi_session_payload("foreign-child"),
    );
    feed_pi(
        &foreign_store,
        &parent_adapter,
        "subagent_started",
        pi_bridge_payload("parent-session", "foreign-child"),
    );
    let snapshot = foreign_store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "foreign-child")
        .unwrap();
    assert_eq!(child.parent_agent_id, None);
    assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
    assert!(adoption_signals(&foreign_store).is_empty());
}

#[test]
fn antigravity_parent_stop_adopts_children_after_late_transcript_flush() {
    let (_dir, store) = hooks_test_store();
    let adapter = hook_definition(Box::leak(Box::new(CorrelationTestAdapter::default())));
    let mut root = antigravity_payload("late-root");
    root["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", root.clone());

    let mut clean = antigravity_payload("late-child-clean");
    clean["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", clean.clone());
    clean["fullyIdle"] = serde_json::json!(true);
    clean["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", clean);

    let mut failed = antigravity_payload("late-child-failed");
    failed["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", failed.clone());
    failed["fullyIdle"] = serde_json::json!(true);
    failed["terminationReason"] = serde_json::json!("max_steps_exceeded");
    feed_antigravity(&store, &adapter, "Stop", failed);

    let before = store.snapshot_cached().unwrap().agents;
    let failed = before
        .iter()
        .find(|state| state.agent_id == "late-child-failed")
        .unwrap();
    assert_eq!(failed.parent_agent_id, None);
    assert_ne!(failed.name.as_deref(), Some("failed-name"));

    root["fullyIdle"] = serde_json::json!(true);
    root["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", root.clone());

    let agents = store.snapshot_cached().unwrap().agents;
    let clean = agents
        .iter()
        .find(|state| state.agent_id == "late-child-clean")
        .unwrap();
    assert_eq!(clean.parent_agent_id.as_deref(), Some("late-root"));
    assert_eq!(clean.status, rimz::agents::AgentStatus::Success);
    assert_eq!(clean.name.as_deref(), Some("clean-name"));
    assert_eq!(clean.role.as_deref(), Some("clean-role"));
    assert_eq!(clean.task.as_deref(), Some("clean-role"));
    assert_eq!(clean.prompt.as_deref(), Some("clean prompt"));
    let failed = agents
        .iter()
        .find(|state| state.agent_id == "late-child-failed")
        .unwrap();
    assert_eq!(failed.parent_agent_id.as_deref(), Some("late-root"));
    assert_eq!(failed.status, rimz::agents::AgentStatus::Failed);
    assert_eq!(failed.name.as_deref(), Some("failed-name"));
    assert_eq!(failed.role.as_deref(), Some("failed-role"));

    let adoption_count = || {
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    rimz::store::event::EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("SubagentAdopted")
                )
            })
            .count()
    };
    assert_eq!(adoption_count(), 2);
    feed_antigravity(&store, &adapter, "Stop", root);
    assert_eq!(adoption_count(), 2, "a repeated parent Stop is idempotent");
}

#[test]
fn antigravity_parent_stop_rematerializes_a_reaped_child() {
    let (_dir, store) = hooks_test_store();
    let adapter = hook_definition(Box::leak(Box::new(CorrelationTestAdapter::default())));
    let mut root = antigravity_payload("reaped-root");
    root["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", root.clone());

    let mut child = antigravity_payload("late-child-reaped");
    child["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", child.clone());
    child["fullyIdle"] = serde_json::json!(true);
    child["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", child);
    let ended = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("late-child-reaped")),
        LifecycleSignal::Ended,
    );
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "hooks-test",
            agent_kind: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event_name: "ReapedSuperseded",
            observation: &ended,
            spawned_subagents: &[],
        })
        .unwrap();
    assert!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .all(|state| state.agent_id != "late-child-reaped")
    );

    root["fullyIdle"] = serde_json::json!(true);
    root["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", root);

    let snapshot = store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "late-child-reaped")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("reaped-root"));
    assert_eq!(child.status, rimz::agents::AgentStatus::Success);
    assert_eq!(child.name.as_deref(), Some("reaped-name"));
    assert_eq!(child.role.as_deref(), Some("reaped-role"));
    assert_eq!(child.task.as_deref(), Some("reaped-role"));
}

#[test]
fn ambiguous_and_cyclic_antigravity_parent_candidates_stay_roots() {
    let (_dir, store) = hooks_test_store();
    let adapter = hook_definition(Box::leak(Box::new(CorrelationTestAdapter::default())));
    for root_id in ["root", "other-root"] {
        let mut payload = antigravity_payload(root_id);
        payload["invocationNum"] = serde_json::json!(0);
        feed_antigravity(&store, &adapter, "PreInvocation", payload);
    }
    seed_subagent_candidate(&store, "ambiguous-parent-a", "root");
    seed_subagent_candidate(&store, "ambiguous-parent-b", "root");
    let mut ambiguous = antigravity_payload("ambiguous-child");
    ambiguous["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", ambiguous);
    assert_eq!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "ambiguous-child")
            .unwrap()
            .parent_agent_id,
        None
    );

    seed_subagent_candidate(&store, "cycle-parent", "cycle-child");
    let mut cyclic = antigravity_payload("cycle-child");
    cyclic["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", cyclic);
    assert_eq!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "cycle-child")
            .unwrap()
            .parent_agent_id,
        None
    );
}

#[test]
fn recovered_binding_stamps_full_pane_and_reowns_to_in_pane_agent_process() {
    let mut observation = root_observation();
    let mut pane = candidate("terminal_30", true);
    pane.view_id = Some("tab_4".to_owned());
    pane.cwd = Some("/repo/main".to_owned());
    pane.pane_pid = Some(1234);
    let child_pid = 5678;

    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "sess-1",
        pane.clone(),
        |root_pid| {
            assert_eq!(root_pid, 1234);
            Some(child_pid)
        },
    );

    assert_eq!(observation.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(observation.pane_stamp.as_ref(), Some(&pane));
    let owner = observation.runtime_owner.as_ref().expect("runtime owner");
    assert_eq!(owner.kind, RuntimeOwnerKind::Agent);
    assert_eq!(owner.pid, child_pid);
}

#[test]
fn recovered_binding_preserves_prior_owner_when_agent_process_is_unknown() {
    let mut observation = root_observation();
    let existing_owner = process_owner(RuntimeOwnerKind::Daemon, "sess-1", std::process::id());
    observation.runtime_owner = Some(existing_owner.clone());
    let mut pane = candidate("terminal_30", true);
    pane.view_id = Some("tab_4".to_owned());
    pane.cwd = Some("/repo/main".to_owned());
    pane.pane_pid = Some(1234);

    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "sess-1",
        pane.clone(),
        |root_pid| {
            assert_eq!(root_pid, 1234);
            None
        },
    );

    assert_eq!(observation.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(observation.pane_stamp.as_ref(), Some(&pane));
    assert_eq!(observation.runtime_owner.as_ref(), Some(&existing_owner));
}

#[test]
fn ingress_accepts_camelcase_field_and_dispatches_the_canonical_event() {
    let payload = serde_json::json!({
        "hookEventName": "session_start",
        "sessionId": "session-1"
    });
    let classified = rimz::agents::definition_by_kind("grok")
        .unwrap()
        .decode_hook("session_start", &payload)
        .unwrap();
    assert_eq!(classified.event_name(), "SessionStart");
    assert_eq!(classified.class(), rimz::agents::AgentHookClass::Lifecycle);

    let explicit = rimz::agents::definition_by_kind("grok")
        .unwrap()
        .decode_hook("post_tool_use", &payload)
        .unwrap();
    assert_eq!(explicit.event_name(), "PostToolUse");
}

// Capabilities this test double has no behavior for.
impl rimz::agents::capabilities::InstallationCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::LaunchCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::SessionCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::TranscriptCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::ContextCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::AccountCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::SpendingCapability for CorrelationTestAdapter {}
impl rimz::agents::capabilities::RuntimeControlCapability for CorrelationTestAdapter {}

// Capabilities this test double has no behavior for.
impl rimz::agents::capabilities::InstallationCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::LaunchCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::SessionCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::TranscriptCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::ContextCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::AccountCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::SpendingCapability for CopilotCorrelationAdapter {}
impl rimz::agents::capabilities::RuntimeControlCapability for CopilotCorrelationAdapter {}

// Capabilities this test double has no behavior for.
impl rimz::agents::capabilities::InstallationCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::LaunchCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::SessionCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::TranscriptCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::ContextCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::AccountCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::SpendingCapability for PiAdoptionTestAdapter {}
impl rimz::agents::capabilities::RuntimeControlCapability for PiAdoptionTestAdapter {}

#[test]
fn the_apply_pool_cap_is_twice_the_cores_plus_one_up_to_32() {
    use super::pool_cap;

    assert_eq!(pool_cap(1), 3);
    assert_eq!(pool_cap(4), 9);
    assert_eq!(pool_cap(15), 31);
    assert_eq!(pool_cap(16), 32);
    assert_eq!(pool_cap(512), 32);
}

#[test]
fn a_stream_reply_line_answers_only_its_own_frame() {
    use super::{Exchange, reply_line};
    use rimz::agents::HookReply;

    assert_eq!(
        reply_line(br#"{"ingress":"a"}"#, "a"),
        Exchange::Reply(HookReply::Silent)
    );
    assert_eq!(
        reply_line(br#"{"ingress":"a","reply":null}"#, "a"),
        Exchange::Reply(HookReply::Json(serde_json::Value::Null))
    );
    assert_eq!(
        reply_line(br#"{"ingress":"a","reply":{"x":1},"extra":2}"#, "a"),
        Exchange::Reply(HookReply::Json(serde_json::json!({"x": 1})))
    );
    assert_eq!(reply_line(br#"{"ingress":"b"}"#, "a"), Exchange::Garbled);
    assert_eq!(reply_line(b"not json", "a"), Exchange::Garbled);
}

#[test]
fn an_antigravity_quota_stop_fails_its_run_with_the_limit_on_apply_and_replay() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};

    let payload: serde_json::Value = serde_json::from_str(include_str!(
        "../../agents/adapters/antigravity/tests/fixtures/quota_stop.json"
    ))
    .unwrap();
    let agent_id = payload["conversationId"].as_str().unwrap();
    let stop_at: jiff::Timestamp = "2026-11-25T18:22:48.276839Z".parse().unwrap();
    let adapter = rimz::agents::definition_by_kind("antigravity").unwrap();
    for replay in [false, true] {
        let (_dir, store) = hooks_test_store();
        let workspace = hooks_test_workspace(Some("main"));
        let run = create_antigravity_child_run(&store, agent_id);
        let frame = rimz::store::ingress::HookIngress {
            boot_id: None,
            schema_version: "1".into(),
            event_id: rimz::EventId::new(),
            ts: stop_at,
            source: adapter.spec().kind_id(),
            event: Some("Stop".into()),
            payload: payload.to_string(),
            cwd: workspace.project_root.clone(),
            hook_pid: std::process::id(),
            env: std::collections::BTreeMap::from([(
                rimz::harness::launch::ENV_RUN_ID.to_owned(),
                run.run_id.to_string(),
            )]),
        };
        let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
        let mut decoded = adapter
            .decode_hook_in("Stop", &payload, &frame.env)
            .unwrap();
        if replay {
            // The first attempt appended the lifecycle and died before the run fold.
            scoped
                .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                    session_name: &workspace.session_name,
                    agent_kind: adapter.spec().kind_id(),
                    event_name: "Stop",
                    observation: decoded.lifecycle().unwrap(),
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        let recovered = store.read_events().unwrap();
        let attempt = if replay {
            FrameAttempt::Replay
        } else {
            FrameAttempt::First
        };
        with_frame_env(&frame, attempt, || {
            super::lifecycle::handle_lifecycle_frame(
                &workspace,
                &scoped,
                adapter,
                &mut decoded,
                &payload,
                rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                &hooks_test_globals(),
                &recovered,
            )
        })
        .unwrap();
        let settled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        let refill_at = stop_at + std::time::Duration::from_secs(438_294);
        let label = "Individual quota reached. Please upgrade your subscription to increase your \
                     limits.";
        assert_eq!(
            (settled.status, settled.limit_failure, settled.failure_tail),
            (
                rimz::store::run::RunStatus::Failed,
                Some(rimz::store::run::LimitFailure {
                    cause: rimz::store::run::LimitFailureCause::RefillKnown,
                    refill_at: Some(refill_at),
                    label: Some(label.to_owned()),
                }),
                Some(format!(
                    "stopped on a provider limit: \"{label}\"; it refills at {}",
                    rimz::utils::time::format_local_timestamp(refill_at)
                )),
            ),
            "replay={replay}"
        );
    }
}
