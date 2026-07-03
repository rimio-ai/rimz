use super::*;

use std::borrow::Cow;

mod chat;
mod context;
mod delivery;
mod identity;
mod observe;
mod rotate;

use chat::*;
use context::*;
use delivery::*;
use identity::{
    agent_identity_env, env_run_id, validate_agent_name_env, validate_non_empty_identity_env,
};
use observe::{expiry_scope_for_event_name, record_lifecycle_observation};
use rotate::*;

pub(super) use identity::fill_root_launch_identity;
#[cfg(test)]
pub(super) use observe::append_lifecycle_event;
#[cfg(test)]
use observe::event_lifecycle_observation;

pub(super) fn handle_lifecycle_hook(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    globals: &GlobalFlags,
) -> Result<()> {
    let agent_id = payload_agent_id(payload);
    let fallback_expiry_scope = expiry_scope_for_event_name(agent, event_name);
    let fallback_expiry = match (agent_id, fallback_expiry_scope) {
        (Some(agent_id), Some(scope)) => Some((agent_id, scope)),
        _ => None,
    };
    let recorded = record_lifecycle_observation(
        workspace,
        ledger,
        agent,
        event_name,
        payload,
        globals,
        fallback_expiry,
    );
    record_native_answer(workspace, ledger, agent, event_name, payload);
    let model_hint = recorded
        .as_ref()
        .and_then(|recorded| recorded.model_hint.as_deref());
    let turn_ended = recorded.as_ref().is_some_and(|recorded| {
        matches!(
            recorded.observation.signal,
            LifecycleSignal::TurnEnded { .. }
        )
    });
    if let Some(agent_id) = agent_id {
        let observed_turn_error = recorded
            .as_ref()
            .and_then(|recorded| recorded.observation.turn_error.clone());
        manage_agent_context(AgentContextHook {
            workspace,
            ledger,
            agent,
            context: LifecycleEventContext {
                event_name,
                payload,
                agent_id,
                model_hint,
                turn_ended,
                observed_turn_error,
            },
        });
    }
    if let Some(recorded) = recorded.as_ref() {
        record_run_lifecycle(ledger, agent, event_name, payload, recorded);
        if let Err(err) =
            record_chat_conversation(workspace, ledger, agent, event_name, payload, recorded)
        {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to record chat entry",
            );
        }
        confirm_sent_message_for_lifecycle(ledger, agent, recorded, &workspace.session_name);
        if recorded.observation.signal == LifecycleSignal::Ended
            && let Some(agent_id) = agent_id
        {
            let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
            if let Err(err) = ledger.archive_messages_for_card(
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
        spawn_queue_delivery_if_checkpoint(workspace, ledger, agent, recorded);
        if recorded.appended_lifecycle {
            spawn_auto_rotation_if_due(workspace, ledger);
        }
    }
    Ok(())
}

struct RecordedLifecycle {
    model_hint: Option<String>,
    observation: AgentLifecycleObservation,
    appended_lifecycle: bool,
}

struct AgentContextHook<'a> {
    workspace: &'a ResolvedWorkspace,
    ledger: &'a Ledger,
    agent: &'a dyn AgentAdapter,
    context: LifecycleEventContext<'a>,
}

struct LifecycleEventContext<'a> {
    event_name: &'a str,
    payload: &'a Value,
    agent_id: &'a str,
    model_hint: Option<&'a str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
}

struct ContextSidecarInput<'a> {
    workspace: &'a ResolvedWorkspace,
    ledger: &'a Ledger,
    agent: &'a dyn AgentAdapter,
    event_name: &'a str,
    payload: &'a Value,
    context_agent_id: &'a str,
    model_hint: Option<&'a str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
}

fn record_run_lifecycle(
    ledger: &Ledger,
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
        ledger.paths(),
        &run_id,
        agent.descriptor().kind,
        &recorded.observation,
        last_message,
    ) {
        Ok(Some(record)) => {
            if let Err(err) = rimz::ledger::wakeup::wake_run(ledger.runtime_paths(), &record) {
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

    fn test_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace_id =
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
        let paths = rimz::ledger::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = rimz::ledger::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        let ledger = Ledger::open(paths, runtime).unwrap();
        (dir, ledger)
    }

    fn workspace_id() -> rimz::ids::WorkspaceId {
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"))
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
        observation.launch.team = Some("pcr".to_owned());
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
        let (_dir, ledger) = test_ledger();
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
        ledger
            .record_sent_message(&command, "session")
            .unwrap()
            .expect("command sent");
        ledger
            .record_sent_message(&prompt, "session")
            .unwrap()
            .expect("prompt sent");

        let compact_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::Compacting,
        );
        confirm_sent_message_for_lifecycle(
            &ledger,
            &rimz::agents::ClaudeAdapter,
            &RecordedLifecycle {
                model_hint: None,
                observation: compact_observation,
                appended_lifecycle: false,
            },
            "session",
        );
        let messages = ledger.list_messages().unwrap();
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
            &ledger,
            &rimz::agents::ClaudeAdapter,
            &RecordedLifecycle {
                model_hint: None,
                observation: real_observation,
                appended_lifecycle: false,
            },
            "session",
        );
        let messages = ledger.list_messages().unwrap();
        assert!(
            messages
                .iter()
                .all(|message| message.message_id != prompt.message_id),
            "delivered prompt self-cleans from the live queue"
        );
        assert!(
            ledger
                .read_events()
                .unwrap()
                .iter()
                .any(|event| event.method == "message.delivered"),
            "terminal delivery event is logged"
        );
    }

    #[test]
    fn turn_end_supplements_partial_realtime_cost_from_prior_transcript() {
        let dir = tempfile::TempDir::new().unwrap();
        let transcript = dir.path().join("2026-06-02T10-00-00-000Z_sess-1.jsonl");
        let mut file = std::fs::File::create(&transcript).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","model":"gpt-5","usage":{{"input":100,"output":50,"cost":{{"total":0.42}}}}}}}}"#
        )
        .unwrap();

        let observed_at = jiff::Timestamp::from_second(1_780_394_400).unwrap();
        let mut prior = rimz::ledger::agent_context::new_record(
            "pi",
            "sess-1",
            rimz::ledger::agent_context::empty_context("pi", observed_at),
        );
        prior.transcript_path = Some(transcript.to_string_lossy().into_owned());
        prior.context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(0.25),
            ..rimz::agents::AgentCost::default()
        });
        prior.context.tokens = Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(128_000),
            used_percentage: Some(10),
            remaining_percentage: None,
            current_usage: Some(rimz::agents::AgentCurrentUsage {
                input_tokens: Some(12_800),
                output_tokens: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            }),
        });

        let mut skipped = None;
        supplement_realtime_cost(
            &rimz::agents::PiAdapter,
            "sess-1",
            false,
            Some(&prior),
            &mut skipped,
        );
        assert!(skipped.is_none());

        let mut refresh = None;
        supplement_realtime_cost(
            &rimz::agents::PiAdapter,
            "sess-1",
            true,
            Some(&prior),
            &mut refresh,
        );

        let refresh = refresh.expect("turn end supplements cost");
        let cost = refresh
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd)
            .expect("supplemented total cost");
        assert!((cost - 0.42).abs() < 1e-9);
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(transcript.to_string_lossy().as_ref())
        );
        assert!(refresh.transcript_stat.is_some());
        assert_eq!(
            refresh
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.used_percentage),
            Some(10),
            "the reconciling walk keeps live tokens when Pi's JSONL has none"
        );

        let workspace_id =
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
        let runtime = rimz::ledger::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        rimz::ledger::agent_context::merge_local_context(
            &runtime,
            "pi",
            "sess-1",
            Some(prior),
            refresh,
            observed_at,
        )
        .unwrap();
        let merged = rimz::ledger::agent_context::read_one(&runtime, "pi", "sess-1").unwrap();
        assert_eq!(
            merged
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd),
            Some(0.42),
            "the turn-end transcript walk overwrites the live push with the authoritative sum"
        );
    }

    fn test_agent() -> AgentState {
        let now = jiff::Timestamp::now();
        AgentState {
            agent_id: rimz::ids::AgentSessionId::from("sess-1"),
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
