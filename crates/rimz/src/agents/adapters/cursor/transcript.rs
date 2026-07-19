//! Privacy-safe Cursor transcript tail parsing.
//!
//! Cursor transcript assistant blocks combine visible prose and thinking. This
//! module deliberately models only the terminal row discriminator and outcome.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::context::{AgentTurnError, TurnErrorClass, TurnSettle, TurnSettleOutcome};
use crate::agents::local_session_cache::StampedPaths;
use crate::agents::transcript_fs::deserialize_optional_string_lossy;
use crate::agents::{
    FieldPatch, LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx, TranscriptStat,
};

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

#[derive(Debug, Default)]
pub(super) struct TurnMarkers {
    pub settle: Option<TurnSettle>,
    pub turn_error: Option<AgentTurnError>,
}

pub(super) fn refresh(ctx: &LocalContextRefreshCtx<'_>) -> Option<LocalContextRefresh> {
    let path = resolve_transcript(
        ctx.agent_id,
        ctx.current_transcript_path.map(Path::new),
        ctx.prior_transcript_path.map(Path::new),
    )?;
    let stat = TranscriptStat::from_path(&path)?;
    if ctx.prior_transcript_stat == Some(&stat) {
        return None;
    }
    let markers = turn_markers_at(&path, stat)?;
    Some(LocalContextRefresh {
        context: LocalContextPatch {
            model_id: ctx
                .model_hint
                .map(ToOwned::to_owned)
                .map_or(FieldPatch::Keep, FieldPatch::Set),
            turn_error: markers
                .turn_error
                .map_or(FieldPatch::Clear, FieldPatch::Set),
            settle: markers.settle.map_or(FieldPatch::Clear, FieldPatch::Set),
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        ..LocalContextRefresh::authoritative_current()
    })
}

/// Re-read Cursor's terminal transcript row for a statusline refresh. The
/// statusline payload carries only the session id, so resolve the same public
/// transcript path as the lifecycle fallback and restamp all three mutually
/// exclusive settle markers before the whole-context sidecar write.
pub(super) fn statusline_turn_markers(conversation_id: &str) -> Option<TurnMarkers> {
    let path = resolve_transcript(conversation_id, None, None)?;
    let stat = TranscriptStat::from_path(&path)?;
    turn_markers_at(&path, stat)
}

pub(super) fn turn_markers_at(path: &Path, stat: TranscriptStat) -> Option<TurnMarkers> {
    let tail = crate::agents::transcript_fs::read_transcript_tail_with_status(path)?;
    let outcome = (!tail.torn_suffix)
        .then(|| latest_terminal(&tail.text))
        .flatten();
    let at = timestamp_from_stat(stat)?;
    Some(match outcome {
        Some(TerminalOutcome::Complete) => TurnMarkers {
            settle: Some(TurnSettle::new(at, TurnSettleOutcome::Complete)),
            ..TurnMarkers::default()
        },
        Some(TerminalOutcome::Interrupted) => TurnMarkers {
            settle: Some(TurnSettle::new(at, TurnSettleOutcome::Interrupted)),
            ..TurnMarkers::default()
        },
        Some(TerminalOutcome::Error(label)) => TurnMarkers {
            turn_error: Some(AgentTurnError {
                class: TurnErrorClass::classify_label(label.as_deref()),
                at,
                label,
            }),
            ..TurnMarkers::default()
        },
        None => TurnMarkers::default(),
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

pub(super) struct DiscoveryCatalog {
    pub(super) topology: StampedPaths,
    pub(super) stable: bool,
    dependencies: HashMap<String, Vec<PathBuf>>,
    resolved: HashMap<String, Option<PathBuf>>,
}

impl DiscoveryCatalog {
    pub(super) fn build(root: &Path, conversation_ids: &[String]) -> Self {
        let mut topology = StampedPaths::exact([root.to_path_buf()]);
        let mut dependencies = conversation_ids
            .iter()
            .map(|id| (id.clone(), Vec::new()))
            .collect::<HashMap<_, _>>();
        let mut matches = conversation_ids
            .iter()
            .map(|id| (id.clone(), Vec::new()))
            .collect::<HashMap<_, _>>();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.filter_map(Result::ok) {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let project = entry.path();
                let transcripts = project.join("agent-transcripts");
                topology.record_exact(project.clone());
                topology.record_exact(transcripts.clone());
                for id in conversation_ids {
                    let conversation = transcripts.join(id);
                    let path = conversation.join(format!("{id}.jsonl"));
                    topology.record_exact_many([conversation.clone(), path.clone()]);
                    dependencies
                        .entry(id.clone())
                        .or_default()
                        .extend([conversation, path.clone()]);
                    if fs::symlink_metadata(&path)
                        .is_ok_and(|metadata| metadata.file_type().is_file())
                    {
                        matches.entry(id.clone()).or_default().push(path);
                    }
                }
            }
        }
        let stable = topology.all_stable() && topology.unchanged();
        let resolved = matches
            .into_iter()
            .map(|(id, mut paths)| {
                paths.sort();
                paths.dedup();
                let resolved = match paths.as_slice() {
                    [path] => Some(path.clone()),
                    _ => None,
                };
                (id, resolved)
            })
            .collect();
        Self {
            topology,
            stable,
            dependencies,
            resolved,
        }
    }

    pub(super) fn resolve(&self, conversation_id: &str) -> Option<&Path> {
        self.stable.then_some(())?;
        self.resolved.get(conversation_id)?.as_deref()
    }

    pub(super) fn dependencies(&self, conversation_id: &str) -> &[PathBuf] {
        self.dependencies
            .get(conversation_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
pub(super) fn parse_terminal_for_test(tail: &str) -> Option<&'static str> {
    match latest_terminal(tail)? {
        TerminalOutcome::Complete => Some("complete"),
        TerminalOutcome::Interrupted => Some("interrupted"),
        TerminalOutcome::Error(_) => Some("error"),
    }
}
