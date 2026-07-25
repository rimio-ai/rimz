use super::*;

#[cfg(test)]
use rimz::agents::AgentState;

mod active_time;
mod context;
mod delivery;
mod identity;
mod observe;
mod reactors;
mod transcript;

use context::*;
use delivery::*;
use identity::{
    agent_identity_env, env_run_id, validate_agent_name_env, validate_non_empty_identity_env,
};
use observe::{record_derived_lifecycle_observation, record_lifecycle_observation};
use reactors::ReactorCtx;
use transcript::*;

pub(super) use identity::fill_root_launch_identity;

pub(crate) fn handle_lifecycle_hook(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    decoded: &mut HookOutput,
    payload: &Value,
    ingress_owner: rimz::agents::HookIngressOwner,
    globals: &GlobalFlags,
) -> Result<()> {
    let agent_id = decoded.event_agent_id().cloned();
    let recorded =
        record_lifecycle_observation(workspace, store, agent, decoded, ingress_owner, globals);
    let event_name = decoded.event_name().to_owned();
    let mut events = recorded
        .as_ref()
        .map(|recorded| recorded.events.clone())
        .unwrap_or_default();
    let (derived_events, derived_rotation_due) = if recorded.as_ref().is_some_and(|recorded| {
        recorded.observation.agent_id.is_some() && recorded.observation.parent_agent_id.is_none()
    }) {
        derive_subagent_lifecycle(workspace, store, agent, ingress_owner, globals)
    } else {
        (Vec::new(), false)
    };
    events.extend(derived_events);
    if derived_rotation_due {
        spawn_auto_rotation(workspace);
    }
    let assistant_message =
        record_assistant_response(workspace, store, agent, decoded, recorded.as_ref());
    if let (Some(run_id), Some((agent_id, message))) = (env_run_id(), assistant_message)
        && let Err(err) = rimz::harness::run::record_assistant_message(
            store.paths(),
            &run_id,
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
    let model_hint = recorded
        .as_ref()
        .and_then(|recorded| recorded.model_hint.as_deref());
    let turn_ended = recorded.as_ref().is_some_and(|recorded| {
        matches!(
            recorded.observation.signal,
            LifecycleSignal::TurnEnded { .. } | LifecycleSignal::TurnInterrupted
        )
    });
    let tool_call = recorded.as_ref().and_then(|recorded| {
        let LifecycleSignal::ToolUsed {
            name: Some(name), ..
        } = &recorded.observation.signal
        else {
            return None;
        };
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        agent
            .spec()
            .tool_signature(payload)
            .map(|digest| (name, digest))
    });
    let tool_run = tool_call
        .as_ref()
        .map_or(rimz::agent_activity::ToolRun::Reset, |(tool, digest)| {
            rimz::agent_activity::ToolRun::Call { tool, digest }
        });
    let transcript_path = recorded
        .as_ref()
        .and_then(|recorded| recorded.observation.transcript_path.as_deref());
    let context_agent_id = recorded
        .as_ref()
        .and_then(|recorded| recorded.observation.agent_id.clone())
        .or_else(|| decoded.context_agent_id().cloned())
        .or_else(|| agent_id.clone());
    active_time::record(
        store,
        agent,
        decoded,
        recorded.as_ref(),
        context_agent_id.as_deref(),
        &event_name,
    );
    if let Some(agent_id) = context_agent_id {
        let parent_agent_id = recorded
            .as_ref()
            .and_then(|recorded| recorded.observation.parent_agent_id.as_deref());
        let parent_activity_id = recorded
            .as_ref()
            .filter(|recorded| {
                matches!(
                    recorded.observation.signal,
                    LifecycleSignal::SubagentStopped { .. }
                )
            })
            .and_then(|recorded| recorded.observation.parent_agent_id.as_deref());
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
            },
        });
    }
    let mut run_completion = None;
    if let Some(recorded) = recorded.as_ref() {
        let assistant_message =
            assistant_message_for_lifecycle(recorded, env_run_id().is_some(), || {
                decoded.final_message().map(ToOwned::to_owned)
            });
        run_completion = record_run_lifecycle(
            store,
            agent,
            &event_name,
            recorded,
            assistant_message.as_deref(),
        );
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
        let questions = match &recorded.observation.signal {
            LifecycleSignal::AwaitingInput { .. } => decoded.questions(),
            _ => &[],
        };
        if let Err(err) = record_conversation(
            workspace,
            store,
            agent,
            recorded,
            assistant_message.as_deref(),
            questions,
            &delivered,
        ) {
            warn!(
                agent = agent.spec().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to record transcript entry",
            );
        }
        if recorded.rotation_due {
            spawn_auto_rotation(workspace);
        }
    }
    reactors::dispatch(
        &ReactorCtx {
            workspace,
            store,
            primary_event_id: recorded
                .as_ref()
                .and_then(|recorded| recorded.primary_event_id.as_ref()),
            run_completion: run_completion.as_ref(),
        },
        &events,
    );
    Ok(())
}

fn assistant_message_for_lifecycle(
    recorded: &RecordedLifecycle,
    supervised_run: bool,
    extract: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let needs_run_message = supervised_run
        && rimz::harness::run::terminal_status_for_signal(&recorded.observation.signal).is_some();
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
        if !snapshot
            .agents
            .iter()
            .any(|state| state.kind.as_str() == kind && state.agent_id == *parent_id)
        {
            continue;
        }
        let prior = snapshot
            .agents
            .iter()
            .find(|state| state.kind.as_str() == kind && state.agent_id == *child_id);
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
        rotation_due |= recorded.rotation_due;
        events.extend(recorded.events);
    }
    (events, rotation_due)
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
    primary_event_id: Option<rimz::ids::EventId>,
    events: Vec<rimz::agents::LifecycleEvent>,
    rotation_due: bool,
    waiting_cleared: bool,
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
) -> Option<rimz::harness::run::RunRecord> {
    let run_id = env_run_id()?;
    match rimz::harness::run::record_lifecycle(
        store.paths(),
        &run_id,
        agent.spec().kind,
        &recorded.observation,
        assistant_message.map(ToOwned::to_owned),
    ) {
        Ok(Some(record)) => {
            let cost_usd = recorded
                .observation
                .agent_id
                .as_ref()
                .and_then(|agent_id| {
                    rimz::store::agent_context::read_one(
                        store.runtime_paths(),
                        agent.spec().kind,
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
            Some(record)
        }
        Ok(None) => None,
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

    #[test]
    fn native_session_end_removes_context_without_lifecycle_identity() {
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
        assert!(decoded.lifecycle().is_none());
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
            primary_event_id: None,
            events: Vec::new(),
            rotation_due: false,
            waiting_cleared: false,
        }
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
                primary_event_id: None,
                events: Vec::new(),
                rotation_due: false,
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
            rimz::agents::definition_by_kind("claude").unwrap(),
            &RecordedLifecycle {
                model_hint: None,
                observation: real_observation,
                primary_event_id: None,
                events: Vec::new(),
                rotation_due: false,
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
            rimz::agents::definition_by_kind("claude").unwrap(),
            &turn_started_recorded(),
            &[],
            false,
            Some(dir.path()),
        );
        record_user_input_for_lifecycle(
            &workspace,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &turn_started_recorded(),
            std::slice::from_ref(&human),
            false,
            Some(dir.path()),
        );
        record_user_input_for_lifecycle(
            &workspace,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &turn_started_recorded(),
            &[agent_message],
            false,
            Some(dir.path()),
        );
        record_user_input_for_lifecycle(
            &workspace,
            rimz::agents::definition_by_kind("claude").unwrap(),
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
        let mut prior = rimz::store::agent_context::new_record(
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
