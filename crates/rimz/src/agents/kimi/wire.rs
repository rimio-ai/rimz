//! Kimi Code's flat durable agent-record log and session-index lookup.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::agents::transcript_fs::{home_dir, read_spend_lines};

#[derive(Clone, Debug)]
pub struct WireRecord {
    pub time: Option<f64>,
    pub event: WireEvent,
}

impl WireRecord {
    pub fn timestamp(&self) -> Option<Timestamp> {
        let millis = self.time?.trunc();
        if millis > i64::MAX as f64 {
            return None;
        }
        Timestamp::from_millisecond(millis as i64).ok()
    }
}

#[derive(Clone, Debug)]
pub enum WireEvent {
    Metadata,
    Prompt {
        kind: PromptKind,
        prompt: PromptRecord,
    },
    ConfigUpdate(ConfigUpdate),
    LlmRequest(RequestAttribution),
    Usage(UsageRecord),
    AppendMessage(AppendedMessage),
    AppendLoopEvent(LoopEvent),
    ContextClear,
    ApplyCompaction {
        tokens_after: Option<u64>,
    },
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Prompt,
    Steer,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TokenUsage {
    #[serde(rename = "inputOther", alias = "input_other")]
    pub input_other: Option<u64>,
    pub output: Option<u64>,
    #[serde(rename = "inputCacheRead", alias = "input_cache_read")]
    pub input_cache_read: Option<u64>,
    #[serde(rename = "inputCacheCreation", alias = "input_cache_creation")]
    pub input_cache_creation: Option<u64>,
}

impl TokenUsage {
    pub fn input_total(&self) -> u64 {
        self.input_other.unwrap_or(0)
            + self.input_cache_read.unwrap_or(0)
            + self.input_cache_creation.unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        self.input_total().saturating_add(self.output.unwrap_or(0))
    }

    pub fn is_zero(&self) -> bool {
        self.total() == 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UsageScope {
    Turn,
    #[default]
    Session,
    Other,
}

impl<'de> Deserialize<'de> for UsageScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(
            match Option::<String>::deserialize(deserializer)?.as_deref() {
                Some("turn") => Self::Turn,
                None | Some("session") => Self::Session,
                Some(_) => Self::Other,
            },
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UsageRecord {
    pub model: String,
    pub usage: TokenUsage,
    #[serde(rename = "usageScope", alias = "scope")]
    pub scope: UsageScope,
}

impl UsageRecord {
    pub fn is_turn_scoped(&self) -> bool {
        self.scope == UsageScope::Turn
    }
}

#[derive(Clone, Debug, Default)]
pub struct PromptRecord {
    pub input: Vec<ContentPart>,
    pub origin: PromptOrigin,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptOrigin {
    User,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default)]
pub enum ContentPart {
    Text(String),
    #[default]
    Other,
}

impl ContentPart {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Other => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum LoopEvent {
    StepBegin {
        id: Option<String>,
    },
    ContentPart {
        step_id: Option<String>,
        part: ContentPart,
    },
    StepEnd {
        id: Option<String>,
        usage: Option<TokenUsage>,
    },
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct AppendedMessage {
    pub role: MessageRole,
    pub content: MessageContent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MessageRole {
    Assistant,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RequestAttribution {
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "modelAlias")]
    pub model_alias: Option<String>,
    #[serde(rename = "thinkingEffort")]
    pub thinking_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ConfigUpdate {
    #[serde(rename = "modelAlias")]
    pub model_alias: Option<String>,
    #[serde(rename = "thinkingEffort")]
    pub thinking_effort: Option<String>,
    #[serde(rename = "profileName")]
    pub profile_name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct EffectiveAttribution {
    pub request: Option<RequestAttribution>,
    pub model_alias: Option<String>,
    pub thinking_effort: Option<String>,
}

impl EffectiveAttribution {
    pub fn observe(&mut self, record: &WireRecord) {
        match &record.event {
            WireEvent::ConfigUpdate(config) => {
                if let Some(alias) = non_empty(config.model_alias.clone()) {
                    self.model_alias = Some(normalize_model_alias(&alias));
                }
                if let Some(effort) = non_empty(config.thinking_effort.clone()) {
                    self.thinking_effort = Some(effort);
                }
            }
            WireEvent::LlmRequest(request) => {
                let mut request = request.clone();
                request.provider = non_empty(request.provider);
                request.model = non_empty(request.model);
                request.model_alias =
                    non_empty(request.model_alias).map(|alias| normalize_model_alias(&alias));
                request.thinking_effort = non_empty(request.thinking_effort);
                if request.model_alias.is_some() {
                    self.model_alias = request.model_alias.clone();
                }
                if request.thinking_effort.is_some() {
                    self.thinking_effort = request.thinking_effort.clone();
                }
                self.request = Some(request);
            }
            _ => {}
        }
    }

    pub fn display_model(&self) -> Option<String> {
        self.model_alias.clone().or_else(|| {
            self.request
                .as_ref()
                .and_then(|request| request.model.clone())
        })
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub fn normalize_model_alias(alias: &str) -> String {
    alias
        .trim()
        .strip_prefix("kimi-code/")
        .unwrap_or(alias.trim())
        .to_owned()
}

#[derive(Debug, Deserialize)]
struct SessionIndexEntry {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "sessionDir")]
    session_dir: PathBuf,
    #[serde(rename = "workDir")]
    _work_dir: PathBuf,
}

pub fn kimi_home() -> PathBuf {
    std::env::var_os("KIMI_CODE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".kimi-code"))
}

pub fn session_dir(session_id: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    session_dir_under(&kimi_home(), session_id, cwd)
}

pub(crate) fn session_dir_under(
    root: &Path,
    session_id: &str,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let indexed = std::fs::read_to_string(root.join("session_index.jsonl"))
        .ok()
        .into_iter()
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<SessionIndexEntry>(&line).ok())
        .filter(|entry| entry.session_id == session_id)
        .filter_map(|entry| validate_session_dir(root, session_id, &entry.session_dir, cwd))
        .next_back();
    indexed.or_else(|| scan_session_dirs(root, session_id, cwd))
}

fn validate_session_dir(
    root: &Path,
    session_id: &str,
    candidate: &Path,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    let sessions = std::fs::canonicalize(root.join("sessions")).ok()?;
    let candidate = std::fs::canonicalize(candidate).ok()?;
    if !candidate.starts_with(&sessions)
        || candidate.file_name().and_then(|name| name.to_str()) != Some(session_id)
    {
        return None;
    }
    let state: Value =
        serde_json::from_slice(&std::fs::read(candidate.join("state.json")).ok()?).ok()?;
    state.as_object()?;
    if let (Some(expected), Some(recorded)) = (
        cwd,
        state.get("workDir").and_then(Value::as_str).map(Path::new),
    ) && !paths_match(expected, recorded)
    {
        return None;
    }
    Some(candidate)
}

fn paths_match(left: &Path, right: &Path) -> bool {
    left == right
        || std::fs::canonicalize(left)
            .ok()
            .zip(std::fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn scan_session_dirs(root: &Path, session_id: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let mut matches = std::fs::read_dir(root.join("sessions"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            validate_session_dir(root, session_id, &entry.path().join(session_id), cwd)
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        session_modified(left)
            .cmp(&session_modified(right))
            .then_with(|| left.cmp(right))
    });
    matches.pop()
}

fn session_modified(path: &Path) -> Option<std::time::SystemTime> {
    [
        path,
        &path.join("agents/main/wire.jsonl"),
        &path.join("state.json"),
    ]
    .into_iter()
    .filter_map(|candidate| std::fs::metadata(candidate).ok()?.modified().ok())
    .max()
}

pub fn wire_path(session_id: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    Some(session_dir(session_id, cwd)?.join("agents/main/wire.jsonl"))
}

#[derive(Deserialize)]
struct RawWireRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "optional_number")]
    time: Option<f64>,
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

fn optional_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Value>::deserialize(deserializer)?
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite() && *value > 0.0))
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawPromptRecord {
    input: Vec<RawContentPart>,
    origin: Value,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawLoopEvent {
    #[serde(rename = "type")]
    kind: String,
    uuid: Option<String>,
    #[serde(rename = "stepUuid")]
    step_uuid: Option<String>,
    part: Option<RawContentPart>,
    usage: Option<TokenUsage>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawAppendedMessage {
    role: String,
    content: Value,
}

pub fn records_from_bytes(bytes: &[u8]) -> Vec<WireRecord> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(record_from_slice)
        .filter(|record| !matches!(record.event, WireEvent::Metadata))
        .collect()
}

/// One torn-write-safe full read of a live Kimi wire. The full record set owns
/// cumulative spend while the logical tail preserves bounded context semantics.
#[derive(Debug)]
pub struct WireSnapshot {
    records: Vec<WireRecord>,
    tail_start: usize,
    consumed_offset: u64,
}

impl WireSnapshot {
    pub fn read(path: &Path) -> Option<Self> {
        let (bytes, consumed_offset) = read_spend_lines(path, 0)?;
        let tail_byte_start = record_aligned_tail_start(&bytes);
        let mut records = Vec::new();
        let mut tail_start = 0;
        let mut offset = 0;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let record_start = offset;
            offset += line.len();
            let Some(record) = record_from_slice(line.strip_suffix(b"\n").unwrap_or(line)) else {
                continue;
            };
            if matches!(record.event, WireEvent::Metadata) {
                continue;
            }
            if record_start < tail_byte_start {
                tail_start += 1;
            }
            records.push(record);
        }
        Some(Self {
            records,
            tail_start,
            consumed_offset,
        })
    }

    pub fn records(&self) -> &[WireRecord] {
        &self.records
    }

    pub fn tail_records(&self) -> &[WireRecord] {
        &self.records[self.tail_start..]
    }

    pub fn consumed_offset(&self) -> u64 {
        self.consumed_offset
    }
}

fn record_aligned_tail_start(bytes: &[u8]) -> usize {
    const TAIL_BYTES: usize = 64 * 1024;
    let normal_start = bytes.len().saturating_sub(TAIL_BYTES);
    if normal_start == 0 || bytes.get(normal_start.wrapping_sub(1)) == Some(&b'\n') {
        return normal_start;
    }
    if let Some(newline) = bytes[normal_start..].iter().position(|byte| *byte == b'\n') {
        let candidate = normal_start + newline + 1;
        if candidate < bytes.len() {
            return candidate;
        }
    }
    bytes[..normal_start]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1)
}

pub(super) fn records_from_str(input: &str) -> Vec<WireRecord> {
    input
        .lines()
        .filter_map(|line| record_from_slice(line.as_bytes()))
        .filter(|record| !matches!(record.event, WireEvent::Metadata))
        .collect()
}

fn record_from_slice(line: &[u8]) -> Option<WireRecord> {
    let raw = serde_json::from_slice::<RawWireRecord>(line).ok()?;
    Some(WireRecord {
        time: raw.time,
        event: decode_event(&raw.kind, raw.fields),
    })
}

fn decode_event(kind: &str, mut fields: serde_json::Map<String, Value>) -> WireEvent {
    match kind {
        "metadata" => WireEvent::Metadata,
        "turn.prompt" | "turn.steer" => from_fields::<RawPromptRecord>(fields)
            .map(|raw| WireEvent::Prompt {
                kind: if kind == "turn.prompt" {
                    PromptKind::Prompt
                } else {
                    PromptKind::Steer
                },
                prompt: PromptRecord {
                    input: raw.input.into_iter().map(typed_content_part).collect(),
                    origin: prompt_origin(&raw.origin),
                },
            })
            .unwrap_or(WireEvent::Unknown),
        "config.update" => from_fields(fields)
            .map(WireEvent::ConfigUpdate)
            .unwrap_or(WireEvent::Unknown),
        "llm.request" => from_fields(fields)
            .map(WireEvent::LlmRequest)
            .unwrap_or(WireEvent::Unknown),
        "usage.record" => from_fields(fields)
            .map(WireEvent::Usage)
            .unwrap_or(WireEvent::Unknown),
        "context.append_message" => fields
            .remove("message")
            .and_then(|message| serde_json::from_value::<RawAppendedMessage>(message).ok())
            .map(|message| {
                WireEvent::AppendMessage(AppendedMessage {
                    role: if message.role == "assistant" {
                        MessageRole::Assistant
                    } else {
                        MessageRole::Other
                    },
                    content: message_content(message.content),
                })
            })
            .unwrap_or(WireEvent::Unknown),
        "context.append_loop_event" => fields
            .remove("event")
            .map(loop_event)
            .map(WireEvent::AppendLoopEvent)
            .unwrap_or(WireEvent::Unknown),
        "context.clear" => WireEvent::ContextClear,
        "context.apply_compaction" => WireEvent::ApplyCompaction {
            tokens_after: fields
                .remove("tokensAfter")
                .and_then(|value| value.as_u64()),
        },
        _ => WireEvent::Unknown,
    }
}

fn from_fields<T: for<'de> Deserialize<'de>>(fields: serde_json::Map<String, Value>) -> Option<T> {
    serde_json::from_value(Value::Object(fields)).ok()
}

fn prompt_origin(value: &Value) -> PromptOrigin {
    if value.get("kind").and_then(Value::as_str) == Some("user") {
        PromptOrigin::User
    } else {
        PromptOrigin::Other
    }
}

fn content_part(value: Value) -> ContentPart {
    if value.get("type").and_then(Value::as_str) == Some("text")
        && let Some(text) = value.get("text").and_then(Value::as_str)
    {
        ContentPart::Text(text.to_owned())
    } else {
        ContentPart::Other
    }
}

fn typed_content_part(part: RawContentPart) -> ContentPart {
    if part.kind == "text"
        && let Some(text) = part.text
    {
        ContentPart::Text(text)
    } else {
        ContentPart::Other
    }
}

fn message_content(value: Value) -> MessageContent {
    match value {
        Value::String(text) => MessageContent::Text(text),
        Value::Array(parts) => MessageContent::Parts(parts.into_iter().map(content_part).collect()),
        _ => MessageContent::Other,
    }
}

fn loop_event(value: Value) -> LoopEvent {
    let Ok(event) = serde_json::from_value::<RawLoopEvent>(value) else {
        return LoopEvent::Other;
    };
    match event.kind.as_str() {
        "step.begin" => LoopEvent::StepBegin { id: event.uuid },
        "content.part" => LoopEvent::ContentPart {
            step_id: event.step_uuid.or(event.uuid),
            part: event.part.map(typed_content_part).unwrap_or_default(),
        },
        "step.end" => LoopEvent::StepEnd {
            id: event.uuid,
            usage: event.usage,
        },
        _ => LoopEvent::Other,
    }
}

pub fn read_records(path: &Path, offset: u64) -> Option<(Vec<WireRecord>, u64)> {
    let (bytes, next) = read_spend_lines(path, offset)?;
    Some((records_from_bytes(&bytes), next))
}

pub fn usage_records(records: &[WireRecord]) -> Vec<(Option<f64>, UsageRecord)> {
    records
        .iter()
        .filter_map(|record| match &record.event {
            WireEvent::Usage(usage) => Some((record.time, usage.clone())),
            _ => None,
        })
        .collect()
}

pub fn latest_context_tokens(records: &[WireRecord]) -> Option<u64> {
    records
        .iter()
        .fold(None, |latest, record| match &record.event {
            WireEvent::AppendLoopEvent(LoopEvent::StepEnd {
                usage: Some(usage), ..
            }) if !usage.is_zero() => Some(usage.total()),
            WireEvent::ContextClear => Some(0),
            WireEvent::ApplyCompaction { tokens_after } => (*tokens_after).or(latest),
            _ => latest,
        })
}

pub fn latest_turn_usage(records: &[WireRecord]) -> Option<UsageRecord> {
    records.iter().rev().find_map(|record| match &record.event {
        WireEvent::Usage(usage) if usage.is_turn_scoped() => Some(usage.clone()),
        _ => None,
    })
}

pub fn effective_attribution(records: &[WireRecord]) -> EffectiveAttribution {
    records.iter().fold(
        EffectiveAttribution::default(),
        |mut attribution, record| {
            attribution.observe(record);
            attribution
        },
    )
}
