use super::*;

use std::ffi::OsStr;
use std::io::Write as _;
use std::time::{Duration, Instant};

use crate::agents::definition::{ConcernCoverage, IntegrationConcern};
use crate::agents::{
    AgentErr, AgentHookClass, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    TranscriptPosition, TranscriptRole,
};
use serde_json::json;

fn local_state(observation: &LocalSessionObservation) -> &LocalSessionState {
    let LocalSessionProjection::Lifecycle(state) = &observation.projection else {
        panic!("Kiro discovery must project lifecycle")
    };
    state
}

fn records(payloads: &[Value]) -> String {
    payloads.iter().enumerate().map(|(index, payload)| {
        format!("{}\n", json!({"id": format!("record-{index}"), "timestamp": format!("2025-01-01T00:00:{index:02}Z"), "payload": payload}))
    }).collect()
}

fn refresh_fixture(root: &Path, fields: Value, payloads: &[Value]) -> PathBuf {
    let workspace = Path::new("/workspace/project");
    let id = "sess_33333333-3333-4333-8333-333333333333";
    let directory = root
        .join("sessions")
        .join(session::workspace_bucket(workspace).unwrap())
        .join(id);
    std::fs::create_dir_all(&directory).unwrap();
    let mut metadata = json!({"id": id, "schemaVersion": "1.0.0", "dataModelVersion": 1, "workspacePaths": [workspace], "createdAt": "2025-01-01T00:00:00Z"});
    metadata
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    std::fs::write(directory.join("session.json"), metadata.to_string()).unwrap();
    let transcript = directory.join("messages.jsonl");
    std::fs::write(&transcript, records(payloads)).unwrap();
    transcript
}

fn refresh_at(transcript: &Path) -> Option<LocalContextRefresh> {
    KiroAdapter.local_context_refresh(
        RefreshTrigger::Tick,
        &LocalContextRefreshCtx {
            login_env: &BTreeMap::new(),
            agent_id: transcript
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            model_hint: None,
            prior_session_name: None,
            current_transcript_path: None,
            prior_transcript_path: transcript.to_str(),
            prior_transcript_stat: None,
            prior_spend_fold: None,
            shared_pricing_cache_path: Path::new("/unused-pricing"),
        },
    )
}

#[test]
fn local_refresh_ignores_a_recorded_transcript_of_another_session() {
    let root = tempfile::tempdir().unwrap();
    let path = refresh_fixture(root.path(), json!({}), &[]);
    let refresh = KiroAdapter.local_context_refresh(
        RefreshTrigger::Tick,
        &LocalContextRefreshCtx {
            login_env: &BTreeMap::new(),
            agent_id: "sess_99999999-9999-4999-8999-999999999999",
            model_hint: None,
            prior_session_name: None,
            current_transcript_path: None,
            prior_transcript_path: path.to_str(),
            prior_transcript_stat: None,
            prior_spend_fold: None,
            shared_pricing_cache_path: Path::new("/unused-pricing"),
        },
    );
    assert!(refresh.is_none());
}

#[test]
fn local_refresh_reads_model_effort_and_title() {
    use crate::agents::{FieldPatch, LocalTokenPatch};
    let root = tempfile::tempdir().unwrap();
    let path = refresh_fixture(
        root.path(),
        json!({"modelId": "claude-sonnet-4.5", "effortLevel": "high", "title": "New Session"}),
        &[],
    );
    let patch = refresh_at(&path).unwrap().context;
    assert_eq!(
        patch.model_id,
        FieldPatch::Set("claude-sonnet-4.5".to_owned())
    );
    assert_eq!(patch.effort, FieldPatch::Set("high".to_owned()));
    assert_eq!(
        patch.session_name,
        FieldPatch::Set("New Session".to_owned())
    );
    assert_eq!(patch.tokens, LocalTokenPatch::PreserveEstablished(None));
    assert_eq!(patch.cost, FieldPatch::Keep);
}

#[test]
fn local_refresh_clears_absent_metadata_fields() {
    use crate::agents::FieldPatch;
    let root = tempfile::tempdir().unwrap();
    let path = refresh_fixture(root.path(), json!({}), &[]);
    let patch = refresh_at(&path).unwrap().context;
    assert_eq!(patch.model_id, FieldPatch::Clear);
    assert_eq!(patch.effort, FieldPatch::Clear);
    assert_eq!(patch.session_name, FieldPatch::Clear);
}

#[test]
fn local_refresh_keeps_metadata_when_it_cannot_be_validated() {
    use crate::agents::FieldPatch;
    let root = tempfile::tempdir().unwrap();
    let path = refresh_fixture(root.path(), json!({}), &[]);
    for metadata in [None, Some("not json"), Some(r#"{"id":"wrong"}"#)] {
        let metadata_path = path.parent().unwrap().join("session.json");
        if let Some(metadata) = metadata {
            std::fs::write(&metadata_path, metadata).unwrap();
        } else {
            std::fs::remove_file(&metadata_path).unwrap();
        }
        let refresh = refresh_at(&path);
        assert!(
            refresh.is_some(),
            "a recorded transcript still refreshes without readable metadata"
        );
        let patch = refresh.unwrap().context;
        assert_eq!(patch.model_id, FieldPatch::Keep);
        assert_eq!(patch.effort, FieldPatch::Keep);
        assert_eq!(patch.session_name, FieldPatch::Keep);
    }
}

fn refreshed_running_agent(path: &Path) -> crate::agents::AgentState {
    let at = "2025-01-01T00:00:00Z".parse().unwrap();
    let mut agent = crate::testkit::agent_state("kiro", "sess", at);
    agent.status = crate::agents::AgentStatus::Running;
    agent.turn_started_at = Some(at);
    let mut context = crate::agents::AgentContext::new("kiro", at);
    refresh_at(path)
        .unwrap()
        .context
        .apply(&mut context, KiroAdapter.spec());
    agent.context = Some(context);
    agent
}

#[test]
fn cancelled_refresh_releases_the_turn_waiter() {
    use crate::agents::{AgentStatus, FieldPatch, TurnCompletion, TurnSettleOutcome};
    let root = tempfile::tempdir().unwrap();
    let path = refresh_fixture(
        root.path(),
        json!({}),
        &[
            json!({"type": "turn_start", "executionId": "turn"}),
            json!({"type": "turn_end", "stopReason": "cancelled"}),
            json!({"type": "session_metadata", "key": "contextUsage", "value": {"usagePercentage": 10}}),
        ],
    );
    let agent = refreshed_running_agent(&path);
    assert_eq!(agent.effective_status(), AgentStatus::Idle);
    assert_eq!(
        TurnCompletion::of(agent.effective_status(), agent.turn_started_at),
        TurnCompletion::Completed
    );
    let settle = agent.context.unwrap().settle.unwrap();
    assert_eq!(settle.outcome, TurnSettleOutcome::Interrupted);
    assert_eq!(settle.at, "2025-01-01T00:00:01Z".parse().unwrap());
    std::fs::write(
        &path,
        records(&[json!({"type": "turn_start", "executionId": "next"})]),
    )
    .unwrap();
    let patch = refresh_at(&path).unwrap().context;
    assert_eq!(patch.settle, FieldPatch::Clear);
    assert_eq!(patch.turn_error, FieldPatch::Clear);
}

#[test]
fn question_refresh_keeps_the_turn_waiter_open() {
    use crate::agents::{AgentStatus, TurnCompletion, TurnSettleOutcome};
    let root = tempfile::tempdir().unwrap();
    let path = refresh_fixture(
        root.path(),
        json!({}),
        &[
            json!({"type": "turn_start", "executionId": "turn"}),
            json!({"type": "pending_interaction", "interactionType": "user_input", "toolCallId": "ask", "question": "Which target?"}),
        ],
    );
    let agent = refreshed_running_agent(&path);
    assert_eq!(agent.effective_status(), AgentStatus::Waiting);
    assert_eq!(
        TurnCompletion::of(agent.effective_status(), agent.turn_started_at),
        TurnCompletion::Open
    );
    let settle = agent.context.unwrap().settle.unwrap();
    assert_eq!(settle.outcome, TurnSettleOutcome::NativeWait);
    assert_eq!(settle.at, "2025-01-01T00:00:01Z".parse().unwrap());
}

#[test]
fn failed_refresh_records_error_without_releasing_the_turn_waiter() {
    use crate::agents::{AgentStatus, TurnCompletion, TurnErrorClass};
    let root = tempfile::tempdir().unwrap();
    for reason in ["error", "refusal"] {
        let path = refresh_fixture(
            root.path(),
            json!({}),
            &[
                json!({"type": "turn_start", "executionId": "turn"}),
                json!({"type": "turn_end", "stopReason": reason}),
                json!({"type": "session_metadata", "key": "contextUsage", "value": {"usagePercentage": 10}}),
            ],
        );
        let agent = refreshed_running_agent(&path);
        assert!(agent.context.as_ref().unwrap().turn_error.is_some());
        assert_eq!(agent.effective_status(), AgentStatus::Running);
        assert_eq!(
            TurnCompletion::of(agent.effective_status(), agent.turn_started_at),
            TurnCompletion::Open
        );
        let context = agent.context.unwrap();
        assert!(context.settle.is_none());
        let error = context.turn_error.unwrap();
        assert_eq!(error.class, TurnErrorClass::Failed);
        assert_eq!(error.at, "2025-01-01T00:00:01Z".parse().unwrap());
        assert!(error.label.is_none());
    }
}

#[test]
fn native_questions_raise_waiting_without_consent_round() {
    let lines = records(&[json!({
        "type": "pending_interaction", "interactionType": "user_input",
        "toolCallId": "question", "question": "Which target?", "executionId": "turn",
        "options": [{"title": "Linux", "recommended": true}, {"title": "macOS"}]
    })]);
    let folded = session::fold_for_test(&lines);
    assert_eq!(folded.0, crate::agents::AgentStatus::Waiting);
    assert_eq!(folded.2.as_deref(), Some("Which target?"));
    assert_eq!(folded.4, Some("2025-01-01T00:00:00Z".parse().unwrap()));
}

#[test]
fn approval_rounds_resolve_only_the_matching_wait() {
    let mut payloads = vec![
        json!({"type": "pending_interaction", "interactionType": "tool_approval", "toolCallId": "tool", "question": "First", "consentRound": 1}),
        json!({"type": "pending_interaction", "interactionType": "tool_approval", "toolCallId": "tool", "question": "Second", "consentRound": 2}),
        json!({"type": "interaction_resolved", "toolCallId": "tool", "consentRound": 1}),
    ];
    for stale_round in [None, Some(1), Some(0)] {
        if let Some(round) = stale_round {
            payloads.push(json!({"type": "interaction_resolved", "toolCallId": "tool", "consentRound": round}));
        }
        let folded = session::fold_for_test(&records(&payloads));
        assert_eq!(folded.0, crate::agents::AgentStatus::Waiting);
        assert_eq!(folded.2.as_deref(), Some("Second"));
    }
    payloads.push(json!({"type": "interaction_resolved", "toolCallId": "tool", "consentRound": 2}));
    assert_eq!(
        session::fold_for_test(&records(&payloads)).0,
        crate::agents::AgentStatus::Running
    );
}

#[test]
fn next_turn_discards_pending_approvals_before_tool_activity() {
    let lines = records(&[
        json!({"type": "pending_interaction", "interactionType": "tool_approval", "toolCallId": "old", "question": "Approve?"}),
        json!({"type": "turn_end", "stopReason": "end_turn"}),
        json!({"type": "turn_start", "executionId": "next"}),
        json!({"type": "tool_call", "toolCallId": "new", "toolName": "fs_write", "status": "completed"}),
    ]);
    let folded = session::fold_for_test(&lines);
    assert_eq!(folded.1, crate::agents::TurnPhase::Acting);
    assert!(folded.2.is_none());
}

#[test]
fn non_user_sources_stay_out_of_transcripts_and_final_replies() {
    for source in ["hook", "api"] {
        let lines = records(&[
            json!({"type": "user", "content": "human"}),
            json!({"type": "user", "source": "chat", "content": "chat"}),
            json!({"type": "user", "source": "steer", "content": "steer"}),
            json!({"type": "user", "source": "future", "content": "future"}),
            json!({"type": "assistant", "operationType": "Say", "content": "reply"}),
            json!({"type": "user", "source": source, "content": "injected"}),
        ]);
        let messages = session::messages(&lines);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["human", "chat", "steer", "future", "reply"],
            "{source}"
        );
        assert_eq!(session::last_reply_in(&lines).as_deref(), Some("reply"));
    }
}

#[test]
fn non_user_sources_do_not_replace_the_latest_prompt() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let id = "sess_33333333-3333-4333-8333-333333333333";
    let session_dir = root
        .path()
        .join("sessions")
        .join(session::workspace_bucket(workspace).unwrap())
        .join(id);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("session.json"), json!({"id": id, "schemaVersion": "1.0.0", "dataModelVersion": 1, "workspacePaths": [workspace], "createdAt": "2025-01-01T00:00:00Z"}).to_string()).unwrap();
    for source in ["hook", "api"] {
        std::fs::write(
            session_dir.join("messages.jsonl"),
            records(&[
                json!({"type": "user", "content": "human"}),
                json!({"type": "user", "source": source, "content": "injected"}),
            ]),
        )
        .unwrap();
        let observations = session::discover_under(root.path(), workspace);
        assert_eq!(
            local_state(&observations[0]).latest_prompt.as_deref(),
            Some("human"),
            "{source}"
        );
    }
}

#[test]
fn native_hooks_decode_session_turn_tool_and_stop() {
    let agent = crate::agents::find_definition("kiro").expect("Kiro definition");
    let definition = agent.spec();
    assert!(definition.has_wired_hook_install());
    assert!(matches!(
        definition.concern_coverage(IntegrationConcern::TurnLifecycle),
        ConcernCoverage::Wired { .. }
    ));

    let session = json!({ "session_id": "sess_redacted", "cwd": "/workspace/project" });
    let cases = [
        ("SessionStart", session.clone()),
        (
            "UserPromptSubmit",
            json!({ "session_id": "sess_redacted", "prompt": "fix the build" }),
        ),
        (
            "PostToolUse",
            json!({ "session_id": "sess_redacted", "tool_name": "fs_write", "tool_response": "ok" }),
        ),
        ("Stop", session),
        (
            "SessionEnd",
            json!({"session_id": "sess_redacted", "reason": "shutdown"}),
        ),
    ];
    for (event, payload) in cases {
        let decoded = agent.decode_hook(event, &payload).expect("hook decodes");
        assert_eq!(decoded.class(), AgentHookClass::Lifecycle, "{event}");
        assert_eq!(decoded.records_progress(), event != "SessionEnd", "{event}");
        assert_eq!(decoded.json_reply().cloned(), None, "{event} stays neutral");
        let observation = decoded.lifecycle().cloned().expect("lifecycle observation");
        assert_eq!(
            observation.agent_id.as_ref().map(|id| id.as_str()),
            Some("sess_redacted")
        );
        match event {
            "SessionEnd" => {
                assert!(decoded.ends_session());
                insta::assert_debug_snapshot!(observation.signal, @"Ended");
            }
            "SessionStart" => assert!(matches!(observation.signal, LifecycleSignal::Registered)),
            "UserPromptSubmit" => {
                assert!(matches!(
                    observation.signal,
                    LifecycleSignal::TurnStarted { .. }
                ));
                assert_eq!(observation.prompt.as_deref(), Some("fix the build"));
            }
            "PostToolUse" => assert!(matches!(
                observation.signal,
                LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: true,
                    ..
                }
            )),
            _ => assert!(matches!(
                observation.signal,
                LifecycleSignal::TurnEnded { errored: false, .. }
            )),
        }
    }

    let unknown = agent
        .decode_hook("PreToolUse", &json!({}))
        .expect("unknown hook decodes");
    assert!(!unknown.records_progress());
    assert!(unknown.lifecycle().is_none());
}

#[test]
fn stop_final_message_is_the_reply_that_closed_the_turn() {
    let shell = include_str!("tests/fixtures/stock_shell_2_21_4/messages.jsonl");
    assert_eq!(
        session::last_reply_in(shell).as_deref(),
        Some("redacted reply")
    );
    let awaiting = format!(
        "{shell}{{\"id\":\"u\",\"timestamp\":\"2026-09-14T05:49:00Z\",\"payload\":{{\"type\":\"user\",\"content\":\"next\"}}}}\n"
    );
    assert!(session::last_reply_in(&awaiting).is_none());
    assert!(session::last_reply_in("").is_none());
}

#[test]
fn stock_store_transcript_context_and_lifecycle_are_normalized() {
    let definition = KiroAdapter.spec();
    assert!(definition.capabilities.local_session_discovery);
    assert!(definition.capabilities.transcript_tail_context);
    assert!(!definition.has_authoritative_account_spend());

    let ping = include_str!("tests/fixtures/stock_ping/messages.jsonl");
    let messages = KiroAdapter.parse_transcript_messages(ping);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "ping");
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "pong");
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("bootstrap"))
    );

    let history = include_str!("tests/fixtures/root/sess_redacted.history");
    let acp = include_str!("tests/fixtures/acp/11111111-1111-4111-8111-111111111111.jsonl");
    assert!(KiroAdapter.parse_transcript_messages(history).is_empty());
    assert!(KiroAdapter.parse_transcript_messages(acp).is_empty());
}

#[test]
fn unsupported_history_stays_out_of_spending_discovery() {
    assert!(
        KiroAdapter
            .transcript_files(&std::collections::BTreeMap::new())
            .is_empty()
    );
    assert!(
        KiroAdapter
            .spending_sources(&std::collections::BTreeMap::new())
            .is_empty()
    );
}

#[test]
fn targeted_transcript_lookup_checks_only_exact_candidates_across_sorted_buckets() {
    let home = tempfile::tempdir().unwrap();
    let session_id = "sess_33333333-3333-4333-8333-333333333333";
    let other_id = "sess_44444444-4444-4444-8444-444444444444";
    let first_workspace = Path::new("/workspace/first");
    let second_workspace = Path::new("/workspace/second");
    let first = home
        .path()
        .join("sessions")
        .join(session::workspace_bucket(first_workspace).unwrap());
    let second = home
        .path()
        .join("sessions")
        .join(session::workspace_bucket(second_workspace).unwrap());
    std::fs::create_dir_all(first.join(other_id)).unwrap();
    std::fs::write(first.join(other_id).join("session.json"), "not json").unwrap();
    std::fs::write(first.join(other_id).join("messages.jsonl"), "not json").unwrap();

    let target = second.join(session_id);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("session.json"),
        serde_json::json!({
            "id": session_id,
            "schemaVersion": "1.0.0",
            "dataModelVersion": 1,
            "workspacePaths": [second_workspace],
            "createdAt": "2025-01-03T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(target.join("messages.jsonl"), "{}").unwrap();

    assert_eq!(
        session::transcript_for_session_under(home.path(), session_id),
        Some(target.join("messages.jsonl"))
    );
    assert!(session::transcript_for_session_under(home.path(), "../escape").is_none());
}

#[test]
fn local_context_refresh_gates_an_unchanged_transcript_stat() {
    let home = tempfile::tempdir().unwrap();
    let session_id = "sess_33333333-3333-4333-8333-333333333333";
    let workspace = Path::new("/workspace/project");
    let session_dir = home
        .path()
        .join("sessions")
        .join(session::workspace_bucket(workspace).unwrap())
        .join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("session.json"),
        serde_json::json!({
            "id": session_id,
            "schemaVersion": "1.0.0",
            "dataModelVersion": 1,
            "workspacePaths": [workspace],
            "createdAt": "2025-01-03T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    let transcript = session_dir.join("messages.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let transcript_path = transcript.to_string_lossy().into_owned();
    let pricing = home.path().join("pricing-cache.json");

    let fresh = KiroAdapter
        .local_context_refresh(
            RefreshTrigger::Tick,
            &crate::agents::LocalContextRefreshCtx {
                login_env: &std::collections::BTreeMap::new(),
                agent_id: session_id,
                model_hint: None,
                prior_session_name: None,
                current_transcript_path: None,
                prior_transcript_path: Some(&transcript_path),
                prior_transcript_stat: None,
                prior_spend_fold: None,
                shared_pricing_cache_path: &pricing,
            },
        )
        .expect("fresh transcript stat");
    assert_eq!(
        fresh.transcript_path.as_deref(),
        Some(transcript_path.as_str())
    );
    let stat = fresh.transcript_stat.expect("fresh transcript stat");

    assert!(
        KiroAdapter
            .local_context_refresh(
                RefreshTrigger::Tick,
                &crate::agents::LocalContextRefreshCtx {
                    login_env: &std::collections::BTreeMap::new(),
                    agent_id: session_id,
                    model_hint: None,
                    prior_session_name: None,
                    current_transcript_path: None,
                    prior_transcript_path: Some(&transcript_path),
                    prior_transcript_stat: Some(&stat),
                    prior_spend_fold: None,
                    shared_pricing_cache_path: &pricing,
                },
            )
            .is_none()
    );
}

#[test]
fn discovery_validates_layout_and_folds_ordered_records() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let bucket = dir
        .path()
        .join("sessions")
        .join(session::workspace_bucket(&workspace).unwrap());
    let session_id = "sess_33333333-3333-4333-8333-333333333333";
    let session_dir = bucket.join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_str(include_str!("tests/fixtures/stock_empty/session.json")).unwrap();
    metadata["workspacePaths"] = serde_json::json!([workspace]);
    std::fs::write(session_dir.join("session.json"), metadata.to_string()).unwrap();
    std::fs::write(
        session_dir.join("messages.jsonl"),
        include_bytes!("tests/fixtures/stock_empty/messages.jsonl"),
    )
    .unwrap();

    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.session_id.as_str(), session_id);
    assert_eq!(
        local_state(observation).status,
        crate::agents::AgentStatus::Idle
    );
    assert_eq!(
        local_state(observation).phase,
        crate::agents::TurnPhase::Idle
    );
    assert_eq!(observation.fresh_binding_at, Some(observation.created_at));
    assert!(observation.first_event_at.is_none());
    assert_eq!(observation.last_activity, observation.created_at);
    assert!(local_state(observation).latest_prompt.is_none());
    assert!(local_state(observation).context_pct.is_none());

    metadata["status"] = serde_json::json!("in_progress");
    std::fs::write(session_dir.join("session.json"), metadata.to_string()).unwrap();
    assert_eq!(
        local_state(&session::discover_under(dir.path(), &workspace)[0]).status,
        crate::agents::AgentStatus::Running
    );

    std::fs::write(
        session_dir.join("messages.jsonl"),
        include_str!("tests/fixtures/stock_ping/messages.jsonl"),
    )
    .unwrap();
    let observations = session::discover_under(dir.path(), &workspace);
    let observation = &observations[0];
    assert_eq!(
        local_state(observation).status,
        crate::agents::AgentStatus::Success
    );
    assert_eq!(
        local_state(observation).phase,
        crate::agents::TurnPhase::Idle
    );
    assert_eq!(
        local_state(observation).latest_prompt.as_deref(),
        Some("ping")
    );
    assert_eq!(local_state(observation).context_pct, Some(13));
    assert_eq!(
        observation.first_event_at,
        Some("2025-01-01T00:00:01Z".parse().unwrap())
    );
    assert_eq!(
        observation.last_activity,
        "2025-01-01T00:00:04.300Z".parse().unwrap()
    );

    for (field, invalid) in [
        ("schemaVersion", serde_json::json!("2.0.0")),
        (
            "id",
            serde_json::json!("sess_44444444-4444-4444-8444-444444444444"),
        ),
        (
            "workspacePaths",
            serde_json::json!([workspace.join("other")]),
        ),
    ] {
        let mut invalid_metadata = metadata.clone();
        invalid_metadata[field] = invalid;
        std::fs::write(
            session_dir.join("session.json"),
            invalid_metadata.to_string(),
        )
        .unwrap();
        assert!(
            session::discover_under(dir.path(), &workspace).is_empty(),
            "accepted invalid {field}"
        );
    }

    metadata["status"] = serde_json::json!("failed");
    std::fs::write(session_dir.join("session.json"), metadata.to_string()).unwrap();
    std::fs::write(session_dir.join("messages.jsonl"), b"").unwrap();
    let observation = &session::discover_under(dir.path(), &workspace)[0];
    assert_eq!(
        local_state(observation).status,
        crate::agents::AgentStatus::Failed
    );
    assert_eq!(
        local_state(observation).phase,
        crate::agents::TurnPhase::Idle
    );

    metadata["status"] = serde_json::Value::Null;
    std::fs::write(session_dir.join("session.json"), metadata.to_string()).unwrap();
    let observation = &session::discover_under(dir.path(), &workspace)[0];
    assert_eq!(
        local_state(observation).status,
        crate::agents::AgentStatus::Idle
    );
    assert_eq!(
        local_state(observation).phase,
        crate::agents::TurnPhase::Idle
    );
}

#[test]
fn discovery_cache_reuses_sessions_and_reparses_one_message_log() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let bucket = dir
        .path()
        .join("sessions")
        .join(session::workspace_bucket(&workspace).unwrap());
    let session_id = "sess_33333333-3333-4333-8333-333333333333";
    let session_dir = bucket.join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_str(include_str!("tests/fixtures/stock_empty/session.json")).unwrap();
    metadata["workspacePaths"] = serde_json::json!([workspace]);
    std::fs::write(session_dir.join("session.json"), metadata.to_string()).unwrap();
    let messages = session_dir.join("messages.jsonl");
    std::fs::write(
        &messages,
        include_bytes!("tests/fixtures/stock_empty/messages.jsonl"),
    )
    .unwrap();
    let mut cache = session::DiscoveryCacheHarness::new();
    let start = Instant::now();

    assert_eq!(
        cache
            .refresh(dir.path(), &[workspace.as_path()], start)
            .len(),
        1
    );
    assert_eq!(cache.work(), (1, 1));
    assert_eq!(
        cache
            .refresh(dir.path(), &[workspace.as_path()], start)
            .len(),
        1
    );
    assert_eq!(cache.work(), (1, 1));

    std::fs::OpenOptions::new()
        .append(true)
        .open(messages)
        .unwrap()
        .write_all(
            b"\n{\"id\":\"event-2\",\"timestamp\":\"2025-01-01T00:01:00Z\",\"payload\":{\"type\":\"user\",\"content\":\"changed\"}}\n",
        )
        .unwrap();
    assert_eq!(
        cache
            .refresh(dir.path(), &[workspace.as_path()], start)
            .len(),
        1
    );
    assert_eq!(cache.work(), (1, 2));

    cache.refresh(
        dir.path(),
        &[workspace.as_path()],
        start + Duration::from_secs(30),
    );
    assert_eq!(cache.work(), (2, 3));
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_workspace_buckets() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let bucket_name = session::workspace_bucket(&workspace).unwrap();
    let escaped = dir.path().join("escaped").join(&bucket_name);
    let session_dir = escaped.join("sess_11111111-1111-4111-8111-111111111111");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("session.json"),
        format!(
            r#"{{"id":"sess_11111111-1111-4111-8111-111111111111","schemaVersion":"1.0.0","dataModelVersion":1,"workspacePaths":[{}],"createdAt":"2025-01-01T00:00:00Z","status":"idle"}}"#,
            serde_json::to_string(&workspace).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        session_dir.join("messages.jsonl"),
        include_str!("tests/fixtures/stock_ping/messages.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    symlink(&escaped, dir.path().join("sessions").join(bucket_name)).unwrap();

    assert!(session::discover_under(dir.path(), &workspace).is_empty());
}

#[test]
fn approval_waiting_resolution_tool_activity_and_context_clamp_follow_file_order() {
    let lines = include_str!("tests/fixtures/stock_approval/messages.jsonl");
    let pending_end = lines.find("{\"id\":\"context-2a\"").unwrap();
    let pending = session::fold_for_test(&lines[..pending_end]);
    assert_eq!(pending.0, crate::agents::AgentStatus::Waiting);
    assert_eq!(pending.2.as_deref(), Some("Write File"));

    let tool_end = lines.find("{\"id\":\"tool-2-result\"").unwrap();
    let acting = session::fold_for_test(&lines[..tool_end]);
    assert_eq!(acting.0, crate::agents::AgentStatus::Running);
    assert_eq!(acting.1, crate::agents::TurnPhase::Acting);

    let settled = session::fold_for_test(lines);
    assert_eq!(settled.0, crate::agents::AgentStatus::Success);
    assert_eq!(settled.1, crate::agents::TurnPhase::Idle);
    assert_eq!(settled.3, Some(100));
}

#[test]
fn kiro_2_21_4_shell_turn_waits_then_settles_in_file_order() {
    let lines = include_str!("tests/fixtures/stock_shell_2_21_4/messages.jsonl");
    let pending_end = lines.find("-resolved\"").unwrap();
    let pending = session::fold_for_test(&lines[..pending_end]);
    assert_eq!(pending.0, crate::agents::AgentStatus::Waiting);
    assert_eq!(pending.2.as_deref(), Some("Run Command requires approval"));

    let result_start = lines
        .find("-result\",\"timestamp\":\"2026-09-14T05:48")
        .unwrap();
    let running = session::fold_for_test(&lines[..result_start]);
    assert_eq!(running.0, crate::agents::AgentStatus::Running);
    assert!(running.2.is_none());

    let settled = session::fold_for_test(lines);
    assert_eq!(settled.0, crate::agents::AgentStatus::Success);
    assert_eq!(settled.1, crate::agents::TurnPhase::Idle);
    assert!(settled.3.is_some());
}

#[test]
fn completed_tool_calls_drive_the_editing_phase() {
    let lines = concat!(
        "{\"id\":\"start\",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"turn_start\",\"executionId\":\"turn\"}}\n",
        "{\"id\":\"call\",\"timestamp\":\"2025-01-01T00:00:02Z\",\"payload\":{\"type\":\"tool_call\",\"toolCallId\":\"tool\",\"toolName\":\"fs_write\",\"status\":\"completed\"}}\n",
    );
    let folded = session::fold_for_test(lines);
    assert_eq!(folded.0, crate::agents::AgentStatus::Running);
    assert_eq!(folded.1, crate::agents::TurnPhase::Acting);
}

#[test]
fn turn_end_stop_reasons_settle_cancelled_and_failed_turns() {
    let pending = "{\"id\":\"pending\",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"pending_interaction\",\"interactionType\":\"tool_approval\",\"toolCallId\":\"tool\",\"question\":\"Approve tool\"}}\n";
    for (stop_reason, status) in [
        ("cancelled", crate::agents::AgentStatus::Idle),
        ("error", crate::agents::AgentStatus::Failed),
        ("refusal", crate::agents::AgentStatus::Failed),
        ("max_tokens", crate::agents::AgentStatus::Success),
    ] {
        let lines = format!(
            "{pending}{{\"id\":\"end\",\"timestamp\":\"2025-01-01T00:00:02Z\",\"payload\":{{\"type\":\"turn_end\",\"stopReason\":\"{stop_reason}\"}}}}\n"
        );
        let folded = session::fold_for_test(&lines);
        assert_eq!(folded.0, status, "{stop_reason}");
        assert_eq!(folded.1, crate::agents::TurnPhase::Idle, "{stop_reason}");
        assert_eq!(
            folded.2.is_some(),
            status == crate::agents::AgentStatus::Success,
            "{stop_reason}"
        );
    }
}

#[test]
fn invalid_context_metadata_preserves_the_latest_valid_percentage() {
    let lines = concat!(
        "{\"id\":\"valid\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"payload\":{\"type\":\"session_metadata\",\"key\":\"contextUsage\",\"value\":{\"usagePercentage\":42.0}}}\n",
        "{\"id\":\"invalid\",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"session_metadata\",\"key\":\"contextUsage\",\"value\":null}}\n",
    );

    assert_eq!(session::fold_for_test(lines).3, Some(42));
}

#[test]
fn approval_resolution_restores_the_latest_remaining_wait() {
    let lines = concat!(
        "{\"id\":\"pending-old\",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"pending_interaction\",\"interactionType\":\"tool_approval\",\"toolCallId\":\"tool-old\",\"question\":\"Old approval\"}}\n",
        "{\"id\":\"pending-new\",\"timestamp\":\"2025-01-01T00:00:02Z\",\"payload\":{\"type\":\"pending_interaction\",\"interactionType\":\"tool_approval\",\"toolCallId\":\"tool-new\",\"question\":\"New approval\"}}\n",
        "{\"id\":\"resolve-new\",\"timestamp\":\"2025-01-01T00:00:03Z\",\"payload\":{\"type\":\"interaction_resolved\",\"toolCallId\":\"tool-new\"}}\n",
        "{\"id\":\"resolve-unknown\",\"timestamp\":\"2025-01-01T00:00:04Z\",\"payload\":{\"type\":\"interaction_resolved\",\"toolCallId\":\"tool-unknown\"}}\n",
        "{\"id\":\"resolve-old\",\"timestamp\":\"2025-01-01T00:00:05Z\",\"payload\":{\"type\":\"interaction_resolved\",\"toolCallId\":\"tool-old\"}}\n",
    );

    let before_new_resolution = lines.find("{\"id\":\"resolve-new\"").unwrap();
    let newest = session::fold_for_test(&lines[..before_new_resolution]);
    assert_eq!(newest.0, crate::agents::AgentStatus::Waiting);
    assert_eq!(newest.2.as_deref(), Some("New approval"));
    assert_eq!(newest.4, Some("2025-01-01T00:00:02Z".parse().unwrap()));

    let before_unknown_resolution = lines.find("{\"id\":\"resolve-unknown\"").unwrap();
    let older = session::fold_for_test(&lines[..before_unknown_resolution]);
    assert_eq!(older.0, crate::agents::AgentStatus::Waiting);
    assert_eq!(older.2.as_deref(), Some("Old approval"));
    assert_eq!(older.4, Some("2025-01-01T00:00:01Z".parse().unwrap()));

    let before_old_resolution = lines.find("{\"id\":\"resolve-old\"").unwrap();
    assert_eq!(
        session::fold_for_test(&lines[..before_old_resolution]),
        older
    );

    let resolved = session::fold_for_test(lines);
    assert_eq!(resolved.0, crate::agents::AgentStatus::Running);
    assert_eq!(resolved.1, crate::agents::TurnPhase::Reasoning);
    assert!(resolved.2.is_none());
    assert!(resolved.4.is_none());
}

#[test]
fn successful_turn_end_retains_unresolved_approval_detail() {
    let lines = concat!(
        "{\"id\":\"pending\",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"pending_interaction\",\"interactionType\":\"tool_approval\",\"toolCallId\":\"tool\",\"question\":\"Approve tool\"}}\n",
        "{\"id\":\"turn-end\",\"timestamp\":\"2025-01-01T00:00:02Z\",\"payload\":{\"type\":\"turn_end\",\"executionId\":\"turn\",\"stopReason\":\"end_turn\"}}\n",
    );

    let folded = session::fold_for_test(lines);
    assert_eq!(folded.0, crate::agents::AgentStatus::Success);
    assert_eq!(folded.1, crate::agents::TurnPhase::Idle);
    assert_eq!(folded.2.as_deref(), Some("Approve tool"));
    assert_eq!(folded.4, Some("2025-01-01T00:00:01Z".parse().unwrap()));
}

#[test]
fn transcript_cursor_retains_torn_final_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("messages.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"{\"id\":\"a\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"payload\":{\"type\":\"assistant\",\"operationType\":\"Say\",\"content\":\"one\"}}\n{\"id\":\"b\"").unwrap();
    let page = KiroAdapter
        .read_assistant_transcript_page(&path, None, TranscriptPosition::START)
        .unwrap();
    assert_eq!(page.messages, ["one"]);
    let next = page.next;
    file.write_all(b",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"assistant\",\"operationType\":\"Say\",\"content\":\"two\"}}").unwrap();
    let page = KiroAdapter
        .read_assistant_transcript_page(&path, None, next)
        .unwrap();
    assert_eq!(page.messages, ["two"]);
}

#[test]
fn workspace_hash_and_resume_parser_are_exact() {
    assert_eq!(
        session::workspace_bucket(Path::new("/workspace/project")).as_deref(),
        Some("e3af8a7251583e76")
    );
    for command in [
        "kiro-cli chat --v3 --resume-id sess_11111111-1111-4111-8111-111111111111",
        "kiro-cli --resume-id=sess_11111111-1111-4111-8111-111111111111",
        "kiro-cli-chat --resume-id sess_11111111-1111-4111-8111-111111111111",
    ] {
        assert_eq!(
            KiroAdapter
                .resumed_session_id_from_cmdline(command)
                .as_deref(),
            Some("sess_11111111-1111-4111-8111-111111111111")
        );
    }
    for command in [
        "kiro-cli-term --resume-id sess_11111111-1111-4111-8111-111111111111",
        "kiro-cli --resume-id unrelated",
        "echo --resume-id sess_11111111-1111-4111-8111-111111111111",
    ] {
        assert!(
            KiroAdapter
                .resumed_session_id_from_cmdline(command)
                .is_none()
        );
    }
}

#[test]
fn hook_install_writes_reclaims_and_removes_the_managed_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    let current = Some(install::MIN_SUPPORTED_CLI);

    let preview = install::preview_at(&path, current).unwrap();
    assert!(!preview.files[0].existed);
    assert_eq!(preview.files[0].candidate, HOOK_SOURCE);
    let report = install::install_at(&path, current).unwrap();
    assert!(!report.files[0].existed);
    assert_eq!(
        report.installed_events,
        [
            "SessionStart",
            "UserPromptSubmit",
            "PostToolUse",
            "Stop",
            "SessionEnd"
        ]
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK_SOURCE);
    assert!(KIRO_MANAGED_SOURCE.installed_at(&path));
    assert!(install::install_at(&path, None).unwrap().files[0].existed);

    let removed = install::uninstall_from(&path).unwrap();
    assert!(removed.files[0].existed);
    assert_eq!(removed.removed_events.len(), 5);
    assert!(!path.exists());
}

#[test]
fn hook_install_refuses_releases_before_one_home() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    let old = Some(crate::agents::version::CliVersion::new(2, 30, 0));
    for err in [
        install::install_at(&path, old).map(|_| ()).unwrap_err(),
        install::preview_at(&path, old).map(|_| ()).unwrap_err(),
    ] {
        assert!(
            err.to_string().contains("upgrade to 2.31.0 or later"),
            "{err}"
        );
    }
    assert!(!path.exists());
    assert!(install::install_at(&path, Some(install::MIN_SUPPORTED_CLI)).is_ok());
}

#[test]
fn legacy_owned_hook_file_is_reclaimed_and_removed() {
    let legacy = r#"{"version":"v1","hooks":[{"action":{"command":"/old/rimz hooks feed --source kiro --event Stop"}}]}"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    std::fs::write(&path, legacy).unwrap();
    assert!(install::managed_at(&path));
    assert!(!KIRO_MANAGED_SOURCE.installed_at(&path));
    let preview = install::preview_at(&path, None).unwrap();
    assert!(preview.files[0].existed);
    assert_eq!(preview.files[0].original.as_deref(), Some(legacy));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
    assert!(install::install_at(&path, None).unwrap().files[0].existed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK_SOURCE);

    std::fs::write(&path, legacy).unwrap();
    let removed = install::uninstall_from(&path).unwrap();
    assert!(removed.files[0].existed);
    assert_eq!(
        removed.removed_events,
        [
            "SessionStart",
            "UserPromptSubmit",
            "PostToolUse",
            "Stop",
            "SessionEnd"
        ]
    );
    assert!(!path.exists());
}

#[test]
fn user_hook_file_is_refused_and_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let user_config = r#"{"version":"v1","hooks":[{"action":{"command":"my-hook"}}]}"#;
    std::fs::write(&path, user_config).unwrap();

    assert!(!install::managed_at(&path));
    assert!(install::install_at(&path, None).is_err());
    assert!(install::preview_at(&path, None).is_err());
    let report = install::uninstall_from(&path).unwrap();
    assert!(report.files[0].existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), user_config);
}

#[test]
fn hooks_path_prefers_override_then_kiro_home() {
    let override_path = OsStr::new("/tmp/override.json");
    let home = OsStr::new("/home/user");
    assert_eq!(
        install::resolve_hooks_path(
            Some(override_path),
            Some(OsStr::new("/selected")),
            Some(home)
        )
        .unwrap(),
        std::path::PathBuf::from("/tmp/override.json")
    );
    assert_eq!(
        install::resolve_hooks_path(None, None, Some(home)).unwrap(),
        std::path::PathBuf::from("/home/user/.kiro/hooks/rimz.json")
    );
    assert!(matches!(
        install::resolve_hooks_path(None, None, None),
        Err(AgentErr::Install { .. })
    ));
    assert_eq!(
        install::resolve_home(Some(OsStr::new("/tmp/kiro")), Some(home)).unwrap(),
        std::path::PathBuf::from("/tmp/kiro")
    );
    assert_eq!(
        install::resolve_hooks_path(None, Some(OsStr::new("/selected")), Some(home)).unwrap(),
        PathBuf::from("/selected/hooks/rimz.json")
    );
}

#[test]
fn session_store_follows_kiro_home() {
    let env = BTreeMap::from([
        ("KIRO_HOME".to_owned(), "/tmp/kiro".to_owned()),
        ("HOME".to_owned(), "/home/user".to_owned()),
    ]);
    assert_eq!(
        KiroAdapter.config_home(&env),
        Some(std::path::PathBuf::from("/tmp/kiro"))
    );
    assert_eq!(
        KiroAdapter.skills_home(&env),
        Some(std::path::PathBuf::from("/tmp/kiro/skills"))
    );
    assert_eq!(KiroAdapter.config_home(&BTreeMap::new()), None);
}

#[test]
fn invalid_kiro_homes_fall_back_to_home() {
    for value in ["", "relative", "/", "/home", "/home/user", "/home/user/.."] {
        assert_eq!(
            install::resolve_home(Some(OsStr::new(value)), Some(OsStr::new("/home/user"))),
            Some(PathBuf::from("/home/user/.kiro")),
            "{value}"
        );
    }
}

#[test]
fn session_lookups_share_the_selected_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home/user");
    let selected = root.path().join("selected");
    let workspace = Path::new("/workspace/project");
    let id = "sess_33333333-3333-4333-8333-333333333333";
    for kiro_home in [
        selected.to_str().unwrap(),
        "relative",
        "/",
        home.to_str().unwrap(),
        root.path().to_str().unwrap(),
    ] {
        let expected = if kiro_home == selected.to_str().unwrap() {
            selected.clone()
        } else {
            home.join(".kiro")
        };
        let session_dir = expected
            .join("sessions")
            .join(session::workspace_bucket(workspace).unwrap())
            .join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("session.json"), json!({"id": id, "schemaVersion": "1.0.0", "dataModelVersion": 1, "workspacePaths": [workspace], "createdAt": "2025-01-03T00:00:00Z"}).to_string()).unwrap();
        std::fs::write(session_dir.join("messages.jsonl"), "").unwrap();
        let env = BTreeMap::from([
            ("HOME".to_owned(), home.to_string_lossy().into_owned()),
            ("KIRO_HOME".to_owned(), kiro_home.to_owned()),
        ]);
        assert_eq!(
            session::transcript_for_session(id, &env),
            Some(session_dir.join("messages.jsonl")),
            "{kiro_home}"
        );
        assert_eq!(
            session::discover(&[workspace], &env).len(),
            1,
            "{kiro_home}"
        );
        assert_eq!(
            install::resolve_hooks_path(None, Some(OsStr::new(kiro_home)), Some(home.as_os_str()))
                .unwrap(),
            expected.join("hooks/rimz.json")
        );
    }
}

#[test]
fn launch_resume_and_presets_use_v3_surface() {
    let mut args = KiroAdapter
        .spec()
        .launch
        .permission_args(crate::agents::PermissionMode::Yolo);
    assert_eq!(
        KiroAdapter.launch_command(&args, Some("review")),
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--trust-all-tools".to_owned(),
            "review".to_owned()
        ])
    );
    KiroAdapter.spec().launch.strip_permission_args(&mut args);
    assert!(args.is_empty());
    assert_eq!(
        KiroAdapter.launch_command(
            &["--agent".to_owned(), "reviewer".to_owned()],
            Some("review")
        ),
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--agent".to_owned(),
            "reviewer".to_owned(),
            "review".to_owned(),
        ])
    );
    assert_eq!(
        KiroAdapter.resume_command("sess_redacted", Path::new("/work")),
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--resume-id".to_owned(),
            "sess_redacted".to_owned(),
        ])
    );
    assert!(
        KiroAdapter
            .spec()
            .launch
            .fork_command("sess_redacted")
            .is_none()
    );
    assert_eq!(
        KiroAdapter.spec().launch.compact_command(""),
        Some("/compact".to_owned())
    );
}

#[test]
fn presence_matches_launcher_and_chat_engine() {
    let definition = KiroAdapter.spec();
    assert!(definition.runs_as("kiro-cli"));
    assert!(definition.runs_as("kiro-cli-chat"));
    assert!(!definition.runs_as("kiro-cli-term"));
}
