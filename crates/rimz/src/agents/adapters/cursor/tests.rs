use super::*;

use crate::agents::lifecycle::{TurnPhase, step};
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output};
use crate::agents::{AgentErr, AgentStatus, LocalSessionProjection};
use md5::{Digest as _, Md5};
use prost::Message;
use rusqlite::Connection;
use serde_json::json;
use sha2::Sha256;
use std::time::{Duration, Instant};

#[test]
fn participant_start_accepts_only_a_nonempty_absolute_project_dir() {
    use std::ffi::OsStr;

    assert_eq!(
        cursor_project_dir(Some(OsStr::new("/repo/worktree"))),
        Some(PathBuf::from("/repo/worktree")),
    );
    for value in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("relative/path")),
    ] {
        assert_eq!(cursor_project_dir(value), None);
    }
}

#[test]
fn version_parser_preserves_the_cursor_date_build_token() {
    assert_eq!(
        CursorAdapter
            .parse_version("2026.07.09-a3815c0\n", "")
            .as_deref(),
        Some("2026.07.09-a3815c0")
    );
    assert_eq!(CursorAdapter.parse_version("2026.7.9-a3815c0", ""), None);
    assert_eq!(
        CursorAdapter.parse_version("Cursor release 2026.07.09-a3815c0", ""),
        None
    );
}

#[test]
fn agent_identity_accepts_cursor_and_rejects_grok_alias() {
    assert!(agent_binary_is_cursor("2026.07.17-3e2a980\n", ""));
    assert!(agent_binary_is_cursor("", "2026.07.17-3e2a980\n"));
    assert!(!agent_binary_is_cursor(
        "grok 0.2.106 (bde89716f6) [stable]",
        ""
    ));
    assert!(!agent_binary_is_cursor("", ""));

    let identity = CursorAdapter
        .spec()
        .ambiguous_bin_identity("agent")
        .expect("cursor's `agent` name is ambiguous");
    assert!((identity.verify)("2026.07.17-3e2a980", ""));
    assert!(
        CursorAdapter
            .spec()
            .ambiguous_bin_identity("cursor-agent")
            .is_none()
    );
}

#[test]
fn cursor_launch_fallback_never_reuses_the_ambiguous_agent_name() {
    assert_eq!(cursor_launch_binary(None), "cursor-agent");
    assert_eq!(
        cursor_launch_binary(Some(PathBuf::from("/opt/cursor/agent"))),
        "/opt/cursor/agent"
    );
}

struct CursorAskFixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    session: PathBuf,
    session_id: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl CursorAskFixture {
    fn new(pending: Vec<Value>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("cursor-home");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace_text = workspace.to_str().unwrap();
        let bucket = home
            .join("chats")
            .join(hex::encode(Md5::digest(workspace_text.as_bytes())));
        let session_id = "11111111-1111-4111-8111-111111111111".to_owned();
        let session = bucket.join(&session_id);
        std::fs::create_dir_all(&session).unwrap();
        let created_at_ms = 1_735_689_600_000;
        let updated_at_ms = created_at_ms + 20_000;
        std::fs::write(
            session.join("meta.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "createdAtMs": created_at_ms,
                "updatedAtMs": updated_at_ms,
                "hasConversation": true,
                "cwd": workspace,
            }))
            .unwrap(),
        )
        .unwrap();
        let transcript = home
            .join("projects/project/agent-transcripts")
            .join(&session_id)
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{\"type\":\"turn_started\"}\n").unwrap();

        let connection = Connection::open(session.join("store.db")).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE blobs(id TEXT PRIMARY KEY, data BLOB);\
                 CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);",
            )
            .unwrap();
        drop(connection);
        let fixture = Self {
            _dir: dir,
            home,
            workspace,
            session,
            session_id,
            created_at_ms,
            updated_at_ms,
        };
        fixture.replace_pending(pending);
        fixture
    }

    fn replace_pending(&self, pending: Vec<Value>) {
        self.replace_state(pending, Vec::new(), None, None);
    }

    fn replace_state(
        &self,
        pending: Vec<Value>,
        messages: Vec<Vec<u8>>,
        mode: Option<&str>,
        plan_uri: Option<&str>,
    ) -> Vec<Vec<u8>> {
        let message_ids = messages
            .iter()
            .map(|message| Sha256::digest(message).to_vec())
            .collect::<Vec<_>>();
        self.write_state(pending, messages, message_ids.clone(), mode, plan_uri);
        message_ids
    }

    fn write_state(
        &self,
        pending: Vec<Value>,
        messages: Vec<Vec<u8>>,
        message_ids: Vec<Vec<u8>>,
        mode: Option<&str>,
        plan_uri: Option<&str>,
    ) {
        let root = session::ConversationStateStructure {
            message_ids,
            pending_tool_calls: pending
                .into_iter()
                .map(|value| serde_json::to_string(&value).unwrap())
                .collect(),
        }
        .encode_to_vec();
        let blob_id = hex::encode(Sha256::digest(&root));
        let mut store_metadata = json!({
            "agentId": self.session_id,
            "createdAt": self.created_at_ms,
            "latestRootBlobId": blob_id,
        });
        if let Some(mode) = mode {
            store_metadata["mode"] = json!(mode);
        }
        if let Some(plan_uri) = plan_uri {
            store_metadata["currentPlanUri"] = json!(plan_uri);
        }
        let store_metadata = serde_json::to_vec(&store_metadata).unwrap();
        let connection = Connection::open(self.session.join("store.db")).unwrap();
        connection.execute("DELETE FROM blobs", []).unwrap();
        connection.execute("DELETE FROM meta", []).unwrap();
        connection
            .execute(
                "INSERT INTO blobs(id, data) VALUES (?1, ?2)",
                (&blob_id, &root),
            )
            .unwrap();
        for message in messages {
            let message_id = hex::encode(Sha256::digest(&message));
            connection
                .execute(
                    "INSERT INTO blobs(id, data) VALUES (?1, ?2)",
                    (&message_id, &message),
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES ('0', ?1)",
                [hex::encode(store_metadata)],
            )
            .unwrap();
    }

    fn observations(&self) -> Vec<crate::agents::LocalSessionObservation> {
        session::discover_under(&self.home, &self.workspace)
    }
}

#[test]
fn discovery_cache_reuses_positive_and_negative_sqlite_results() {
    let fixture = CursorAskFixture::new(vec![pending_ask("Which color?", false, None)]);
    let missing_id = "22222222-2222-4222-8222-222222222222";
    let missing = fixture.session.parent().unwrap().join(missing_id);
    std::fs::create_dir_all(&missing).unwrap();
    let mut cache = session::DiscoveryCacheHarness::new();
    let start = Instant::now();

    assert_eq!(
        cache
            .refresh(&fixture.home, &[fixture.workspace.as_path()], start)
            .len(),
        1
    );
    assert_eq!(cache.work(), (1, 1));
    assert_eq!(
        cache
            .refresh(&fixture.home, &[fixture.workspace.as_path()], start)
            .len(),
        1
    );
    assert_eq!(cache.work(), (1, 1));

    let metadata_path = fixture.session.join("meta.json");
    let mut metadata = std::fs::read(&metadata_path).unwrap();
    metadata.push(b'\n');
    std::fs::write(metadata_path, metadata).unwrap();
    assert_eq!(
        cache
            .refresh(&fixture.home, &[fixture.workspace.as_path()], start)
            .len(),
        1
    );
    assert_eq!(cache.work(), (1, 2));

    cache.refresh(
        &fixture.home,
        &[fixture.workspace.as_path()],
        start + Duration::from_secs(30),
    );
    assert_eq!(cache.work(), (2, 3));
}

fn message(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

fn create_plan_message() -> Vec<u8> {
    message(json!({
        "role": "tool",
        "content": [{
            "type": "tool-result",
            "toolCallId": "tool-call-1",
            "toolName": "CreatePlan",
            "result": "Plan file created at: /workspace/.cursor/plans/example.plan.md"
        }]
    }))
}

struct CursorSubagentFixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    bucket: PathBuf,
}

impl CursorSubagentFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("cursor-home");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bucket = home.join("chats").join(hex::encode(Md5::digest(
            workspace.to_str().unwrap().as_bytes(),
        )));
        std::fs::create_dir_all(&bucket).unwrap();
        Self {
            _dir: dir,
            home,
            workspace,
            bucket,
        }
    }

    fn add_child(
        &self,
        child_id: &str,
        parent_agent_id: Option<&str>,
        type_name: Option<&str>,
        created_at: i64,
        schema: i64,
    ) -> PathBuf {
        let session = self.bucket.join(child_id);
        std::fs::create_dir_all(&session).unwrap();
        let subagent_info = parent_agent_id.map(|parent_agent_id| {
            json!({
                "parentAgentId": parent_agent_id,
                "rootParentAgentId": parent_agent_id,
                "toolCallId": format!("call-{child_id}"),
                "typeName": type_name,
            })
        });
        self.write_store(
            &session.join("store.db"),
            schema,
            &json!({
                "agentId": child_id,
                "latestRootBlobId": "a".repeat(64),
                "createdAt": created_at,
                "subagentInfo": subagent_info,
            }),
        );
        session
    }

    fn write_store(&self, path: &Path, schema: i64, metadata: &Value) {
        let connection = Connection::open(path).unwrap();
        connection
            .pragma_update(None, "user_version", schema)
            .unwrap();
        connection
            .execute("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES ('0', ?1)",
                [hex::encode(serde_json::to_vec(metadata).unwrap())],
            )
            .unwrap();
    }

    fn replace_store_metadata(&self, session: &Path, encoded: &str) {
        let connection = Connection::open(session.join("store.db")).unwrap();
        connection
            .execute("UPDATE meta SET value = ?1 WHERE key = '0'", [encoded])
            .unwrap();
    }

    fn write_transcript(&self, child_id: &str, lines: &[Value]) -> PathBuf {
        let path = self
            .home
            .join("projects/project/agent-transcripts")
            .join(child_id)
            .join(format!("{child_id}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&path, text).unwrap();
        path
    }

    fn observations(&self) -> Vec<AgentLifecycleObservation> {
        CursorAdapter.derive_subagent_observations_under(&self.home, &self.workspace)
    }

    fn records(&self) -> Vec<session::CursorSubagentRecord> {
        session::discover_subagent_chats(&self.home, &self.workspace)
    }
}

fn cursor_child_user_message(task: &str) -> Value {
    json!({
        "role": "user",
        "message": {
            "content": [{
                "type": "text",
                "text": format!("<user_query>\n  {task}  \n</user_query>"),
            }],
        },
    })
}

fn pending_ask(prompt: &str, run_async: bool, sentinel: Option<&str>) -> Value {
    json!({
        "id": "assistant-message",
        "role": "assistant",
        "content": [
            { "type": "reasoning", "text": sentinel.unwrap_or("") },
            {
                "type": "tool-call",
                "toolCallId": "tool-call-1",
                "toolName": "AskQuestion",
                "args": {
                    "runAsync": run_async,
                    "questions": [{
                        "id": "question-1",
                        "prompt": prompt,
                        "options": [{ "id": "secret", "label": sentinel.unwrap_or("private option") }]
                    }]
                }
            }
        ],
        "providerOptions": {
            "cursor": { "pendingToolCallStartedAtMs": 1_735_689_610_000i64 }
        }
    })
}

#[test]
fn lifecycle_maps_identity_prompt_tools_outcomes_and_compaction() {
    let registered = hook_lifecycle(
        &CursorAdapter,
        "sessionStart",
        &json!({
            "conversation_id": "conv-1",
            "model": "legacy-model",
            "model_id": "cursor/model",
            "model_params": [
                { "id": "context", "value": "200k" },
                { "id": "effort", "value": "high" },
                { "id": "future", "value": "kept-tolerant" }
            ],
            "transcript_path": "/tmp/transcript.jsonl"
        }),
    );
    assert_eq!(registered.agent_id.as_deref(), Some("conv-1"));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.launch.model.as_deref(), Some("cursor/model"));
    assert_eq!(registered.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        registered.transcript_path.as_deref(),
        Some("/tmp/transcript.jsonl")
    );
    assert_eq!(
        step(None, None, &registered.signal).next.status,
        AgentStatus::Idle
    );

    let prompt = hook_lifecycle(
        &CursorAdapter,
        "beforeSubmitPrompt",
        &json!({ "conversation_id": "conv-1", "prompt": "  fix auth  " }),
    );
    assert_eq!(prompt.task.as_deref(), Some("fix auth"));
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    let running = step(None, None, &prompt.signal).next;
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(running.phase, TurnPhase::Reasoning);

    for (tool, edits) in [
        ("Write", Some(true)),
        ("Shell", Some(false)),
        ("Read", None),
    ] {
        let observation = hook_observation(
            &CursorAdapter,
            "postToolUse",
            &json!({ "conversation_id": "conv-1", "tool_name": tool, "cwd": "/work" }),
        );
        assert_eq!(
            observation.map(|observation| observation.signal),
            edits.map(|edits| LifecycleSignal::ToolUsed {
                mutates: true,
                edits,
                name: None,
                native_key: None,
            }),
            "{tool}",
        );
    }
    assert!(
        hook_observation(
            &CursorAdapter,
            "postToolUseFailure",
            &json!({ "conversation_id": "conv-1", "tool_name": "Write" })
        )
        .is_none()
    );

    for (status, signal) in [
        (
            "completed",
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        ("aborted", LifecycleSignal::TurnInterrupted),
        (
            "error",
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
    ] {
        let observation = hook_lifecycle(
            &CursorAdapter,
            "stop",
            &json!({ "conversation_id": "conv-1", "status": status }),
        );
        assert_eq!(observation.signal, signal);
    }

    let compacting = hook_lifecycle(
        &CursorAdapter,
        "preCompact",
        &json!({
            "conversation_id": "conv-1",
            "context_usage_percent": 83.6,
            "context_tokens": 167200,
            "context_window_size": 200000
        }),
    );
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    assert_eq!(compacting.usage.context_pct, Some(84));
    assert_eq!(compacting.usage.context_window, Some(200_000));
    assert_eq!(compacting.usage.total_tokens, None);
    let transition = step(Some(&running), None, &compacting.signal);
    assert!(transition.next.compacting);
    assert_eq!(transition.next.status, AgentStatus::Running);

    let ended = hook_lifecycle(
        &CursorAdapter,
        "sessionEnd",
        &json!({ "conversation_id": "conv-1" }),
    );
    assert_eq!(ended.signal, LifecycleSignal::Ended);
    assert!(
        hook_output(
            &CursorAdapter,
            "sessionEnd",
            &json!({ "conversation_id": "c1" })
        )
        .ends_session()
    );
}

#[test]
fn cursor_subagent_lifecycle_keeps_exact_identity_and_child_only_enrichment() {
    let started = hook_lifecycle(
        &CursorAdapter,
        "subagentStart",
        &json!({
            "conversation_id": "parent-common",
            "subagent_id": " child-1 ",
            "parent_conversation_id": " parent-1 ",
            "subagent_type": " generalPurpose ",
            "task": "  inspect hooks  ",
            "subagent_model": "default",
            "git_branch": " feature/hooks ",
            "model_id": "parent-model",
            "transcript_path": "/tmp/parent.jsonl"
        }),
    );
    assert_eq!(started.agent_id.as_deref(), Some("child-1"));
    assert_eq!(started.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(started.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(started.agent_name.as_deref(), Some("generalPurpose"));
    assert_eq!(started.launch.role.as_deref(), Some("generalPurpose"));
    assert_eq!(started.task.as_deref(), Some("inspect hooks"));
    assert_eq!(started.launch.model.as_deref(), Some("auto"));
    assert_eq!(started.worktree_branch.as_deref(), Some("feature/hooks"));
    assert_eq!(started.transcript_path, None);

    let stopped = hook_lifecycle(
        &CursorAdapter,
        "subagentStop",
        &json!({
            "conversation_id": "parent-common",
            "subagent_id": "child-1",
            "parent_conversation_id": "parent-1",
            "subagent_type": "generalPurpose",
            "status": "completed",
            "description": "  inspect hooks fallback  ",
            "model_id": "parent-model",
            "transcript_path": "/tmp/parent.jsonl",
            "agent_transcript_path": "/tmp/child-1.jsonl"
        }),
    );
    assert_eq!(stopped.agent_id.as_deref(), Some("child-1"));
    assert_eq!(stopped.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(
        stopped.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    assert_eq!(stopped.agent_name.as_deref(), Some("generalPurpose"));
    assert_eq!(stopped.launch.role.as_deref(), Some("generalPurpose"));
    assert_eq!(stopped.task.as_deref(), Some("inspect hooks fallback"));
    assert_eq!(stopped.launch.model, None);
    assert_eq!(
        stopped.transcript_path.as_deref(),
        Some("/tmp/child-1.jsonl")
    );
}

#[test]
fn cursor_subagent_stop_status_fails_closed() {
    for (status, errored) in [
        (Some("completed"), false),
        (Some("aborted"), false),
        (Some("error"), true),
        (Some("future"), true),
        (None, true),
    ] {
        let mut payload = json!({
            "subagent_id": "child-1",
            "parent_conversation_id": "parent-1",
        });
        if let Some(status) = status {
            payload["status"] = json!(status);
        }
        let observed = hook_lifecycle(&CursorAdapter, "subagentStop", &payload);
        assert_eq!(
            observed.signal,
            LifecycleSignal::SubagentStopped { errored },
            "status={status:?}",
        );
    }
}

#[test]
fn cursor_subagent_malformed_identity_is_quarantined() {
    for payload in [
        json!({
            "parent_conversation_id": "parent-1",
            "conversation_id": "root-fallback"
        }),
        json!({
            "subagent_id": "child-1",
            "conversation_id": "root-fallback"
        }),
        json!({
            "subagent_id": " ",
            "parent_conversation_id": "parent-1",
            "conversation_id": "root-fallback"
        }),
        json!({
            "subagent_id": 7,
            "parent_conversation_id": "parent-1",
            "conversation_id": "root-fallback"
        }),
        json!({
            "subagent_id": "same",
            "parent_conversation_id": "same",
            "conversation_id": "root-fallback"
        }),
    ] {
        for event in ["subagentStart", "subagentStop"] {
            assert_eq!(
                hook_observation(&CursorAdapter, event, &payload),
                None,
                "event={event} payload={payload}",
            );
        }
    }
}

#[test]
fn cursor_chats_store_derives_running_finished_and_errored_children() {
    let fixture = CursorSubagentFixture::new();
    fixture.add_child(
        "child-running",
        Some("parent-1"),
        Some("generalPurpose"),
        1_735_689_600_000,
        1,
    );
    fixture.write_transcript(
        "child-running",
        &[
            cursor_child_user_message("inspect hooks"),
            json!({"role":"assistant","message":{"content":[]}}),
        ],
    );
    fixture.add_child(
        "child-finished",
        Some("parent-1"),
        Some("explore"),
        1_735_689_601_000,
        1,
    );
    let finished_path = fixture.write_transcript(
        "child-finished",
        &[
            cursor_child_user_message("map the store"),
            json!({"type":"turn_ended","status":"success"}),
        ],
    );
    fixture.add_child(
        "child-errored",
        Some("parent-1"),
        Some("generalPurpose"),
        1_735_689_602_000,
        1,
    );
    fixture.write_transcript(
        "child-errored",
        &[
            cursor_child_user_message("check failure"),
            json!({"type":"turn_ended","status":"error"}),
        ],
    );
    fixture.add_child(
        "child-completed",
        Some("parent-1"),
        Some("generalPurpose"),
        1_735_689_603_000,
        1,
    );
    fixture.write_transcript(
        "child-completed",
        &[
            cursor_child_user_message("check completion alias"),
            json!({"type":"turn_ended","status":"completed"}),
        ],
    );

    let observations = fixture.observations();
    assert_eq!(observations.len(), 7);
    let running = observations
        .iter()
        .find(|observation| observation.agent_id.as_deref() == Some("child-running"))
        .expect("running child start");
    assert_eq!(running.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(running.parent_agent_id.as_deref(), Some("parent-1"));
    assert_eq!(running.agent_name.as_deref(), Some("generalPurpose"));
    assert_eq!(running.launch.role.as_deref(), Some("generalPurpose"));
    assert_eq!(running.task.as_deref(), Some("inspect hooks"));
    assert_eq!(running.transcript_path, None);

    let finished = observations
        .iter()
        .filter(|observation| observation.agent_id.as_deref() == Some("child-finished"))
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 2);
    assert_eq!(finished[0].signal, LifecycleSignal::SubagentStarted);
    assert_eq!(
        finished[1].signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    assert_eq!(finished[1].task.as_deref(), Some("map the store"));
    assert_eq!(
        finished[1].transcript_path.as_deref(),
        Some(finished_path.to_string_lossy().as_ref())
    );

    let errored = observations
        .iter()
        .find(|observation| {
            observation.agent_id.as_deref() == Some("child-errored")
                && matches!(observation.signal, LifecycleSignal::SubagentStopped { .. })
        })
        .expect("errored child stop");
    assert_eq!(
        errored.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    let completed = observations
        .iter()
        .find(|observation| {
            observation.agent_id.as_deref() == Some("child-completed")
                && matches!(observation.signal, LifecycleSignal::SubagentStopped { .. })
        })
        .expect("completed child stop");
    assert_eq!(
        completed.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
}

#[test]
fn cursor_chats_store_fails_closed_on_child_identity_schema_and_admission_drift() {
    let fixture = CursorSubagentFixture::new();
    fixture.add_child("missing-parent", None, Some("explore"), 1, 1);
    fixture.add_child("blank-parent", Some("  "), Some("explore"), 2, 1);
    fixture.add_child("equal-parent", Some("equal-parent"), Some("explore"), 3, 1);
    fixture.add_child("wrong-schema", Some("parent-1"), Some("explore"), 4, 2);
    let malformed = fixture.add_child(
        "malformed-metadata",
        Some("parent-1"),
        Some("explore"),
        5,
        1,
    );
    fixture.replace_store_metadata(&malformed, &hex::encode(b"{"));
    let root = fixture.add_child("root-marker", Some("parent-1"), Some("explore"), 6, 1);
    std::fs::write(root.join("meta.json"), "{}").unwrap();

    #[cfg(unix)]
    {
        let symlinked = fixture.bucket.join("symlinked-store");
        std::fs::create_dir_all(&symlinked).unwrap();
        let target = fixture.home.join("outside-store.db");
        fixture.write_store(
            &target,
            1,
            &json!({
                "agentId": "symlinked-store",
                "latestRootBlobId": "a".repeat(64),
                "createdAt": 7,
                "subagentInfo": {"parentAgentId":"parent-1"},
            }),
        );
        std::os::unix::fs::symlink(target, symlinked.join("store.db")).unwrap();
    }

    assert!(fixture.records().is_empty());
}

#[test]
fn cursor_chats_store_bounds_newest_workspace_directories() {
    let fixture = CursorSubagentFixture::new();
    for index in 0..40 {
        fixture.add_child(
            &format!("child-{index:02}"),
            Some("parent-1"),
            Some("explore"),
            index,
            1,
        );
    }
    assert_eq!(fixture.records().len(), 32);
}

#[test]
fn cursor_ask_projects_only_sanitized_pane_wait_truth() {
    const SENTINEL: &str = "PRIVATE_REASONING_AND_OPTION_SENTINEL";
    let fixture = CursorAskFixture::new(vec![pending_ask(
        "  What color do you like most?  ",
        false,
        Some(SENTINEL),
    )]);
    let observations = fixture.observations();
    let [observation] = observations.as_slice() else {
        panic!("expected one open Ask observation");
    };
    assert_eq!(observation.kind.as_str(), "cursor");
    assert_eq!(observation.session_id.as_str(), fixture.session_id);
    assert_eq!(observation.workspace, fixture.workspace);
    assert_eq!(observation.fresh_binding_at, None);
    assert!(observation.transcript_path.ends_with(format!(
        "agent-transcripts/{0}/{0}.jsonl",
        fixture.session_id
    )));
    let LocalSessionProjection::Lifecycle(state) = &observation.projection else {
        panic!("Cursor Ask must carry lifecycle display truth");
    };
    assert_eq!(state.status, AgentStatus::Waiting);
    assert_eq!(state.phase, TurnPhase::Idle);
    assert_eq!(
        state.native_prompt_detail.as_deref(),
        Some("What color do you like most?")
    );
    assert_eq!(
        state.waiting_since.map(jiff::Timestamp::as_millisecond),
        Some(1_735_689_610_000)
    );
    let normalized = serde_json::to_string(observation).unwrap();
    assert!(!normalized.contains(SENTINEL));
    assert!(!normalized.contains("private option"));
    assert!(!normalized.contains("tool-call-1"));
    assert!(!normalized.contains("store.db"));
}

#[test]
fn cursor_plan_proposal_projects_pane_only_wait_truth() {
    let fixture = CursorAskFixture::new(Vec::new());
    fixture.replace_state(
        Vec::new(),
        vec![create_plan_message()],
        Some("plan"),
        Some("file:///workspace/.cursor/plans/example.plan.md"),
    );

    let observations = fixture.observations();
    let [observation] = observations.as_slice() else {
        panic!("expected one open plan approval observation");
    };
    let LocalSessionProjection::Lifecycle(state) = &observation.projection else {
        panic!("Cursor plan approval must carry lifecycle display truth");
    };
    assert_eq!(state.status, AgentStatus::Waiting);
    assert_eq!(state.phase, TurnPhase::Idle);
    assert_eq!(
        state.native_prompt_detail.as_deref(),
        Some("Ready to build?")
    );
    assert_eq!(
        state.waiting_since.map(jiff::Timestamp::as_millisecond),
        Some(fixture.updated_at_ms)
    );
}

#[test]
fn cursor_pending_ask_takes_precedence_over_plan_proposal() {
    let fixture = CursorAskFixture::new(Vec::new());
    fixture.replace_state(
        vec![pending_ask("Which direction?", false, None)],
        vec![create_plan_message()],
        Some("plan"),
        Some("file:///workspace/.cursor/plans/example.plan.md"),
    );

    let observations = fixture.observations();
    let [observation] = observations.as_slice() else {
        panic!("expected one open Ask observation");
    };
    let LocalSessionProjection::Lifecycle(state) = &observation.projection else {
        panic!("Cursor Ask must carry lifecycle display truth");
    };
    assert_eq!(
        state.native_prompt_detail.as_deref(),
        Some("Which direction?")
    );
    assert_eq!(
        state.waiting_since.map(jiff::Timestamp::as_millisecond),
        Some(1_735_689_610_000)
    );
}

#[test]
fn cursor_plan_proposal_fails_closed_on_gate_and_message_drift() {
    const PLAN_URI: &str = "file:///workspace/.cursor/plans/example.plan.md";
    let fixture = CursorAskFixture::new(Vec::new());

    fixture.replace_state(
        Vec::new(),
        vec![create_plan_message()],
        None,
        Some(PLAN_URI),
    );
    assert!(fixture.observations().is_empty(), "mode is required");

    fixture.replace_state(
        Vec::new(),
        vec![create_plan_message()],
        Some("default"),
        Some(PLAN_URI),
    );
    assert!(fixture.observations().is_empty(), "mode must be plan");

    fixture.replace_state(Vec::new(), vec![create_plan_message()], Some("plan"), None);
    assert!(
        fixture.observations().is_empty(),
        "currentPlanUri is required"
    );

    fixture.replace_state(
        Vec::new(),
        vec![create_plan_message()],
        Some("plan"),
        Some("  "),
    );
    assert!(
        fixture.observations().is_empty(),
        "currentPlanUri must be nonempty"
    );

    fixture.replace_state(Vec::new(), Vec::new(), Some("plan"), Some(PLAN_URI));
    assert!(
        fixture.observations().is_empty(),
        "a last message is required"
    );

    fixture.replace_state(
        Vec::new(),
        vec![message(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "The plan is approved." }]
        }))],
        Some("plan"),
        Some(PLAN_URI),
    );
    assert!(
        fixture.observations().is_empty(),
        "a later assistant message supersedes the plan proposal"
    );

    fixture.replace_state(
        Vec::new(),
        vec![message(json!({
            "role": "tool",
            "content": [{
                "type": "tool-result",
                "toolName": "Shell",
                "result": "done"
            }]
        }))],
        Some("plan"),
        Some(PLAN_URI),
    );
    assert!(
        fixture.observations().is_empty(),
        "the last result must be CreatePlan"
    );

    let message_ids = fixture.replace_state(
        Vec::new(),
        vec![create_plan_message()],
        Some("plan"),
        Some(PLAN_URI),
    );
    let message_id = hex::encode(&message_ids[0]);
    let connection = Connection::open(fixture.session.join("store.db")).unwrap();
    connection
        .execute("DELETE FROM blobs WHERE id = ?1", [&message_id])
        .unwrap();
    drop(connection);
    assert!(
        fixture.observations().is_empty(),
        "the last message blob must exist"
    );

    let message_ids = fixture.replace_state(
        Vec::new(),
        vec![create_plan_message()],
        Some("plan"),
        Some(PLAN_URI),
    );
    let message_id = hex::encode(&message_ids[0]);
    let connection = Connection::open(fixture.session.join("store.db")).unwrap();
    connection
        .execute("UPDATE blobs SET data = X'00' WHERE id = ?1", [&message_id])
        .unwrap();
    drop(connection);
    assert!(
        fixture.observations().is_empty(),
        "the last message hash must match its id"
    );

    fixture.replace_state(
        Vec::new(),
        vec![vec![b'x'; 256 * 1024 + 1]],
        Some("plan"),
        Some(PLAN_URI),
    );
    assert!(
        fixture.observations().is_empty(),
        "the last message must stay inside the byte bound"
    );

    fixture.replace_state(
        Vec::new(),
        vec![b"{".to_vec()],
        Some("plan"),
        Some(PLAN_URI),
    );
    assert!(
        fixture.observations().is_empty(),
        "the last message must be valid JSON"
    );

    fixture.write_state(
        Vec::new(),
        vec![create_plan_message()],
        vec![vec![0; 31]],
        Some("plan"),
        Some(PLAN_URI),
    );
    assert!(
        fixture.observations().is_empty(),
        "the last message id must be a SHA-256 digest"
    );
}

#[test]
fn cursor_ask_rejects_async_ambiguity_and_control_prompts() {
    let fixture = CursorAskFixture::new(vec![pending_ask("Question?", true, None)]);
    assert!(
        fixture.observations().is_empty(),
        "async Ask is not blocking"
    );

    fixture.replace_pending(vec![
        pending_ask("First?", false, None),
        pending_ask("Second?", false, None),
    ]);
    assert!(
        fixture.observations().is_empty(),
        "two sync Asks are ambiguous"
    );

    fixture.replace_pending(vec![pending_ask(
        "<system-reminder>synthetic</system-reminder>",
        false,
        None,
    )]);
    assert!(
        fixture.observations().is_empty(),
        "control payloads cannot become native prompt detail"
    );
}

#[test]
fn cursor_ask_fails_closed_on_schema_path_timestamp_hash_and_size_drift() {
    let fixture = CursorAskFixture::new(vec![pending_ask("Question?", false, None)]);

    let connection = Connection::open(fixture.session.join("store.db")).unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);
    assert!(fixture.observations().is_empty());

    let connection = Connection::open(fixture.session.join("store.db")).unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    connection
        .execute("UPDATE blobs SET data = X'00'", [])
        .unwrap();
    drop(connection);
    assert!(fixture.observations().is_empty(), "content hash mismatch");

    fixture.replace_pending(vec![pending_ask("Question?", false, None)]);
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(fixture.session.join("meta.json")).unwrap()).unwrap();
    metadata["cwd"] = json!(fixture.workspace.join("other"));
    std::fs::write(
        fixture.session.join("meta.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    assert!(fixture.observations().is_empty(), "workspace mismatch");

    metadata["cwd"] = json!(fixture.workspace);
    metadata["updatedAtMs"] = json!(fixture.created_at_ms - 1);
    std::fs::write(
        fixture.session.join("meta.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    assert!(fixture.observations().is_empty(), "reversed timestamps");

    metadata["updatedAtMs"] = json!(fixture.created_at_ms + 20_000);
    std::fs::write(
        fixture.session.join("meta.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    let mut late_ask = pending_ask("Question?", false, None);
    late_ask["providerOptions"]["cursor"]["pendingToolCallStartedAtMs"] =
        json!(fixture.created_at_ms + 30_000);
    fixture.replace_pending(vec![late_ask]);
    assert!(
        fixture.observations().is_empty(),
        "Ask start must fall inside the session metadata bounds"
    );

    fixture.replace_pending(vec![json!({
        "role": "assistant",
        "content": [{
            "type": "reasoning",
            "text": "x".repeat(256 * 1024)
        }]
    })]);
    assert!(fixture.observations().is_empty(), "pending JSON byte bound");
}

#[test]
fn malformed_payloads_degrade_without_losing_the_event() {
    let observations: Vec<_> = [
        Value::Null,
        json!({ "status": "completed" }),
        json!({ "conversation_id": "  " }),
    ]
    .iter()
    .map(|payload| hook_lifecycle(&CursorAdapter, "sessionStart", payload))
    .collect();
    assert!(
        observations
            .iter()
            .all(|observation| observation.agent_id.is_none())
    );
    insta::assert_json_snapshot!(observations, @r###"
    [
      {
        "signal": {
          "signal": "registered"
        }
      },
      {
        "signal": {
          "signal": "registered"
        }
      },
      {
        "signal": {
          "signal": "registered"
        }
      }
    ]
    "###);
}

#[test]
fn malformed_fields_preserve_identity_response_and_token_composition() {
    let payload = json!({
        "conversation_id": "conv-1",
        "model_id": "cursor/model",
        "model": 7,
        "model_params": [false, {"id": "effort", "value": "high"}, {"id": 9, "value": []}],
        "transcript_path": "/tmp/conv.jsonl",
        "status": "completed",
        "input_tokens": 0,
        "output_tokens": "12",
        "cache_read_tokens": 3,
        "cache_write_tokens": {},
        "context_tokens": 999
    });
    let observed = hook_lifecycle(&CursorAdapter, "stop", &payload);
    assert_eq!(observed.agent_id.as_deref(), Some("conv-1"));
    assert_eq!(observed.launch.model.as_deref(), Some("cursor/model"));
    assert_eq!(observed.launch.effort.as_deref(), Some("high"));
    assert_eq!(observed.usage.fresh_input_tokens, Some(0));
    assert_eq!(observed.usage.output_tokens, Some(12));
    assert_eq!(observed.usage.cache_read_input_tokens, Some(3));
    assert_eq!(observed.usage.cache_write_input_tokens, None);
    assert_eq!(observed.usage.total_tokens, None);

    assert_eq!(
        hook_output(
            &CursorAdapter,
            "afterAgentResponse",
            &json!({"conversation_id": "conv-1", "text": "  safe final  ", "input_tokens": 9})
        )
        .assistant_message()
        .map(str::to_owned),
        Some("safe final".to_owned())
    );
    assert!(
        hook_output(&CursorAdapter, "stop", &json!({"text": "unsafe fallback"}))
            .assistant_message()
            .map(str::to_owned)
            .is_none()
    );
}

#[test]
fn stop_input_total_subtracts_cache_without_underflow() {
    let observed = hook_lifecycle(
        &CursorAdapter,
        "stop",
        &json!({
            "conversation_id": "conv-1",
            "status": "completed",
            "input_tokens": 22_725,
            "output_tokens": 26,
            "cache_read_tokens": 8_704,
            "cache_write_tokens": 0
        }),
    );
    assert_eq!(observed.usage.fresh_input_tokens, Some(14_021));
    assert_eq!(observed.usage.cache_read_input_tokens, Some(8_704));
    assert_eq!(observed.usage.output_tokens, Some(26));

    let malformed = hook_lifecycle(
        &CursorAdapter,
        "stop",
        &json!({
            "conversation_id": "conv-1",
            "status": "completed",
            "input_tokens": 3,
            "cache_read_tokens": 4,
            "cache_write_tokens": 5
        }),
    );
    assert_eq!(malformed.usage.fresh_input_tokens, Some(0));
}

#[test]
fn turn_cost_leaves_auto_unpriced_and_prices_explicit_fast_models() {
    let prices = PriceBook::fixture();
    for event_name in ["stop", "afterAgentResponse"] {
        assert!(
            CursorAdapter
                .price_turn_locally(
                    event_name,
                    &json!({
                        "generation_id": " gen-1 ",
                        "status": "completed",
                        "model_id": "default",
                        "input_tokens": 22_725,
                        "output_tokens": 26,
                        "cache_read_tokens": 8_704,
                        "cache_write_tokens": 0
                    }),
                    &prices,
                )
                .is_none()
        );
    }

    let response = CursorAdapter
        .price_turn_locally(
            "afterAgentResponse",
            &json!({
                "generation_id": "gen-1",
                "model_id": "gpt-5.4",
                "input_tokens": 22_725,
                "output_tokens": 26,
                "cache_read_tokens": 8_704,
                "cache_write_tokens": 0
            }),
            &prices,
        )
        .unwrap();
    assert_eq!(response.turn_id, "gen-1");
    assert!(response.cost_usd > 0.0);

    let payload = |model: &str| {
        json!({
            "generation_id": "gen-2",
            "status": "completed",
            "model_id": model,
            "input_tokens": 1_000,
            "output_tokens": 100,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        })
    };
    let base = CursorAdapter
        .price_turn_locally("stop", &payload("gpt-5.4"), &prices)
        .unwrap();
    let fast = CursorAdapter
        .price_turn_locally("stop", &payload("gpt-5.4-fast"), &prices)
        .unwrap();
    assert!((fast.cost_usd - base.cost_usd * 2.0).abs() < 1e-12);
    assert!(
        CursorAdapter
            .price_turn_locally("stop", &payload("unknown-future-model"), &prices)
            .is_none()
    );
    assert!(
        CursorAdapter
            .price_turn_locally(
                "stop",
                &json!({"generation_id": "", "status": "completed", "model_id": "default", "input_tokens": 1}),
                &prices,
            )
            .is_none()
    );
    assert!(
        CursorAdapter
            .price_turn_locally("postToolUse", &payload("gpt-5.4"), &prices)
            .is_none()
    );
    assert!(
        CursorAdapter
            .price_turn_locally(
                "stop",
                &json!({"generation_id": "gen", "status": "running", "model_id": "default", "input_tokens": 1}),
                &prices,
            )
            .is_none()
    );
    assert!(
        hook_observation(
            &CursorAdapter,
            "afterAgentResponse",
            &json!({"conversation_id": "conv-1", "input_tokens": 1})
        )
        .is_none(),
        "response pricing must not become a lifecycle or token source"
    );
}

#[test]
fn transcript_tail_reads_only_terminal_rows_and_resolves_exact_paths() {
    const SENTINEL: &str = "THINKING_SENTINEL_DO_NOT_INGEST";
    let fixture = include_str!("tests/fixtures/transcript.jsonl");
    assert!(
        fixture.contains(SENTINEL),
        "fixture exercises the privacy boundary"
    );
    assert_eq!(transcript::parse_terminal_for_test(fixture), None);
    let healed = format!("{}}}\n", fixture.trim_end());
    assert_eq!(
        transcript::parse_terminal_for_test(&healed),
        Some("complete")
    );
    assert!(CursorAdapter.parse_transcript_messages(fixture).is_empty());
    let page = CursorAdapter.stream_assistant_messages(fixture);
    assert!(page.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project-a/agent-transcripts/conv-1");
    std::fs::create_dir_all(&project).unwrap();
    let discovered = project.join("conv-1.jsonl");
    std::fs::write(&discovered, fixture).unwrap();
    assert_eq!(
        transcript::discover_under(dir.path(), "conv-1"),
        Some(discovered.clone())
    );
    assert!(transcript::discover_under(dir.path(), "../conv-1").is_none());

    let project_b = dir.path().join("project-b/agent-transcripts/conv-1");
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::write(project_b.join("conv-1.jsonl"), fixture).unwrap();
    assert!(transcript::discover_under(dir.path(), "conv-1").is_none());

    let current = dir.path().join("current.jsonl");
    let prior = dir.path().join("prior.jsonl");
    std::fs::write(&current, fixture).unwrap();
    std::fs::write(&prior, fixture).unwrap();
    assert_eq!(
        transcript::resolve_transcript("conv-1", Some(&current), Some(&prior)),
        Some(current)
    );
}

#[test]
fn transcript_recovery_requires_the_terminal_row_to_be_last() {
    let terminal = "{\"type\":\"turn_ended\",\"status\":\"success\"}";
    for later in [
        "{\"role\":\"user\",\"message\":{\"content\":[]}}",
        "{\"role\":\"assistant\",\"message\":{\"content\":[]}}",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_call\",\"name\":\"Read\"}]}}",
        "{\"type\":\"future_record\"}",
        "not-json",
    ] {
        let tail = format!("{terminal}\n{later}\n");
        assert_eq!(transcript::parse_terminal_for_test(&tail), None, "{later}");

        let healed = format!("{tail}{terminal}\n");
        assert_eq!(
            transcript::parse_terminal_for_test(&healed),
            Some("complete"),
            "{later}",
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    std::fs::write(&path, format!("{terminal}\n{{\"role\":\"user\"")).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    let pricing = dir.path().join("pricing-cache.json");
    let refresh = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        prior_session_name: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("torn transcript still registers its path");
    assert_eq!(refresh.context.settle, crate::agents::FieldPatch::Clear);

    std::fs::write(&path, format!("{terminal}\n{terminal}\n")).unwrap();
    let healed = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        prior_session_name: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: refresh.transcript_stat.as_ref(),
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("new complete terminal refresh");
    assert_eq!(
        healed.context.settle.as_set().map(|settle| settle.outcome),
        Some(crate::agents::TurnSettleOutcome::Complete)
    );
}

#[test]
fn transcript_refresh_registers_live_file_and_recovers_interruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    std::fs::write(&path, "{\"role\":\"user\",\"message\":{\"content\":[]}}\n").unwrap();
    let path_string = path.to_string_lossy().into_owned();
    let pricing = dir.path().join("pricing-cache.json");
    let first = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: Some("cursor/model"),
        prior_session_name: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("file identity refresh");
    assert_eq!(first.transcript_path.as_deref(), Some(path_string.as_str()));
    assert_eq!(
        first.context.model_id.as_set().map(String::as_str),
        Some("cursor/model")
    );
    assert_eq!(first.context.settle, crate::agents::FieldPatch::Clear);
    assert_eq!(first.context.turn_error, crate::agents::FieldPatch::Clear);

    std::fs::write(
        &path,
        "{\"role\":\"user\",\"message\":{\"content\":[]}}\n{\"type\":\"turn_ended\",\"status\":\"aborted\"}\n",
    )
    .unwrap();
    let interrupted = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        prior_session_name: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: first.transcript_stat.as_ref(),
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("changed transcript refresh");
    assert_eq!(
        interrupted
            .context
            .settle
            .as_set()
            .map(|settle| settle.outcome),
        Some(crate::agents::TurnSettleOutcome::Interrupted)
    );
    assert_eq!(
        interrupted.context.tokens,
        crate::agents::LocalTokenPatch::PreserveEstablished(None)
    );
    assert!(interrupted.context.cost.as_set().is_none());
}

#[test]
fn transcript_refresh_recovers_a_same_path_whole_file_rewrite() {
    const SENTINEL: &str = "THINKING_SENTINEL_DO_NOT_INGEST";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    let one_turn = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
        "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
    );
    std::fs::write(&path, one_turn).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    let pricing = dir.path().join("pricing-cache.json");
    let first = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        prior_session_name: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("first completed snapshot");
    let first_complete = first
        .context
        .settle
        .as_set()
        .expect("first terminal marker")
        .at;
    let first_stat = first.transcript_stat.expect("first transcript stat");

    let two_turns = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<sentinel>\"}]}}\n",
        "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
    )
    .replace("<sentinel>", SENTINEL);
    std::fs::write(&path, &two_turns).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)),
        )
        .unwrap();

    let rewritten = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        prior_session_name: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: Some(&first_stat),
        prior_spend_fold: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("whole-file rewrite refresh");
    assert_ne!(rewritten.transcript_stat, Some(first_stat));
    assert!(
        rewritten
            .context
            .settle
            .as_set()
            .is_some_and(|settle| settle.at > first_complete)
    );
    assert!(
        CursorAdapter
            .parse_transcript_messages(&two_turns)
            .is_empty()
    );
    assert!(
        CursorAdapter
            .stream_assistant_messages(&two_turns)
            .is_empty()
    );
}

#[test]
fn every_wired_event_returns_cursor_neutral_json() {
    // Cursor's hook contract requires JSON on every wired event, so the neutral
    // reply is `{}` — never silence, which Cursor reads as a malformed hook.
    for hook in CURSOR_HOOKS {
        assert_eq!(
            hook_output(&CursorAdapter, hook.event, &Value::Null)
                .json_reply()
                .cloned(),
            Some(json!({})),
            "{}",
            hook.event
        );
    }
    assert_eq!(
        hook_output(&CursorAdapter, "future", &Value::Null)
            .json_reply()
            .cloned(),
        None
    );
}

#[test]
fn launch_modes_presets_resume_and_compaction_are_cursor_native() {
    assert_eq!(
        CursorAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Ask),
        Vec::<String>::new()
    );
    assert_eq!(
        CursorAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Plan),
        vec!["--mode=plan"]
    );
    assert_eq!(
        CursorAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
        vec!["--auto-review"]
    );
    assert_eq!(
        CursorAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Yolo),
        vec!["--force", "--sandbox", "disabled"]
    );
    assert_eq!(
        CursorAdapter.spec().launch.compact_command(),
        Some("/summarize")
    );

    let launch = CursorAdapter
        .launch_command(&["--auto-review".to_owned()], Some("fix it"))
        .expect("fresh interactive launch");
    assert_eq!(
        &launch[launch.len() - 3..],
        ["--auto-review", "--", "fix it"]
    );
    assert!(
        launch.iter().all(|arg| arg != "-p" && arg != "--print"),
        "RimZ supervised Cursor runs stay on the interactive hook transport: {launch:?}",
    );

    let resume = CursorAdapter
        .resume_command("conv-1", Path::new("/tmp"))
        .expect("resume command");
    assert_eq!(&resume[resume.len() - 2..], ["--resume", "conv-1"]);
    assert!(CursorAdapter.spec().launch.fork_command("conv-1").is_none());
}

#[test]
fn hook_install_merges_idempotently_and_uninstalls_only_owned_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let state_path = dir.path().join("cursor-statusline.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "version": 99,
            "future": { "kept": true },
            "hooks": {
                "sessionStart": [{ "command": "user-hook" }],
                "futureEvent": [{ "command": "future-hook" }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let original_config = r#"{ "statusLine": { "type": "command", "command": "'/tmp/my status' --plain '$HOME'", "padding": 2, "updateIntervalMs": 500, "timeoutMs": 1000, "future": true }, "theme": "dark" }
"#;
    std::fs::write(&config_path, original_config).unwrap();

    let report = install::install_into(&path, &config_path, &state_path).expect("install");
    assert_eq!(report.files.len(), 3);
    assert!(report.files[..2].iter().all(|file| file.existed));
    assert!(!report.files[2].existed);
    assert_eq!(report.installed_events.len(), CURSOR_HOOKS.len());
    assert!(install::hooks_installed_at(&path));
    assert!(install::statusline_installed_at(&config_path));
    let once = std::fs::read_to_string(&path).unwrap();
    let config_once = std::fs::read_to_string(&config_path).unwrap();
    let second = install::install_into(&path, &config_path, &state_path).expect("second install");
    assert_eq!(
        second.files.len(),
        2,
        "untouched sidecar stays out of reports"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), once);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), config_once);

    let installed: Value = serde_json::from_str(&once).unwrap();
    assert_eq!(installed["version"], 1);
    assert_eq!(installed["future"]["kept"], true);
    assert_eq!(
        installed["hooks"]["sessionStart"][0]["command"],
        "user-hook"
    );
    assert_eq!(
        installed["hooks"]["futureEvent"][0]["command"],
        "future-hook"
    );
    assert!(install::managed_artifacts_at(&path));
    assert!(install::statusline_artifact_at(&config_path));
    let installed_config: Value = serde_json::from_str(&config_once).unwrap();
    assert_eq!(installed_config["statusLine"]["padding"], 2);
    assert_eq!(installed_config["statusLine"]["updateIntervalMs"], 500);
    assert!(installed_config["statusLine"].get("future").is_none());
    assert!(
        installed_config["statusLine"]
            .get("_rimz_managed")
            .is_none()
    );
    assert!(
        installed_config["statusLine"]
            .get("_rimz_wrapped")
            .is_none()
    );
    assert_eq!(
        install::wrapped_status_line_command_at(&config_path, &state_path).as_deref(),
        Some("'/tmp/my status' --plain '$HOME'")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&state_path).unwrap()).unwrap(),
        json!({
            "statusLine": {
                "type": "command",
                "command": "'/tmp/my status' --plain '$HOME'",
                "padding": 2,
                "updateIntervalMs": 500,
                "timeoutMs": 1000,
                "future": true
            }
        })
    );
    assert!(state_path.exists(), "the sidecar is a managed artifact");

    let preview = install::preview_at(&path, &config_path, &state_path).expect("preview");
    assert_eq!(
        preview.files.len(),
        2,
        "untouched sidecar stays out of previews"
    );
    assert_eq!(preview.files[0].candidate, once);
    assert_eq!(preview.files[1].candidate, config_once);
    let uninstall = install::uninstall_from(&path, &config_path, &state_path).expect("uninstall");
    assert_eq!(uninstall.files.len(), 3);
    assert_eq!(uninstall.removed_events.len(), CURSOR_HOOKS.len());
    assert!(!install::managed_artifacts_at(&path));
    let uninstalled: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        uninstalled["hooks"]["sessionStart"][0]["command"],
        "user-hook"
    );
    assert_eq!(
        uninstalled["hooks"]["futureEvent"][0]["command"],
        "future-hook"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(config_path).unwrap()).unwrap(),
        serde_json::from_str::<Value>(original_config).unwrap()
    );
    assert!(!state_path.exists());
}

#[test]
fn cursor_config_sanitization_preserves_statusline_ownership_and_restore_state() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let state_path = dir.path().join("cursor-statusline.json");
    let original = json!({
        "theme": "dark",
        "statusLine": {
            "type": "command",
            "command": "user-status --compact",
            "padding": 3,
            "future": { "kept_on_restore": true }
        }
    });
    std::fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&original).unwrap()),
    )
    .unwrap();

    let preview = install::preview_at(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(preview.files.len(), 3);
    assert_eq!(preview.files[2].path, state_path);
    assert!(!preview.files[2].existed);
    assert_eq!(
        preview.status_line_change,
        Some(crate::agents::StatusLineChange::Wrapping {
            original: "user-status --compact".to_owned()
        })
    );
    install::install_into(&hooks_path, &config_path, &state_path).unwrap();

    let mut sanitized = original.as_object().unwrap().clone();
    sanitized.insert(
        "statusLine".to_owned(),
        json!({
            "type": "command",
            "command": RIMZ_STATUS_LINE_COMMAND
        }),
    );
    let sanitized = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(sanitized)).unwrap()
    );
    std::fs::write(&config_path, &sanitized).unwrap();

    assert!(install::statusline_installed_at(&config_path));
    let preview = install::preview_at(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(preview.files.len(), 2);
    assert_eq!(preview.files[1].candidate, sanitized);
    assert_eq!(
        preview.status_line_change,
        Some(crate::agents::StatusLineChange::Unchanged)
    );
    assert_eq!(
        install::wrapped_status_line_command_at(&config_path, &state_path).as_deref(),
        Some("user-status --compact")
    );

    let state_before = std::fs::read(&state_path).unwrap();
    let second = install::install_into(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(second.files.len(), 2);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), sanitized);
    assert_eq!(std::fs::read(&state_path).unwrap(), state_before);

    install::uninstall_from(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&config_path).unwrap()).unwrap(),
        original
    );
    assert!(!state_path.exists());
}

#[test]
fn legacy_inline_statusline_state_migrates_and_remains_an_uninstall_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let state_path = dir.path().join("cursor-statusline.json");
    let wrapped = json!({
        "type": "command",
        "command": "legacy-user-status",
        "padding": 4,
        "future": true
    });
    let legacy = json!({
        "theme": "dark",
        "statusLine": {
            "type": "command",
            "command": RIMZ_STATUS_LINE_COMMAND,
            "padding": 4,
            "_rimz_managed": true,
            "_rimz_wrapped": wrapped
        }
    });
    std::fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&legacy).unwrap()),
    )
    .unwrap();

    assert!(install::statusline_installed_at(&config_path));
    let preview = install::preview_at(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(preview.files.len(), 3);
    assert_eq!(
        preview.status_line_change,
        Some(crate::agents::StatusLineChange::Unchanged)
    );
    let candidate: Value = serde_json::from_str(&preview.files[1].candidate).unwrap();
    assert!(candidate["statusLine"].get("_rimz_managed").is_none());
    assert!(candidate["statusLine"].get("_rimz_wrapped").is_none());

    install::install_into(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&state_path).unwrap()).unwrap(),
        json!({ "statusLine": wrapped })
    );
    install::uninstall_from(&hooks_path, &config_path, &state_path).unwrap();
    let restored: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(restored["statusLine"], wrapped);

    let fallback_config = dir.path().join("legacy-cli-config.json");
    let fallback_hooks = dir.path().join("legacy-hooks.json");
    let missing_state = dir.path().join("missing-state.json");
    std::fs::write(
        &fallback_config,
        format!("{}\n", serde_json::to_string_pretty(&legacy).unwrap()),
    )
    .unwrap();
    assert_eq!(
        install::wrapped_status_line_command_at(&fallback_config, &missing_state).as_deref(),
        Some("legacy-user-status")
    );
    install::uninstall_from(&fallback_hooks, &fallback_config, &missing_state).unwrap();
    let fallback: Value =
        serde_json::from_str(&std::fs::read_to_string(fallback_config).unwrap()).unwrap();
    assert_eq!(fallback["statusLine"], wrapped);
}

#[test]
fn foreign_cursor_statusline_stays_unowned_and_survives_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let state_path = dir.path().join("cursor-statusline.json");
    let foreign = json!({
        "statusLine": {
            "type": "command",
            "command": "foreign-status",
            "padding": 2
        }
    });
    std::fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&foreign).unwrap()),
    )
    .unwrap();

    assert!(!install::statusline_installed_at(&config_path));
    assert!(!install::statusline_artifact_at(&config_path));
    let preview = install::preview_at(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(
        preview.status_line_change,
        Some(crate::agents::StatusLineChange::Wrapping {
            original: "foreign-status".to_owned()
        })
    );

    let report = install::uninstall_from(&hooks_path, &config_path, &state_path).unwrap();
    assert_eq!(report.files.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(config_path).unwrap()).unwrap(),
        foreign
    );
}

#[test]
fn legacy_hook_install_is_detected_and_repaired_additively() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let state_path = dir.path().join("cursor-statusline.json");
    let legacy_events = [
        "sessionStart",
        "beforeSubmitPrompt",
        "postToolUse",
        "postToolUseFailure",
        "afterAgentResponse",
        "stop",
        "sessionEnd",
        "preCompact",
    ];
    let mut hooks = serde_json::Map::new();
    for event in legacy_events {
        hooks.insert(
            (*event).to_owned(),
            json!([
                { "command": format!("user-{event}-hook") },
                { "command": RIMZ_HOOK_COMMAND }
            ]),
        );
    }
    hooks.insert(
        "futureEvent".to_owned(),
        json!([{ "command": "future-user-hook" }]),
    );
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "hooks": hooks,
            "future": { "kept": true }
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(!install::hooks_installed_at(&path));
    let preview =
        install::preview_at(&path, &config_path, &state_path).expect("legacy repair preview");
    let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(candidate["future"]["kept"], true);
    assert_eq!(
        candidate["hooks"]["futureEvent"][0]["command"],
        "future-user-hook"
    );
    for hook in CURSOR_HOOKS {
        let event = hook.event;
        let entries = candidate["hooks"][event].as_array().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["command"] == RIMZ_HOOK_COMMAND)
                .count(),
            1,
            "{event}",
        );
        if legacy_events.contains(&event) {
            assert!(
                entries.iter().any(|entry| {
                    entry["command"] == Value::String(format!("user-{event}-hook"))
                })
            );
        }
    }

    install::install_into(&path, &config_path, &state_path).expect("repair legacy install");
    assert!(install::hooks_installed_at(&path));
    assert!(install::statusline_installed_at(&config_path));
    let once = std::fs::read(&path).unwrap();
    install::install_into(&path, &config_path, &state_path).expect("second repair install");
    assert_eq!(std::fs::read(&path).unwrap(), once);
}

#[test]
fn incomplete_and_malformed_hook_configs_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let state_path = dir.path().join("cursor-statusline.json");
    install::install_into(&path, &config_path, &state_path).expect("install");
    let mut root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    root["hooks"].as_object_mut().unwrap().remove("stop");
    std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    assert!(!install::hooks_installed_at(&path));

    std::fs::write(&path, "{").unwrap();
    assert!(matches!(
        install::install_into(&path, &config_path, &state_path),
        Err(AgentErr::InstallParse {
            agent: "cursor",
            ..
        })
    ));
}

#[test]
fn hook_command_preserves_parent_pid_attribution() {
    assert_eq!(CursorAdapter.spec().bin_names, ["cursor-agent", "agent"]);
    assert!(!CursorAdapter.spec().bin_names.contains(&"cursor"));
    assert!(RIMZ_HOOK_COMMAND.starts_with("RIMZ_AGENT_PID=$PPID"));
    assert!(RIMZ_HOOK_COMMAND.contains("--source cursor"));
}
