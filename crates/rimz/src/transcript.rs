//! Durable RimZ-owned cross-provider conversation log.
//!
//! The log is append-only JSONL under `transcript/<bucket-start>.jsonl` in the
//! workspace state root. This transcript is distinct from provider-native
//! transcript and session files.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::buckets::{bucket_file_name, bucket_files};
use crate::disk::paths::{StatePaths, rimz_home, workspaces_dir_under};
use crate::disk::retention::TRANSCRIPT_FILE_DAYS as FILE_DAYS;
use crate::disk::{atomic, lock};
use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::ids::{AskId, compose_channel};
use crate::workspace::{KnownWorkspace, known_workspaces_under};

pub const HARNESS_FROM: &str = "rimz";

#[derive(Debug, thiserror::Error)]
pub enum TranscriptLogErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, TranscriptLogErr>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    Prompt,
    Message,
    SubagentReport,
    Wait,
    Assistant,
    Ask,
    Answer,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub at: Timestamp,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AskId>,
    /// Queue record that opened this prompt/message turn, when delivery was
    /// confirmed against a RimZ-authored message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// Queue record's creation time; `at` stays the record time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_at: Option<Timestamp>,
    /// Messages whose receiving turn authored this entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Direct parent of a pane-backed `rimz subagents` child. Absent for root
    /// agents; classifies the session as a child for transcript scoping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<AgentSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_kind: Option<AgentKind>,
    pub entry: TranscriptKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<AskAnswer>,
}

impl TranscriptEntry {
    /// RimZ-authored automation: fleet digests, waits and signals, and headerless system prompts. Human rendering skips these and the `Assistant`/`Error` output of the turns they open; conversation counts skip these.
    pub fn is_harness(&self) -> bool {
        matches!(
            self.entry,
            TranscriptKind::SubagentReport | TranscriptKind::Wait
        ) || (self.entry == TranscriptKind::Prompt && self.from.as_deref() == Some(HARNESS_FROM))
    }

    pub fn new(
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        entry: TranscriptKind,
        text: String,
    ) -> Self {
        Self {
            at,
            kind,
            agent_id,
            id: None,
            message_id: None,
            enqueued_at: None,
            reply_to: Vec::new(),
            channel: None,
            name: None,
            profile: None,
            role: None,
            parent_agent_id: None,
            parent_agent_kind: None,
            entry,
            from: None,
            text,
            questions: Vec::new(),
            answers: Vec::new(),
        }
    }
}

/// Chat transcript lane: launch-stamped channel when known, else worktree
/// basename fallback for older agent payloads that carry only a path.
pub fn entry_channel(stamped: Option<&str>, worktree_path: Option<&str>) -> Option<String> {
    compose_channel(
        stamped,
        worktree_path.and_then(|path| path.rsplit('/').next().filter(|value| !value.is_empty())),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<AskOption>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
    /// Claude uses a different keyboard contract when any option carries a
    /// rich preview: digits move focus and Enter selects. Persist the bit so a
    /// later `rimz answer` does not guess from lossy option labels.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_option_previews: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AskOptionWire", into = "AskOptionWire")]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caution: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AskOptionWire {
    Label(String),
    Detailed {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caution: Option<String>,
    },
}

impl From<AskOptionWire> for AskOption {
    fn from(value: AskOptionWire) -> Self {
        match value {
            AskOptionWire::Label(label) => Self {
                label,
                description: None,
                caution: None,
            },
            AskOptionWire::Detailed {
                label,
                description,
                caution,
            } => Self {
                label,
                description,
                caution,
            },
        }
    }
}

impl From<AskOption> for AskOptionWire {
    fn from(value: AskOption) -> Self {
        match (value.description, value.caution) {
            (Some(description), caution) => AskOptionWire::Detailed {
                label: value.label,
                description: Some(description),
                caution,
            },
            (None, Some(caution)) => AskOptionWire::Detailed {
                label: value.label,
                description: None,
                caution: Some(caution),
            },
            (None, None) => AskOptionWire::Label(value.label),
        }
    }
}

impl From<String> for AskOption {
    fn from(label: String) -> Self {
        Self {
            label,
            description: None,
            caution: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn answers_text(answers: &[AskAnswer]) -> String {
    answers
        .iter()
        .filter_map(|answer| {
            let mut line = answer
                .chosen
                .iter()
                .filter_map(|choice| non_empty(choice))
                .collect::<Vec<_>>()
                .join(", ");
            if line.is_empty() {
                return None;
            }
            if let Some(note) = answer.note.as_deref().and_then(non_empty) {
                line.push_str(" (note: ");
                line.push_str(note);
                line.push(')');
            }
            Some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

/// Append one transcript entry. Callers must not already hold the workspace
/// lock; append takes it to serialize hook children writing long JSONL lines.
#[must_use = "durability barrier; check the result"]
pub fn append(paths: &StatePaths, entry: &TranscriptEntry) -> Result<()> {
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    append_locked(paths, entry)
}

/// Append an id-stamped answer exactly once across the native hook and CLI
/// confirmation writers. The workspace lock covers both the duplicate check
/// and append, so a PostToolUse racing `rimz answer` still yields one record.
pub fn append_answer_if_missing(paths: &StatePaths, entry: &TranscriptEntry) -> Result<bool> {
    debug_assert_eq!(entry.entry, TranscriptKind::Answer);
    let Some(id) = entry.id.as_ref() else {
        append(paths, entry)?;
        return Ok(true);
    };
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    if read_all(paths)?.into_iter().any(|existing| {
        existing.entry == TranscriptKind::Answer && existing.id.as_ref() == Some(id)
    }) {
        return Ok(false);
    }
    append_locked(paths, entry)?;
    Ok(true)
}

fn append_locked(paths: &StatePaths, entry: &TranscriptEntry) -> Result<()> {
    fs::create_dir_all(&paths.transcript_dir).map_err(|source| TranscriptLogErr::Io {
        path: paths.transcript_dir.clone(),
        source,
    })?;
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    atomic::append_record_bytes(&bucket_path(paths, entry.at), &line)?;
    Ok(())
}

pub fn read_all(paths: &StatePaths) -> Result<Vec<TranscriptEntry>> {
    let mut files = transcript_files(&paths.transcript_dir)?;
    files.sort();

    let mut entries = Vec::new();
    for path in files {
        entries.extend(read_bucket(&path)?);
    }
    entries.sort_by_key(|entry| entry.at);
    Ok(entries)
}

/// The distinct channels stamped on the workspace's transcript entries.
pub fn channels(paths: &StatePaths) -> Result<BTreeSet<String>> {
    Ok(read_all(paths)?
        .into_iter()
        .filter_map(|entry| entry.channel)
        .collect())
}

/// Every known workspace whose transcript log has entries stamped with
/// `channel`. Best-effort inventory for read commands: transcript evidence
/// only, unreadable workspaces and logs skipped.
pub fn workspaces_with_channel(channel: &str) -> Vec<KnownWorkspace> {
    workspaces_with_channel_under(&rimz_home(), channel)
}

/// [`workspaces_with_channel`] over an explicit state root, for tests.
fn workspaces_with_channel_under(state_root: &Path, channel: &str) -> Vec<KnownWorkspace> {
    let known = match known_workspaces_under(&workspaces_dir_under(state_root)) {
        Ok(known) => known,
        Err(err) => {
            tracing::debug!(error = %err, "cannot enumerate workspaces for channel lookup");
            return Vec::new();
        }
    };
    known
        .into_iter()
        .filter(|workspace| {
            if !workspace.project_root.is_dir() {
                tracing::debug!(workspace = %workspace.workspace_id, root = %workspace.project_root.display(), "skipping workspace whose project root is gone");
                return false;
            }
            let runtime = crate::RuntimePaths::under_named(workspace.workspace_id.clone(), workspace.dir_name.clone(), state_root);
            let paths = StatePaths::under_named(workspace.workspace_id.clone(), workspace.dir_name.clone(), state_root, &runtime);
            match channels(&paths) {
                Ok(channels) => channels.contains(channel),
                Err(err) => {
                    tracing::debug!(workspace = %workspace.workspace_id, error = %err, "skipping workspace in channel lookup");
                    false
                }
            }
        })
        .collect()
}

/// The agent's latest open native ask: the newest `Ask` entry for
/// `(kind, agent_id)` with no later `Answer` entry.
///
/// Bucket files and their entries are walked newest-first. Answer ids accumulate
/// across buckets until the scan reaches the newest ask they do not close.
pub fn latest_open_ask(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<Option<TranscriptEntry>> {
    let mut files = transcript_files(&paths.transcript_dir)?;
    files.sort_by(|left, right| right.cmp(left));
    let mut closed = std::collections::BTreeSet::new();

    for path in files {
        for entry in read_bucket(&path)?.into_iter().rev() {
            if &entry.kind != kind || &entry.agent_id != agent_id {
                continue;
            }
            match entry.entry {
                TranscriptKind::Ask => match entry.id.as_ref() {
                    Some(id) if closed.contains(id.as_str()) => {}
                    _ => return Ok(Some(entry)),
                },
                TranscriptKind::Answer => match entry.id.as_ref() {
                    Some(id) => {
                        closed.insert(id.as_str().to_owned());
                    }
                    None => return Ok(None),
                },
                _ => {}
            }
        }
    }
    Ok(None)
}

/// The agent's newest `Assistant` entry stamped at or after `since`: the final
/// message of a turn that opened then.
///
/// Buckets are walked newest-first and the walk stops at the first bucket that
/// reaches back before `since`, since every older bucket predates it.
pub fn latest_assistant(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    since: Timestamp,
) -> Result<Option<TranscriptEntry>> {
    let mut files = transcript_files(&paths.transcript_dir)?;
    files.sort_by(|left, right| right.cmp(left));
    for path in files {
        let entries = read_bucket(&path)?;
        let reaches_before = entries.first().is_some_and(|entry| entry.at < since);
        if let Some(entry) = entries.into_iter().rev().find(|entry| {
            entry.at >= since
                && entry.entry == TranscriptKind::Assistant
                && &entry.kind == kind
                && &entry.agent_id == agent_id
        }) {
            return Ok(Some(entry));
        }
        if reaches_before {
            break;
        }
    }
    Ok(None)
}

/// The one decode point for the log, so every reader — [`read_all`] and the
/// newest-first bucket walks alike — sees the same entries.
fn read_bucket(path: &Path) -> Result<Vec<TranscriptEntry>> {
    let text = fs::read_to_string(path).map_err(|source| TranscriptLogErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<TranscriptEntry>(line).ok())
        .filter(|entry| !is_legacy_paste_fragment(entry))
        .collect())
}

/// A prompt entry that is only a provider paste-wrapper tag, written by RimZ
/// 0.4.3 and earlier.
///
/// Claude Code wraps a bracketed paste in `<pasted_content id="X">` /
/// `</pasted_content id="X">` lines and RimZ delivers every queued prompt by
/// bracketed paste, so until `agents::payload::sanitize_user_prompt` learned to
/// peel the envelope each delivery left two tag lines recorded as headerless
/// human prompts — inflating the "from you" count and unhiding harness turns in
/// `rimz agents logs`. They are not conversation in any view, so the reader
/// drops them; the append-only files keep the raw record for forensics.
///
/// This is a frozen description of what was written, not a grammar for what
/// arrives: it never needs to grow, and a genuine prompt carrying a wrapper
/// pair mid-text is many lines and so never matches.
///
/// It is not version-scoped, though: it runs against every entry ever read,
/// including ones written after the peel landed. A prompt whose entire text is
/// one tag line is indistinguishable from a fragment and is dropped too — a
/// human pasting the tag itself, while discussing this bug, is the only way to
/// produce one. That is the deliberate cost of matching a whole entry exactly,
/// which is the only form safe to apply to text RimZ did not normalize.
fn is_legacy_paste_fragment(entry: &TranscriptEntry) -> bool {
    entry.entry == TranscriptKind::Prompt
        && entry.from.is_none()
        && entry.message_id.is_none()
        && is_paste_tag(entry.text.trim())
}

/// Whether `text` is exactly one `pasted_content` open or close tag.
fn is_paste_tag(text: &str) -> bool {
    let Some(id) = text
        .strip_prefix("<pasted_content id=\"")
        .or_else(|| text.strip_prefix("</pasted_content id=\""))
        .and_then(|rest| rest.strip_suffix("\">"))
    else {
        return false;
    };
    !id.is_empty() && !id.contains(['"', '\n'])
}

fn transcript_files(dir: &Path) -> Result<Vec<PathBuf>> {
    bucket_files(dir).map_err(|source| TranscriptLogErr::Io {
        path: dir.to_path_buf(),
        source,
    })
}

fn bucket_path(paths: &StatePaths, at: Timestamp) -> PathBuf {
    paths.transcript_dir.join(bucket_file_name(at, FILE_DAYS))
}

#[cfg(test)]
mod tests;
