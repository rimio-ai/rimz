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
        boot_id: None,
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
        "keep working",
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
        boot_id: None,
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
    spawn_queue_delivery_with(&workspace, &store, &receipt.events[0], None, |spawn| {
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

#[test]
fn a_textless_turn_start_after_a_swap_leaves_the_replaced_sessions_prompt_to_retry() {
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageStatus};

    let dir = tempfile::tempdir().unwrap();
    let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let sent_at = jiff::Timestamp::now() - std::time::Duration::from_secs(3600);
    let mut old = rimz::testkit::agent_state("claude", "sess-old", sent_at);
    old.name = Some("coder".to_owned());
    old.launch_id = Some("launch_coder".into());
    let sent = |text: &str, sent_epoch: Option<u32>| {
        let mut message =
            MessageRecord::new(workspace_id(), &old, text.to_owned(), DeliveryGate::Done);
        message.status = MessageStatus::Sent;
        message.last_sent_at = Some(sent_at);
        message.sent_epoch = sent_epoch;
        store.queue_message(&message, "hooks-test").unwrap();
        message
    };
    let ack = |session: &str, epoch: Option<u32>| {
        let mut started = RecordedLifecycle {
            model_hint: None,
            observation: AgentLifecycleObservation::new(
                Some(session.into()),
                LifecycleSignal::TurnStarted { turn_id: None },
            ),
            receipt: Default::default(),
        };
        started.observation.agent_name = Some("coder".to_owned());
        started.observation.launch_id = Some("launch_coder".into());
        started.observation.launch.epoch = epoch;
        confirm_sent_message_for_lifecycle(&store, agent, &started, &workspace)
    };

    let replaced = sent("typed into the old session", Some(0));
    assert!(
        ack("sess-new", Some(1)).is_empty(),
        "the holder's text-less turn start confirmed a prompt typed into the session it replaced"
    );
    let report = store
        .reconcile_stale_messages("hooks-test", jiff::Timestamp::now(), 3)
        .unwrap();
    assert_eq!(report.requeued, 1);
    let mut holder = old.clone();
    holder.agent_id = "sess-new".into();
    holder.epoch = 1;
    let requeued = store
        .list_messages()
        .unwrap()
        .into_iter()
        .find(|message| message.message_id == replaced.message_id)
        .expect("the prompt stays live");
    assert_eq!(requeued.status, MessageStatus::Queued);
    assert!(requeued.same_agent_card(&holder));

    let same_epoch = sent("typed into this session", Some(1));
    assert_eq!(
        ack("sess-new", Some(1))
            .iter()
            .map(|message| &message.message_id)
            .collect::<Vec<_>>(),
        vec![&same_epoch.message_id]
    );
    let legacy = sent("an older binary's write", None);
    assert_eq!(
        ack("sess-new", Some(1))
            .iter()
            .map(|message| &message.message_id)
            .collect::<Vec<_>>(),
        vec![&legacy.message_id]
    );
}

#[test]
fn a_molt_to_another_kind_confirms_and_wakes_the_records_queued_for_the_agent() {
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageStatus};

    let dir = tempfile::tempdir().unwrap();
    let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let workspace = workspace();
    let codex = rimz::agents::definition_by_kind("codex").unwrap();
    let mut old = rimz::testkit::agent_state("claude", "sess-old", jiff::Timestamp::now());
    old.name = Some("coder".to_owned());
    old.launch_id = Some("launch_coder".into());
    let mut sent = MessageRecord::new(
        workspace_id(),
        &old,
        "typed into the holder".to_owned(),
        DeliveryGate::Done,
    );
    sent.status = MessageStatus::Sent;
    sent.last_sent_at = Some(jiff::Timestamp::now());
    sent.sent_epoch = Some(1);
    store.queue_message(&sent, "hooks-test").unwrap();
    let queued = MessageRecord::new(
        workspace_id(),
        &old,
        "parked for later".to_owned(),
        DeliveryGate::Done,
    );
    store.queue_message(&queued, "hooks-test").unwrap();

    let mut started = RecordedLifecycle {
        model_hint: None,
        observation: AgentLifecycleObservation::new(
            Some("sess-new".into()),
            LifecycleSignal::TurnStarted { turn_id: None },
        ),
        receipt: Default::default(),
    };
    started.observation.agent_name = Some("molted-name".to_owned());
    started.observation.launch_id = Some("launch_coder".into());
    started.observation.launch.epoch = Some(1);
    let ids = |records: Vec<MessageRecord>| {
        records
            .into_iter()
            .map(|record| record.message_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(in_flight_messages_for_lifecycle(&store, codex, &started)),
        vec![sent.message_id.clone()]
    );
    assert_eq!(
        ids(confirm_sent_message_for_lifecycle(
            &store, codex, &started, &workspace
        )),
        vec![sent.message_id.clone()]
    );

    let mut registered =
        AgentLifecycleObservation::new(Some("sess-new".into()), LifecycleSignal::Registered);
    registered.launch_id = Some("launch_coder".into());
    let receipt = store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: codex.spec().kind_id(),
            event_name: "SessionStart",
            observation: &registered,
            spawned_subagents: &[],
        })
        .unwrap();
    let mut spawns = Vec::new();
    spawn_queue_delivery_with(
        &workspace,
        &store,
        &receipt.events[0],
        registered.launch_id.as_ref(),
        |spawn| spawns.push(spawn.args.last().cloned()),
    );
    assert_eq!(spawns, vec![Some(queued.message_id.to_string())]);
}

#[test]
fn triggered_compaction_end_acknowledges_sent_command_and_echo_or_failure_does_not() {
    use rimz::store::message::{DeliveryGate, MessageBody, MessageRecord, MessageStatus};

    let cases = [
        ("auto true", Some(true), false, false, true),
        ("auto false", Some(false), false, false, true),
        ("triggerless echo", None, false, false, false),
        ("flagged echo", Some(true), false, true, false),
        ("failed close", Some(true), true, false, false),
    ];
    let agent = rimz::agents::definition_by_kind("antigravity").unwrap();
    let workspace = workspace();
    for (label, auto, failed, echo, should_confirm) in cases {
        let dir = tempfile::tempdir().unwrap();
        let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
        let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        let state = rimz::testkit::agent_state("antigravity", "sess-1", jiff::Timestamp::now());
        let mut command = MessageRecord::new(
            workspace_id(),
            &state,
            "/compact".to_owned(),
            DeliveryGate::Done,
        )
        .with_body(MessageBody::Command);
        command.status = MessageStatus::Sent;
        command.last_sent_at = Some(jiff::Timestamp::now());
        store
            .queue_message(&command, &workspace.session_name)
            .unwrap();

        let recorded = RecordedLifecycle {
            model_hint: None,
            observation: AgentLifecycleObservation::new(
                Some(state.agent_id.clone()),
                LifecycleSignal::CompactionEnded { auto, failed, echo },
            ),
            receipt: Default::default(),
        };
        let delivered = confirm_sent_message_for_lifecycle(&store, agent, &recorded, &workspace);
        assert_eq!(
            delivered.len(),
            usize::from(should_confirm),
            "{label}: delivered={delivered:?}"
        );
        let still_live = store
            .list_messages()
            .unwrap()
            .into_iter()
            .any(|msg| msg.message_id == command.message_id);
        assert_eq!(still_live, !should_confirm, "{label}");
    }
}

#[test]
fn in_turn_compaction_close_with_prompt_delivers_tail_acknowledges_command_and_writes_harness_transcript()
 {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::store::message::{
        DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, SoftCompactStep,
    };

    let agent = rimz::agents::definition_by_kind("antigravity").unwrap();
    let workspace = workspace();

    for requeued_tail in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
        let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        let session_id = "55e06aa1-53b9-4aac-ae73-3d46b4948532";
        let state = rimz::testkit::agent_state("antigravity", session_id, jiff::Timestamp::now());
        store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: state.kind.clone(),
                event_name: "PreInvocation",
                observation: &AgentLifecycleObservation::new(
                    Some(state.agent_id.clone()),
                    LifecycleSignal::TurnStarted { turn_id: None },
                ),
                spawned_subagents: &[],
            })
            .unwrap();

        let mut command = MessageRecord::new(
            workspace_id(),
            &state,
            "/compact".to_owned(),
            DeliveryGate::Done,
        )
        .with_body(MessageBody::Command)
        .with_sender(MessageSender::System)
        .with_automated(true);
        let mut tail = MessageRecord::new(
            workspace_id(),
            &state,
            "continue".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::System)
        .with_automated(true);
        tail.follows_command = Some(command.message_id.clone());
        tail.soft_compact = Some(SoftCompactStep::Continue);
        let tail = store
            .queue_message_with_tail(&command, &tail, &workspace.session_name)
            .unwrap();
        command.status = MessageStatus::Sent;
        command.last_sent_at = Some(jiff::Timestamp::now());
        let mut tail_sent = tail.clone();
        tail_sent.status = MessageStatus::Sent;
        tail_sent.last_sent_at = Some(jiff::Timestamp::now());
        store
            .record_sent_batch(
                &[command.clone(), tail_sent.clone()],
                &workspace.session_name,
            )
            .unwrap();
        if requeued_tail {
            // Simulate reconciliation having re-queued the tail as awaiting late ack before the
            // hook landed: the close must deliver the tail before compaction ack runs queued
            // premise withdrawal.
            let mut requeued = tail.clone();
            requeued.status = MessageStatus::Queued;
            requeued.unconfirmed_sends = 1;
            requeued.last_sent_at = Some(jiff::Timestamp::now());
            store
                .queue_message(&requeued, &workspace.session_name)
                .unwrap();
        }

        let agy_home = dir.path().join("agy-home");
        let transcript_dir = agy_home
            .join("brain")
            .join(session_id)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let transcript_path = transcript_dir.join("transcript_full.jsonl");
        let transcript = [
            serde_json::json!({
                "step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-24T22:10:00Z",
                "content": "<USER_REQUEST>\nstart\n</USER_REQUEST>"
            }),
            serde_json::json!({
                "step_index": 1, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-24T22:10:14Z",
                "content": "<USER_REQUEST>\n/compact continue\n</USER_REQUEST>"
            }),
            serde_json::json!({
                "step_index": 2, "source": "SYSTEM", "type": "CHECKPOINT",
                "status": "DONE", "created_at": "2026-11-24T22:10:19Z",
                "content": "# Resuming from a compaction"
            }),
            serde_json::json!({
                "step_index": 3, "source": "MODEL", "type": "PLANNER_RESPONSE",
                "status": "DONE", "created_at": "2026-11-24T22:10:27Z",
                "content": ""
            }),
        ]
        .into_iter()
        .map(|row| row.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(&transcript_path, transcript).unwrap();
        let payload = serde_json::json!({
            "conversationId": session_id,
            "workspacePaths": [workspace.project_root],
            "transcriptPath": transcript_path,
            "invocationNum": 1,
            "initialNumSteps": 1,
            "lastUserInput": "/compact continue"
        });
        let login_env = std::collections::BTreeMap::from([(
            "RIMZ_ANTIGRAVITY_HOME".to_owned(),
            agy_home.to_string_lossy().into_owned(),
        )]);
        let mut decoded = agent
            .decode_hook_in("PostInvocation", &payload, &login_env)
            .unwrap();

        let frame = rimz::store::ingress::HookIngress {
            boot_id: None,
            schema_version: "1".into(),
            event_id: rimz::ids::EventId::new(),
            ts: jiff::Timestamp::now(),
            source: state.kind.clone(),
            event: Some("PostInvocation".into()),
            payload: payload.to_string(),
            cwd: workspace.project_root.clone(),
            hook_pid: std::process::id(),
            env: Default::default(),
        };
        let recovered = store.read_events().unwrap();
        with_frame_env(&frame, FrameAttempt::First, || {
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
            .unwrap();
        });

        assert!(
            store.list_messages().unwrap().is_empty(),
            "requeued_tail={requeued_tail}: both command and tail should leave live queue"
        );
        let history = store.list_message_history().unwrap();
        let cmd_done = history
            .iter()
            .find(|msg| msg.message_id == command.message_id)
            .expect("command in history");
        let tail_done = history
            .iter()
            .find(|msg| msg.message_id == tail.message_id)
            .expect("tail in history");
        assert_eq!(
            cmd_done.status,
            MessageStatus::Delivered,
            "requeued_tail={requeued_tail}"
        );
        assert_eq!(
            tail_done.status,
            MessageStatus::Delivered,
            "requeued_tail={requeued_tail}: tail must be delivered, not withdrawn ({:?})",
            tail_done.last_error
        );

        let entries = rimz::transcript::read_since(store.paths(), jiff::Timestamp::UNIX_EPOCH)
            .unwrap()
            .entries;
        let prompt_entries: Vec<_> = entries
            .iter()
            .filter(|entry| entry.text == "continue")
            .collect();
        assert_eq!(
            prompt_entries.len(),
            1,
            "requeued_tail={requeued_tail}: expected one transcript entry: {entries:?}"
        );
        assert_eq!(
            prompt_entries[0].origin(),
            rimz::transcript::EntryOrigin::Harness,
            "requeued_tail={requeued_tail}"
        );
        assert_eq!(
            prompt_entries[0].message_id.as_ref(),
            Some(&tail.message_id),
            "requeued_tail={requeued_tail}"
        );

        let row = agent_state(&store, agent, &state.agent_id).expect("row");
        assert_eq!(row.compaction_count, 1, "requeued_tail={requeued_tail}");
        assert_eq!(
            row.compacted_awaiting_prompt, None,
            "requeued_tail={requeued_tail}"
        );
    }
}

#[test]
fn mid_turn_harness_prompt_preserves_open_turn_and_shows_final_reply_in_all_text_views() {
    use rimz::harness::hook_drain::{FrameAttempt, with_frame_env};
    use rimz::store::message::{
        DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, SoftCompactStep,
    };

    let agent = rimz::agents::definition_by_kind("antigravity").unwrap();
    let workspace = workspace();
    let globals = crate::cli::GlobalFlags {
        root: None,
        mux: None,
        zellij: false,
        tmux: false,
        color: crate::cli::ColorWhen::Never,
    };
    let apply = |store: &Store, event_name: &str, payload: &serde_json::Value, mut decoded| {
        let frame = rimz::store::ingress::HookIngress {
            boot_id: None,
            schema_version: "1".into(),
            event_id: rimz::ids::EventId::new(),
            ts: jiff::Timestamp::now(),
            source: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event: Some(event_name.into()),
            payload: payload.to_string(),
            cwd: workspace.project_root.clone(),
            hook_pid: std::process::id(),
            env: Default::default(),
        };
        let recovered = store.read_events().unwrap();
        with_frame_env(&frame, FrameAttempt::First, || {
            super::super::handle_lifecycle_frame(
                &workspace,
                store,
                agent,
                &mut decoded,
                payload,
                rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
                &globals,
                &recovered,
            )
            .unwrap();
        });
    };

    #[derive(Clone, Copy)]
    enum MidTurnCase {
        CompactionContinue,
        ReminderSnapshot,
    }

    for recorded_opener in [false, true] {
        for case in [
            MidTurnCase::CompactionContinue,
            MidTurnCase::ReminderSnapshot,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
            let runtime =
                rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
            let store = Store::open(paths, runtime).unwrap();
            let session_id = "20f06582-0fde-4cc2-9245-18a4bfd509f0";
            let state =
                rimz::testkit::agent_state("antigravity", session_id, jiff::Timestamp::now());
            let opener_text = "Run all steps and reply ALL_STEPS_DONE.";
            let opener_msg = recorded_opener.then(|| {
                let mut msg = MessageRecord::new(
                    workspace_id(),
                    &state,
                    opener_text.to_owned(),
                    DeliveryGate::Done,
                )
                .with_sender(MessageSender::Human);
                store.queue_message(&msg, &workspace.session_name).unwrap();
                msg.status = MessageStatus::Sent;
                msg.last_sent_at = Some(jiff::Timestamp::now());
                store
                    .record_sent_batch(std::slice::from_ref(&msg), &workspace.session_name)
                    .unwrap();
                msg
            });

            let agy_home = dir.path().join("agy-home");
            let transcript_dir = agy_home
                .join("brain")
                .join(session_id)
                .join(".system_generated")
                .join("logs");
            std::fs::create_dir_all(&transcript_dir).unwrap();
            let transcript_path = transcript_dir.join("transcript_full.jsonl");
            let login_env = std::collections::BTreeMap::from([(
                "RIMZ_ANTIGRAVITY_HOME".to_owned(),
                agy_home.to_string_lossy().into_owned(),
            )]);

            // Step 1: Turn opens via PreInvocation(inv=0) with either typed or recorded opener.
            let open_transcript = serde_json::json!({
                "step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-25T09:30:01Z",
                "content": format!("<USER_REQUEST>\n{opener_text}\n</USER_REQUEST>")
            })
            .to_string();
            std::fs::write(&transcript_path, &open_transcript).unwrap();
            let pre_payload = serde_json::json!({
                "conversationId": session_id,
                "workspacePaths": [workspace.project_root],
                "transcriptPath": transcript_path,
                "invocationNum": 0,
                "initialNumSteps": 1,
                "lastUserInput": opener_text
            });
            let pre_decoded = agent
                .decode_hook_in("PreInvocation", &pre_payload, &login_env)
                .unwrap();
            apply(&store, "PreInvocation", &pre_payload, pre_decoded);

            let expected_opener: Vec<_> = opener_msg
                .as_ref()
                .map(|msg| msg.message_id.clone())
                .into_iter()
                .collect();
            assert_eq!(
                rimz::store::agent_context::read_one(
                    store.runtime_paths(),
                    "antigravity",
                    session_id
                )
                .map(|record| record.context.turn_opened_by)
                .unwrap_or_default(),
                expected_opener
            );

            // Step 2: Mid-turn harness text arrives (either CompactionEnded continue or reminder snapshot).
            let (mid_text, mid_id) = match case {
                MidTurnCase::CompactionContinue => {
                    let mut command = MessageRecord::new(
                        workspace_id(),
                        &state,
                        "/compact".to_owned(),
                        DeliveryGate::Done,
                    )
                    .with_body(MessageBody::Command)
                    .with_sender(MessageSender::System)
                    .with_automated(true);
                    let mut tail = MessageRecord::new(
                        workspace_id(),
                        &state,
                        "continue".to_owned(),
                        DeliveryGate::Done,
                    )
                    .with_sender(MessageSender::System)
                    .with_automated(true);
                    tail.follows_command = Some(command.message_id.clone());
                    tail.soft_compact = Some(SoftCompactStep::Continue);
                    let mut tail = store
                        .queue_message_with_tail(&command, &tail, &workspace.session_name)
                        .unwrap();
                    command.status = MessageStatus::Sent;
                    command.last_sent_at = Some(jiff::Timestamp::now());
                    tail.status = MessageStatus::Sent;
                    tail.last_sent_at = Some(jiff::Timestamp::now());
                    store
                        .record_sent_batch(&[command, tail.clone()], &workspace.session_name)
                        .unwrap();
                    let mid_transcript = [
                        open_transcript.clone(),
                        serde_json::json!({
                            "step_index": 1, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                            "status": "DONE", "created_at": "2026-11-25T09:30:13Z",
                            "content": "<USER_REQUEST>\n/compact continue\n</USER_REQUEST>"
                        })
                        .to_string(),
                        serde_json::json!({
                            "step_index": 2, "source": "SYSTEM", "type": "CHECKPOINT",
                            "status": "DONE", "created_at": "2026-11-25T09:30:20Z",
                            "content": "# Resuming from a compaction"
                        })
                        .to_string(),
                        serde_json::json!({
                            "step_index": 3, "source": "MODEL", "type": "PLANNER_RESPONSE",
                            "status": "DONE", "created_at": "2026-11-25T09:30:25Z",
                            "content": "ALL_STEPS_DONE"
                        })
                        .to_string(),
                    ]
                    .join("\n");
                    std::fs::write(&transcript_path, &mid_transcript).unwrap();
                    let post_payload = serde_json::json!({
                        "conversationId": session_id,
                        "workspacePaths": [workspace.project_root],
                        "transcriptPath": transcript_path,
                        "invocationNum": 1,
                        "initialNumSteps": 1,
                        "lastUserInput": "/compact continue"
                    });
                    let post_decoded = agent
                        .decode_hook_in("PostInvocation", &post_payload, &login_env)
                        .unwrap();
                    apply(&store, "PostInvocation", &post_payload, post_decoded);
                    ("continue", tail.message_id)
                }
                MidTurnCase::ReminderSnapshot => {
                    let reminder_text = "Your context is getting high. Wrap up and compact.";
                    let mut reminder = MessageRecord::new(
                        workspace_id(),
                        &state,
                        reminder_text.to_owned(),
                        DeliveryGate::Any,
                    )
                    .with_sender(MessageSender::System)
                    .with_automated(true);
                    reminder.soft_compact = Some(SoftCompactStep::WrapUp);
                    store
                        .queue_message(&reminder, &workspace.session_name)
                        .unwrap();
                    reminder.status = MessageStatus::Sent;
                    reminder.last_sent_at = Some(jiff::Timestamp::now());
                    store
                        .record_sent_batch(std::slice::from_ref(&reminder), &workspace.session_name)
                        .unwrap();
                    let mid_transcript = [
                        open_transcript.clone(),
                        serde_json::json!({
                            "step_index": 1, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                            "status": "DONE", "created_at": "2026-11-25T09:30:13Z",
                            "content": format!("<USER_REQUEST>\n{reminder_text}\n</USER_REQUEST>")
                        })
                        .to_string(),
                        serde_json::json!({
                            "step_index": 2, "source": "MODEL", "type": "PLANNER_RESPONSE",
                            "status": "DONE", "created_at": "2026-11-25T09:30:20Z",
                            "content": "ALL_STEPS_DONE"
                        })
                        .to_string(),
                    ]
                    .join("\n");
                    std::fs::write(&transcript_path, &mid_transcript).unwrap();
                    let post_payload = serde_json::json!({
                        "conversationId": session_id,
                        "workspacePaths": [workspace.project_root],
                        "transcriptPath": transcript_path,
                        "invocationNum": 1,
                        "initialNumSteps": 1,
                        "lastUserInput": reminder_text
                    });
                    let post_decoded = agent
                        .decode_hook_in("PostInvocation", &post_payload, &login_env)
                        .unwrap();
                    apply(&store, "PostInvocation", &post_payload, post_decoded);
                    (reminder_text, reminder.message_id)
                }
            };

            // Harness mid-turn text must NOT overwrite turn_opened_by.
            assert_eq!(
                rimz::store::agent_context::read_one(
                    store.runtime_paths(),
                    "antigravity",
                    session_id
                )
                .map(|record| record.context.turn_opened_by)
                .unwrap_or_default(),
                expected_opener,
                "recorded_opener={recorded_opener}: mid-turn harness text must keep open turn opener"
            );

            // Step 3: Turn finishes with final reply ALL_STEPS_DONE.
            let stop_payload = serde_json::json!({
                "conversationId": session_id,
                "workspacePaths": [workspace.project_root],
                "transcriptPath": transcript_path,
                "fullyIdle": true,
                "terminationReason": "NO_TOOL_CALL"
            });
            let stop_decoded = agent
                .decode_hook_in("Stop", &stop_payload, &login_env)
                .unwrap();
            apply(&store, "Stop", &stop_payload, stop_decoded);

            // Verify raw log keeps both the harness entry (with its id) and ALL_STEPS_DONE with original reply_to.
            let raw = rimz::transcript::read_all(store.paths()).unwrap();
            let reply = raw
                .iter()
                .find(|entry| entry.text == "ALL_STEPS_DONE")
                .expect("ALL_STEPS_DONE recorded");
            assert_eq!(reply.reply_to, expected_opener);
            assert!(
                raw.iter().any(|entry| entry.text == mid_text
                    && entry.message_id.as_ref() == Some(&mid_id)
                    && entry.origin() == rimz::transcript::EntryOrigin::Harness),
                "mid-turn harness entry must remain recorded in log for --json"
            );

            // Verify all three human text views show ALL_STEPS_DONE and hide the harness text.
            for (label, flat, last) in [
                ("transcript", false, None),
                ("transcript --flat", true, None),
                ("agents show recent", false, Some(6)),
            ] {
                let rendered = crate::cli::transcript::render_text_view_in_paths(
                    &workspace,
                    store.paths(),
                    Some("@all"),
                    last,
                    flat,
                    crate::cli::transcript::Hidden::Skip,
                )
                .unwrap();
                assert!(
                    rendered.contains("ALL_STEPS_DONE"),
                    "recorded_opener={recorded_opener} view={label}: reply missing:\n{rendered}"
                );
                assert!(
                    rendered.contains(opener_text),
                    "recorded_opener={recorded_opener} view={label}: opener missing:\n{rendered}"
                );
                assert!(
                    !rendered.contains(mid_text),
                    "recorded_opener={recorded_opener} view={label}: harness text leaked:\n{rendered}"
                );
            }
        }
    }

    // Also verify a person's steer and a mixed [harness, person] mid-turn report keep replacing turn_opened_by
    // with the person's id only, and a turn-end continue still owns its own turn.
    {
        let dir = tempfile::tempdir().unwrap();
        let paths = rimz::disk::paths::StatePaths::under(workspace_id(), dir.path()).unwrap();
        let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id(), dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        let session_id = "0f251024-d1db-4c68-8a51-3ea03faa87d7";
        let state = rimz::testkit::agent_state("antigravity", session_id, jiff::Timestamp::now());
        let agy_home = dir.path().join("agy-home");
        let transcript_dir = agy_home
            .join("brain")
            .join(session_id)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let transcript_path = transcript_dir.join("transcript_full.jsonl");
        let login_env = std::collections::BTreeMap::from([(
            "RIMZ_ANTIGRAVITY_HOME".to_owned(),
            agy_home.to_string_lossy().into_owned(),
        )]);

        let mut opener_msg = MessageRecord::new(
            workspace_id(),
            &state,
            "initial task".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::Human);
        store
            .queue_message(&opener_msg, &workspace.session_name)
            .unwrap();
        opener_msg.status = MessageStatus::Sent;
        opener_msg.last_sent_at = Some(jiff::Timestamp::now());
        store
            .record_sent_batch(std::slice::from_ref(&opener_msg), &workspace.session_name)
            .unwrap();

        let open_row = serde_json::json!({
            "step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT",
            "status": "DONE", "created_at": "2026-11-25T09:42:00Z",
            "content": "<USER_REQUEST>\ninitial task\n</USER_REQUEST>"
        })
        .to_string();
        std::fs::write(&transcript_path, &open_row).unwrap();
        let pre_payload = serde_json::json!({
            "conversationId": session_id,
            "workspacePaths": [workspace.project_root],
            "transcriptPath": transcript_path,
            "invocationNum": 0,
            "initialNumSteps": 1,
            "lastUserInput": "initial task"
        });
        let pre_decoded = agent
            .decode_hook_in("PreInvocation", &pre_payload, &login_env)
            .unwrap();
        apply(&store, "PreInvocation", &pre_payload, pre_decoded);

        // Mixed mid-turn report: harness reminder + human steer in one invocation.
        let mut reminder = MessageRecord::new(
            workspace_id(),
            &state,
            "wrap up soon".to_owned(),
            DeliveryGate::Any,
        )
        .with_sender(MessageSender::System)
        .with_automated(true);
        store
            .queue_message(&reminder, &workspace.session_name)
            .unwrap();
        reminder.status = MessageStatus::Sent;
        reminder.last_sent_at = Some(jiff::Timestamp::now());
        store
            .record_sent_batch(std::slice::from_ref(&reminder), &workspace.session_name)
            .unwrap();

        let mut human_steer = MessageRecord::new(
            workspace_id(),
            &state,
            "also check tests".to_owned(),
            DeliveryGate::Any,
        )
        .with_sender(MessageSender::Human);
        store
            .queue_message(&human_steer, &workspace.session_name)
            .unwrap();
        human_steer.status = MessageStatus::Sent;
        human_steer.last_sent_at = Some(jiff::Timestamp::now());
        store
            .record_sent_batch(std::slice::from_ref(&human_steer), &workspace.session_name)
            .unwrap();

        let mixed_transcript = [
            open_row,
            serde_json::json!({
                "step_index": 1, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-25T09:42:10Z",
                "content": "<USER_REQUEST>\nwrap up soon\n</USER_REQUEST>"
            })
            .to_string(),
            serde_json::json!({
                "step_index": 2, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-25T09:42:11Z",
                "content": "<USER_REQUEST>\nalso check tests\n</USER_REQUEST>"
            })
            .to_string(),
            serde_json::json!({
                "step_index": 3, "source": "MODEL", "type": "PLANNER_RESPONSE",
                "status": "DONE", "created_at": "2026-11-25T09:42:15Z",
                "content": ""
            })
            .to_string(),
        ]
        .join("\n");
        std::fs::write(&transcript_path, &mixed_transcript).unwrap();
        let mixed_payload = serde_json::json!({
            "conversationId": session_id,
            "workspacePaths": [workspace.project_root],
            "transcriptPath": transcript_path,
            "invocationNum": 1,
            "initialNumSteps": 1,
            "lastUserInput": "also check tests"
        });
        let mixed_decoded = agent
            .decode_hook_in("PostInvocation", &mixed_payload, &login_env)
            .unwrap();
        apply(&store, "PostInvocation", &mixed_payload, mixed_decoded);

        assert_eq!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", session_id)
                .map(|record| record.context.turn_opened_by)
                .unwrap_or_default(),
            vec![human_steer.message_id.clone()],
            "mixed mid-turn report must replace turn_opened_by with non-harness section ids only"
        );

        // A standalone person's steer in mid-turn replaces turn_opened_by unchanged.
        let mut solo_steer = MessageRecord::new(
            workspace_id(),
            &state,
            "focus on unit tests".to_owned(),
            DeliveryGate::Any,
        )
        .with_sender(MessageSender::Human);
        store
            .queue_message(&solo_steer, &workspace.session_name)
            .unwrap();
        solo_steer.status = MessageStatus::Sent;
        solo_steer.last_sent_at = Some(jiff::Timestamp::now());
        store
            .record_sent_batch(std::slice::from_ref(&solo_steer), &workspace.session_name)
            .unwrap();
        let solo_transcript = [
            mixed_transcript,
            serde_json::json!({
                "step_index": 4, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-25T09:42:20Z",
                "content": "<USER_REQUEST>\nfocus on unit tests\n</USER_REQUEST>"
            })
            .to_string(),
            serde_json::json!({
                "step_index": 5, "source": "MODEL", "type": "PLANNER_RESPONSE",
                "status": "DONE", "created_at": "2026-11-25T09:42:25Z",
                "content": ""
            })
            .to_string(),
        ]
        .join("\n");
        std::fs::write(&transcript_path, &solo_transcript).unwrap();
        let solo_payload = serde_json::json!({
            "conversationId": session_id,
            "workspacePaths": [workspace.project_root],
            "transcriptPath": transcript_path,
            "invocationNum": 2,
            "initialNumSteps": 4,
            "lastUserInput": "focus on unit tests"
        });
        let solo_decoded = agent
            .decode_hook_in("PostInvocation", &solo_payload, &login_env)
            .unwrap();
        apply(&store, "PostInvocation", &solo_payload, solo_decoded);
        assert_eq!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", session_id)
                .map(|record| record.context.turn_opened_by)
                .unwrap_or_default(),
            vec![solo_steer.message_id],
            "a person's steer keeps replacing turn_opened_by"
        );

        // Turn-end continue opening its own turn still records its id on turn_opened_by and hides its reply.
        let mut turn_end_continue = MessageRecord::new(
            workspace_id(),
            &state,
            "continue".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::System)
        .with_automated(true);
        turn_end_continue.soft_compact = Some(SoftCompactStep::Continue);
        store
            .queue_message(&turn_end_continue, &workspace.session_name)
            .unwrap();
        turn_end_continue.status = MessageStatus::Sent;
        turn_end_continue.last_sent_at = Some(jiff::Timestamp::now());
        store
            .record_sent_batch(
                std::slice::from_ref(&turn_end_continue),
                &workspace.session_name,
            )
            .unwrap();
        let turn_end_transcript = [
            solo_transcript,
            serde_json::json!({
                "step_index": 6, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": "2026-11-25T09:42:40Z",
                "content": "<USER_REQUEST>\n/compact continue\n</USER_REQUEST>"
            })
            .to_string(),
            serde_json::json!({
                "step_index": 7, "source": "MODEL", "type": "PLANNER_RESPONSE",
                "status": "DONE", "created_at": "2026-11-25T09:42:45Z",
                "content": "READY_FOR_NEXT_TASK"
            })
            .to_string(),
        ]
        .join("\n");
        std::fs::write(&transcript_path, &turn_end_transcript).unwrap();
        let continue_pre = serde_json::json!({
            "conversationId": session_id,
            "workspacePaths": [workspace.project_root],
            "transcriptPath": transcript_path,
            "invocationNum": 0,
            "initialNumSteps": 7,
            "lastUserInput": "/compact continue"
        });
        let continue_decoded = agent
            .decode_hook_in("PreInvocation", &continue_pre, &login_env)
            .unwrap();
        apply(&store, "PreInvocation", &continue_pre, continue_decoded);
        assert_eq!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "antigravity", session_id)
                .map(|record| record.context.turn_opened_by)
                .unwrap_or_default(),
            vec![turn_end_continue.message_id],
            "a continue opening its own turn still replaces turn_opened_by"
        );
        let continue_stop = serde_json::json!({
            "conversationId": session_id,
            "workspacePaths": [workspace.project_root],
            "transcriptPath": transcript_path,
            "fullyIdle": true,
            "terminationReason": "NO_TOOL_CALL"
        });
        let continue_stop_decoded = agent
            .decode_hook_in("Stop", &continue_stop, &login_env)
            .unwrap();
        apply(&store, "Stop", &continue_stop, continue_stop_decoded);
        let rendered = crate::cli::transcript::render_text_view_in_paths(
            &workspace,
            store.paths(),
            Some("@all"),
            None,
            false,
            crate::cli::transcript::Hidden::Skip,
        )
        .unwrap();
        assert!(
            !rendered.contains("READY_FOR_NEXT_TASK"),
            "turn-end continue's own reply must stay hidden:\n{rendered}"
        );
    }
}
