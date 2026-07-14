//! Validated Antigravity CLI local-session discovery and transcript projection.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentStatus, LocalSessionObservation, TranscriptMessage, TranscriptRole, read_transcript_tail,
    sanitize_user_prompt,
};
use crate::ids::{AgentKind, AgentSessionId};

const MAX_DISCOVERED_SESSIONS: usize = 512;

#[derive(Deserialize)]
struct TranscriptRecord {
    #[serde(rename = "step_index")]
    _step_index: u64,
    source: String,
    #[serde(rename = "type")]
    record_type: String,
    status: String,
    created_at: String,
    content: Option<String>,
}

#[derive(Clone)]
struct FoldedSession {
    first_event_at: Option<Timestamp>,
    last_activity: Option<Timestamp>,
    status: AgentStatus,
    phase: TurnPhase,
    latest_prompt: Option<String>,
}

pub(super) fn discover(workspace: &Path) -> Vec<LocalSessionObservation> {
    let Some(home) = home() else {
        return Vec::new();
    };
    discover_under(&home, workspace)
}

pub(super) fn discover_under(home: &Path, workspace: &Path) -> Vec<LocalSessionObservation> {
    if !workspace.is_absolute() {
        return Vec::new();
    }
    let current = latest_conversation(home, workspace);
    let mut transcripts = transcript_files_under(home);
    transcripts.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(std::cmp::Reverse)
    });
    let mut observations = transcripts
        .into_iter()
        .take(MAX_DISCOVERED_SESSIONS)
        .filter_map(|path| observation(&path, workspace, current.as_deref()))
        .collect::<Vec<_>>();
    observations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.session_id.cmp(&right.session_id))
    });
    observations
}

pub(super) fn messages(lines: &str) -> Vec<TranscriptMessage> {
    parse_records(lines)
        .filter_map(|record| {
            (record.status == "DONE").then_some(())?;
            let role = visible_role(&record)?;
            let content = record.content.as_deref()?;
            let text = match role {
                TranscriptRole::User => sanitize_user_prompt(Some(content)),
                TranscriptRole::Assistant => normalized(Some(content)),
            }?;
            Some(TranscriptMessage {
                role,
                at: record.created_at.parse::<Timestamp>().ok(),
                text,
            })
        })
        .collect()
}

pub(super) fn transcript_for_session(session_id: &str) -> Option<PathBuf> {
    valid_conversation_id(session_id).then_some(())?;
    let home = home()?;
    let path = transcript_path(&home, session_id);
    valid_transcript_under(&home, &path, session_id).then_some(path)
}

pub(super) fn valid_transcript(path: &Path, session_id: &str) -> bool {
    let Some(home) = home() else {
        return false;
    };
    valid_transcript_under(&home, path, session_id)
}

pub(super) fn resumed_session_id(cmdline: &str) -> Option<AgentSessionId> {
    let tokens = shlex::split(cmdline)?;
    let program = Path::new(tokens.first()?).file_name()?.to_str()?;
    if program != "agy" {
        return None;
    }
    let mut index = 1;
    while index < tokens.len() {
        let token = &tokens[index];
        let candidate = if let Some(id) = token.strip_prefix("--conversation=") {
            Some(id)
        } else if token == "--conversation" {
            index += 1;
            tokens.get(index).map(String::as_str)
        } else {
            None
        };
        if let Some(id) = candidate {
            return valid_conversation_id(id).then(|| AgentSessionId::from(id));
        }
        index += 1;
    }
    None
}

pub(super) fn valid_conversation_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && id.len() <= 256
        && id != "."
        && id != ".."
        && !id.starts_with('-')
        && !id
            .chars()
            .any(|ch| ch.is_control() || ch == '/' || ch == '\\')
}

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let lines = include_str!("tests/fixtures/transcript.jsonl");
    let folded = fold(lines);
    let created_at = folded.first_event_at.unwrap();
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("antigravity"),
        session_id: AgentSessionId::from("11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from("/provider/brain/11111111/transcript.jsonl"),
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: Some(created_at),
        last_activity: folded.last_activity.unwrap(),
        status: folded.status,
        phase: folded.phase,
        latest_prompt: folded.latest_prompt,
        native_prompt_detail: None,
        waiting_since: None,
        context_pct: None,
    }
}

fn home() -> Option<PathBuf> {
    resolve_home(
        std::env::var_os("RIMZ_ANTIGRAVITY_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(super) fn resolve_home(override_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    override_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".gemini/antigravity-cli"))
        })
}

fn latest_conversation(home: &Path, workspace: &Path) -> Option<String> {
    let path = home.join("cache/last_conversations.json");
    regular_file(&path).then_some(())?;
    let values = serde_json::from_slice::<std::collections::BTreeMap<PathBuf, String>>(
        &fs::read(path).ok()?,
    )
    .ok()?;
    values
        .get(workspace)
        .map(String::as_str)
        .filter(|id| valid_conversation_id(id))
        .map(ToOwned::to_owned)
}

fn transcript_files_under(home: &Path) -> Vec<PathBuf> {
    let brain = home.join("brain");
    if !regular_dir(&brain) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&brain) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_dir())?;
            let id = entry.file_name().into_string().ok()?;
            valid_conversation_id(&id).then_some(())?;
            let path = transcript_path(home, &id);
            valid_transcript_under(home, &path, &id).then_some(path)
        })
        .collect()
}

fn transcript_path(home: &Path, session_id: &str) -> PathBuf {
    home.join("brain")
        .join(session_id)
        .join(".system_generated/logs/transcript.jsonl")
}

fn valid_transcript_under(home: &Path, path: &Path, session_id: &str) -> bool {
    if !valid_conversation_id(session_id) || !regular_file(path) {
        return false;
    }
    let brain = home.join("brain");
    let session = brain.join(session_id);
    if !regular_dir(&brain) || !regular_dir(&session) {
        return false;
    }
    let Ok(canonical_brain) = fs::canonicalize(&brain) else {
        return false;
    };
    let Ok(canonical_session) = fs::canonicalize(&session) else {
        return false;
    };
    let Ok(canonical_path) = fs::canonicalize(path) else {
        return false;
    };
    canonical_session.parent() == Some(canonical_brain.as_path())
        && canonical_path == canonical_session.join(".system_generated/logs/transcript.jsonl")
}

fn observation(
    path: &Path,
    workspace: &Path,
    current_session_id: Option<&str>,
) -> Option<LocalSessionObservation> {
    let session_id = path.ancestors().nth(3)?.file_name()?.to_str()?.to_owned();
    valid_conversation_id(&session_id).then_some(())?;
    let lines = read_transcript_tail(path)?;
    let folded = fold(&lines);
    let created_at = folded.first_event_at?;
    let last_activity = folded.last_activity?;
    Some(LocalSessionObservation {
        kind: AgentKind::new_unchecked("antigravity"),
        session_id: AgentSessionId::from(session_id.clone()),
        workspace: workspace.to_path_buf(),
        transcript_path: path.to_path_buf(),
        created_at,
        // Only the provider's current-workspace cache authorizes fresh-session
        // pairing. Other conversations remain available solely for exact
        // `--conversation` command-line binding.
        fresh_binding_at: (current_session_id == Some(session_id.as_str())).then_some(created_at),
        first_event_at: Some(created_at),
        last_activity,
        status: folded.status,
        phase: folded.phase,
        latest_prompt: folded.latest_prompt,
        native_prompt_detail: None,
        waiting_since: None,
        context_pct: None,
    })
}

fn fold(lines: &str) -> FoldedSession {
    let mut folded = FoldedSession {
        first_event_at: None,
        last_activity: None,
        status: AgentStatus::Idle,
        phase: TurnPhase::Idle,
        latest_prompt: None,
    };
    for record in parse_records(lines) {
        if record.status != "DONE" {
            continue;
        }
        let Some(role) = visible_role(&record) else {
            continue;
        };
        let Ok(at) = record.created_at.parse::<Timestamp>() else {
            continue;
        };
        folded.first_event_at.get_or_insert(at);
        folded.last_activity = Some(at);
        match role {
            TranscriptRole::User => {
                folded.status = AgentStatus::Running;
                folded.phase = TurnPhase::Reasoning;
                folded.latest_prompt = sanitize_user_prompt(record.content.as_deref());
            }
            TranscriptRole::Assistant => {
                folded.status = AgentStatus::Success;
                folded.phase = TurnPhase::Idle;
            }
        }
    }
    folded
}

fn visible_role(record: &TranscriptRecord) -> Option<TranscriptRole> {
    match (record.source.as_str(), record.record_type.as_str()) {
        ("USER_EXPLICIT", "USER_INPUT") => Some(TranscriptRole::User),
        ("MODEL", "PLANNER_RESPONSE") => Some(TranscriptRole::Assistant),
        _ => None,
    }
}

fn parse_records(lines: &str) -> impl Iterator<Item = TranscriptRecord> + '_ {
    lines
        .lines()
        .filter_map(|line| serde_json::from_str::<TranscriptRecord>(line).ok())
}

fn normalized(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn regular_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}
