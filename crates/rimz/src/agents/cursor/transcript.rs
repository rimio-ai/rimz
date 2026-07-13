//! Privacy-safe Cursor transcript tail parsing.
//!
//! Cursor transcript assistant blocks combine visible prose and thinking. This
//! module deliberately models only the terminal row discriminator and outcome.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::context::{AgentTurnError, TurnErrorClass};
use crate::agents::transcript_fs::deserialize_optional_string_lossy;
use crate::agents::{LocalContextRefresh, LocalContextRefreshCtx, TranscriptStat};

#[derive(Debug, Deserialize)]
struct TerminalRecord {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    r#type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum TerminalOutcome {
    Complete,
    Interrupted,
    Error(Option<String>),
}

pub(super) fn refresh(ctx: &LocalContextRefreshCtx<'_>) -> Option<LocalContextRefresh> {
    let path = resolve_transcript(
        ctx.agent_id,
        ctx.current_transcript_path.map(Path::new),
        ctx.prior_transcript_path.map(Path::new),
    )?;
    let stat = transcript_stat(&path)?;
    if ctx.prior_transcript_stat == Some(&stat) {
        return None;
    }
    let tail = crate::agents::transcript_fs::read_transcript_tail_with_status(&path)?;
    let outcome = (!tail.torn_suffix)
        .then(|| latest_terminal(&tail.text))
        .flatten();
    let at = timestamp_from_stat(stat)?;
    let (turn_complete, turn_interrupted, turn_error) = match outcome {
        Some(TerminalOutcome::Complete) => (Some(at), None, None),
        Some(TerminalOutcome::Interrupted) => (None, Some(at), None),
        Some(TerminalOutcome::Error(label)) => (
            None,
            None,
            Some(AgentTurnError {
                class: TurnErrorClass::classify_label(label.as_deref()),
                at,
                label,
            }),
        ),
        None => (None, None, None),
    };
    Some(LocalContextRefresh {
        model_id: ctx.model_hint.map(ToOwned::to_owned),
        effort: None,
        tokens: None,
        cost: None,
        turn_error,
        turn_complete,
        turn_interrupted,
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
    })
}

fn latest_terminal(tail: &str) -> Option<TerminalOutcome> {
    let line = tail.lines().rev().find(|line| !line.trim().is_empty())?;
    let record = serde_json::from_str::<TerminalRecord>(line).ok()?;
    if record.r#type.as_deref() != Some("turn_ended") {
        return None;
    }
    match record.status.as_deref() {
        Some("success" | "completed") => Some(TerminalOutcome::Complete),
        Some("aborted") => Some(TerminalOutcome::Interrupted),
        Some("error") => Some(TerminalOutcome::Error(
            record.error.map(|error| error.chars().take(500).collect()),
        )),
        _ => None,
    }
}

pub(super) fn transcript_stat(path: &Path) -> Option<TranscriptStat> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(TranscriptStat {
        mtime_secs: modified.as_secs().try_into().unwrap_or(i64::MAX),
        mtime_nanos: modified.subsec_nanos(),
        len: metadata.len(),
    })
}

fn timestamp_from_stat(stat: TranscriptStat) -> Option<Timestamp> {
    Timestamp::new(stat.mtime_secs, stat.mtime_nanos as i32).ok()
}

pub(super) fn resolve_transcript(
    conversation_id: &str,
    current: Option<&Path>,
    prior: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = current.filter(|path| path.is_file()) {
        return Some(path.to_path_buf());
    }
    if let Some(path) = prior.filter(|path| path.is_file()) {
        return Some(path.to_path_buf());
    }
    discover_under(
        &crate::agents::transcript_fs::home_dir().join(".cursor/projects"),
        conversation_id,
    )
}

pub(super) fn discover_under(root: &Path, conversation_id: &str) -> Option<PathBuf> {
    let conversation_id = conversation_id.trim();
    if conversation_id.is_empty()
        || Path::new(conversation_id).components().count() != 1
        || !matches!(
            Path::new(conversation_id).components().next(),
            Some(Component::Normal(_))
        )
    {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    let mut matches = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| {
            entry
                .path()
                .join("agent-transcripts")
                .join(conversation_id)
                .join(format!("{conversation_id}.jsonl"))
        })
        .filter(|path| path.is_file());
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

#[cfg(test)]
pub(super) fn parse_terminal_for_test(tail: &str) -> Option<&'static str> {
    match latest_terminal(tail)? {
        TerminalOutcome::Complete => Some("complete"),
        TerminalOutcome::Interrupted => Some("interrupted"),
        TerminalOutcome::Error(_) => Some("error"),
    }
}
