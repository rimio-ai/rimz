//! Parent-transcript correlation for Copilot child hook sessions.

use std::path::Path;

use serde::Deserialize;

use super::paths;
use crate::agents::transcript_fs::read_transcript_tail;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CorrelationRecord {
    #[serde(rename = "subagent.started")]
    SubagentStarted { data: SubagentStartedData },
    #[serde(rename = "tool.execution_start")]
    ToolExecutionStart { data: ToolExecutionStartData },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentStartedData {
    tool_call_id: String,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    agent_display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecutionStartData {
    tool_call_id: String,
    tool_name: String,
    #[serde(default)]
    arguments: TaskArguments,
}

#[derive(Debug, Default, Deserialize)]
struct TaskArguments {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Correlated {
    pub(super) agent_name: Option<String>,
    pub(super) task: Option<String>,
    pub(super) prompt: Option<String>,
}

pub(super) fn correlate(
    parent_transcript: &Path,
    parent_id: &str,
    child_id: &str,
) -> Option<Correlated> {
    let transcript = paths::validated_transcript_path(parent_transcript, parent_id)?;
    let tail = read_transcript_tail(&transcript)?;
    let mut started = None;
    let mut execution = None;
    for line in tail.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<CorrelationRecord>(line) else {
            continue;
        };
        match record {
            CorrelationRecord::SubagentStarted { data } if data.tool_call_id == child_id => {
                started = Some(data);
            }
            CorrelationRecord::ToolExecutionStart { data }
                if data.tool_call_id == child_id && data.tool_name == "task" =>
            {
                execution = Some(data.arguments);
            }
            _ => {}
        }
    }
    let started = started?;
    let execution = execution.unwrap_or_default();
    Some(Correlated {
        agent_name: non_empty(execution.name).or_else(|| non_empty(started.agent_name)),
        task: non_empty(execution.description).or_else(|| non_empty(started.agent_display_name)),
        prompt: non_empty(execution.prompt),
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
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
    fn correlates_child_and_maps_task_metadata() {
        let (_dir, path) = transcript(FIXTURE);

        assert_eq!(
            correlate(&path, "parent-session", "toolu_alpha"),
            Some(Correlated {
                agent_name: Some("researcher".to_owned()),
                task: Some("Inspect auth retry".to_owned()),
                prompt: Some("Trace the retry flow".to_owned()),
            })
        );
    }

    #[test]
    fn sibling_child_id_does_not_match() {
        let (_dir, path) = transcript(FIXTURE);

        assert_eq!(correlate(&path, "parent-session", "toolu_missing"), None);
    }

    #[test]
    fn malformed_lines_do_not_hide_a_valid_relation() {
        let contents = format!("{{malformed\n{FIXTURE}");
        let (_dir, path) = transcript(&contents);

        assert!(correlate(&path, "parent-session", "toolu_alpha").is_some());
    }

    #[test]
    fn started_metadata_fills_missing_task_arguments() {
        let (_dir, path) = transcript(FIXTURE);

        assert_eq!(
            correlate(&path, "parent-session", "toolu_beta"),
            Some(Correlated {
                agent_name: Some("general-purpose".to_owned()),
                task: Some("Test reviewer".to_owned()),
                prompt: Some("Review the tests".to_owned()),
            })
        );
    }
}
