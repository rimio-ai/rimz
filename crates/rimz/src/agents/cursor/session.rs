//! Version-pinned, read-only Cursor CLI local Ask discovery.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use md5::{Digest as _, Md5};
use prost::Message;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentStatus, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    sanitize_user_prompt,
};
use crate::ids::{AgentKind, AgentSessionId};

const META_SCHEMA_VERSION: u32 = 1;
const STORE_SCHEMA_VERSION: i64 = 1;
const MAX_SESSIONS_PER_WORKSPACE: usize = 32;
const MAX_META_BYTES: u64 = 64 * 1024;
const MAX_STORE_META_HEX_BYTES: usize = 128 * 1024;
const MAX_ROOT_BLOB_BYTES: usize = 2 * 1024 * 1024;
const MAX_PENDING_CALLS: usize = 64;
const MAX_PENDING_JSON_BYTES: usize = 256 * 1024;
const MAX_TOOL_CALL_ID_BYTES: usize = 512;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMetadata {
    schema_version: u32,
    created_at_ms: i64,
    updated_at_ms: i64,
    has_conversation: bool,
    cwd: PathBuf,
    #[serde(default)]
    is_subagent: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreMetadata {
    agent_id: String,
    created_at: i64,
    latest_root_blob_id: String,
    #[serde(default)]
    subagent_info: Option<Value>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct ConversationStateStructure {
    #[prost(string, repeated, tag = "4")]
    pub(super) pending_tool_calls: Vec<String>,
}

#[derive(Deserialize)]
struct PendingAssistant {
    #[serde(default)]
    content: Vec<PendingContent>,
    #[serde(default, rename = "providerOptions")]
    provider_options: ProviderOptions,
}

#[derive(Deserialize)]
struct PendingContent {
    #[serde(default, rename = "type")]
    content_type: String,
    #[serde(default, rename = "toolCallId")]
    tool_call_id: String,
    #[serde(default, rename = "toolName")]
    tool_name: String,
    #[serde(default)]
    args: AskArgs,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskArgs {
    #[serde(default)]
    run_async: bool,
    #[serde(default)]
    questions: Vec<AskQuestion>,
}

#[derive(Deserialize)]
struct AskQuestion {
    prompt: String,
}

#[derive(Default, Deserialize)]
struct ProviderOptions {
    #[serde(default)]
    cursor: CursorProviderOptions,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorProviderOptions {
    pending_tool_call_started_at_ms: Option<i64>,
}

struct OpenAsk {
    detail: String,
    waiting_since: Timestamp,
}

pub(super) fn discover(workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
    let Some(home) = cursor_home(std::env::var_os("HOME").as_deref()) else {
        return Vec::new();
    };
    workspaces
        .iter()
        .flat_map(|workspace| discover_under(&home, workspace))
        .collect()
}

pub(super) fn cursor_home(home: Option<&OsStr>) -> Option<PathBuf> {
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".cursor"))
}

pub(super) fn discover_under(home: &Path, workspace: &Path) -> Vec<LocalSessionObservation> {
    let workspace = crate::worktree::normalize_path_lexical(workspace);
    let Some(workspace_text) = workspace.to_str() else {
        return Vec::new();
    };
    if !workspace.is_absolute() || !regular_dir(&workspace) {
        return Vec::new();
    }
    let bucket = home
        .join("chats")
        .join(hex::encode(Md5::digest(workspace_text.as_bytes())));
    if !regular_dir(&bucket) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&bucket) else {
        return Vec::new();
    };
    let mut sessions = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((std::cmp::Reverse(modified), entry.path()))
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let mut observations = sessions
        .into_iter()
        .take(MAX_SESSIONS_PER_WORKSPACE)
        .filter_map(|(_, session)| observation(home, &bucket, &session, &workspace))
        .collect::<Vec<_>>();
    observations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.session_id.cmp(&right.session_id))
    });
    observations
}

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let created_at = Timestamp::from_second(1_735_689_600).unwrap();
    let waiting_since = Timestamp::from_second(1_735_689_610).unwrap();
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("cursor"),
        session_id: AgentSessionId::from("11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from(
            "/provider/projects/project/agent-transcripts/11111111/11111111.jsonl",
        ),
        created_at,
        fresh_binding_at: None,
        first_event_at: Some(created_at),
        last_activity: Timestamp::from_second(1_735_689_620).unwrap(),
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status: AgentStatus::Waiting,
            phase: TurnPhase::Idle,
            latest_prompt: None,
            native_prompt_detail: Some("Which color?".to_owned()),
            waiting_since: Some(waiting_since),
            context_pct: None,
        }),
    }
}

fn observation(
    home: &Path,
    bucket: &Path,
    session: &Path,
    workspace: &Path,
) -> Option<LocalSessionObservation> {
    let session_id = session.file_name()?.to_str()?;
    valid_session_id(session_id).then_some(())?;
    regular_dir(session).then_some(())?;
    session
        .parent()
        .is_some_and(|parent| parent == bucket)
        .then_some(())?;

    let metadata_path = session.join("meta.json");
    let store_path = session.join("store.db");
    bounded_regular_file(&metadata_path, MAX_META_BYTES).then_some(())?;
    regular_file(&store_path).then_some(())?;
    let metadata = serde_json::from_slice::<ChatMetadata>(&fs::read(&metadata_path).ok()?).ok()?;
    validate_metadata(&metadata, workspace).then_some(())?;

    let created_at = timestamp_ms(metadata.created_at_ms)?;
    let updated_at = timestamp_ms(metadata.updated_at_ms)?;
    let open_ask = read_open_ask(&store_path, session_id, metadata.created_at_ms)?;
    (open_ask.waiting_since >= created_at && open_ask.waiting_since <= updated_at).then_some(())?;
    let transcript_path = super::transcript::discover_under(&home.join("projects"), session_id)?;

    Some(LocalSessionObservation {
        kind: AgentKind::new_unchecked("cursor"),
        session_id: AgentSessionId::from(session_id),
        workspace: workspace.to_path_buf(),
        transcript_path,
        created_at,
        fresh_binding_at: None,
        first_event_at: Some(created_at),
        last_activity: updated_at,
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status: AgentStatus::Waiting,
            phase: TurnPhase::Idle,
            latest_prompt: None,
            native_prompt_detail: Some(open_ask.detail),
            waiting_since: Some(open_ask.waiting_since),
            context_pct: None,
        }),
    })
}

fn validate_metadata(metadata: &ChatMetadata, workspace: &Path) -> bool {
    metadata.schema_version == META_SCHEMA_VERSION
        && metadata.has_conversation
        && !metadata.is_subagent
        && metadata.created_at_ms >= 0
        && metadata.created_at_ms <= metadata.updated_at_ms
        && metadata.cwd.is_absolute()
        && crate::worktree::normalize_path_lexical(&metadata.cwd) == workspace
}

fn read_open_ask(store_path: &Path, session_id: &str, created_at_ms: i64) -> Option<OpenAsk> {
    let connection = Connection::open_with_flags(
        store_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    connection.busy_timeout(Duration::ZERO).ok()?;
    let schema: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .ok()?;
    (schema == STORE_SCHEMA_VERSION).then_some(())?;

    let encoded: String = connection
        .query_row(
            "SELECT length(value), value FROM meta WHERE key = '0'",
            [],
            |row| {
                let bytes: i64 = row.get(0)?;
                if !(0..=MAX_STORE_META_HEX_BYTES as i64).contains(&bytes) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                row.get(1)
            },
        )
        .ok()?;
    encoded.len().is_multiple_of(2).then_some(())?;
    let decoded = hex::decode(encoded).ok()?;
    let store_metadata = serde_json::from_slice::<StoreMetadata>(&decoded).ok()?;
    (store_metadata.agent_id == session_id
        && store_metadata.created_at == created_at_ms
        && store_metadata.subagent_info.is_none()
        && valid_blob_id(&store_metadata.latest_root_blob_id))
    .then_some(())?;

    let blob: Vec<u8> = connection
        .query_row(
            "SELECT length(data), data FROM blobs WHERE id = ?1",
            [&store_metadata.latest_root_blob_id],
            |row| {
                let bytes: i64 = row.get(0)?;
                if !(0..=MAX_ROOT_BLOB_BYTES as i64).contains(&bytes) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                row.get(1)
            },
        )
        .ok()?;
    (hex::encode(Sha256::digest(&blob)) == store_metadata.latest_root_blob_id).then_some(())?;
    let root = ConversationStateStructure::decode(blob.as_slice()).ok()?;
    parse_open_ask(&root.pending_tool_calls)
}

fn parse_open_ask(pending: &[String]) -> Option<OpenAsk> {
    (pending.len() <= MAX_PENDING_CALLS).then_some(())?;
    let mut candidates = Vec::new();
    for raw in pending {
        (raw.len() <= MAX_PENDING_JSON_BYTES).then_some(())?;
        let assistant = serde_json::from_str::<PendingAssistant>(raw).ok()?;
        for content in assistant.content {
            if content.content_type != "tool-call"
                || content.tool_name != "AskQuestion"
                || content.args.run_async
            {
                continue;
            }
            let tool_call_id = content.tool_call_id.trim();
            if tool_call_id.is_empty()
                || tool_call_id.len() > MAX_TOOL_CALL_ID_BYTES
                || tool_call_id.chars().any(char::is_control)
            {
                return None;
            }
            let detail = content
                .args
                .questions
                .iter()
                .find_map(|question| sanitize_user_prompt(Some(&question.prompt)))?;
            let waiting_since = assistant
                .provider_options
                .cursor
                .pending_tool_call_started_at_ms
                .and_then(timestamp_ms)?;
            candidates.push(OpenAsk {
                detail,
                waiting_since,
            });
        }
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

fn valid_session_id(id: &str) -> bool {
    let mut components = Path::new(id).components();
    !id.is_empty()
        && id.len() <= 256
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
}

fn valid_blob_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn timestamp_ms(value: i64) -> Option<Timestamp> {
    Timestamp::from_millisecond(value).ok()
}

fn regular_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn bounded_regular_file(path: &Path, max_bytes: u64) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() <= max_bytes)
}
