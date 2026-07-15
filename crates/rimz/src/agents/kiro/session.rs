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
    AgentStatus, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    TranscriptMessage, TranscriptRole, read_transcript_tail,
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
    status: Option<String>,
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

#[derive(Clone, Copy)]
enum TurnState {
    Idle,
    Running(TurnPhase),
    Waiting,
    Success,
}

impl TurnState {
    fn from_metadata_status(status: Option<&str>) -> Self {
        match status {
            Some("active" | "running") => Self::Running(TurnPhase::Reasoning),
            _ => Self::Idle,
        }
    }

    fn observation(self) -> (AgentStatus, TurnPhase) {
        match self {
            Self::Idle => (AgentStatus::Idle, TurnPhase::Idle),
            Self::Running(phase) => (AgentStatus::Running, phase),
            Self::Waiting => (AgentStatus::Waiting, TurnPhase::Idle),
            Self::Success => (AgentStatus::Success, TurnPhase::Idle),
        }
    }
}

#[derive(Clone)]
struct Waiting {
    since: Timestamp,
    detail: Option<String>,
}

struct FoldedSession {
    first_event_at: Option<Timestamp>,
    last_activity: Timestamp,
    turn: TurnState,
    latest_prompt: Option<String>,
    waiting: Option<Waiting>,
    pending: BTreeMap<String, Waiting>,
    context_pct: Option<u8>,
}

impl FoldedSession {
    fn new(created_at: Timestamp, metadata_status: Option<&str>) -> Self {
        Self {
            first_event_at: None,
            last_activity: created_at,
            turn: TurnState::from_metadata_status(metadata_status),
            latest_prompt: None,
            waiting: None,
            pending: BTreeMap::new(),
            context_pct: None,
        }
    }

    fn apply(&mut self, at: Timestamp, payload: Payload) {
        self.first_event_at.get_or_insert(at);
        self.last_activity = at;
        match payload {
            Payload::User { content } => self.latest_prompt = normalized(content),
            Payload::TurnStart { execution_id } => self.start_turn(execution_id),
            Payload::PendingInteraction {
                interaction_type,
                tool_call_id,
                question,
            } => self.request_approval(at, interaction_type, tool_call_id, question),
            Payload::InteractionResolved { tool_call_id } => {
                self.resolve_approval(tool_call_id);
            }
            Payload::ToolCall {
                tool_call_id,
                tool_name,
                status,
            } => self.apply_approved_tool(tool_call_id, tool_name, status),
            Payload::ToolResult {
                tool_call_id,
                success,
            } => self.apply_successful_tool_result(tool_call_id, success),
            Payload::SessionMetadata { key, value } => self.apply_context_metadata(key, value),
            Payload::SessionEvent { category, context } => {
                self.apply_successful_pause(category, context);
            }
            Payload::TurnEnd {
                execution_id,
                stop_reason,
            } => self.apply_successful_turn_end(execution_id, stop_reason),
            Payload::Assistant { .. } | Payload::Unknown => {}
        }
    }

    fn start_turn(&mut self, execution_id: Option<String>) {
        if non_empty(execution_id.as_deref()).is_none() {
            return;
        }
        self.turn = TurnState::Running(TurnPhase::Reasoning);
        self.waiting = None;
    }

    fn request_approval(
        &mut self,
        at: Timestamp,
        interaction_type: Option<String>,
        tool_call_id: Option<String>,
        question: Option<String>,
    ) {
        if interaction_type.as_deref() != Some("tool_approval") {
            return;
        }
        let Some(tool_call_id) = normalized(tool_call_id) else {
            return;
        };
        let waiting = Waiting {
            since: at,
            detail: normalized(question),
        };
        self.pending.insert(tool_call_id, waiting.clone());
        self.turn = TurnState::Waiting;
        self.waiting = Some(waiting);
    }

    fn resolve_approval(&mut self, tool_call_id: Option<String>) {
        let Some(tool_call_id) = normalized(tool_call_id) else {
            return;
        };
        if self.pending.remove(&tool_call_id).is_none() {
            return;
        }
        if let Some((_, waiting)) = self.pending.iter().max_by_key(|(_, waiting)| waiting.since) {
            self.turn = TurnState::Waiting;
            self.waiting = Some(waiting.clone());
        } else {
            self.turn = TurnState::Running(TurnPhase::Reasoning);
            self.waiting = None;
        }
    }

    fn apply_approved_tool(
        &mut self,
        tool_call_id: Option<String>,
        tool_name: Option<String>,
        status: Option<String>,
    ) {
        if non_empty(tool_call_id.as_deref()).is_none()
            || non_empty(status.as_deref()) != Some("approved")
            || !self.pending.is_empty()
        {
            return;
        }
        let phase = if non_empty(tool_name.as_deref()) == Some("fs_write") {
            TurnPhase::Acting
        } else {
            self.active_phase()
        };
        self.turn = TurnState::Running(phase);
    }

    fn apply_successful_tool_result(
        &mut self,
        tool_call_id: Option<String>,
        success: Option<bool>,
    ) {
        if non_empty(tool_call_id.as_deref()).is_none()
            || success != Some(true)
            || !self.pending.is_empty()
        {
            return;
        }
        self.turn = TurnState::Running(self.active_phase());
    }

    fn active_phase(&self) -> TurnPhase {
        match self.turn {
            TurnState::Running(phase) if phase != TurnPhase::Idle => phase,
            _ => TurnPhase::Reasoning,
        }
    }

    fn apply_context_metadata(&mut self, key: Option<String>, value: Option<ContextUsage>) {
        if key.as_deref() != Some("contextUsage") {
            return;
        }
        let Some(context_pct) = value
            .and_then(|value| value.usage_percentage)
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 100.0).round() as u8)
        else {
            return;
        };
        self.context_pct = Some(context_pct);
    }

    fn apply_successful_pause(
        &mut self,
        category: Option<String>,
        context: Option<SessionEventContext>,
    ) {
        if category.as_deref() == Some("session_pause")
            && context.and_then(|context| context.status).as_deref() == Some("success")
            && self.pending.is_empty()
        {
            self.turn = TurnState::Success;
        }
    }

    fn apply_successful_turn_end(
        &mut self,
        execution_id: Option<String>,
        stop_reason: Option<String>,
    ) {
        if non_empty(execution_id.as_deref()).is_some()
            && stop_reason.as_deref() == Some("end_turn")
        {
            self.turn = TurnState::Success;
        }
    }
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
        Some("idle"),
    );
    let (status, phase) = folded.turn.observation();
    let (native_prompt_detail, waiting_since) = folded.waiting.map_or((None, None), |waiting| {
        (waiting.detail, Some(waiting.since))
    });
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("kiro"),
        session_id: AgentSessionId::from("sess_11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from("/workspace/project/messages.jsonl"),
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: folded.first_event_at,
        last_activity: folded.last_activity,
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status,
            phase,
            latest_prompt: folded.latest_prompt,
            native_prompt_detail,
            waiting_since,
            context_pct: folded.context_pct,
        }),
    }
}

fn observation(session: ValidatedSession, workspace: &Path) -> Option<LocalSessionObservation> {
    let created_at = session.metadata.created_at.parse::<Timestamp>().ok()?;
    let lines = read_transcript_tail(&session.messages).unwrap_or_default();
    let folded = fold(&lines, created_at, session.metadata.status.as_deref());
    let (status, phase) = folded.turn.observation();
    let (native_prompt_detail, waiting_since) = folded.waiting.map_or((None, None), |waiting| {
        (waiting.detail, Some(waiting.since))
    });
    Some(LocalSessionObservation {
        kind: AgentKind::new_unchecked("kiro"),
        session_id: AgentSessionId::from(session.metadata.id),
        workspace: workspace.to_path_buf(),
        transcript_path: session.messages,
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: folded.first_event_at,
        last_activity: folded.last_activity,
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status,
            phase,
            latest_prompt: folded.latest_prompt,
            native_prompt_detail,
            waiting_since,
            context_pct: folded.context_pct,
        }),
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

fn fold(lines: &str, created_at: Timestamp, metadata_status: Option<&str>) -> FoldedSession {
    let mut folded = FoldedSession::new(created_at, metadata_status);
    for (at, payload) in parse_records(lines) {
        folded.apply(at, payload);
    }
    folded
}

#[cfg(test)]
pub(super) fn fold_for_test(
    lines: &str,
) -> (
    AgentStatus,
    TurnPhase,
    Option<String>,
    Option<u8>,
    Option<Timestamp>,
) {
    let folded = fold(lines, Timestamp::UNIX_EPOCH, Some("idle"));
    let (status, phase) = folded.turn.observation();
    let waiting_since = folded.waiting.as_ref().map(|waiting| waiting.since);
    (
        status,
        phase,
        folded.waiting.and_then(|waiting| waiting.detail),
        folded.context_pct,
        waiting_since,
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
