use super::*;

#[derive(Debug, Clone, Copy)]
enum IdleStopRun {
    None,
    Child,
    Team,
    Peer,
}

fn idle_stop_turn(
    prompt: &str,
    sender: Option<rimz::store::message::MessageSender>,
    frame_offset: i64,
    run: IdleStopRun,
    replay: bool,
    withdraws: bool,
) {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::store::idle_stop::{self, IdleStopRequest};
    let dir = tempfile::tempdir().unwrap();
    let workspace = workspace();
    let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let id = rimz::ids::AgentSessionId::from(rimz::ids::EventId::new().to_string());
    let mut state = rimz::testkit::agent_state("claude", id.as_str(), jiff::Timestamp::UNIX_EPOCH);
    if matches!(run, IdleStopRun::Team | IdleStopRun::Peer) {
        store
            .append_event(&rimz::store::event::EventEnvelope::agent_launched(
                workspace_id(),
                &workspace.session_name,
                &state.kind,
                rimz::store::event::AgentLaunchPayload {
                    system_prompt_digest: None,
                    agent_id: id.clone(),
                    launch_id: Some(id.clone()),
                    agent_name: "idle-seat".into(),
                    agent_name_explicit: true,
                    launch: rimz::agents::LaunchParams {
                        launched_by: Some(Box::new(rimz::agents::LaunchedBy {
                            kind: rimz::ids::AgentKind::new_unchecked("codex"),
                            agent_id: "launcher-id".into(),
                        })),
                        team: matches!(run, IdleStopRun::Team).then(|| "forge".into()),
                        ..Default::default()
                    },
                    state: rimz::store::event::AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(workspace.worktree_root.to_string_lossy().into_owned()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .unwrap();
        state = agent_state(&store, agent, &id).unwrap();
        let record = rimz::harness::run::create_peer_prompt(
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
        assert_eq!(
            super::super::session_run_id(&store, agent, Some(&id)),
            Some(record.run_id)
        );
    }
    if let Some(sender) = sender {
        let mut message = rimz::store::message::MessageRecord::new(
            workspace_id(),
            &state,
            rimz::store::message::parse_message_header(prompt)
                .map(|(_, _, body)| body)
                .unwrap_or_else(|| prompt.into()),
            rimz::store::message::DeliveryGate::Done,
        )
        .with_sender(sender);
        message.status = rimz::store::message::MessageStatus::Sent;
        if prompt == "/compact" {
            message = message.with_body(rimz::store::message::MessageBody::Command);
        }
        store
            .queue_message(&message, &workspace.session_name)
            .unwrap();
    }
    let at = jiff::Timestamp::from_second(1_000).unwrap();
    idle_stop::arm(
        store.paths(),
        IdleStopRequest {
            kind: state.kind.clone(),
            agent_id: id.clone(),
            stop: rimz::agents::state::IdleStop {
                after_secs: 300,
                requested_at: at,
                requested_by: Some("@lead".into()),
            },
        },
    )
    .unwrap();
    let before = std::fs::read(&store.paths().idle_stop_requests).unwrap();
    let payload = serde_json::json!({"session_id": id, "prompt": prompt});
    let mut frame = rimz::store::ingress::HookIngress {
        schema_version: "1".into(),
        event_id: rimz::ids::EventId::new(),
        ts: jiff::Timestamp::from_second(1_000 + frame_offset).unwrap(),
        source: state.kind.clone(),
        event: Some("UserPromptSubmit".into()),
        payload: payload.to_string(),
        cwd: workspace.project_root.clone(),
        hook_pid: std::process::id(),
        env: Default::default(),
    };
    if matches!(run, IdleStopRun::Child) {
        frame.env.insert(
            rimz::harness::launch::ENV_RUN_ID.into(),
            rimz::ids::RunId::new().to_string(),
        );
    }
    let mut decoded = agent.decode_hook("UserPromptSubmit", &payload).unwrap();
    if replay {
        store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: state.kind.clone(),
                event_name: "UserPromptSubmit",
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
    for _ in 0..if withdraws { 2 } else { 1 } {
        with_frame_env(&frame, attempt, || {
            super::super::handle_lifecycle_frame(
                &workspace,
                &store,
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
        })
        .unwrap();
        decoded = agent.decode_hook("UserPromptSubmit", &payload).unwrap();
    }
    assert_eq!(
        idle_stop::read(store.paths()).is_empty(),
        withdraws,
        "{prompt}, {frame_offset}, run={run:?}, replay={replay}"
    );
    if !withdraws {
        assert_eq!(
            std::fs::read(&store.paths().idle_stop_requests).unwrap(),
            before
        );
    }
    let assists = rimz::harness::assist_log::recent(&rimz::disk::paths::logs_dir(), None)
        .into_iter()
        .map(|record| serde_json::to_value(record).unwrap())
        .filter(|record| {
            record["assist"] == "idle_stop_withdrawn" && record["agent_id"] == id.as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(assists.len(), usize::from(withdraws));
    if withdraws {
        assert_eq!(assists[0]["idle_after_secs"], 300);
        assert_eq!(assists[0]["requested_by"], "@lead");
        assert_eq!(assists[0]["requested_at"], at.to_string());
    }
    if prompt.starts_with("<task-notification>") || !matches!(run, IdleStopRun::None) {
        assert!(
            rimz::agents::spending::user_input::load_in(user_input_state_root(&store).unwrap())
                .is_empty()
        );
    }
}

#[test]
fn typed_human_turn_withdraws_idle_stop() {
    idle_stop_turn("keep working", None, 1, IdleStopRun::None, false, true);
}

#[test]
fn human_turn_on_team_run_withdraws_idle_stop_without_spend_input() {
    idle_stop_turn("keep working", None, 1, IdleStopRun::Team, false, true);
}

#[test]
fn human_turn_on_peer_run_withdraws_idle_stop_without_spend_input() {
    idle_stop_turn("keep working", None, 1, IdleStopRun::Peer, false, true);
}

#[test]
fn user_shell_message_withdraws_idle_stop() {
    idle_stop_turn(
        "Type: USER_MESSAGE\nFrom: @user\nContent:\nkeep working",
        Some(rimz::store::message::MessageSender::Human),
        1,
        IdleStopRun::None,
        false,
        true,
    );
}

#[test]
fn failed_idle_stop_withdrawal_keeps_the_rest_of_the_frame() {
    use std::os::unix::fs::PermissionsExt;

    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::store::idle_stop::{self, IdleStopRequest};
    let dir = tempfile::tempdir().unwrap();
    let workspace = workspace();
    let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let id = rimz::ids::AgentSessionId::from(rimz::ids::EventId::new().to_string());
    let kind = rimz::ids::AgentKind::new_unchecked("claude");
    idle_stop::arm(
        store.paths(),
        IdleStopRequest {
            kind: kind.clone(),
            agent_id: id.clone(),
            stop: rimz::agents::state::IdleStop {
                after_secs: 300,
                requested_at: jiff::Timestamp::from_second(1_000).unwrap(),
                requested_by: Some("@lead".into()),
            },
        },
    )
    .unwrap();
    let before = std::fs::read(&store.paths().idle_stop_requests).unwrap();
    // A read-only records directory fails the withdrawal's publish and nothing
    // else this frame writes: the transcript lives under the audit class.
    let records = store
        .paths()
        .idle_stop_requests
        .parent()
        .unwrap()
        .to_owned();
    std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o500)).unwrap();
    // Permissions do not bind root, so there is nothing to prove.
    if std::fs::write(records.join("probe"), b"").is_ok() {
        return;
    }
    let payload = serde_json::json!({"session_id": id, "prompt": "keep working"});
    let frame = rimz::store::ingress::HookIngress {
        schema_version: "1".into(),
        event_id: rimz::ids::EventId::new(),
        ts: jiff::Timestamp::from_second(1_001).unwrap(),
        source: kind,
        event: Some("UserPromptSubmit".into()),
        payload: payload.to_string(),
        cwd: workspace.project_root.clone(),
        hook_pid: std::process::id(),
        env: Default::default(),
    };
    let mut decoded = agent.decode_hook("UserPromptSubmit", &payload).unwrap();
    let recovered = store.read_events().unwrap();
    let handled = with_frame_env(&frame, FrameAttempt::First, || {
        super::super::handle_lifecycle_frame(
            &workspace,
            &store,
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
    });
    std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o700)).unwrap();
    handled.unwrap();

    assert_eq!(
        std::fs::read(&store.paths().idle_stop_requests).unwrap(),
        before
    );
    assert!(
        !rimz::harness::assist_log::recent(&rimz::disk::paths::logs_dir(), None)
            .into_iter()
            .map(|record| serde_json::to_value(record).unwrap())
            .any(|record| record["assist"] == "idle_stop_withdrawn"
                && record["agent_id"] == id.as_str())
    );
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert!(
        entries.iter().any(|entry| {
            entry.entry == rimz::transcript::TranscriptKind::Prompt && entry.text == "keep working"
        }),
        "{entries:?}"
    );
}

#[test]
fn earlier_tied_supervised_and_replayed_turns_keep_idle_stop() {
    for (offset, run, replay) in [
        (-1, IdleStopRun::None, false),
        (0, IdleStopRun::None, false),
        (1, IdleStopRun::Child, false),
        (1, IdleStopRun::None, true),
    ] {
        idle_stop_turn("keep working", None, offset, run, replay, false);
    }
}

#[test]
fn rimz_deliveries_and_background_notices_keep_idle_stop() {
    use rimz::store::message::{HarnessNotice, MessageSender};
    for (notice, header) in [
        (HarnessNotice::Wait, "WAIT"),
        (HarnessNotice::Signal, "SIGNAL"),
        (HarnessNotice::Stage, "STAGE"),
        (HarnessNotice::SubagentReport, "SUBAGENT_REPORT"),
    ] {
        idle_stop_turn(
            &format!("Type: {header}\nFrom: @rimz\nContent:\na delivery"),
            Some(MessageSender::Harness { notice }),
            1,
            IdleStopRun::None,
            false,
            false,
        );
    }
    idle_stop_turn(
        "Type: AGENT_MESSAGE\nFrom: @codex\nContent:\nan agent message",
        Some(MessageSender::Agent {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            agent_id: None,
            name: None,
            profile: None,
            role: None,
            channel: None,
        }),
        1,
        IdleStopRun::None,
        false,
        false,
    );
    idle_stop_turn(
        "--no-from text",
        Some(MessageSender::System),
        1,
        IdleStopRun::None,
        false,
        false,
    );
    idle_stop_turn(
        "/compact",
        Some(MessageSender::System),
        1,
        IdleStopRun::None,
        false,
        false,
    );
    idle_stop_turn(
        "<task-notification><task-id>b1</task-id><status>completed</status></task-notification>",
        None,
        1,
        IdleStopRun::None,
        false,
        false,
    );
}

#[test]
fn registered_checkpoint_spawns_delivery_for_the_queued_head() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = workspace();
    let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = rimz::store::message::MessageRecord::new(
        workspace_id(),
        &state,
        "follow up".into(),
        rimz::store::message::DeliveryGate::Done,
    );
    store
        .queue_message(&message, &workspace.session_name)
        .unwrap();
    let receipt = store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: state.kind.clone(),
            event_name: "SessionStart",
            observation: &AgentLifecycleObservation::new(
                Some(state.agent_id.clone()),
                LifecycleSignal::Registered,
            ),
            spawned_subagents: &[],
        })
        .unwrap();
    let mut spawns = Vec::new();
    spawn_queue_delivery_with(&workspace, &store, &receipt.events[0], |spawn| {
        spawns.push(spawn.args.clone())
    });
    assert_eq!(
        spawns,
        vec![vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "message".to_owned(),
            "deliver".to_owned(),
            "--message-id".to_owned(),
            message.message_id.to_string()
        ]]
    );
}

fn record_user_input_for_lifecycle(
    workspace: &ResolvedWorkspace,
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
    delivered: &[rimz::store::message::MessageRecord],
    supervised: bool,
    state_root: Option<&std::path::Path>,
) {
    let sections = rimz::store::message::classify_submitted_prompt(
        recorded
            .observation
            .prompt
            .as_deref()
            .unwrap_or("human prompt"),
        &delivered.iter().collect::<Vec<_>>(),
        &[],
    );
    super::record_user_input_for_lifecycle(
        workspace,
        agent,
        recorded,
        rimz::store::message::human_opened_turn(
            &recorded.observation,
            &Value::Null,
            &sections,
            delivered,
            supervised,
        ),
        state_root,
    );
}

fn workspace_id() -> rimz::ids::WorkspaceId {
    rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"))
}

fn workspace() -> ResolvedWorkspace {
    ResolvedWorkspace {
        workspace_id: workspace_id(),
        project_root: std::path::PathBuf::from("/tmp/hooks-test"),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
        worktree_branch: None,
        session_name: "hooks-test".to_owned(),
        mux_hint: None,
    }
}

fn turn_started() -> RecordedLifecycle {
    let mut observation = AgentLifecycleObservation::new(
        Some(rimz::ids::AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    observation.worktree_path = Some("/tmp/hooks-test/worktree".to_owned());
    RecordedLifecycle {
        model_hint: None,
        observation,
        receipt: Default::default(),
    }
}

#[test]
fn claude_wrapped_harness_delivery_does_not_record_user_input() {
    use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};

    let dir = tempfile::tempdir().unwrap();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = MessageRecord::new(
        workspace_id(),
        &state,
        "Implement is yours.".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Stage,
    });
    let payload = serde_json::json!({
        "session_id": "sess-1",
        "prompt": format!("<pasted_content id=\"e676\">\nType: STAGE\nFrom: @rimz\nContent:\n{}\n</pasted_content id=\"e676\">", message.text),
    });
    let decoded = agent.decode_hook("UserPromptSubmit", &payload).unwrap();
    let mut started = turn_started();
    started.observation.prompt = decoded.lifecycle().unwrap().prompt.clone();

    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &started,
        &[message],
        false,
        Some(dir.path()),
    );

    assert!(rimz::agents::spending::user_input::load_in(dir.path()).is_empty());
}

#[test]
fn turn_started_records_only_unsupervised_user_inputs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let agent_state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::now());
    let human = rimz::store::message::MessageRecord::new(
        workspace_id(),
        &agent_state,
        "human prompt".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    );
    let agent_message = human
        .clone()
        .with_sender(rimz::store::message::MessageSender::Agent {
            agent_id: None,
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: Some("coder".to_owned()),
            channel: None,
        });

    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        &[],
        false,
        Some(dir.path()),
    );
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        std::slice::from_ref(&human),
        false,
        Some(dir.path()),
    );
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        std::slice::from_ref(&agent_message),
        false,
        Some(dir.path()),
    );
    let mut mixed = turn_started();
    mixed.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
        "Type: AGENT_MESSAGE\nFrom: @coder\nContent:\nhuman prompttyped directly",
    ));
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &mixed,
        std::slice::from_ref(&agent_message),
        false,
        Some(dir.path()),
    );
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        std::slice::from_ref(&human),
        true,
        Some(dir.path()),
    );
    for excluded in [
        human.clone().with_automated(true),
        rimz::store::message::MessageRecord {
            gate: rimz::store::message::DeliveryGate::Resume,
            ..human.clone()
        },
    ] {
        record_user_input_for_lifecycle(
            &workspace,
            agent,
            &turn_started(),
            &[excluded],
            false,
            Some(dir.path()),
        );
    }
    // A turn start whose prompt the adapter could not read classifies into no
    // sections at all — antigravity with an unreadable transcript, a plugin
    // that omits the optional prompt. What it delivered decides: nothing
    // confirmed leaves the turn the human's, while a confirmed agent or
    // harness record is proof that something else opened it.
    for (delivered, opens_the_window) in [
        (Vec::new(), true),
        (vec![agent_message.clone()], false),
        (vec![human.clone()], true),
    ] {
        let before = rimz::agents::spending::user_input::load_in(dir.path()).len();
        super::record_user_input_for_lifecycle(
            &workspace,
            agent,
            &turn_started(),
            rimz::store::message::human_opened_turn(
                &turn_started().observation,
                &Value::Null,
                &[],
                &delivered,
                false,
            ),
            Some(dir.path()),
        );
        let after = rimz::agents::spending::user_input::load_in(dir.path()).len();
        assert_eq!(after > before, opens_the_window, "delivered: {delivered:?}");
    }

    let records = rimz::agents::spending::user_input::load_in(dir.path());
    assert_eq!(records.len(), 5);
    assert!(
        records
            .iter()
            .all(|record| record.kind.as_str() == "claude")
    );
    assert!(records.iter().all(|record| {
        record.origin.as_deref() == Some(std::path::Path::new("/tmp/hooks-test/worktree"))
    }));
}
