//! Validated Antigravity CLI local-session discovery and transcript projection.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use url::Url;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::local_session_cache::{
    ProviderPathStamp, full_scan_due, normalized_workspace_inputs, stamp_paths, stamps_unchanged,
};
use crate::agents::{
    AgentStatus, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    SpawnedSubagent, TranscriptMessage, TranscriptRole, read_transcript_tail,
    read_transcript_tail_with_status, sanitize_user_prompt,
};
use crate::ids::{AgentKind, AgentSessionId};

const MAX_DISCOVERED_SESSIONS: usize = 512;
const TRANSCRIPT_BASENAMES: [&str; 2] = ["transcript_full.jsonl", "transcript.jsonl"];

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
    #[serde(default, deserialize_with = "deserialize_tool_calls")]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize)]
struct ToolCall {
    name: String,
    #[serde(default)]
    args: ToolCallArgs,
}

#[derive(Default, Deserialize)]
struct ToolCallArgs {
    #[serde(default, deserialize_with = "deserialize_questions")]
    questions: Vec<ToolQuestion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CorrelatedSubagent {
    pub type_name: String,
    pub role: String,
    pub prompt: String,
}

#[derive(Deserialize)]
struct CorrelationRecord {
    #[serde(rename = "step_index")]
    _step_index: u64,
    source: String,
    #[serde(rename = "type")]
    record_type: String,
    status: String,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<Value>,
}

#[derive(Deserialize)]
struct InvokeSubagentCall {
    args: InvokeSubagentArgs,
}

#[derive(Deserialize)]
struct InvokeSubagentArgs {
    #[serde(rename = "Subagents")]
    subagents: Vec<InvokeSubagentSpec>,
}

#[derive(Clone, Deserialize)]
struct InvokeSubagentSpec {
    #[serde(rename = "Prompt")]
    prompt: String,
    #[serde(rename = "Role")]
    role: String,
    #[serde(rename = "TypeName")]
    type_name: String,
    #[serde(rename = "Workspace")]
    workspace: Option<String>,
}

#[derive(Deserialize)]
struct InvokeSubagentResult {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "logAbsoluteUri")]
    log_absolute_uri: String,
    #[serde(rename = "workspaceUris")]
    workspace_uris: Option<Vec<String>>,
}

struct ValidatedSubagent {
    spec: InvokeSubagentSpec,
    result: InvokeSubagentResult,
    expected_workspace: PathBuf,
}

#[derive(Deserialize)]
struct ToolQuestion {
    question: String,
}

#[derive(Clone)]
struct FoldedSession {
    first_event_at: Option<Timestamp>,
    last_activity: Option<Timestamp>,
    status: AgentStatus,
    phase: TurnPhase,
    latest_prompt: Option<String>,
    native_prompt_detail: Option<String>,
    waiting_since: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveryKey {
    home: PathBuf,
    workspaces: Vec<PathBuf>,
}

#[derive(Clone)]
struct ConversationCandidate {
    id: String,
    session_dir: PathBuf,
    generated_dir: PathBuf,
    logs_dir: PathBuf,
    preferred: PathBuf,
    fallback: PathBuf,
}

impl ConversationCandidate {
    fn dependencies(&self) -> Vec<PathBuf> {
        vec![
            self.session_dir.clone(),
            self.generated_dir.clone(),
            self.logs_dir.clone(),
            self.preferred.clone(),
            self.fallback.clone(),
        ]
    }

    fn selected_path(&self, stamps: &[(PathBuf, ProviderPathStamp)]) -> Option<PathBuf> {
        stamps
            .iter()
            .find(|(path, stamp)| path == &self.preferred && stamp.is_file())
            .or_else(|| {
                stamps
                    .iter()
                    .find(|(path, stamp)| path == &self.fallback && stamp.is_file())
            })
            .map(|(path, _)| path.clone())
    }
}

#[derive(Clone)]
struct DiscoveredConversation {
    session_id: String,
    transcript_path: PathBuf,
    created_at: Timestamp,
    first_event_at: Option<Timestamp>,
    last_activity: Timestamp,
    projection: LocalSessionProjection,
}

#[derive(Clone)]
struct CachedConversation {
    dependencies: Vec<(PathBuf, ProviderPathStamp)>,
    conversation: Option<DiscoveredConversation>,
}

#[derive(Clone)]
struct CachedIndex {
    stamp: ProviderPathStamp,
    values: BTreeMap<PathBuf, String>,
}

#[derive(Default)]
struct AntigravityDiscoverySnapshot {
    key: Option<DiscoveryKey>,
    last_full_scan: Option<Instant>,
    topology: Vec<(PathBuf, ProviderPathStamp)>,
    catalog: Vec<ConversationCandidate>,
    conversations: HashMap<String, CachedConversation>,
    index: Option<CachedIndex>,
    #[cfg(test)]
    work: DiscoveryWork,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct DiscoveryWork {
    full_scans: usize,
    index_parses: usize,
    tail_folds: usize,
}

thread_local! {
    static DISCOVERY: RefCell<AntigravityDiscoverySnapshot> = RefCell::new(AntigravityDiscoverySnapshot::default());
}

#[cfg(test)]
pub(super) struct DiscoveryCacheHarness(AntigravityDiscoverySnapshot);

#[cfg(test)]
impl DiscoveryCacheHarness {
    pub(super) fn new() -> Self {
        Self(AntigravityDiscoverySnapshot::default())
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

    pub(super) fn work(&self) -> (usize, usize, usize) {
        (
            self.0.work.full_scans,
            self.0.work.index_parses,
            self.0.work.tail_folds,
        )
    }
}

pub(super) fn discover(workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
    let Some(home) = home() else {
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

impl AntigravityDiscoverySnapshot {
    fn refresh(&mut self, key: DiscoveryKey, now: Instant) -> Vec<LocalSessionObservation> {
        let key_changed = self.key.as_ref() != Some(&key);
        let topology_changed = !key_changed && !stamps_unchanged(&self.topology);
        let forced = key_changed || topology_changed || full_scan_due(self.last_full_scan, now);
        if forced {
            self.rebuild_catalog(&key, now);
        }

        let mut selected = self
            .catalog
            .iter()
            .cloned()
            .filter_map(|candidate| {
                let dependencies = stamp_paths(candidate.dependencies());
                let path = candidate.selected_path(&dependencies)?;
                let modified = dependencies
                    .iter()
                    .find(|(candidate, _)| candidate == &path)
                    .and_then(|(_, stamp)| stamp.modified);
                Some((candidate, dependencies, modified))
            })
            .collect::<Vec<_>>();
        selected.sort_by(|left, right| {
            right
                .2
                .cmp(&left.2)
                .then_with(|| left.0.id.cmp(&right.0.id))
        });
        selected.truncate(MAX_DISCOVERED_SESSIONS);
        let conversations = selected
            .into_iter()
            .filter_map(|(candidate, dependencies, _)| {
                self.refresh_conversation(&key.home, candidate, dependencies, forced)
            })
            .collect::<Vec<_>>();
        let index = self.refresh_index(&key.home, forced);

        let catalog_ids = self
            .catalog
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<std::collections::HashSet<_>>();
        self.conversations.retain(|id, _| catalog_ids.contains(id));

        let mut observations = Vec::new();
        for workspace in &key.workspaces {
            let current = index
                .get(workspace)
                .filter(|id| valid_conversation_id(id))
                .map(String::as_str);
            let mut workspace_observations = conversations
                .iter()
                .map(|conversation| materialize(conversation, workspace, current))
                .collect::<Vec<_>>();
            workspace_observations.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then(left.session_id.cmp(&right.session_id))
            });
            observations.extend(workspace_observations);
        }
        observations
    }

    fn rebuild_catalog(&mut self, key: &DiscoveryKey, now: Instant) {
        #[cfg(test)]
        {
            self.work.full_scans += 1;
        }
        let brain = key.home.join("brain");
        let topology_before = stamp_paths([brain.clone()]);
        let mut catalog = Vec::new();
        if ProviderPathStamp::read(&brain).is_dir()
            && let Ok(entries) = fs::read_dir(&brain)
        {
            for entry in entries.filter_map(Result::ok) {
                if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                    continue;
                }
                let Ok(id) = entry.file_name().into_string() else {
                    continue;
                };
                if !valid_conversation_id(&id) {
                    continue;
                }
                let session_dir = entry.path();
                let generated_dir = session_dir.join(".system_generated");
                let logs_dir = generated_dir.join("logs");
                catalog.push(ConversationCandidate {
                    id,
                    session_dir,
                    generated_dir,
                    preferred: logs_dir.join(TRANSCRIPT_BASENAMES[0]),
                    fallback: logs_dir.join(TRANSCRIPT_BASENAMES[1]),
                    logs_dir,
                });
            }
        }
        let stable = stamps_unchanged(&topology_before);
        self.key = Some(key.clone());
        self.last_full_scan = stable.then_some(now);
        self.topology = stamp_paths([brain]);
        self.catalog = stable.then_some(catalog).unwrap_or_default();
    }

    fn refresh_conversation(
        &mut self,
        home: &Path,
        candidate: ConversationCandidate,
        before: Vec<(PathBuf, ProviderPathStamp)>,
        forced: bool,
    ) -> Option<DiscoveredConversation> {
        if !forced
            && let Some(cached) = self.conversations.get(&candidate.id)
            && cached.dependencies == before
            && before.iter().all(|(_, stamp)| stamp.is_stable())
        {
            return cached.conversation.clone();
        }
        #[cfg(test)]
        {
            self.work.tail_folds += 1;
        }
        let selected = candidate.selected_path(&before);
        let conversation = selected
            .as_deref()
            .and_then(|path| discover_conversation(home, path, &candidate.id));
        let after = stamp_paths(candidate.dependencies());
        if before != after || after.iter().any(|(_, stamp)| !stamp.is_stable()) {
            self.conversations.remove(&candidate.id);
            return None;
        }
        self.conversations.insert(
            candidate.id,
            CachedConversation {
                dependencies: after,
                conversation: conversation.clone(),
            },
        );
        conversation
    }

    fn refresh_index(&mut self, home: &Path, forced: bool) -> BTreeMap<PathBuf, String> {
        let path = home.join("cache/last_conversations.json");
        let before = ProviderPathStamp::read(&path);
        if !forced
            && let Some(cached) = &self.index
            && cached.stamp == before
            && before.is_stable()
        {
            return cached.values.clone();
        }
        #[cfg(test)]
        {
            self.work.index_parses += 1;
        }
        let values = if before.is_file() {
            fs::read(&path)
                .ok()
                .and_then(|body| serde_json::from_slice::<BTreeMap<PathBuf, String>>(&body).ok())
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        let after = ProviderPathStamp::read(&path);
        if before != after || !after.is_stable() {
            self.index = None;
            return BTreeMap::new();
        }
        self.index = Some(CachedIndex {
            stamp: after,
            values: values.clone(),
        });
        values
    }
}

fn discover_conversation(
    home: &Path,
    path: &Path,
    session_id: &str,
) -> Option<DiscoveredConversation> {
    valid_transcript_under(home, path, session_id).then_some(())?;
    let lines = read_transcript_tail(path)?;
    let folded = fold(&lines);
    let created_at = folded.first_event_at?;
    let last_activity = folded.last_activity?;
    Some(DiscoveredConversation {
        session_id: session_id.to_owned(),
        transcript_path: path.to_path_buf(),
        created_at,
        first_event_at: Some(created_at),
        last_activity,
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status: folded.status,
            phase: folded.phase,
            latest_prompt: folded.latest_prompt,
            native_prompt_detail: folded.native_prompt_detail,
            waiting_since: folded.waiting_since,
            context_pct: None,
        }),
    })
}

fn materialize(
    conversation: &DiscoveredConversation,
    workspace: &Path,
    current_session_id: Option<&str>,
) -> LocalSessionObservation {
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("antigravity"),
        session_id: AgentSessionId::from(conversation.session_id.clone()),
        workspace: workspace.to_path_buf(),
        transcript_path: conversation.transcript_path.clone(),
        created_at: conversation.created_at,
        fresh_binding_at: (current_session_id == Some(conversation.session_id.as_str()))
            .then_some(conversation.created_at),
        first_event_at: conversation.first_event_at,
        last_activity: conversation.last_activity,
        projection: conversation.projection.clone(),
    }
}

#[cfg(test)]
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
                TranscriptRole::User => normalize_user_content(Some(content)),
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
    transcript_for_session_under(&home, session_id)
}

pub(super) fn valid_transcript(path: &Path, session_id: &str) -> bool {
    let Some(home) = home() else {
        return false;
    };
    valid_transcript_under(&home, path, session_id)
}

pub(super) fn latest_prompt(path: &Path, session_id: &str) -> Option<String> {
    let home = home()?;
    latest_prompt_under(&home, path, session_id)
}

pub(super) fn latest_prompt_under(home: &Path, path: &Path, session_id: &str) -> Option<String> {
    valid_transcript_under(home, path, session_id).then_some(())?;
    let lines = read_transcript_tail(path)?;
    fold(&lines).latest_prompt
}

pub(super) fn correlate_subagent(
    parent_transcript: &Path,
    parent_id: &str,
    parent_workspace: &Path,
    child_id: &str,
    child_workspace: &Path,
) -> Option<CorrelatedSubagent> {
    let home = home()?;
    correlate_subagent_under(
        &home,
        parent_transcript,
        parent_id,
        parent_workspace,
        child_id,
        child_workspace,
    )
}

pub(super) fn spawned_subagents(
    parent_transcript: &Path,
    parent_id: &str,
    parent_workspace: &Path,
) -> Vec<SpawnedSubagent> {
    let Some(home) = home() else {
        return Vec::new();
    };
    spawned_subagents_under(&home, parent_transcript, parent_id, parent_workspace)
}

pub(super) fn correlate_subagent_under(
    home: &Path,
    parent_transcript: &Path,
    parent_id: &str,
    parent_workspace: &Path,
    child_id: &str,
    child_workspace: &Path,
) -> Option<CorrelatedSubagent> {
    valid_conversation_id(child_id).then_some(())?;
    let child_workspace = fs::canonicalize(child_workspace).ok()?;
    let mut matched =
        validated_subagents_under(home, parent_transcript, parent_id, parent_workspace)?
            .into_iter()
            .filter(|pair| {
                pair.result.conversation_id.trim() == child_id
                    && pair.expected_workspace == child_workspace
            })
            .map(|pair| CorrelatedSubagent {
                type_name: pair.spec.type_name.trim().to_owned(),
                role: pair.spec.role.trim().to_owned(),
                prompt: pair.spec.prompt.trim().to_owned(),
            });
    let first = matched.next()?;
    matched.next().is_none().then_some(first)
}

pub(super) fn spawned_subagents_under(
    home: &Path,
    parent_transcript: &Path,
    parent_id: &str,
    parent_workspace: &Path,
) -> Vec<SpawnedSubagent> {
    validated_subagents_under(home, parent_transcript, parent_id, parent_workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|pair| SpawnedSubagent {
            child_agent_id: AgentSessionId::from(pair.result.conversation_id.trim()),
            agent_name: normalized(Some(&pair.spec.type_name)),
            role: normalized(Some(&pair.spec.role)),
            prompt: sanitize_user_prompt(Some(&pair.spec.prompt)),
            model: None,
            total_tokens: None,
        })
        .collect()
}

fn validated_subagents_under(
    home: &Path,
    parent_transcript: &Path,
    parent_id: &str,
    parent_workspace: &Path,
) -> Option<Vec<ValidatedSubagent>> {
    valid_transcript_under(home, parent_transcript, parent_id).then_some(())?;
    let parent_workspace = fs::canonicalize(parent_workspace).ok()?;
    let tail = read_transcript_tail_with_status(parent_transcript)?;
    (!tail.torn_suffix).then_some(())?;

    let mut pending = std::collections::VecDeque::<Vec<InvokeSubagentSpec>>::new();
    let mut seen_ids = std::collections::BTreeSet::<String>::new();
    let mut validated = Vec::new();
    for line in tail.text.lines().filter(|line| !line.trim().is_empty()) {
        let record = serde_json::from_str::<CorrelationRecord>(line).ok()?;
        if record.source == "MODEL"
            && record.record_type == "PLANNER_RESPONSE"
            && record.status == "DONE"
        {
            for call in record.tool_calls {
                if call.get("name").and_then(Value::as_str) != Some("invoke_subagent") {
                    continue;
                }
                let call = serde_json::from_value::<InvokeSubagentCall>(call).ok()?;
                validate_subagent_specs(&call.args.subagents)?;
                pending.push_back(call.args.subagents);
            }
            continue;
        }
        if record.source != "MODEL" || record.record_type != "INVOKE_SUBAGENT" {
            continue;
        }
        (record.status == "DONE").then_some(())?;
        let specs = pending.pop_front()?;
        let results = parse_invoke_subagent_results(record.content.as_deref()?, specs.len())?;
        for (spec, result) in specs.into_iter().zip(results) {
            let result_id = result.conversation_id.trim();
            valid_conversation_id(result_id).then_some(())?;
            seen_ids.insert(result_id.to_owned()).then_some(())?;
            let log_path = file_uri_path(&result.log_absolute_uri)?;
            valid_transcript_under(home, &log_path, result_id).then_some(())?;
            let expected_workspace = match spec.workspace.as_deref().map(str::trim) {
                None | Some("inherit") => parent_workspace.clone(),
                Some(path) => {
                    let path = Path::new(path);
                    path.is_absolute().then_some(())?;
                    fs::canonicalize(path).ok()?
                }
            };
            validate_result_workspaces(result.workspace_uris.as_deref(), &expected_workspace)?;
            validated.push(ValidatedSubagent {
                spec,
                result,
                expected_workspace,
            });
        }
    }
    pending.is_empty().then_some(())?;
    Some(validated)
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
    let lines = include_str!("tests/fixtures/transcript_full.jsonl");
    let folded = fold(lines);
    let created_at = folded.first_event_at.unwrap();
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("antigravity"),
        session_id: AgentSessionId::from("11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from("/provider/brain/11111111/transcript_full.jsonl"),
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: Some(created_at),
        last_activity: folded.last_activity.unwrap(),
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status: folded.status,
            phase: folded.phase,
            latest_prompt: folded.latest_prompt,
            native_prompt_detail: folded.native_prompt_detail,
            waiting_since: folded.waiting_since,
            context_pct: None,
        }),
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

#[cfg(test)]
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

#[cfg(test)]
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
            transcript_for_session_under(home, &id)
        })
        .collect()
}

fn transcript_for_session_under(home: &Path, session_id: &str) -> Option<PathBuf> {
    valid_conversation_id(session_id).then_some(())?;
    TRANSCRIPT_BASENAMES.iter().find_map(|basename| {
        let path = transcript_path(home, session_id, basename);
        valid_transcript_under(home, &path, session_id).then_some(path)
    })
}

fn transcript_path(home: &Path, session_id: &str, basename: &str) -> PathBuf {
    home.join("brain")
        .join(session_id)
        .join(".system_generated/logs")
        .join(basename)
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
        && TRANSCRIPT_BASENAMES.iter().any(|basename| {
            canonical_path
                == canonical_session
                    .join(".system_generated/logs")
                    .join(basename)
        })
}

#[cfg(test)]
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
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
            status: folded.status,
            phase: folded.phase,
            latest_prompt: folded.latest_prompt,
            native_prompt_detail: folded.native_prompt_detail,
            waiting_since: folded.waiting_since,
            context_pct: None,
        }),
    })
}

fn fold(lines: &str) -> FoldedSession {
    let mut folded = FoldedSession {
        first_event_at: None,
        last_activity: None,
        status: AgentStatus::Idle,
        phase: TurnPhase::Idle,
        latest_prompt: None,
        native_prompt_detail: None,
        waiting_since: None,
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
                folded.native_prompt_detail = None;
                folded.waiting_since = None;
                folded.status = AgentStatus::Running;
                folded.phase = TurnPhase::Reasoning;
                folded.latest_prompt = normalize_user_content(record.content.as_deref());
            }
            TranscriptRole::Assistant => {
                if let Some(question) = native_question(&record) {
                    folded.status = AgentStatus::Waiting;
                    folded.phase = TurnPhase::Idle;
                    folded.native_prompt_detail = Some(question);
                    folded.waiting_since = Some(at);
                } else {
                    folded.status = AgentStatus::Success;
                    folded.phase = TurnPhase::Idle;
                    folded.native_prompt_detail = None;
                    folded.waiting_since = None;
                }
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

fn validate_subagent_specs(specs: &[InvokeSubagentSpec]) -> Option<()> {
    (!specs.is_empty()).then_some(())?;
    specs
        .iter()
        .all(|spec| {
            !(spec.prompt.trim().is_empty() && spec.role.trim().is_empty())
                && spec
                    .workspace
                    .as_deref()
                    .is_none_or(|workspace| !workspace.trim().is_empty())
        })
        .then_some(())
}

fn parse_invoke_subagent_results(
    content: &str,
    expected: usize,
) -> Option<Vec<InvokeSubagentResult>> {
    (expected > 0).then_some(())?;
    let start = content.find('{')?;
    let objects = &content[start..];
    let mut stream =
        serde_json::Deserializer::from_str(objects).into_iter::<InvokeSubagentResult>();
    let mut results = Vec::with_capacity(expected);
    for _ in 0..expected {
        results.push(stream.next()?.ok()?);
    }
    let trailing = objects.get(stream.byte_offset()..)?.trim_start();
    (!trailing.contains('{')).then_some(())?;
    Some(results)
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let uri = Url::parse(uri.trim()).ok()?;
    (uri.scheme() == "file").then_some(())?;
    uri.to_file_path().ok()
}

fn validate_result_workspaces(uris: Option<&[String]>, expected: &Path) -> Option<()> {
    let Some(uris) = uris else {
        return Some(());
    };
    (!uris.is_empty()).then_some(())?;
    uris.iter()
        .map(|uri| file_uri_path(uri).and_then(|path| fs::canonicalize(path).ok()))
        .collect::<Option<Vec<_>>>()?
        .iter()
        .any(|workspace| workspace == expected)
        .then_some(())
}

fn normalize_user_content(value: Option<&str>) -> Option<String> {
    const OPEN: &str = "<USER_REQUEST>";
    const CLOSE: &str = "</USER_REQUEST>";

    let value = value?.trim();
    let request = match value.strip_prefix(OPEN) {
        Some(wrapped) => wrapped.split_once(CLOSE)?.0,
        None => value,
    };
    sanitize_user_prompt(Some(request))
}

fn native_question(record: &TranscriptRecord) -> Option<String> {
    record
        .tool_calls
        .iter()
        .filter(|call| call.name == "ask_question")
        .flat_map(|call| &call.args.questions)
        .map(|question| question.question.trim())
        .find(|question| !question.is_empty())
        .map(ToOwned::to_owned)
}

fn deserialize_tool_calls<'de, D>(deserializer: D) -> Result<Vec<ToolCall>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|call| serde_json::from_value(call.clone()).ok())
        .collect())
}

fn deserialize_questions<'de, D>(deserializer: D) -> Result<Vec<ToolQuestion>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let value = match value {
        Value::String(encoded) => serde_json::from_str(&encoded).unwrap_or(Value::Null),
        value => value,
    };
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|question| serde_json::from_value::<ToolQuestion>(question.clone()).ok())
        .filter(|question| !question.question.trim().is_empty())
        .collect())
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
