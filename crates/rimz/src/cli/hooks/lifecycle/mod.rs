use super::*;
use rimz::store::snapshot::find_agent;
use std::process::{Command, Stdio};

#[cfg(test)]
use rimz::agents::AgentState;

mod active_time;
mod context;
mod delivery;
mod identity;
mod observe;
mod reactors;
mod runtime_env;
mod transcript;

use context::*;
use delivery::*;
use identity::{
    agent_identity_env, env_run_id, validate_agent_name_env, validate_non_empty_identity_env,
};
use observe::{
    record_derived_lifecycle_observation, record_lifecycle_observation, release_resolved_keyed_ask,
};
use reactors::ReactorCtx;
use transcript::*;

pub(super) use identity::fill_root_launch_identity;
#[cfg(test)]
pub(super) use observe::root_identity_rollup;

pub(super) fn handle_lifecycle_hook(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    decoded: &mut HookOutput,
    payload: &Value,
    ingress_owner: rimz::agents::HookIngressOwner,
    globals: &GlobalFlags,
) -> Result<()> {
    let agent_id = decoded.event_agent_id().cloned();
    let released = release_resolved_keyed_ask(
        workspace,
        store,
        agent,
        decoded,
        payload,
        ingress_owner,
        globals,
    );
    let recorded =
        record_lifecycle_observation(workspace, store, agent, decoded, ingress_owner, globals);
    if let Some(recorded) = recorded.as_ref()
        && let Some(side) = recorded.receipt.side_conversation.as_ref()
    {
        if let Some(host) = side.host.as_ref() {
            touch_agent_activity(
                workspace,
                store,
                agent,
                decoded.event_name(),
                host.as_str(),
                rimz::agent_activity::ToolRun::Reset,
                false,
            );
            active_time::record_side_conversation(
                store,
                agent,
                &recorded.observation.signal,
                host.as_str(),
                side.host_running,
                decoded.event_name(),
            );
        }
        if recorded.receipt.rotation_due {
            spawn_auto_rotation(workspace);
        }
        return Ok(());
    }
    let event_name = decoded.event_name().to_owned();
    let mut events: Vec<_> = released
        .iter()
        .chain(recorded.as_ref())
        .flat_map(|recorded| recorded.receipt.events.clone())
        .collect();
    let (derived_events, derived_rotation_due) = if recorded.as_ref().is_some_and(|recorded| {
        recorded.observation.agent_id.is_some() && recorded.observation.parent_agent_id.is_none()
    }) {
        derive_subagent_lifecycle(workspace, store, agent, ingress_owner, globals)
    } else {
        (Vec::new(), false)
    };
    events.extend(derived_events);
    if derived_rotation_due || released.is_some_and(|released| released.receipt.rotation_due) {
        spawn_auto_rotation(workspace);
    }
    // A cache ping's open or close is not agent activity: it leaves the
    // heartbeat, active time, and the supervised run's reply and verdict alone.
    let ping_edge = recorded
        .as_ref()
        .and_then(|recorded| recorded.receipt.transition.as_ref())
        .is_some_and(|transition| transition.ping.is_some());
    let mut run_id = session_run_id(store, agent, agent_id.as_ref());
    let assistant_message =
        record_assistant_response(workspace, store, agent, decoded, recorded.as_ref());
    if let (Some(run_id), Some((agent_id, message)), false) =
        (run_id.as_ref(), assistant_message, ping_edge)
        && let Err(err) = rimz::harness::run::record_assistant_message(
            store.paths(),
            run_id,
            agent.spec().kind,
            &agent_id,
            message,
        )
    {
        warn!(
            agent = agent.spec().kind,
            event = %event_name,
            run_id = %run_id,
            error = %err,
            "lifecycle: failed to seed supervised response",
        );
    }
    record_native_answer(workspace, store, agent, decoded, recorded.as_ref());
    let (model_hint, transcript_path, observed_agent_id, parent_agent_id, signal) =
        match recorded.as_ref() {
            Some(recorded) => (
                recorded.model_hint.as_deref(),
                recorded.observation.transcript_path.as_deref(),
                recorded.observation.agent_id.clone(),
                recorded.observation.parent_agent_id.as_deref(),
                Some(&recorded.observation.signal),
            ),
            None => (None, None, None, None, None),
        };
    let turn_ended = matches!(
        signal,
        Some(LifecycleSignal::TurnEnded { .. } | LifecycleSignal::TurnInterrupted { .. })
    );
    let root_tool_used =
        parent_agent_id.is_none() && matches!(signal, Some(LifecycleSignal::ToolUsed { .. }));
    let tool_call = match signal {
        Some(LifecycleSignal::ToolUsed {
            name: Some(name), ..
        }) if !name.trim().is_empty() => agent
            .spec()
            .tool_signature(payload)
            .map(|digest| (name.trim(), digest)),
        _ => None,
    };
    let tool_run = tool_call
        .as_ref()
        .map_or(rimz::agent_activity::ToolRun::Reset, |(tool, digest)| {
            rimz::agent_activity::ToolRun::Call { tool, digest }
        });
    let context_agent_id = observed_agent_id
        .or_else(|| decoded.context_agent_id().cloned())
        .or_else(|| agent_id.clone());
    if !ping_edge {
        active_time::record(
            store,
            agent,
            decoded,
            recorded.as_ref(),
            context_agent_id.as_deref(),
            &event_name,
        );
    }
    if let Some(agent_id) = context_agent_id {
        let parent_activity_id = match signal {
            Some(LifecycleSignal::SubagentStopped { .. }) => parent_agent_id,
            _ => None,
        };
        manage_agent_context(AgentContextHook {
            workspace,
            store,
            agent,
            context: LifecycleEventContext {
                event_name: &event_name,
                decoded,
                payload,
                agent_id: agent_id.as_str(),
                parent_agent_id,
                parent_activity_id,
                model_hint,
                transcript_path,
                turn_ended,
                tool_run,
                tool_used: root_tool_used,
                ping_edge,
            },
        });
    }
    let mut run_completion = None;
    if let Some(recorded) = recorded.as_ref() {
        let assistant_message = assistant_message_for_lifecycle(recorded, run_id.is_some(), || {
            decoded.final_message().map(ToOwned::to_owned)
        });
        if !ping_edge {
            run_completion = record_run_lifecycle(
                store,
                agent,
                &event_name,
                recorded,
                assistant_message.as_deref(),
                run_id.as_ref(),
            );
        }
        if root_tool_used
            && agent.spec().capabilities.hook_context.is_some()
            && let Some(run_id) = run_id.as_ref()
            && let Err(error) = rimz::harness::run::claim_rung(
                store.paths(),
                run_id,
                jiff::Timestamp::now(),
                |rung| agent.attach_hook_context(decoded, &rung.text()),
            )
        {
            warn!(%run_id, %error, "lifecycle: failed to claim deadline context");
        }
        let in_flight = in_flight_messages_for_lifecycle(store, agent, recorded);
        let delivered = confirm_sent_message_for_lifecycle(store, agent, recorded, workspace);
        if run_id.is_none() {
            run_id = session_run_id(store, agent, recorded.observation.agent_id.as_ref());
        }
        let sections = rimz::store::message::classify_submitted_prompt(
            recorded.observation.prompt.as_deref().unwrap_or_default(),
            &delivered.iter().collect::<Vec<_>>(),
            &in_flight.iter().collect::<Vec<_>>(),
        );
        record_user_input_for_lifecycle(
            workspace,
            agent,
            recorded,
            &sections,
            &delivered,
            run_id.is_some(),
            user_input_state_root(store),
        );
        let questions = match &recorded.observation.signal {
            LifecycleSignal::AwaitingInput { .. } => decoded.questions(),
            _ => &[],
        };
        if let Err(err) = record_conversation(
            workspace,
            store,
            agent,
            recorded,
            ConversationInput {
                assistant_message: assistant_message.as_deref(),
                questions,
                sections: &sections,
                run_id: run_id.as_ref(),
            },
        ) {
            warn!(
                agent = agent.spec().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to record transcript entry",
            );
        }
        runtime_env::attach_runtime_env(
            workspace,
            store,
            agent,
            decoded,
            recorded,
            &sections,
            ingress_owner,
        );
        if recorded.receipt.rotation_due {
            spawn_auto_rotation(workspace);
        }
    }
    reactors::dispatch(
        &ReactorCtx {
            workspace,
            store,
            primary_event_id: recorded
                .as_ref()
                .and_then(|recorded| recorded.receipt.primary_event_id.as_ref()),
            run_completion: run_completion.as_ref(),
        },
        &events,
    );
    Ok(())
}

/// Spawn a hook-triggered `rimz` helper detached, with all stdio nulled (the
/// fresh-stdio invariant for hook helper children). The hook drops the child
/// into the shared reaper, so it returns before the helper runs and never adds
/// latency to the agent's turn. Best-effort: a spawn failure is logged and
/// ignored; durable queue work remains pending for a later transition.
fn spawn_refresh_detached(spawn: &rimz::agents::RefreshSpawn) {
    let exe = rimz::proc::rimz_exe();
    let mut cmd = Command::new(exe);
    cmd.args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = rimz::child_process::spawn_detached_reaped(&mut cmd, "adapter-refresh") {
        warn!(error = %err, "lifecycle: failed to spawn the adapter refresh helper");
    }
}

fn assistant_message_for_lifecycle(
    recorded: &RecordedLifecycle,
    supervised_run: bool,
    extract: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let needs_run_message =
        supervised_run && recorded.observation.signal.terminal_disposition().is_some();
    let needs_conversation_message = recorded.observation.parent_agent_id.is_none()
        && matches!(
            recorded.observation.signal,
            LifecycleSignal::TurnEnded { .. } | LifecycleSignal::AwaitingInput { .. }
        );
    (needs_run_message || needs_conversation_message)
        .then(extract)
        .flatten()
}

fn derive_subagent_lifecycle(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    ingress_owner: rimz::agents::HookIngressOwner,
    globals: &GlobalFlags,
) -> (Vec<rimz::agents::LifecycleEvent>, bool) {
    let observations = agent.derive_subagent_observations(&workspace.worktree_root);
    if observations.is_empty() {
        return (Vec::new(), false);
    }
    let snapshot = match store.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            debug!(
                kind = agent.spec().kind,
                error = %err,
                "lifecycle: skipped derived subagents because the prior rollup was unreadable",
            );
            return (Vec::new(), false);
        }
    };
    let kind = agent.spec().kind;
    let mut rotation_due = false;
    let mut events = Vec::new();
    for observation in observations {
        let (Some(child_id), Some(parent_id)) = (
            observation.agent_id.as_ref(),
            observation.parent_agent_id.as_ref(),
        ) else {
            continue;
        };
        if find_agent(&snapshot.agents, kind, parent_id).is_none() {
            continue;
        }
        let prior = find_agent(&snapshot.agents, kind, child_id);
        let event_name = match &observation.signal {
            LifecycleSignal::SubagentStarted if prior.is_none() => "chatsStoreSubagentStart",
            LifecycleSignal::SubagentStopped { .. }
                if !prior.is_some_and(|state| {
                    matches!(
                        state.status,
                        rimz::agents::AgentStatus::Success | rimz::agents::AgentStatus::Failed
                    )
                }) =>
            {
                "chatsStoreSubagentStop"
            }
            _ => continue,
        };
        let recorded = record_derived_lifecycle_observation(
            workspace,
            store,
            agent,
            event_name,
            observation,
            ingress_owner,
            globals,
        );
        rotation_due |= recorded.receipt.rotation_due;
        events.extend(recorded.receipt.events);
    }
    (events, rotation_due)
}

fn user_input_state_root(_store: &Store) -> Option<&std::path::Path> {
    #[cfg(test)]
    {
        _store.paths().root.ancestors().nth(2)
    }
    #[cfg(not(test))]
    None
}

struct RecordedLifecycle {
    model_hint: Option<String>,
    observation: AgentLifecycleObservation,
    receipt: rimz::store::writer::AgentLifecycleReceipt,
}

fn spawn_auto_rotation(workspace: &ResolvedWorkspace) {
    spawn_refresh_detached(&rimz::agents::RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "workspace".to_owned(),
            "rotate-events".to_owned(),
        ],
    });
}

struct AgentContextHook<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    agent: &'a AgentDefinition,
    context: LifecycleEventContext<'a>,
}

struct LifecycleEventContext<'a> {
    event_name: &'a str,
    decoded: &'a mut HookOutput,
    payload: &'a Value,
    agent_id: &'a str,
    parent_agent_id: Option<&'a str>,
    parent_activity_id: Option<&'a str>,
    model_hint: Option<&'a str>,
    transcript_path: Option<&'a str>,
    turn_ended: bool,
    tool_run: rimz::agent_activity::ToolRun<'a>,
    tool_used: bool,
    ping_edge: bool,
}

struct ContextSidecarInput<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    agent: &'a AgentDefinition,
    event_name: &'a str,
    decoded: &'a mut HookOutput,
    payload: &'a Value,
    context_agent_id: &'a str,
    model_hint: Option<&'a str>,
    transcript_path: Option<&'a str>,
    turn_ended: bool,
}

fn record_run_lifecycle(
    store: &Store,
    agent: &AgentDefinition,
    event_name: &str,
    recorded: &RecordedLifecycle,
    assistant_message: Option<&str>,
    run_id: Option<&rimz::RunId>,
) -> Option<rimz::store::run::RunRecord> {
    let run_id = run_id?;
    match rimz::harness::run::settle_lifecycle(
        store,
        run_id,
        agent,
        &recorded.observation,
        assistant_message.map(ToOwned::to_owned),
    ) {
        Ok(record) => record,
        Err(err) => {
            warn!(
                agent = agent.spec().kind,
                event = %event_name,
                run_id = %run_id,
                error = %err,
                "lifecycle: failed to update the supervised run",
            );
            None
        }
    }
}

fn session_run_id(
    store: &Store,
    agent: &AgentDefinition,
    agent_id: Option<&rimz::ids::AgentSessionId>,
) -> Option<rimz::RunId> {
    if let Some(run_id) = env_run_id() {
        return Some(run_id);
    }
    let agent_id = agent_id?;
    // SessionEnd has already removed the peer from the live snapshot.
    let peer = agent_state(store, agent, agent_id).or_else(|| {
        let snapshot = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .inspect_err(|error| warn!(%error, "lifecycle: failed to read peer identity"))
            .ok()?;
        find_agent(&snapshot.agents, agent.spec().kind, agent_id).cloned()
    })?;
    let open = if peer.is_team_seat() {
        rimz::harness::run::open_team_run(store.paths(), &peer)
    } else {
        rimz::harness::run::open_peer_run(store.paths(), &peer)
    };
    match open {
        Ok(record) => record.map(|record| record.run_id),
        Err(error) => {
            warn!(%error, "lifecycle: failed to find peer or team run");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace_id =
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
        let paths = rimz::disk::paths::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        (dir, store)
    }

    #[test]
    fn native_child_session_end_keeps_root_context_cleanup_identity() {
        let (_dir, store) = test_store();
        let mut decoded = rimz::agents::definition_by_kind("claude")
            .unwrap()
            .decode_hook(
                "SessionEnd",
                &serde_json::json!({
                    "session_id": "root-session",
                    "agent_id": "foreign-child"
                }),
            )
            .expect("session end decodes");
        assert!(decoded.ends_session());
        let observation = decoded
            .lifecycle()
            .expect("child session end has lifecycle identity");
        assert_eq!(observation.agent_id.as_deref(), Some("foreign-child"));
        assert_eq!(observation.parent_agent_id.as_deref(), Some("root-session"));
        assert_eq!(
            decoded
                .context_agent_id()
                .map(rimz::ids::AgentSessionId::as_str),
            Some("root-session")
        );
        let event_name = decoded.event_name().to_owned();
        let agent_id = decoded.context_agent_id().unwrap().to_string();

        let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
        context.model_id = Some("claude-sonnet".to_owned());
        rimz::store::agent_context::merge_observed(
            store.runtime_paths(),
            "claude",
            "root-session",
            context,
        )
        .expect("context sidecar writes");
        manage_agent_context(AgentContextHook {
            workspace: &test_workspace(),
            store: &store,
            agent: rimz::agents::definition_by_kind("claude").unwrap(),
            context: LifecycleEventContext {
                event_name: &event_name,
                decoded: &mut decoded,
                payload: &serde_json::json!({}),
                agent_id: &agent_id,
                parent_agent_id: None,
                parent_activity_id: None,
                model_hint: None,
                transcript_path: None,
                turn_ended: false,
                tool_run: rimz::agent_activity::ToolRun::Reset,
                tool_used: false,
                ping_edge: false,
            },
        });
        assert!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "root-session")
                .is_none()
        );
    }

    fn transcript_entry(
        entry: rimz::transcript::TranscriptKind,
        text: &str,
        at: &str,
    ) -> rimz::transcript::TranscriptEntry {
        rimz::transcript::TranscriptEntry::new(
            at.parse().expect("timestamp"),
            rimz::ids::AgentKind::new_unchecked("claude"),
            rimz::ids::AgentSessionId::from("sess-1"),
            entry,
            text.to_owned(),
        )
    }

    #[test]
    fn native_ask_log_detects_open_ask() {
        let (_dir, store) = test_store();
        let ask = transcript_entry(
            rimz::transcript::TranscriptKind::Ask,
            "approve?",
            "2026-06-01T00:00:00Z",
        );
        rimz::transcript::append(store.paths(), &ask).expect("append ask");

        assert!(has_open_native_ask(&store, "claude", "sess-1"));
    }

    #[test]
    fn native_ask_log_detects_answered_ask() {
        let (_dir, store) = test_store();
        let mut ask = transcript_entry(
            rimz::transcript::TranscriptKind::Ask,
            "approve?",
            "2026-06-01T00:00:00Z",
        );
        ask.id = Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap());
        let mut answer = transcript_entry(
            rimz::transcript::TranscriptKind::Answer,
            "yes",
            "2026-06-01T00:00:01Z",
        );
        answer.id = ask.id.clone();
        rimz::transcript::append(store.paths(), &ask).expect("append ask");
        rimz::transcript::append(store.paths(), &answer).expect("append answer");

        assert!(!has_open_native_ask(&store, "claude", "sess-1"));
        assert_eq!(
            latest_native_ask_id(&store, "claude", "sess-1")
                .as_ref()
                .map(rimz::ids::AskId::as_str),
            Some("ask_0123456789abcdef")
        );
    }

    #[test]
    fn native_ask_log_treats_empty_log_as_closed() {
        let (_dir, store) = test_store();

        assert!(!has_open_native_ask(&store, "claude", "sess-1"));
    }

    fn workspace_id() -> rimz::ids::WorkspaceId {
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"))
    }

    fn test_workspace() -> ResolvedWorkspace {
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

    #[test]
    fn lifecycle_confirms_matching_message_body() {
        let (_dir, store) = test_store();
        let agent = test_agent();
        let command = rimz::store::message::MessageRecord::new(
            workspace_id(),
            &agent,
            "/compact".to_owned(),
            rimz::store::message::DeliveryGate::Done,
        )
        .with_body(rimz::store::message::MessageBody::Command);
        let prompt = rimz::store::message::MessageRecord::new(
            workspace_id(),
            &agent,
            "real prompt".to_owned(),
            rimz::store::message::DeliveryGate::Done,
        );
        store.queue_message(&command, "session").unwrap();
        store.queue_message(&prompt, "session").unwrap();
        store
            .record_sent_batch(std::slice::from_ref(&command), "session")
            .unwrap()
            .first()
            .expect("command sent");
        store
            .record_sent_batch(std::slice::from_ref(&prompt), "session")
            .unwrap()
            .first()
            .expect("prompt sent");

        let compact_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::Compacting,
        );
        confirm_sent_message_for_lifecycle(
            &store,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &RecordedLifecycle {
                model_hint: None,
                observation: compact_observation,
                receipt: Default::default(),
            },
            &test_workspace(),
        );
        let messages = store.list_messages().unwrap();
        assert!(
            messages
                .iter()
                .all(|message| message.message_id != command.message_id),
            "delivered command self-cleans from the live queue"
        );
        assert_eq!(
            messages
                .iter()
                .find(|message| message.message_id == prompt.message_id)
                .unwrap()
                .status,
            rimz::store::message::MessageStatus::Sent,
            "compaction cannot confirm the prompt behind it"
        );

        let mut real_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        real_observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
            "Type: USER_MESSAGE\nFrom: @user\nContent:\nreal prompt",
        ));
        confirm_sent_message_for_lifecycle(
            &store,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &RecordedLifecycle {
                model_hint: None,
                observation: real_observation,
                receipt: Default::default(),
            },
            &test_workspace(),
        );
        let messages = store.list_messages().unwrap();
        assert!(
            messages
                .iter()
                .all(|message| message.message_id != prompt.message_id),
            "delivered prompt self-cleans from the live queue"
        );
        assert!(
            store
                .read_events()
                .unwrap()
                .iter()
                .any(|event| event.method == "message.delivered"),
            "terminal delivery event is logged"
        );
    }

    #[test]
    fn turn_end_reconciles_resumed_claude_cost_upward() {
        let dir = tempfile::TempDir::new().unwrap();
        let transcript = dir.path().join("2026-06-02T10-00-00-000Z_sess-1.jsonl");
        let pricing_cache_path = dir.path().join("pricing-cache.json");
        let mut file = std::fs::File::create(&transcript).unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-06-02T10:00:00.000Z","costUSD":15.91,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        )
        .unwrap();

        let observed_at = jiff::Timestamp::from_second(1_780_394_400).unwrap();
        let mut prior = rimz::agents::context::record::AgentContextRecord::new(
            "claude",
            "sess-1",
            rimz::agents::AgentContext::new("claude", observed_at),
        );
        prior.transcript_path = Some(transcript.to_string_lossy().into_owned());
        prior.context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(0.0),
            ..rimz::agents::AgentCost::default()
        });

        let mut skipped = None;
        supplement_realtime_cost(
            rimz::agents::definition_by_kind("claude").unwrap(),
            "sess-1",
            &pricing_cache_path,
            false,
            Some(&prior),
            &mut skipped,
        );
        assert!(skipped.is_none());

        let mut refresh = None;
        supplement_realtime_cost(
            rimz::agents::definition_by_kind("claude").unwrap(),
            "sess-1",
            &pricing_cache_path,
            true,
            Some(&prior),
            &mut refresh,
        );

        let refresh = refresh.expect("turn end reconciles resumed cost");
        let cost = refresh
            .context
            .cost
            .as_set()
            .and_then(|cost| cost.total_cost_usd)
            .expect("supplemented total cost");
        assert!((cost - 15.91).abs() < 1e-9);
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(transcript.to_string_lossy().as_ref())
        );
        assert!(refresh.transcript_stat.is_some());

        prior.context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(99.0),
            ..rimz::agents::AgentCost::default()
        });
        let mut no_downgrade = None;
        supplement_realtime_cost(
            rimz::agents::definition_by_kind("claude").unwrap(),
            "sess-1",
            &pricing_cache_path,
            true,
            Some(&prior),
            &mut no_downgrade,
        );
        assert!(no_downgrade.is_none());
    }

    #[test]
    fn opencode_wal_commit_refreshes_realtime_cost() {
        let dir = tempfile::TempDir::new().unwrap();
        let transcript = dir.path().join("opencode.db");
        let pricing_cache_path = dir.path().join("pricing-cache.json");
        let connection = rusqlite::Connection::open(&transcript).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;\
                 PRAGMA wal_autocheckpoint = 0;\
                 CREATE TABLE message (id TEXT, session_id TEXT, data TEXT);",
            )
            .unwrap();
        let initial = r#"{"cost":1.25,"modelID":"gpt","providerID":"openai","time":{"created":1750000000000},"tokens":{"input":10,"output":5}}"#;
        let updated = r#"{"cost":9.75,"modelID":"gpt","providerID":"openai","time":{"created":1750000000000},"tokens":{"input":10,"output":5}}"#;
        assert_eq!(initial.len(), updated.len());
        connection
            .execute(
                "INSERT INTO message (id, session_id, data) VALUES ('msg', 'sess-1', ?1)",
                [initial],
            )
            .unwrap();

        let adapter = rimz::agents::definition_by_kind("opencode").unwrap();
        let initial_stat = adapter.transcript_stat(&transcript).unwrap();
        assert!(initial_stat.companion.is_some());
        let main_before = rimz::agents::TranscriptStat::from_path(&transcript).unwrap();
        let observed_at = jiff::Timestamp::from_second(1_750_000_000).unwrap();
        let mut prior = rimz::agents::context::record::AgentContextRecord::new(
            "opencode",
            "sess-1",
            rimz::agents::AgentContext::new("opencode", observed_at),
        );
        prior.transcript_path = Some(transcript.to_string_lossy().into_owned());
        prior.transcript_stat = Some(initial_stat);
        prior.context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(1.25),
            ..rimz::agents::AgentCost::default()
        });

        let mut unchanged = None;
        supplement_realtime_cost(
            adapter,
            "sess-1",
            &pricing_cache_path,
            true,
            Some(&prior),
            &mut unchanged,
        );
        assert!(unchanged.is_none(), "an exact logical stat is a fast hit");

        connection
            .execute("UPDATE message SET data = ?1 WHERE id = 'msg'", [updated])
            .unwrap();
        let main_after = rimz::agents::TranscriptStat::from_path(&transcript).unwrap();
        let updated_stat = adapter.transcript_stat(&transcript).unwrap();
        assert_eq!(main_after, main_before, "the commit stayed in the held WAL");
        assert_ne!(updated_stat.companion, initial_stat.companion);

        let mut refresh = None;
        supplement_realtime_cost(
            adapter,
            "sess-1",
            &pricing_cache_path,
            true,
            Some(&prior),
            &mut refresh,
        );

        let refresh = refresh.expect("the WAL change invalidates turn-end cost");
        assert_eq!(refresh.transcript_stat, Some(updated_stat));
        assert_eq!(
            refresh
                .context
                .cost
                .into_set()
                .and_then(|cost| cost.total_cost_usd),
            Some(9.75)
        );
    }

    fn test_agent() -> AgentState {
        let now = jiff::Timestamp::now();
        rimz::testkit::agent_state("claude", "sess-1", now)
    }
}
