use super::*;

use std::borrow::Cow;

#[cfg(test)]
use rimz::agents::AgentState;

mod context;
mod delivery;
mod identity;
mod observe;
mod rotate;
mod transcript;

use context::*;
use delivery::*;
use identity::{
    agent_identity_env, env_run_id, validate_agent_name_env, validate_non_empty_identity_env,
};
use observe::record_lifecycle_observation;
use rotate::*;
use transcript::*;

pub(super) use identity::fill_root_launch_identity;
#[cfg(test)]
pub(super) use observe::append_lifecycle_event;
#[cfg(test)]
use observe::event_lifecycle_observation;

pub(crate) fn handle_lifecycle_hook(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    owner_pid: Option<u32>,
    globals: &GlobalFlags,
) -> Result<()> {
    let agent_id = payload_agent_id(payload);
    let recorded = record_lifecycle_observation(
        workspace, store, agent, event_name, payload, owner_pid, globals,
    );
    let assistant_message = record_assistant_response(
        workspace,
        store,
        agent,
        event_name,
        payload,
        recorded.as_ref(),
    );
    if let (Some(run_id), Some((agent_id, message))) = (env_run_id(), assistant_message)
        && let Err(err) = rimz::harness::run::record_assistant_message(
            store.paths(),
            &run_id,
            agent.descriptor().kind,
            &agent_id,
            message,
        )
    {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            run_id = %run_id,
            error = %err,
            "lifecycle: failed to seed supervised response",
        );
    }
    record_native_answer(
        workspace,
        store,
        agent,
        event_name,
        payload,
        recorded.as_ref(),
    );
    let model_hint = recorded
        .as_ref()
        .and_then(|recorded| recorded.model_hint.as_deref());
    let turn_ended = recorded.as_ref().is_some_and(|recorded| {
        matches!(
            recorded.observation.signal,
            LifecycleSignal::TurnEnded { .. } | LifecycleSignal::TurnInterrupted
        )
    });
    let transcript_path = recorded
        .as_ref()
        .and_then(|recorded| recorded.observation.transcript_path.as_deref());
    if let Some(agent_id) = agent_id {
        let observed_turn_error = recorded
            .as_ref()
            .and_then(|recorded| recorded.observation.turn_error.clone());
        manage_agent_context(AgentContextHook {
            workspace,
            store,
            agent,
            context: LifecycleEventContext {
                event_name,
                payload,
                agent_id,
                model_hint,
                transcript_path,
                turn_ended,
                observed_turn_error,
            },
        });
    }
    if let Some(recorded) = recorded.as_ref() {
        record_run_lifecycle(store, agent, event_name, payload, recorded);
        let delivered =
            confirm_sent_message_for_lifecycle(store, agent, recorded, &workspace.session_name);
        record_user_input_for_lifecycle(
            workspace,
            agent,
            recorded,
            &delivered,
            env_run_id().is_some(),
            user_input_state_root(store),
        );
        if let Err(err) = record_conversation(
            workspace, store, agent, event_name, payload, recorded, &delivered,
        ) {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to record transcript entry",
            );
        }
        if recorded.observation.signal == LifecycleSignal::Ended
            && let Some(agent_id) = agent_id
        {
            let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
            if let Err(err) = store.archive_messages_watching_card(
                &kind,
                &rimz::ids::AgentSessionId::from(agent_id),
                recorded.observation.agent_name.as_deref(),
                &workspace.session_name,
            ) {
                warn!(
                    error = %err,
                    kind = agent.descriptor().kind,
                    agent_id,
                    "lifecycle: failed to archive messages watching ended agent",
                );
            }
            if let Err(err) = store.archive_messages_for_card(
                &kind,
                &rimz::ids::AgentSessionId::from(agent_id),
                recorded.observation.agent_name.as_deref(),
                "receiver ended",
                &workspace.session_name,
            ) {
                warn!(
                    error = %err,
                    kind = agent.descriptor().kind,
                    agent_id,
                    "lifecycle: failed to archive receiver messages",
                );
            }
        }
        spawn_queue_delivery_if_checkpoint(workspace, store, agent, recorded);
        if recorded.appended_lifecycle {
            spawn_auto_rotation_if_due(workspace, store);
        }
    }
    Ok(())
}

fn user_input_state_root(_store: &Store) -> Option<&std::path::Path> {
    #[cfg(test)]
    {
        _store.paths().root.ancestors().nth(3)
    }
    #[cfg(not(test))]
    None
}

struct RecordedLifecycle {
    model_hint: Option<String>,
    observation: AgentLifecycleObservation,
    appended_lifecycle: bool,
    waiting_cleared: bool,
}

struct AgentContextHook<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    agent: &'a dyn AgentAdapter,
    context: LifecycleEventContext<'a>,
}

struct LifecycleEventContext<'a> {
    event_name: &'a str,
    payload: &'a Value,
    agent_id: &'a str,
    model_hint: Option<&'a str>,
    transcript_path: Option<&'a str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
}

struct ContextSidecarInput<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    agent: &'a dyn AgentAdapter,
    event_name: &'a str,
    payload: &'a Value,
    context_agent_id: &'a str,
    model_hint: Option<&'a str>,
    transcript_path: Option<&'a str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
}

fn record_run_lifecycle(
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    recorded: &RecordedLifecycle,
) {
    let Some(run_id) = env_run_id() else {
        return;
    };
    let last_message = rimz::harness::run::terminal_status_for_signal(&recorded.observation.signal)
        .is_some()
        .then(|| agent.last_assistant_message(event_name, payload, &recorded.observation))
        .flatten();
    match rimz::harness::run::record_lifecycle(
        store.paths(),
        &run_id,
        agent.descriptor().kind,
        &recorded.observation,
        last_message,
    ) {
        Ok(Some(record)) => {
            let cost_usd = recorded
                .observation
                .agent_id
                .as_ref()
                .and_then(|agent_id| {
                    rimz::store::agent_context::read_one(
                        store.runtime_paths(),
                        agent.descriptor().kind,
                        agent_id.as_str(),
                    )
                })
                .and_then(|record| record.context.cost)
                .and_then(|cost| cost.total_cost_usd);
            let token_totals = record
                .agent_id
                .as_ref()
                .zip(record.transcript_path.as_deref())
                .and_then(|(agent_id, transcript_path)| {
                    let prices = rimz::agents::pricing::cached_book(
                        &store.runtime_paths().shared_pricing_cache_path(),
                    );
                    rimz::agents::spending::session_token_totals(
                        agent,
                        agent_id.as_str(),
                        std::path::Path::new(transcript_path),
                        &prices,
                    )
                });
            let record = rimz::harness::run::record_spend(
                store.paths(),
                &record.run_id,
                cost_usd,
                token_totals.map(|totals| totals.input),
                token_totals.map(|totals| totals.output),
            )
            .unwrap_or(record);
            if let Err(err) = rimz::store::wakeup::wake_run(store.runtime_paths(), &record) {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    run_id = %run_id,
                    error = %err,
                    "lifecycle: failed to wake the completed run",
                );
            }
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                run_id = %run_id,
                error = %err,
                "lifecycle: failed to update the supervised run",
            );
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
        let paths = rimz::store::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = rimz::store::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        (dir, store)
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
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
            worktree_branch: None,
            session_name: "hooks-test".to_owned(),
            mux_hint: None,
        }
    }

    fn turn_started_recorded() -> RecordedLifecycle {
        let mut observation = AgentLifecycleObservation::new(
            Some(rimz::ids::AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnStarted,
        );
        observation.worktree_path = Some("/tmp/hooks-test/worktree".to_owned());
        RecordedLifecycle {
            model_hint: None,
            observation,
            appended_lifecycle: false,
            waiting_cleared: false,
        }
    }

    #[test]
    fn lifecycle_event_observation_trims_carry_forward_fields_after_identity() {
        let mut observation = AgentLifecycleObservation::new(
            Some(rimz::ids::AgentSessionId::from("sess-1")),
            LifecycleSignal::Registered,
        );
        observation.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        observation.worktree_path = Some("/tmp/project".to_owned());
        observation.worktree_branch = Some("feature".to_owned());
        observation.launch.role = Some("coder".to_owned());
        observation.launch.team = Some("forge".to_owned());
        observation.launch.channel = Some("event-log".to_owned());
        observation.launch.profile = Some("claude-coder".to_owned());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%1"));

        let identity = event_lifecycle_observation(&observation);
        assert_eq!(
            identity.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );
        assert_eq!(identity.worktree_path.as_deref(), Some("/tmp/project"));
        assert_eq!(identity.worktree_branch.as_deref(), Some("feature"));
        assert_eq!(identity.pane_id.as_ref().map(PaneId::raw), Some("%1"));

        observation.signal = LifecycleSignal::TurnStarted;
        let trimmed = event_lifecycle_observation(&observation);
        assert!(trimmed.transcript_path.is_none());
        assert!(trimmed.worktree_path.is_none());
        assert!(trimmed.worktree_branch.is_none());
        assert!(trimmed.launch.role.is_none());
        assert!(trimmed.launch.team.is_none());
        assert!(trimmed.launch.channel.is_none());
        assert!(trimmed.launch.profile.is_none());
        assert_eq!(trimmed.pane_id.as_ref().map(PaneId::raw), Some("%1"));
        assert_eq!(
            observation.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl"),
            "downstream run-record/context paths keep the full observation"
        );

        observation.signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        };
        let turn_end = event_lifecycle_observation(&observation);
        assert_eq!(
            turn_end.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl"),
            "a provider can first publish its authoritative path at turn end"
        );
        assert!(turn_end.worktree_path.is_none());
    }

    #[test]
    fn auto_rotation_decision_respects_threshold_and_debounce() {
        let threshold = crate::cli::workspace::DEFAULT_EVENT_LOG_ROTATE_BYTES;
        assert!(!auto_rotation_size_due(threshold - 1));
        assert!(auto_rotation_size_due(threshold));
        assert!(auto_rotation_stamp_due(None));
        assert!(!auto_rotation_stamp_due(Some(
            AUTO_ROTATE_DEBOUNCE - std::time::Duration::from_secs(1)
        )));
        assert!(auto_rotation_stamp_due(Some(AUTO_ROTATE_DEBOUNCE)));
    }

    #[test]
    fn lifecycle_confirms_matching_message_body() {
        let (_dir, store) = test_store();
        let agent = test_agent();
        let command = rimz::message::MessageRecord::new(
            workspace_id(),
            &agent,
            "/compact".to_owned(),
            true,
            rimz::message::DeliveryGate::Done,
        )
        .with_body(rimz::message::MessageBody::Command);
        let prompt = rimz::message::MessageRecord::new(
            workspace_id(),
            &agent,
            "real prompt".to_owned(),
            true,
            rimz::message::DeliveryGate::Done,
        );
        store
            .record_sent_message(&command, "session")
            .unwrap()
            .expect("command sent");
        store
            .record_sent_message(&prompt, "session")
            .unwrap()
            .expect("prompt sent");

        let compact_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::Compacting,
        );
        confirm_sent_message_for_lifecycle(
            &store,
            &rimz::agents::ClaudeAdapter,
            &RecordedLifecycle {
                model_hint: None,
                observation: compact_observation,
                appended_lifecycle: false,
                waiting_cleared: false,
            },
            "session",
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
            rimz::message::MessageStatus::Sent,
            "compaction cannot confirm the prompt behind it"
        );

        let mut real_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::TurnStarted,
        );
        real_observation.prompt = Some("real prompt".to_owned());
        confirm_sent_message_for_lifecycle(
            &store,
            &rimz::agents::ClaudeAdapter,
            &RecordedLifecycle {
                model_hint: None,
                observation: real_observation,
                appended_lifecycle: false,
                waiting_cleared: false,
            },
            "session",
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
    fn turn_started_records_only_unsupervised_user_inputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = test_workspace();
        let agent_state = test_agent();
        let human = rimz::message::MessageRecord::new(
            workspace_id(),
            &agent_state,
            "human prompt".to_owned(),
            true,
            rimz::message::DeliveryGate::Done,
        );
        let agent_message = human
            .clone()
            .with_sender(rimz::message::MessageSender::Agent {
                kind: rimz::ids::AgentKind::new_unchecked("codex"),
                name: None,
                profile: None,
                role: Some("coder".to_owned()),
                channel: None,
            });

        record_user_input_for_lifecycle(
            &workspace,
            &rimz::agents::ClaudeAdapter,
            &turn_started_recorded(),
            &[],
            false,
            Some(dir.path()),
        );
        record_user_input_for_lifecycle(
            &workspace,
            &rimz::agents::ClaudeAdapter,
            &turn_started_recorded(),
            std::slice::from_ref(&human),
            false,
            Some(dir.path()),
        );
        record_user_input_for_lifecycle(
            &workspace,
            &rimz::agents::ClaudeAdapter,
            &turn_started_recorded(),
            &[agent_message],
            false,
            Some(dir.path()),
        );
        record_user_input_for_lifecycle(
            &workspace,
            &rimz::agents::ClaudeAdapter,
            &turn_started_recorded(),
            &[human],
            true,
            Some(dir.path()),
        );

        let records = rimz::agents::spending::user_input::load_in(dir.path());
        assert_eq!(records.len(), 2);
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
        let mut prior = rimz::store::agent_context::new_record(
            "claude",
            "sess-1",
            rimz::store::agent_context::empty_context("claude", observed_at),
        );
        prior.transcript_path = Some(transcript.to_string_lossy().into_owned());
        prior.context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(0.0),
            ..rimz::agents::AgentCost::default()
        });

        let mut skipped = None;
        supplement_realtime_cost(
            &rimz::agents::ClaudeAdapter,
            "sess-1",
            &pricing_cache_path,
            false,
            Some(&prior),
            &mut skipped,
        );
        assert!(skipped.is_none());

        let mut refresh = None;
        supplement_realtime_cost(
            &rimz::agents::ClaudeAdapter,
            "sess-1",
            &pricing_cache_path,
            true,
            Some(&prior),
            &mut refresh,
        );

        let refresh = refresh.expect("turn end reconciles resumed cost");
        let cost = refresh
            .cost
            .as_ref()
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
            &rimz::agents::ClaudeAdapter,
            "sess-1",
            &pricing_cache_path,
            true,
            Some(&prior),
            &mut no_downgrade,
        );
        assert!(no_downgrade.is_none());
    }

    fn test_agent() -> AgentState {
        let now = jiff::Timestamp::now();
        rimz::testkit::agent_state("claude", "sess-1", now)
    }
}
