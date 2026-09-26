//! Parent-transcript fold for Copilot child hook sessions.
//!
//! A child's hook `sessionId` is the top-level `agentId` of the parent's
//! `subagent.started` record. Copilot CLI 1.0.83 mints a fresh UUID for it;
//! 1.0.71 set it to the task `toolCallId`, which stays the fallback key.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::paths;
use crate::agents::transcript_fs::{
    deserialize_optional_string_lossy, deserialize_optional_u64_lossy, read_transcript_tail,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CorrelationRecord {
    #[serde(rename = "subagent.started")]
    SubagentStarted {
        data: SubagentStartedData,
        #[serde(
            default,
            rename = "agentId",
            deserialize_with = "deserialize_optional_string_lossy"
        )]
        agent_id: Option<String>,
    },
    #[serde(rename = "subagent.completed")]
    SubagentCompleted { data: SubagentCompletedData },
    #[serde(rename = "tool.execution_start")]
    ToolExecutionStart { data: ToolExecutionStartData },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentStartedData {
    tool_call_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    agent_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    agent_display_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentCompletedData {
    tool_call_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecutionStartData {
    tool_call_id: String,
    tool_name: String,
    #[serde(default)]
    arguments: TaskArguments,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TaskArguments {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    prompt: Option<String>,
}

#[derive(Debug, Default)]
struct FoldedChild {
    agent_id: Option<String>,
    execution: Option<ToolExecutionStartData>,
    started: Option<SubagentStartedData>,
    completed: Option<SubagentCompletedData>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Correlated {
    pub(super) child_id: String,
    pub(super) agent_name: Option<String>,
    pub(super) task: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) model: Option<String>,
    pub(super) total_tokens: Option<u64>,
    pub(super) completed: bool,
}

pub(super) fn correlate(
    parent_transcript: &Path,
    parent_id: &str,
    child_id: &str,
) -> Option<Correlated> {
    fold(parent_transcript, parent_id)?
        .into_iter()
        .find(|child| child.child_id == child_id)
}

pub(super) fn completed(parent_transcript: &Path, parent_id: &str) -> Vec<Correlated> {
    fold(parent_transcript, parent_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|child| child.completed)
        .collect()
}

fn fold(parent_transcript: &Path, parent_id: &str) -> Option<Vec<Correlated>> {
    let transcript = paths::validated_transcript_path(parent_transcript, parent_id)?;
    let tail = read_transcript_tail(&transcript)?;
    let mut children = BTreeMap::<String, FoldedChild>::new();
    for line in tail.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<CorrelationRecord>(line) else {
            continue;
        };
        match record {
            CorrelationRecord::SubagentStarted { data, agent_id } => {
                let child = children.entry(data.tool_call_id.clone()).or_default();
                child.agent_id = normalized(agent_id.as_deref());
                child.started = Some(data);
            }
            CorrelationRecord::SubagentCompleted { data } => {
                let child_id = data.tool_call_id.clone();
                children.entry(child_id).or_default().completed = Some(data);
            }
            CorrelationRecord::ToolExecutionStart { data } if data.tool_name == "task" => {
                let child_id = data.tool_call_id.clone();
                children.entry(child_id).or_default().execution = Some(data);
            }
            _ => {}
        }
    }
    Some(
        children
            .into_iter()
            .filter_map(|(tool_call_id, child)| correlated(tool_call_id, child))
            .collect(),
    )
}

fn correlated(tool_call_id: String, child: FoldedChild) -> Option<Correlated> {
    let started = child.started?;
    let execution = child.execution.unwrap_or(ToolExecutionStartData {
        tool_call_id: tool_call_id.clone(),
        tool_name: "task".to_owned(),
        arguments: TaskArguments::default(),
        model: None,
    });
    let completed = child.completed;
    let model = completed
        .as_ref()
        .and_then(|record| normalized(record.model.as_deref()))
        .or_else(|| normalized(started.model.as_deref()))
        .or_else(|| normalized(execution.model.as_deref()));
    Some(Correlated {
        child_id: child.agent_id.unwrap_or(tool_call_id),
        agent_name: normalized(execution.arguments.name.as_deref())
            .or_else(|| normalized(started.agent_name.as_deref())),
        task: normalized(execution.arguments.description.as_deref())
            .or_else(|| normalized(started.agent_display_name.as_deref())),
        prompt: normalized(execution.arguments.prompt.as_deref()),
        model,
        total_tokens: completed.as_ref().and_then(|record| record.total_tokens),
        completed: completed.is_some(),
    })
}

fn normalized(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("tests/fixtures/subagents.jsonl");

    fn transcript(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("parent-session");
        std::fs::create_dir(&session).unwrap();
        let path = session.join("events.jsonl");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn correlates_start_and_completion_metadata_by_exact_child_id() {
        let (_dir, path) = transcript(FIXTURE);

        use crate::agents::capabilities::HookCapability;
        let children =
            super::super::CopilotAdapter.spawned_subagents(crate::agents::SubagentSpawnInput {
                parent_agent_id: &crate::ids::AgentSessionId::from("parent-session"),
                parent_transcript_path: Some(&path),
                parent_workspace: None,
            });
        let child = children
            .iter()
            .find(|child| child.child_agent_id == "toolu_alpha")
            .unwrap();
        assert_eq!(child.usage.run_total_tokens, Some(22_116));
        assert_eq!(child.usage.total_tokens, None);

        assert_eq!(
            correlate(&path, "parent-session", "toolu_alpha"),
            Some(Correlated {
                child_id: "toolu_alpha".to_owned(),
                agent_name: Some("researcher".to_owned()),
                task: Some("Inspect auth retry".to_owned()),
                prompt: Some("Trace the retry flow".to_owned()),
                model: Some("claude-haiku-4.5".to_owned()),
                total_tokens: Some(22_116),
                completed: true,
            })
        );
    }

    #[test]
    fn correlates_uuid_child_ids_through_the_start_record_agent_id() {
        let (_dir, path) = transcript(include_str!("tests/fixtures/subagents-agent-id.jsonl"));
        let child_id = "6f1c2e0a-1111-4a5b-9c3d-000000000001";

        assert_eq!(correlate(&path, "parent-session", "call_alpha"), None);
        assert_eq!(
            correlate(&path, "parent-session", child_id),
            Some(Correlated {
                child_id: child_id.to_owned(),
                agent_name: Some("readme-first-line".to_owned()),
                task: Some("Read README first line".to_owned()),
                prompt: Some("View README.md and report its first line".to_owned()),
                model: Some("gpt-5.6-luna".to_owned()),
                total_tokens: Some(11_444),
                completed: true,
            })
        );
        assert_eq!(
            completed(&path, "parent-session")
                .into_iter()
                .map(|child| child.child_id)
                .collect::<Vec<_>>(),
            [child_id]
        );
    }

    #[test]
    fn start_records_without_agent_id_fall_back_to_the_tool_call_id() {
        let without_agent_id = FIXTURE.replace(r#","agentId":"toolu_alpha""#, "");
        let (_dir, path) = transcript(&without_agent_id);

        assert!(correlate(&path, "parent-session", "toolu_alpha").is_some());
    }

    #[test]
    fn provider_start_is_required_and_tool_model_is_the_fallback() {
        let without_start = FIXTURE
            .lines()
            .filter(|line| !line.contains(r#""subagent.started""#))
            .collect::<Vec<_>>()
            .join("\n");
        let (_dir, path) = transcript(&without_start);
        assert!(correlate(&path, "parent-session", "toolu_alpha").is_none());

        let start_without_model = FIXTURE
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n")
            .replace(r#","model":"claude-haiku-4.5"}"#, "}");
        let (_dir, path) = transcript(&start_without_model);
        assert_eq!(
            correlate(&path, "parent-session", "toolu_alpha").and_then(|child| child.model),
            Some("claude-haiku-4.5".to_owned())
        );
    }

    #[test]
    fn incomplete_children_correlate_but_do_not_spawn() {
        let before_completion = FIXTURE.lines().take(2).collect::<Vec<_>>().join("\n");
        let (_dir, path) = transcript(&before_completion);
        let child = correlate(&path, "parent-session", "toolu_alpha").unwrap();
        assert_eq!(child.model.as_deref(), Some("claude-haiku-4.5"));
        assert_eq!(child.total_tokens, None);
        assert!(!child.completed);
        assert!(completed(&path, "parent-session").is_empty());
    }

    #[test]
    fn malformed_and_sibling_records_do_not_hide_valid_children() {
        let contents = format!("{{malformed\n{FIXTURE}");
        let (_dir, path) = transcript(&contents);

        assert_eq!(completed(&path, "parent-session").len(), 2);
        assert_eq!(correlate(&path, "parent-session", "toolu_missing"), None);
        assert_eq!(
            correlate(&path, "parent-session", "toolu_beta")
                .unwrap()
                .agent_name
                .as_deref(),
            Some("general-purpose")
        );
    }
}
