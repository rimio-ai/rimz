use super::*;

use std::io::Write;
use std::path::{Path, PathBuf};

use super::super::subagents::{spawned_subagents_under, subagent_transcripts_under};
use crate::ids::AgentSessionId;

fn legacy_session() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.jsonl");
    let children = dir.path().join("parent/subagents");
    std::fs::create_dir_all(&children).unwrap();
    std::fs::write(&parent, "").unwrap();
    (dir, parent, children)
}

fn chat_session() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("parent");
    let parent = session.join("chat.jsonl");
    let children = session.join("subagents");
    std::fs::create_dir_all(&children).unwrap();
    std::fs::write(&parent, "").unwrap();
    (dir, parent, children)
}

fn assistant(agent_id: &str, stop_reason: &str) -> serde_json::Value {
    json!({
        "type": "assistant",
        "isSidechain": true,
        "agentId": agent_id,
        "timestamp": "2026-07-20T12:00:00Z",
        "message": {
            "model": "claude-sonnet-4-5",
            "stop_reason": stop_reason,
            "content": [{"type": "text", "text": "working"}],
            "usage": {
                "input_tokens": 5,
                "cache_read_input_tokens": 6,
                "cache_creation_input_tokens": 7,
                "output_tokens": 8
            }
        }
    })
}

fn interrupted(agent_id: &str) -> serde_json::Value {
    json!({
        "type": "user",
        "isSidechain": true,
        "agentId": agent_id,
        "timestamp": "2026-07-20T12:01:00Z",
        "message": {"content": "[Request interrupted by user for tool use]"}
    })
}

fn write_records(path: &Path, records: &[serde_json::Value]) {
    let contents = records
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{contents}\n")).unwrap();
}

#[test]
fn interrupted_child_recovers_identity_and_usage() {
    let (_dir, parent, children) = legacy_session();
    let child = children.join("agent-child-1.jsonl");
    write_records(
        &child,
        &[assistant("child-1", "tool_use"), interrupted("child-1")],
    );
    std::fs::write(
        child.with_extension("meta.json"),
        json!({
            "agentType": " general-purpose ",
            "description": "<user_query> inspect lifecycle </user_query>",
            "toolUseId": "tool-1",
            "spawnDepth": 1
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(
        ClaudeAdapter.spawned_subagents(SubagentSpawnInput {
            parent_agent_id: &AgentSessionId::from("parent"),
            parent_transcript_path: Some(&parent),
            parent_workspace: None,
        }),
        vec![SpawnedSubagent {
            child_agent_id: AgentSessionId::from("child-1"),
            agent_name: Some("general-purpose".to_owned()),
            role: None,
            prompt: Some("inspect lifecycle".to_owned()),
            model: Some("claude-sonnet-4-5".to_owned()),
            total_tokens: Some(26),
        }]
    );
}

#[test]
fn marker_only_child_closes_without_optional_identity() {
    let (_dir, parent, children) = chat_session();
    write_records(&children.join("agent-zero.jsonl"), &[interrupted("zero")]);

    let children = spawned_subagents_under(&parent);
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].child_agent_id, "zero");
    assert!(children[0].agent_name.is_none());
    assert!(children[0].prompt.is_none());
    assert!(children[0].model.is_none());
    assert_eq!(children[0].total_tokens, Some(0));
}

#[test]
fn live_and_cleanly_completed_children_are_not_derived_closes() {
    let (_dir, parent, children) = legacy_session();
    write_records(
        &children.join("agent-running.jsonl"),
        &[assistant("running", "tool_use")],
    );
    write_records(
        &children.join("agent-completed.jsonl"),
        &[assistant("completed", "end_turn")],
    );

    assert!(spawned_subagents_under(&parent).is_empty());
}

#[test]
fn malformed_files_degrade_independently() {
    let (_dir, parent, children) = legacy_session();
    let valid = children.join("agent-valid.jsonl");
    write_records(&valid, &[interrupted("valid")]);
    std::fs::write(valid.with_extension("meta.json"), "{torn").unwrap();

    std::fs::write(children.join("agent-empty.jsonl"), "").unwrap();
    std::fs::write(children.join("agent-torn.jsonl"), "{torn").unwrap();
    write_records(
        &children.join("not-an-agent.jsonl"),
        &[interrupted("not-an-agent")],
    );
    let torn_suffix = children.join("agent-torn-suffix.jsonl");
    write_records(&torn_suffix, &[interrupted("torn-suffix")]);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&torn_suffix)
        .unwrap()
        .write_all(b"{torn")
        .unwrap();

    let found = spawned_subagents_under(&parent);
    assert_eq!(
        found
            .iter()
            .map(|child| child.child_agent_id.as_str())
            .collect::<Vec<_>>(),
        ["torn-suffix", "valid"]
    );
    assert!(found.iter().all(|child| child.agent_name.is_none()));
}

#[test]
fn nested_replay_cannot_close_the_outer_child() {
    let (_dir, parent, children) = legacy_session();
    let mut nested_usage = assistant("nested", "tool_use");
    nested_usage["message"]["model"] = json!("nested-model");
    nested_usage["message"]["usage"]["input_tokens"] = json!(999);
    write_records(
        &children.join("agent-outer.jsonl"),
        &[
            assistant("outer", "tool_use"),
            interrupted("outer"),
            nested_usage,
        ],
    );
    write_records(
        &children.join("agent-only-nested.jsonl"),
        &[interrupted("nested")],
    );

    let found = spawned_subagents_under(&parent);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].child_agent_id, "outer");
    assert_eq!(found[0].model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(found[0].total_tokens, Some(26));
    assert!(
        ClaudeAdapter
            .spawned_subagents(SubagentSpawnInput {
                parent_agent_id: &AgentSessionId::from("parent"),
                parent_transcript_path: None,
                parent_workspace: None,
            })
            .is_empty()
    );
}

#[test]
fn spend_transcripts_include_sorted_companions_for_both_session_layouts() {
    for session in [legacy_session(), chat_session()] {
        let (_dir, parent, children) = session;
        std::fs::write(children.join("agent-z.jsonl"), "").unwrap();
        std::fs::write(children.join("agent-a.jsonl"), "").unwrap();
        std::fs::write(children.join("agent-a.meta.json"), "").unwrap();

        assert_eq!(
            subagent_transcripts_under(&parent),
            [
                children.join("agent-a.jsonl"),
                children.join("agent-z.jsonl")
            ]
        );
        assert_eq!(
            ClaudeAdapter.session_spend_transcripts("parent", Some(&parent)),
            [
                parent.clone(),
                children.join("agent-a.jsonl"),
                children.join("agent-z.jsonl")
            ]
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("missing.jsonl");
    std::fs::write(&parent, "").unwrap();
    assert_eq!(
        ClaudeAdapter.session_spend_transcripts("missing", Some(&parent)),
        [parent]
    );
}
