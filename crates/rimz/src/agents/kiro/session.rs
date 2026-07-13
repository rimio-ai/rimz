//! Validated Kiro CLI v3 local-session discovery and transcript projection.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentStatus, LocalSessionObservation, TranscriptMessage, TranscriptRole, read_transcript_tail,
};
use crate::ids::{AgentKind, AgentSessionId};

const SCHEMA_VERSION: &str = "1.0.0";
const DATA_MODEL_VERSION: u32 = 1;
const MAX_SESSIONS_PER_WORKSPACE: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetadata {
    id: String,
    schema_version: String,
    data_model_version: u32,
    workspace_paths: Vec<PathBuf>,
    created_at: String,
    status: String,
}

#[derive(Deserialize)]
struct Envelope {
    id: String,
    timestamp: String,
    payload: Payload,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Payload {
    #[serde(rename = "user")]
    User { content: Option<String> },
    #[serde(rename = "assistant")]
    Assistant {
        content: Option<String>,
        #[serde(rename = "operationType")]
        operation_type: Option<String>,
    },
    #[serde(rename = "turn_start")]
    TurnStart {
        #[serde(rename = "executionId")]
        execution_id: Option<String>,
    },
    #[serde(rename = "turn_end")]
    TurnEnd {
        #[serde(rename = "executionId")]
        execution_id: Option<String>,
        #[serde(rename = "stopReason")]
        stop_reason: Option<String>,
    },
    #[serde(rename = "tool_call")]
    ToolCall {
        #[serde(rename = "toolCallId")]
        tool_call_id: Option<String>,
        #[serde(rename = "toolName")]
        tool_name: Option<String>,
        status: Option<String>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: Option<String>,
        success: Option<bool>,
    },
    #[serde(rename = "pending_interaction")]
    PendingInteraction {
        #[serde(rename = "interactionType")]
        interaction_type: Option<String>,
        #[serde(rename = "toolCallId")]
        tool_call_id: Option<String>,
        question: Option<String>,
    },
    #[serde(rename = "interaction_resolved")]
    InteractionResolved {
        #[serde(rename = "toolCallId")]
        tool_call_id: Option<String>,
    },
    #[serde(rename = "session_metadata")]
    SessionMetadata {
        key: Option<String>,
        value: Option<ContextUsage>,
    },
    #[serde(rename = "session_event")]
    SessionEvent {
        category: Option<String>,
        context: Option<SessionEventContext>,
    },
    #[serde(rename = "usage_summary")]
    UsageSummary {},
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextUsage {
    usage_percentage: Option<f64>,
}

#[derive(Deserialize)]
struct SessionEventContext {
    status: Option<String>,
}

struct ValidatedSession {
    metadata: SessionMetadata,
    messages: PathBuf,
}

#[derive(Clone)]
struct FoldedSession {
    first_event_at: Option<Timestamp>,
    last_activity: Timestamp,
    status: AgentStatus,
    phase: TurnPhase,
    latest_prompt: Option<String>,
    native_prompt_detail: Option<String>,
    waiting_since: Option<Timestamp>,
    context_pct: Option<u8>,
}

pub(super) fn workspace_bucket(workspace: &Path) -> Option<String> {
    workspace.is_absolute().then(|| {
        let digest = Sha256::digest(workspace.as_os_str().as_encoded_bytes());
        hex::encode(digest)[..16].to_owned()
    })
}

pub(super) fn discover(workspace: &Path) -> Vec<LocalSessionObservation> {
    let Some(home) = super::install::home() else {
        return Vec::new();
    };
    discover_under(&home, workspace)
}

pub(super) fn discover_under(home: &Path, workspace: &Path) -> Vec<LocalSessionObservation> {
    let Some(bucket_name) = workspace_bucket(workspace) else {
        return Vec::new();
    };
    let sessions_root = home.join("sessions");
    let bucket = sessions_root.join(bucket_name);
    if !regular_dir(&sessions_root)
        || !regular_dir(&bucket)
        || fs::canonicalize(&bucket)
            .ok()
            .and_then(|bucket| bucket.parent().map(Path::to_path_buf))
            != fs::canonicalize(&sessions_root).ok()
    {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&bucket) else {
        return Vec::new();
    };
    let mut observations = entries
        .filter_map(Result::ok)
        .filter_map(|entry| validate_session(&bucket, &entry.path(), workspace))
        .take(MAX_SESSIONS_PER_WORKSPACE)
        .filter_map(|session| observation(session, workspace))
        .collect::<Vec<_>>();
    observations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.session_id.cmp(&right.session_id))
    });
    observations
}

pub(super) fn transcript_files() -> Vec<PathBuf> {
    let Some(home) = super::install::home() else {
        return Vec::new();
    };
    let sessions_root = home.join("sessions");
    if !regular_dir(&sessions_root) {
        return Vec::new();
    }
    let Ok(buckets) = fs::read_dir(&sessions_root) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for bucket in buckets.filter_map(Result::ok) {
        let Ok(file_type) = bucket.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let bucket_path = bucket.path();
        if fs::canonicalize(&bucket_path)
            .ok()
            .and_then(|bucket| bucket.parent().map(Path::to_path_buf))
            != fs::canonicalize(&sessions_root).ok()
        {
            continue;
        }
        let Ok(sessions) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for session in sessions.filter_map(Result::ok) {
            let path = session.path();
            if session
                .file_type()
                .is_ok_and(|file_type| file_type.is_dir())
                && let Some(validated) = validate_unscoped_session(&bucket_path, &path)
            {
                paths.push(validated.messages);
            }
        }
    }
    paths.sort();
    paths
}

pub(super) fn transcript_for_session(session_id: &str) -> Option<PathBuf> {
    valid_session_id(session_id).then_some(())?;
    transcript_files().into_iter().find(|path| {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some(session_id)
    })
}

pub(super) fn valid_transcript(path: &Path, session_id: &str) -> bool {
    let Some(session) = path.parent() else {
        return false;
    };
    let Some(bucket) = session.parent() else {
        return false;
    };
    session.file_name().and_then(|name| name.to_str()) == Some(session_id)
        && validate_unscoped_session(bucket, session)
            .is_some_and(|validated| validated.messages == path)
}

pub(super) fn messages(lines: &str) -> Vec<TranscriptMessage> {
    parse_records(lines)
        .filter_map(|(at, payload)| match payload {
            Payload::User { content } => normalized(content).map(|text| TranscriptMessage {
                role: TranscriptRole::User,
                at: Some(at),
                text,
            }),
            Payload::Assistant {
                content,
                operation_type,
            } if operation_type.as_deref() == Some("Say") => {
                normalized(content).map(|text| TranscriptMessage {
                    role: TranscriptRole::Assistant,
                    at: Some(at),
                    text,
                })
            }
            _ => None,
        })
        .collect()
}

pub(super) fn resumed_session_id(cmdline: &str) -> Option<AgentSessionId> {
    let mut tokens = cmdline.split_whitespace().peekable();
    let program = tokens.next()?;
    let program = Path::new(program).file_name()?.to_str()?;
    if program == "kiro-cli" {
        if tokens.peek().copied() == Some("chat") {
            tokens.next();
        }
    } else if program != "kiro-cli-chat" {
        return None;
    }
    while let Some(token) = tokens.next() {
        let id = if let Some(id) = token.strip_prefix("--resume-id=") {
            id
        } else if token == "--resume-id" {
            tokens.next()?
        } else {
            continue;
        };
        return valid_session_id(id).then(|| AgentSessionId::from(id));
    }
    None
}

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let created_at = "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    let folded = fold(
        include_str!("tests/fixtures/stock_ping/messages.jsonl"),
        created_at,
        "idle",
    );
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("kiro"),
        session_id: AgentSessionId::from("sess_11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from("/workspace/project/messages.jsonl"),
        created_at,
        first_event_at: folded.first_event_at,
        last_activity: folded.last_activity,
        status: folded.status,
        phase: folded.phase,
        latest_prompt: folded.latest_prompt,
        native_prompt_detail: folded.native_prompt_detail,
        waiting_since: folded.waiting_since,
        context_pct: folded.context_pct,
    }
}

fn observation(session: ValidatedSession, workspace: &Path) -> Option<LocalSessionObservation> {
    let created_at = session.metadata.created_at.parse::<Timestamp>().ok()?;
    let lines = read_transcript_tail(&session.messages).unwrap_or_default();
    let folded = fold(&lines, created_at, &session.metadata.status);
    Some(LocalSessionObservation {
        kind: AgentKind::new_unchecked("kiro"),
        session_id: AgentSessionId::from(session.metadata.id),
        workspace: workspace.to_path_buf(),
        transcript_path: session.messages,
        created_at,
        first_event_at: folded.first_event_at,
        last_activity: folded.last_activity,
        status: folded.status,
        phase: folded.phase,
        latest_prompt: folded.latest_prompt,
        native_prompt_detail: folded.native_prompt_detail,
        waiting_since: folded.waiting_since,
        context_pct: folded.context_pct,
    })
}

fn validate_session(bucket: &Path, session: &Path, workspace: &Path) -> Option<ValidatedSession> {
    let validated = validate_unscoped_session(bucket, session)?;
    validated
        .metadata
        .workspace_paths
        .iter()
        .any(|path| path == workspace)
        .then_some(validated)
}

fn validate_unscoped_session(bucket: &Path, session: &Path) -> Option<ValidatedSession> {
    let id = session.file_name()?.to_str()?;
    valid_session_id(id).then_some(())?;
    fs::symlink_metadata(session)
        .ok()?
        .file_type()
        .is_dir()
        .then_some(())?;
    let canonical_bucket = fs::canonicalize(bucket).ok()?;
    let canonical_session = fs::canonicalize(session).ok()?;
    (canonical_session.parent() == Some(canonical_bucket.as_path())).then_some(())?;
    let metadata_path = session.join("session.json");
    let messages = session.join("messages.jsonl");
    (regular_file(&metadata_path) && regular_file(&messages)).then_some(())?;
    let metadata: SessionMetadata = serde_json::from_slice(&fs::read(metadata_path).ok()?).ok()?;
    let bucket_name = bucket.file_name()?.to_str()?;
    (metadata.id == id
        && metadata.schema_version == SCHEMA_VERSION
        && metadata.data_model_version == DATA_MODEL_VERSION
        && metadata.workspace_paths.iter().any(|workspace| {
            workspace.is_absolute() && workspace_bucket(workspace).as_deref() == Some(bucket_name)
        }))
    .then_some(())?;
    Some(ValidatedSession { metadata, messages })
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn regular_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn valid_session_id(id: &str) -> bool {
    id.strip_prefix("sess_")
        .is_some_and(|uuid| Uuid::parse_str(uuid).is_ok())
}

fn parse_records(lines: &str) -> impl Iterator<Item = (Timestamp, Payload)> + '_ {
    lines.lines().filter_map(|line| {
        let envelope: Envelope = serde_json::from_str(line).ok()?;
        (!envelope.id.trim().is_empty()).then_some(())?;
        let at = envelope.timestamp.parse::<Timestamp>().ok()?;
        Some((at, envelope.payload))
    })
}

fn fold(lines: &str, created_at: Timestamp, metadata_status: &str) -> FoldedSession {
    let mut folded = FoldedSession {
        first_event_at: None,
        last_activity: created_at,
        status: match metadata_status {
            "active" | "running" => AgentStatus::Running,
            _ => AgentStatus::Idle,
        },
        phase: match metadata_status {
            "active" | "running" => TurnPhase::Reasoning,
            _ => TurnPhase::Idle,
        },
        latest_prompt: None,
        native_prompt_detail: None,
        waiting_since: None,
        context_pct: None,
    };
    let mut pending: BTreeMap<String, (Timestamp, Option<String>)> = BTreeMap::new();
    for (at, payload) in parse_records(lines) {
        folded.first_event_at.get_or_insert(at);
        folded.last_activity = at;
        match payload {
            Payload::User { content } => folded.latest_prompt = normalized(content),
            Payload::TurnStart { execution_id } if non_empty(execution_id.as_deref()).is_some() => {
                folded.status = AgentStatus::Running;
                folded.phase = TurnPhase::Reasoning;
                folded.waiting_since = None;
                folded.native_prompt_detail = None;
            }
            Payload::PendingInteraction {
                interaction_type,
                tool_call_id,
                question,
            } if interaction_type.as_deref() == Some("tool_approval") => {
                if let Some(tool_call_id) = normalized(tool_call_id) {
                    let question = normalized(question);
                    pending.insert(tool_call_id, (at, question.clone()));
                    folded.status = AgentStatus::Waiting;
                    folded.phase = TurnPhase::Idle;
                    folded.waiting_since = Some(at);
                    folded.native_prompt_detail = question;
                }
            }
            Payload::InteractionResolved { tool_call_id } => {
                if normalized(tool_call_id)
                    .and_then(|id| pending.remove(&id))
                    .is_some()
                {
                    if let Some((_, (since, detail))) =
                        pending.iter().max_by_key(|(_, (since, _))| *since)
                    {
                        folded.status = AgentStatus::Waiting;
                        folded.phase = TurnPhase::Idle;
                        folded.waiting_since = Some(*since);
                        folded.native_prompt_detail = detail.clone();
                    } else {
                        folded.status = AgentStatus::Running;
                        folded.phase = TurnPhase::Reasoning;
                        folded.waiting_since = None;
                        folded.native_prompt_detail = None;
                    }
                }
            }
            Payload::ToolCall {
                tool_call_id,
                tool_name,
                status,
            } if non_empty(tool_call_id.as_deref()).is_some()
                && non_empty(status.as_deref()) == Some("approved") =>
            {
                if pending.is_empty() {
                    folded.status = AgentStatus::Running;
                    folded.phase = if non_empty(tool_name.as_deref()) == Some("fs_write") {
                        TurnPhase::Acting
                    } else if folded.phase == TurnPhase::Idle {
                        TurnPhase::Reasoning
                    } else {
                        folded.phase
                    };
                }
            }
            Payload::ToolResult {
                tool_call_id,
                success,
            } if non_empty(tool_call_id.as_deref()).is_some() && success == Some(true) => {
                if pending.is_empty() {
                    folded.status = AgentStatus::Running;
                    if folded.phase == TurnPhase::Idle {
                        folded.phase = TurnPhase::Reasoning;
                    }
                }
            }
            Payload::SessionMetadata { key, value } if key.as_deref() == Some("contextUsage") => {
                let context_pct = value
                    .and_then(|value| value.usage_percentage)
                    .filter(|value| value.is_finite())
                    .map(|value| value.clamp(0.0, 100.0).round() as u8);
                if context_pct.is_some() {
                    folded.context_pct = context_pct;
                }
            }
            Payload::SessionEvent { category, context }
                if category.as_deref() == Some("session_pause")
                    && context
                        .as_ref()
                        .and_then(|context| context.status.as_deref())
                        == Some("success") =>
            {
                if pending.is_empty() {
                    folded.status = AgentStatus::Success;
                    folded.phase = TurnPhase::Idle;
                }
            }
            Payload::TurnEnd {
                execution_id,
                stop_reason,
            } if non_empty(execution_id.as_deref()).is_some()
                && stop_reason.as_deref() == Some("end_turn") =>
            {
                folded.status = AgentStatus::Success;
                folded.phase = TurnPhase::Idle;
            }
            _ => {}
        }
    }
    folded
}

#[cfg(test)]
pub(super) fn fold_for_test(lines: &str) -> (AgentStatus, TurnPhase, Option<String>, Option<u8>) {
    let folded = fold(lines, Timestamp::UNIX_EPOCH, "idle");
    (
        folded.status,
        folded.phase,
        folded.native_prompt_detail,
        folded.context_pct,
    )
}

fn normalized(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
