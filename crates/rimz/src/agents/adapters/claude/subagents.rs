//! Provider-durable recovery for Claude child brackets interrupted by Esc.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{statusline, usage_from_transcript};
use crate::agents::{SpawnedSubagent, non_empty_trimmed, read_transcript_tail};
use crate::ids::AgentSessionId;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SubagentMetadata {
    agent_type: Option<String>,
    description: Option<String>,
}

pub(super) fn spawned_subagents_under(parent_transcript: &Path) -> Vec<SpawnedSubagent> {
    let Some(subagents_dir) = subagents_dir(parent_transcript) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(subagents_dir) else {
        return Vec::new();
    };
    let mut transcripts = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| path.extension() == Some(OsStr::new("jsonl")))
        .collect::<Vec<_>>();
    transcripts.sort();

    transcripts
        .into_iter()
        .filter_map(interrupted_subagent)
        .collect()
}

fn subagents_dir(parent_transcript: &Path) -> Option<PathBuf> {
    if parent_transcript.file_name() == Some(OsStr::new("chat.jsonl")) {
        return Some(parent_transcript.parent()?.join("subagents"));
    }
    Some(parent_transcript.with_extension("").join("subagents"))
}

fn interrupted_subagent(transcript: PathBuf) -> Option<SpawnedSubagent> {
    let child_id = transcript.file_stem()?.to_str()?.strip_prefix("agent-")?;
    if child_id.is_empty() {
        return None;
    }
    let tail = read_transcript_tail(&transcript)?;
    statusline::detect_subagent_interrupted(&tail, child_id).then_some(())?;

    let metadata = read_metadata(&transcript.with_extension("meta.json"));
    let usage = usage_from_transcript(&transcript);
    Some(SpawnedSubagent {
        child_agent_id: AgentSessionId::from(child_id),
        agent_name: metadata.agent_type.as_deref().and_then(non_empty_trimmed),
        role: None,
        prompt: metadata.description.as_deref().and_then(non_empty_trimmed),
        model: usage.model,
        total_tokens: usage.total_tokens,
    })
}

fn read_metadata(path: &Path) -> SubagentMetadata {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}
