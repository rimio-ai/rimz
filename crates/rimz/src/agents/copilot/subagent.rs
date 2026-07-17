//! Parent-transcript fold for Copilot child hook sessions.

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
    SubagentStarted { data: SubagentStartedData },
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
            CorrelationRecord::SubagentStarted { data } => {
                let child_id = data.tool_call_id.clone();
                children.entry(child_id).or_default().started = Some(data);
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
            .filter_map(|(child_id, child)| correlated(child_id, child))
            .collect(),
    )
}

fn correlated(child_id: String, child: FoldedChild) -> Option<Correlated> {
    let started = child.started?;
    let execution = child.execution.unwrap_or(ToolExecutionStartData {
        tool_call_id: child_id.clone(),
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
        child_id,
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
