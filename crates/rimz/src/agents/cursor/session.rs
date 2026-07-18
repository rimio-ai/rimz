//! Version-pinned, read-only Cursor CLI local wait and subagent discovery.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use md5::{Digest as _, Md5};
use prost::Message;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use sha2::Sha256;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::local_session_cache::{
    IncrementalCatalog, ProviderPathStamp, ProviderPathState, StableValueCache, StampedPaths,
    normalized_workspace_inputs,
};
use crate::agents::{
    AgentStatus, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    non_empty_trimmed, sanitize_user_prompt,
};
use crate::ids::{AgentKind, AgentSessionId};

const META_SCHEMA_VERSION: u32 = 1;
const STORE_SCHEMA_VERSION: i64 = 1;
const MAX_SESSIONS_PER_WORKSPACE: usize = 32;
const MAX_META_BYTES: u64 = 64 * 1024;
const MAX_STORE_META_HEX_BYTES: usize = 128 * 1024;
const MAX_ROOT_BLOB_BYTES: usize = 2 * 1024 * 1024;
const MAX_MESSAGE_BLOB_BYTES: usize = 256 * 1024;
const MAX_PENDING_CALLS: usize = 64;
const MAX_PENDING_JSON_BYTES: usize = 256 * 1024;
const MAX_TOOL_CALL_ID_BYTES: usize = 512;
const MAX_TRANSCRIPT_PREFIX_BYTES: u64 = 256 * 1024;
const MAX_TRANSCRIPT_PREFIX_LINES: usize = 64;

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
    mode: Option<String>,
    #[serde(default)]
    current_plan_uri: Option<String>,
    #[serde(default)]
    subagent_info: Option<SubagentInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentInfo {
    parent_agent_id: String,
    #[serde(default)]
    type_name: Option<String>,
}

#[derive(Deserialize)]
struct TranscriptMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    message: Option<TranscriptMessageBody>,
}

#[derive(Deserialize)]
struct TranscriptMessageBody {
    #[serde(default)]
    content: Vec<TranscriptContent>,
}

#[derive(Deserialize)]
struct TranscriptContent {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct TranscriptTerminal {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CursorSubagentTerminal {
    pub(super) errored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CursorSubagentRecord {
    pub(super) child_id: AgentSessionId,
    pub(super) parent_agent_id: AgentSessionId,
    pub(super) type_name: Option<String>,
    pub(super) task: Option<String>,
    pub(super) created_at: Timestamp,
    pub(super) transcript_path: Option<PathBuf>,
    pub(super) terminal: Option<CursorSubagentTerminal>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct ConversationStateStructure {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub(super) message_ids: Vec<Vec<u8>>,
    #[prost(string, repeated, tag = "4")]
    pub(super) pending_tool_calls: Vec<String>,
}

#[derive(Deserialize)]
struct ConversationMessage {
    role: String,
    content: Vec<MessageContent>,
}

#[derive(Deserialize)]
struct MessageContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(rename = "toolName")]
    tool_name: String,
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

struct OpenWait {
    detail: String,
    waiting_since: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveryKey {
    home: PathBuf,
    workspaces: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ChatCandidate {
    workspace: PathBuf,
    bucket: PathBuf,
    session: PathBuf,
}

#[derive(Default)]
struct CursorDiscoverySnapshot {
    catalog: IncrementalCatalog<DiscoveryKey, ChatCandidate>,
    selected_ids: Vec<String>,
    transcript_topology: StampedPaths,
    transcripts: Option<super::transcript::DiscoveryCatalog>,
    candidates: StableValueCache<ChatCandidate, Option<LocalSessionObservation>>,
    #[cfg(test)]
    work: DiscoveryWork,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct DiscoveryWork {
    full_scans: usize,
    sqlite_reads: usize,
}

thread_local! {
    static DISCOVERY: RefCell<CursorDiscoverySnapshot> = RefCell::new(CursorDiscoverySnapshot::default());
}

#[cfg(test)]
pub(super) struct DiscoveryCacheHarness(CursorDiscoverySnapshot);

#[cfg(test)]
impl DiscoveryCacheHarness {
    pub(super) fn new() -> Self {
        Self(CursorDiscoverySnapshot::default())
    }

    pub(super) fn refresh(
        &mut self,
        home: &Path,
        workspaces: &[&Path],
        now: Instant,
    ) -> Vec<LocalSessionObservation> {
        self.0.refresh(
            DiscoveryKey {
                home: home.to_path_buf(),
                workspaces: normalized_workspace_inputs(workspaces),
            },
            now,
        )
    }

    pub(super) fn work(&self) -> (usize, usize) {
        (self.0.work.full_scans, self.0.work.sqlite_reads)
    }
}

pub(super) fn discover(workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
    let Some(home) = cursor_home(std::env::var_os("HOME").as_deref()) else {
        return Vec::new();
    };
    let key = DiscoveryKey {
        home,
        workspaces: normalized_workspace_inputs(workspaces),
    };
    if key.workspaces.is_empty() {
        return Vec::new();
    }
    DISCOVERY.with(|snapshot| snapshot.borrow_mut().refresh(key, Instant::now()))
}

impl CursorDiscoverySnapshot {
    fn refresh(&mut self, key: DiscoveryKey, now: Instant) -> Vec<LocalSessionObservation> {
        let scan = self.catalog.refresh(key.clone(), now, |topology| {
            topology.record_kind_only_many(key.workspaces.iter().cloned());
            let mut catalog = Vec::new();
            for workspace in &key.workspaces {
                let Some((bucket, workspace)) = chats_bucket(&key.home, workspace) else {
                    continue;
                };
                topology.record_exact(bucket.clone());
                let Ok(entries) = fs::read_dir(&bucket) else {
                    continue;
                };
                catalog.extend(
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                        .map(|entry| ChatCandidate {
                            workspace: workspace.clone(),
                            bucket: bucket.clone(),
                            session: entry.path(),
                        }),
                );
            }
            catalog
        });
        let forced = scan.attempted();
        #[cfg(test)]
        if forced {
            self.work.full_scans += 1;
        }

        let selected = self.selected_candidates(&key);
        let mut selected_ids = selected
            .iter()
            .filter_map(|candidate| candidate.session.file_name()?.to_str())
            .filter(|id| valid_session_id(id))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        selected_ids.sort();
        selected_ids.dedup();
        let transcript_topology_changed = !self.transcript_topology.unchanged();
        if self.transcripts.is_none()
            || self.selected_ids != selected_ids
            || transcript_topology_changed
        {
            self.rebuild_transcripts(&key.home, selected_ids, transcript_topology_changed);
        }

        let mut observations = Vec::new();
        for candidate in selected {
            let Some(session_id) = candidate
                .session
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let transcript = self
                .transcripts
                .as_ref()
                .and_then(|catalog| catalog.resolve(&session_id))
                .map(Path::to_path_buf);
            let mut dependency_paths = vec![
                candidate.session.clone(),
                candidate.session.join("meta.json"),
                candidate.session.join("store.db"),
                candidate.session.join("store.db-wal"),
                candidate.session.join("store.db-journal"),
            ];
            if let Some(catalog) = &self.transcripts {
                dependency_paths.extend(catalog.dependencies(&session_id).iter().cloned());
            }
            dependency_paths.sort();
            dependency_paths.dedup();
            let mut dependencies = StampedPaths::default();
            dependencies.record_kind_only(candidate.session.clone());
            dependencies.record_exact_many(
                dependency_paths
                    .into_iter()
                    .filter(|path| path != &candidate.session),
            );
            if let Some(observation) =
                self.refresh_candidate(candidate, transcript.as_deref(), dependencies, forced)
            {
                observations.push(observation);
            }
        }
        let catalog = self.catalog.entries();
        self.candidates
            .retain(|candidate| catalog.contains(candidate));
        observations.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.session_id.cmp(&right.session_id))
        });
        observations
    }

    fn selected_candidates(&self, key: &DiscoveryKey) -> Vec<ChatCandidate> {
        let mut selected = Vec::new();
        for workspace in &key.workspaces {
            let mut bucket = self
                .catalog
                .entries()
                .iter()
                .filter(|candidate| &candidate.workspace == workspace)
                .filter_map(|candidate| {
                    let stamp = ProviderPathStamp::read(&candidate.session);
                    stamp.is_dir().then(|| (candidate.clone(), stamp.modified))
                })
                .collect::<Vec<_>>();
            bucket.sort_by(|left, right| {
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| left.0.session.cmp(&right.0.session))
            });
            selected.extend(
                bucket
                    .into_iter()
                    .take(MAX_SESSIONS_PER_WORKSPACE)
                    .map(|(candidate, _)| candidate),
            );
        }
        selected
    }

    fn rebuild_transcripts(
        &mut self,
        home: &Path,
        selected_ids: Vec<String>,
        topology_changed: bool,
    ) {
        let catalog =
            super::transcript::DiscoveryCatalog::build(&home.join("projects"), &selected_ids);
        if topology_changed || !catalog.stable {
            self.catalog.invalidate();
        }
        self.transcript_topology = catalog.topology.recapture();
        self.selected_ids = selected_ids;
        self.transcripts = Some(catalog);
    }

    fn refresh_candidate(
        &mut self,
        candidate: ChatCandidate,
        transcript: Option<&Path>,
        dependencies: StampedPaths,
        forced: bool,
    ) -> Option<LocalSessionObservation> {
        let optional_paths_are_safe = dependencies.iter().all(|(path, stamp)| {
            if path == candidate.session.join("store.db-wal")
                || path == candidate.session.join("store.db-journal")
            {
                matches!(
                    stamp.state,
                    ProviderPathState::Missing | ProviderPathState::File
                )
            } else {
                true
            }
        });
        #[cfg(test)]
        let sqlite_reads = &mut self.work.sqlite_reads;
        let result = self
            .candidates
            .refresh(candidate.clone(), dependencies, forced, |_| {
                if !optional_paths_are_safe {
                    return None;
                }
                transcript.and_then(|transcript| {
                    #[cfg(test)]
                    {
                        *sqlite_reads += 1;
                    }
                    observation_with_transcript(
                        &candidate.bucket,
                        &candidate.session,
                        &candidate.workspace,
                        transcript,
                    )
                })
            });
        result.into_current().flatten()
    }
}

pub(super) fn cursor_home(home: Option<&OsStr>) -> Option<PathBuf> {
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".cursor"))
}

#[cfg(any(test, feature = "testkit"))]
pub(super) fn discover_under(home: &Path, workspace: &Path) -> Vec<LocalSessionObservation> {
    let key = DiscoveryKey {
        home: home.to_path_buf(),
        workspaces: normalized_workspace_inputs(&[workspace]),
    };
    CursorDiscoverySnapshot::default().refresh(key, Instant::now())
}

pub(super) fn discover_subagent_chats(home: &Path, workspace: &Path) -> Vec<CursorSubagentRecord> {
    let Some((bucket, _workspace)) = chats_bucket(home, workspace) else {
        return Vec::new();
    };
    let mut records = newest_chat_dirs(&bucket)
        .into_iter()
        .filter_map(|session| subagent_record(home, &bucket, &session))
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.child_id.cmp(&right.child_id))
    });
    records
}

fn chats_bucket(home: &Path, workspace: &Path) -> Option<(PathBuf, PathBuf)> {
    let workspace = crate::worktree::normalize_path_lexical(workspace);
    let workspace_text = workspace.to_str()?;
    if !workspace.is_absolute() || !regular_dir(&workspace) {
        return None;
    }
    let bucket = home
        .join("chats")
        .join(hex::encode(Md5::digest(workspace_text.as_bytes())));
    if !regular_dir(&bucket) {
        return None;
    }
    Some((bucket, workspace))
}

fn newest_chat_dirs(bucket: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(bucket) else {
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
    sessions
        .into_iter()
        .take(MAX_SESSIONS_PER_WORKSPACE)
        .map(|(_, session)| session)
        .collect()
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

fn observation_with_transcript(
    bucket: &Path,
    session: &Path,
    workspace: &Path,
    transcript_path: &Path,
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
    let open_wait = read_open_wait(
        &store_path,
        session_id,
        metadata.created_at_ms,
        metadata.updated_at_ms,
    )?;
    (open_wait.waiting_since >= created_at && open_wait.waiting_since <= updated_at)
        .then_some(())?;
    Some(LocalSessionObservation {
        kind: AgentKind::new_unchecked("cursor"),
        session_id: AgentSessionId::from(session_id),
        workspace: workspace.to_path_buf(),
        transcript_path: transcript_path.to_path_buf(),
        created_at,
        fresh_binding_at: None,
        first_event_at: Some(created_at),
        last_activity: updated_at,
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status: AgentStatus::Waiting,
            phase: TurnPhase::Idle,
            latest_prompt: None,
            native_prompt_detail: Some(open_wait.detail),
            waiting_since: Some(open_wait.waiting_since),
            context_pct: None,
        }),
    })
}

fn subagent_record(home: &Path, bucket: &Path, session: &Path) -> Option<CursorSubagentRecord> {
    let session_id = session.file_name()?.to_str()?;
    valid_session_id(session_id).then_some(())?;
    regular_dir(session).then_some(())?;
    session
        .parent()
        .is_some_and(|parent| parent == bucket)
        .then_some(())?;

    metadata_absent(&session.join("meta.json")).then_some(())?;
    let store_path = session.join("store.db");
    regular_file(&store_path).then_some(())?;
    let (_, store_metadata) = read_store_metadata(&store_path)?;
    (store_metadata.agent_id == session_id).then_some(())?;
    let subagent = store_metadata.subagent_info?;
    let (child_id, parent_agent_id) = crate::agents::identity::validated_subagent_identity(
        Some(session_id),
        Some(&subagent.parent_agent_id),
    )?;
    (store_metadata.created_at >= 0).then_some(())?;
    let created_at = timestamp_ms(store_metadata.created_at)?;
    let transcript_path = super::transcript::discover_under(&home.join("projects"), session_id);
    let (task, terminal) = match transcript_path.as_deref() {
        Some(path) => read_subagent_transcript(path)?,
        None => (None, None),
    };

    Some(CursorSubagentRecord {
        child_id,
        parent_agent_id,
        type_name: subagent.type_name.as_deref().and_then(non_empty_trimmed),
        task,
        created_at,
        transcript_path,
        terminal,
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

fn read_open_wait(
    store_path: &Path,
    session_id: &str,
    created_at_ms: i64,
    updated_at_ms: i64,
) -> Option<OpenWait> {
    let (connection, store_metadata) = read_store_metadata(store_path)?;
    (store_metadata.agent_id == session_id
        && store_metadata.created_at == created_at_ms
        && store_metadata.subagent_info.is_none()
        && valid_blob_id(&store_metadata.latest_root_blob_id))
    .then_some(())?;

    let blob = read_blob(
        &connection,
        &store_metadata.latest_root_blob_id,
        MAX_ROOT_BLOB_BYTES,
    )?;
    (hex::encode(Sha256::digest(&blob)) == store_metadata.latest_root_blob_id).then_some(())?;
    let root = ConversationStateStructure::decode(blob.as_slice()).ok()?;
    if let Some(open_ask) = parse_open_ask(&root.pending_tool_calls) {
        return Some(open_ask);
    }
    root.pending_tool_calls.is_empty().then_some(())?;
    read_open_plan_approval(
        &connection,
        &store_metadata,
        &root.message_ids,
        updated_at_ms,
    )
}

fn read_store_metadata(store_path: &Path) -> Option<(Connection, StoreMetadata)> {
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
    Some((connection, store_metadata))
}

fn read_blob(connection: &Connection, id: &str, max_bytes: usize) -> Option<Vec<u8>> {
    connection
        .query_row(
            "SELECT length(data), data FROM blobs WHERE id = ?1",
            [id],
            |row| {
                let bytes: i64 = row.get(0)?;
                if !(0..=max_bytes as i64).contains(&bytes) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                row.get(1)
            },
        )
        .ok()
}

fn read_subagent_transcript(
    transcript_path: &Path,
) -> Option<(Option<String>, Option<CursorSubagentTerminal>)> {
    let task = read_subagent_task(transcript_path);
    let tail = crate::agents::read_transcript_tail_with_status(transcript_path)?;
    let terminal = if tail.torn_suffix {
        None
    } else {
        let Some(line) = tail.text.lines().rev().find(|line| !line.trim().is_empty()) else {
            return Some((task, None));
        };
        match serde_json::from_str::<TranscriptTerminal>(line) {
            Ok(record) if record.r#type.as_deref() != Some("turn_ended") => None,
            Ok(record) => Some(CursorSubagentTerminal {
                errored: !matches!(
                    record.status.as_deref(),
                    Some("success" | "completed" | "aborted")
                ),
            }),
            Err(_) => Some(CursorSubagentTerminal { errored: true }),
        }
    };
    Some((task, terminal))
}

fn read_subagent_task(transcript_path: &Path) -> Option<String> {
    let file = fs::File::open(transcript_path).ok()?;
    let mut prefix = Vec::new();
    file.take(MAX_TRANSCRIPT_PREFIX_BYTES + 1)
        .read_to_end(&mut prefix)
        .ok()?;
    prefix.truncate(MAX_TRANSCRIPT_PREFIX_BYTES as usize);
    let prefix = String::from_utf8_lossy(&prefix);
    prefix
        .lines()
        .take(MAX_TRANSCRIPT_PREFIX_LINES)
        .filter_map(|line| serde_json::from_str::<TranscriptMessage>(line).ok())
        .find(|message| message.role.as_deref() == Some("user"))?
        .message?
        .content
        .iter()
        .filter_map(|content| content.text.as_deref())
        .find_map(|text| {
            let (_, rest) = text.split_once("<user_query>")?;
            let (query, _) = rest.split_once("</user_query>")?;
            sanitize_user_prompt(Some(query))
        })
}

fn read_open_plan_approval(
    connection: &Connection,
    store_metadata: &StoreMetadata,
    message_ids: &[Vec<u8>],
    updated_at_ms: i64,
) -> Option<OpenWait> {
    (store_metadata.mode.as_deref() == Some("plan")
        && store_metadata
            .current_plan_uri
            .as_deref()
            .is_some_and(|uri| !uri.trim().is_empty()))
    .then_some(())?;
    let message_id = message_ids.last()?;
    (message_id.len() == 32).then_some(())?;
    let message_id = hex::encode(message_id);
    let blob = read_blob(connection, &message_id, MAX_MESSAGE_BLOB_BYTES)?;
    (hex::encode(Sha256::digest(&blob)) == message_id).then_some(())?;
    let message = serde_json::from_slice::<ConversationMessage>(&blob).ok()?;
    (message.role == "tool").then_some(())?;
    let [content] = message.content.as_slice() else {
        return None;
    };
    (content.content_type == "tool-result" && content.tool_name == "CreatePlan").then_some(())?;
    Some(OpenWait {
        detail: "Ready to build?".to_owned(),
        waiting_since: timestamp_ms(updated_at_ms)?,
    })
}

fn parse_open_ask(pending: &[String]) -> Option<OpenWait> {
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
            candidates.push(OpenWait {
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

fn metadata_absent(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn bounded_regular_file(path: &Path, max_bytes: u64) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() <= max_bytes)
}
