//! Borrowed wire decoder for interactive Codex rollout JSONL.
//!
//! One lossy schema owner normalizes headers, visible messages, turn context,
//! token usage, and terminal facts for transcript, discovery, and spend consumers.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::agents::transcript_fs::deserialize_optional_object_lossy;
use crate::agents::{TranscriptMessage, TranscriptRole};

const MAX_ROLLOUT_HEADER_BYTES: u64 = 1024 * 1024;

/// Normalized identity and child metadata from a rollout's `session_meta`
/// header. Hooks remain lifecycle authority; every field here is optional
/// enrichment except `is_subagent`, which local root discovery uses to reject
/// positively identified child rollouts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CodexRolloutHeader {
    pub(super) session_id: Option<String>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) timestamp: Option<Timestamp>,
    pub(super) forked_from_id: Option<String>,
    pub(super) is_subagent: bool,
    pub(super) parent_thread_id: Option<String>,
    pub(super) depth: Option<u32>,
    pub(super) agent_nickname: Option<String>,
    pub(super) agent_path: Option<String>,
    pub(super) agent_role: Option<String>,
    pub(super) multi_agent_version: Option<String>,
}

/// Read and normalize one rollout header without scanning its body.
pub(super) fn read_rollout_header(path: &Path) -> Option<CodexRolloutHeader> {
    let file = File::open(path).ok()?;
    let mut line = Vec::new();
    BufReader::new(file)
        .take(MAX_ROLLOUT_HEADER_BYTES)
        .read_until(b'\n', &mut line)
        .ok()?;
    let record = decode_line(trim_ascii(&line))?;
    let RolloutKind::SessionMeta(payload) = record.kind else {
        return None;
    };
    let spawn = match payload.source.as_ref() {
        Some(CodexSessionSource::Structured(source)) => source
            .subagent
            .as_ref()
            .and_then(|subagent| subagent.thread_spawn.as_ref()),
        Some(CodexSessionSource::Name(name)) => {
            let _ = name;
            None
        }
        Some(CodexSessionSource::Other(value)) => {
            let _ = value;
            None
        }
        None => None,
    };
    let is_subagent = payload
        .thread_source
        .as_deref()
        .is_some_and(|source| source.trim().eq_ignore_ascii_case("subagent"))
        || spawn.is_some();
    Some(CodexRolloutHeader {
        session_id: owned_non_empty(payload.id.as_deref()),
        cwd: owned_non_empty(payload.cwd.as_deref()).map(PathBuf::from),
        timestamp: codex_timestamp(record.timestamp.as_ref()),
        forked_from_id: owned_non_empty(payload.forked_from_id.as_deref()),
        is_subagent,
        parent_thread_id: owned_non_empty(payload.parent_thread_id.as_deref())
            .or_else(|| spawn.and_then(|spawn| owned_non_empty(spawn.parent_thread_id.as_deref()))),
        depth: spawn.and_then(|spawn| spawn.depth),
        agent_nickname: owned_non_empty(payload.agent_nickname.as_deref())
            .or_else(|| spawn.and_then(|spawn| owned_non_empty(spawn.agent_nickname.as_deref()))),
        agent_path: owned_non_empty(payload.agent_path.as_deref())
            .or_else(|| spawn.and_then(|spawn| owned_non_empty(spawn.agent_path.as_deref())))
            .and_then(|path| normalize_agent_path(&path)),
        agent_role: owned_non_empty(payload.agent_role.as_deref())
            .or_else(|| spawn.and_then(|spawn| owned_non_empty(spawn.agent_role.as_deref()))),
        multi_agent_version: owned_non_empty(payload.multi_agent_version.as_deref()),
    })
}

/// Parse visible user and assistant messages from rollout JSONL.
pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| {
            let record = decode_line(line.trim().as_bytes())?;
            let role = match record.kind {
                RolloutKind::UserMessage => TranscriptRole::User,
                RolloutKind::AgentMessage => TranscriptRole::Assistant,
                _ => return None,
            };
            let text = record
                .message
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())?
                .to_owned();
            Some(TranscriptMessage {
                role,
                at: record.event_timestamp(),
                text,
            })
        })
        .collect()
}

/// One decoded interactive rollout line. Valid unknown shapes produce
/// [`RolloutKind::Other`] so consumers stay forward-compatible.
pub(crate) struct RolloutRecord<'a> {
    pub(crate) timestamp: Option<CodexTimestamp<'a>>,
    pub(crate) kind: RolloutKind<'a>,
    pub(crate) error: Option<RolloutError<'a>>,
    pub(crate) message: Option<Cow<'a, str>>,
}

impl RolloutRecord<'_> {
    pub(crate) fn proves_recovery(&self) -> bool {
        matches!(
            self.kind,
            RolloutKind::TurnContext(_)
                | RolloutKind::AgentMessage
                | RolloutKind::TaskStarted
                | RolloutKind::UserMessage
        )
    }

    /// Event timestamps historically accept only provider RFC-3339 strings.
    pub(crate) fn event_timestamp(&self) -> Option<Timestamp> {
        match self.timestamp.as_ref()? {
            CodexTimestamp::String(raw) => raw.trim().parse().ok(),
            CodexTimestamp::Number(_) => None,
        }
    }
}

pub(crate) enum RolloutKind<'a> {
    SessionMeta(CodexSessionMetaPayload<'a>),
    TurnContext(CodexTurnContext<'a>),
    TokenCount(CodexTokenCount<'a>),
    UserMessage,
    AgentMessage,
    TaskStarted,
    TurnAborted,
    TaskComplete(CodexTaskComplete<'a>),
    ItemCompleted(CodexItemCompleted<'a>),
    Other,
}

pub(crate) struct CodexTurnContext<'a> {
    model: ModelFields<'a>,
    effort: Option<Cow<'a, str>>,
}

impl CodexTurnContext<'_> {
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.value()
    }

    pub(crate) fn effort(&self) -> Option<&str> {
        non_empty(self.effort.as_deref())
    }
}

pub(crate) struct CodexTokenCount<'a> {
    model: ModelFields<'a>,
    info: Option<CodexUsageInfo<'a>>,
}

impl CodexTokenCount<'_> {
    pub(crate) fn info(&self) -> Option<&CodexUsageInfo<'_>> {
        self.info.as_ref()
    }

    pub(crate) fn model(&self) -> Option<&str> {
        self.model
            .value()
            .or_else(|| self.info.as_ref().and_then(CodexUsageInfo::model))
    }
}

pub(crate) struct CodexTaskComplete<'a> {
    pub(crate) turn_id: Option<Cow<'a, str>>,
    pub(crate) last_agent_message: Option<Cow<'a, str>>,
    pub(crate) error_field_present: bool,
}

pub(crate) struct CodexItemCompleted<'a> {
    pub(crate) turn_id: Option<Cow<'a, str>>,
    pub(crate) plan_text: Option<Cow<'a, str>>,
}

pub(crate) struct RolloutError<'a> {
    pub(crate) label: Option<Cow<'a, str>>,
    pub(crate) kinds: Vec<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
pub(crate) struct CodexUsageInfo<'a> {
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub(crate) last_token_usage: Option<CodexRawUsage>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub(crate) total_token_usage: Option<CodexRawUsage>,
    #[serde(default)]
    pub(crate) model_context_window: Option<u64>,
    #[serde(flatten, borrow)]
    model: ModelFields<'a>,
}

impl CodexUsageInfo<'_> {
    fn model(&self) -> Option<&str> {
        self.model.value()
    }
}

/// Decode one complete JSONL line without rejecting future entry or payload
/// kinds. Malformed JSON remains absent so torn tail lines are skipped.
pub(crate) fn decode_line(line: &[u8]) -> Option<RolloutRecord<'_>> {
    let envelope = serde_json::from_slice::<RolloutEnvelope<'_>>(line).ok()?;
    let timestamp = envelope.timestamp;
    let top_level_error = schema_error(envelope.error.raw());
    let Some(entry_type) = envelope.entry_type.as_deref() else {
        return Some(RolloutRecord {
            timestamp,
            kind: RolloutKind::Other,
            error: top_level_error,
            message: None,
        });
    };
    match entry_type {
        "session_meta" => {
            let payload = parse_raw::<CodexSessionMetaPayload<'_>>(envelope.payload.raw())?;
            Some(RolloutRecord {
                timestamp,
                kind: RolloutKind::SessionMeta(payload),
                error: top_level_error,
                message: None,
            })
        }
        "turn_context" => {
            let payload =
                parse_raw::<TurnContextPayload<'_>>(envelope.payload.raw()).unwrap_or_default();
            Some(RolloutRecord {
                timestamp,
                kind: RolloutKind::TurnContext(CodexTurnContext {
                    model: payload.model,
                    effort: payload
                        .model_reasoning_effort
                        .or(payload.reasoning_effort)
                        .or(payload.effort),
                }),
                error: top_level_error,
                message: None,
            })
        }
        "event_msg" => decode_event(timestamp, envelope.payload.raw()),
        _ => Some(RolloutRecord {
            timestamp,
            kind: RolloutKind::Other,
            error: top_level_error,
            message: None,
        }),
    }
}

fn decode_event<'a>(
    timestamp: Option<CodexTimestamp<'a>>,
    raw: Option<&'a RawValue>,
) -> Option<RolloutRecord<'a>> {
    let payload = parse_raw::<EventPayload<'a>>(raw)?;
    let payload_type = payload.payload_type.as_deref();
    let error = match payload_type {
        Some("stream_error" | "turn_error" | "error") => Some(payload.error_facts()),
        Some("task_complete") if error_is_present(payload.error.raw()) => {
            Some(payload.error_facts())
        }
        _ => schema_error(payload.error.raw()),
    };
    let message = payload.message.clone();
    let kind = match payload_type {
        Some("token_count") => RolloutKind::TokenCount(CodexTokenCount {
            model: payload.model,
            info: parse_raw(payload.info.raw()),
        }),
        Some("user_message") => RolloutKind::UserMessage,
        Some("agent_message") => RolloutKind::AgentMessage,
        Some("task_started") => RolloutKind::TaskStarted,
        Some("turn_aborted") => RolloutKind::TurnAborted,
        Some("task_complete") => RolloutKind::TaskComplete(CodexTaskComplete {
            turn_id: payload.turn_id,
            last_agent_message: payload.last_agent_message,
            error_field_present: payload.error.present(),
        }),
        Some("item_completed") => RolloutKind::ItemCompleted(CodexItemCompleted {
            turn_id: payload.turn_id,
            plan_text: parse_raw::<CompletedItem<'_>>(payload.item.raw()).and_then(|item| {
                (item.item_type.as_deref() == Some("Plan"))
                    .then_some(item.text)
                    .flatten()
            }),
        }),
        _ => RolloutKind::Other,
    };
    Some(RolloutRecord {
        timestamp,
        kind,
        error,
        message,
    })
}

#[derive(Deserialize)]
struct RolloutEnvelope<'a> {
    #[serde(
        rename = "type",
        borrow,
        default,
        deserialize_with = "deserialize_optional_cow_str"
    )]
    entry_type: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    timestamp: Option<CodexTimestamp<'a>>,
    #[serde(borrow, default)]
    payload: RawField<'a>,
    #[serde(borrow, default)]
    error: RawField<'a>,
}

#[derive(Default, Deserialize)]
struct TurnContextPayload<'a> {
    #[serde(flatten, borrow)]
    model: ModelFields<'a>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    model_reasoning_effort: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    reasoning_effort: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    effort: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
struct EventPayload<'a> {
    #[serde(
        rename = "type",
        borrow,
        default,
        deserialize_with = "deserialize_optional_cow_str"
    )]
    payload_type: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    message: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    error_message: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    last_agent_message: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    turn_id: Option<Cow<'a, str>>,
    #[serde(flatten, borrow)]
    model: ModelFields<'a>,
    #[serde(borrow, default)]
    info: RawField<'a>,
    #[serde(borrow, default)]
    item: RawField<'a>,
    #[serde(borrow, default)]
    error: RawField<'a>,
    #[serde(rename = "codexErrorInfo", borrow, default)]
    codex_error_info_camel: RawField<'a>,
    #[serde(borrow, default)]
    codex_error_info: RawField<'a>,
}

impl<'a> EventPayload<'a> {
    fn error_facts(&self) -> RolloutError<'a> {
        let nested = parse_raw::<ErrorObject<'a>>(self.error.raw());
        let label = self
            .message
            .clone()
            .or_else(|| self.error_message.clone())
            .or_else(|| {
                nested
                    .as_ref()
                    .and_then(|error| raw_string(error.message.raw()))
            })
            .or_else(|| raw_string(self.error.raw()))
            .or_else(|| self.last_agent_message.clone());
        let info = self
            .codex_error_info_camel
            .raw()
            .or_else(|| self.codex_error_info.raw())
            .or_else(|| nested.as_ref().and_then(ErrorObject::error_info));
        RolloutError {
            label,
            kinds: error_kinds(info),
        }
    }
}

#[derive(Default, Deserialize)]
struct CompletedItem<'a> {
    #[serde(
        rename = "type",
        borrow,
        default,
        deserialize_with = "deserialize_optional_cow_str"
    )]
    item_type: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    text: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
pub(crate) struct CodexSessionMetaPayload<'a> {
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    pub(crate) id: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    pub(crate) cwd: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    forked_from_id: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    thread_source: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    parent_thread_id: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    agent_nickname: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    agent_path: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    agent_role: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    multi_agent_version: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    source: Option<CodexSessionSource<'a>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CodexSessionSource<'a> {
    Name(#[serde(borrow, deserialize_with = "deserialize_cow_str")] Cow<'a, str>),
    Structured(#[serde(borrow)] CodexStructuredSessionSource<'a>),
    Other(IgnoredAny),
}

#[derive(Default, Deserialize)]
struct CodexStructuredSessionSource<'a> {
    #[serde(borrow, default)]
    subagent: Option<CodexSubagentSource<'a>>,
}

#[derive(Default, Deserialize)]
struct CodexSubagentSource<'a> {
    #[serde(borrow, default)]
    thread_spawn: Option<CodexThreadSpawnSource<'a>>,
}

#[derive(Default, Deserialize)]
struct CodexThreadSpawnSource<'a> {
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    parent_thread_id: Option<Cow<'a, str>>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    agent_path: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    agent_nickname: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    agent_role: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
struct ModelFields<'a> {
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    model: Option<Cow<'a, str>>,
    #[serde(
        rename = "model_name",
        borrow,
        default,
        deserialize_with = "deserialize_optional_cow_str"
    )]
    model_name: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    metadata: Option<CodexModelMetadata<'a>>,
}

impl ModelFields<'_> {
    fn value(&self) -> Option<&str> {
        non_empty(self.model.as_deref())
            .or_else(|| non_empty(self.model_name.as_deref()))
            .or_else(|| {
                self.metadata
                    .as_ref()
                    .and_then(|metadata| non_empty(metadata.model.as_deref()))
            })
    }
}

#[derive(Default, Deserialize)]
pub(crate) struct CodexModelMetadata<'a> {
    #[serde(borrow, default, deserialize_with = "deserialize_optional_cow_str")]
    pub(crate) model: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
struct ErrorObject<'a> {
    #[serde(borrow, default)]
    message: RawField<'a>,
    #[serde(rename = "codexErrorInfo", borrow, default)]
    codex_error_info_camel: RawField<'a>,
    #[serde(borrow, default)]
    codex_error_info: RawField<'a>,
    #[serde(flatten, borrow)]
    extra: BTreeMap<Cow<'a, str>, IgnoredAny>,
}

impl<'a> ErrorObject<'a> {
    fn error_info(&self) -> Option<&'a RawValue> {
        self.codex_error_info_camel
            .raw()
            .or_else(|| self.codex_error_info.raw())
    }

    fn is_empty(&self) -> bool {
        !self.message.present()
            && !self.codex_error_info_camel.present()
            && !self.codex_error_info.present()
            && self.extra.is_empty()
    }
}

#[derive(Default)]
struct RawField<'a>(Option<&'a RawValue>);

impl<'a> RawField<'a> {
    fn raw(&self) -> Option<&'a RawValue> {
        self.0
    }

    fn present(&self) -> bool {
        self.0.is_some()
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawField<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        <&'de RawValue>::deserialize(deserializer).map(|raw| Self(Some(raw)))
    }
}

/// Codex timestamps appear as ISO-8601 strings or Unix seconds/milliseconds.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum CodexTimestamp<'a> {
    String(#[serde(borrow, deserialize_with = "deserialize_cow_str")] Cow<'a, str>),
    Number(u64),
}

/// Raw token counts with aliases normalized at deserialization time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct CodexRawUsage {
    pub(crate) input_tokens: u64,
    pub(crate) cached_input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) reasoning_output_tokens: u64,
    pub(crate) total_tokens: u64,
    #[serde(skip)]
    pub(crate) reported: u8,
}

const INPUT_REPORTED: u8 = 1 << 0;
const CACHED_REPORTED: u8 = 1 << 1;
const OUTPUT_REPORTED: u8 = 1 << 2;
const TOTAL_REPORTED: u8 = 1 << 3;

impl CodexRawUsage {
    pub(crate) fn input_reported(self) -> bool {
        self.reported & INPUT_REPORTED != 0
    }

    pub(crate) fn cached_reported(self) -> bool {
        self.reported & CACHED_REPORTED != 0
    }

    pub(crate) fn output_reported(self) -> bool {
        self.reported & OUTPUT_REPORTED != 0
    }

    pub(crate) fn total_reported(self) -> bool {
        self.reported & TOTAL_REPORTED != 0
    }
}

#[derive(Default, Deserialize)]
struct CodexRawUsageFields {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    prompt_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cached_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_read_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cached_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    completion_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    reasoning_output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    reasoning_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_tokens: Option<u64>,
}

impl<'de> Deserialize<'de> for CodexRawUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = CodexRawUsageFields::deserialize(deserializer)?;
        let input = fields
            .input_tokens
            .or(fields.prompt_tokens)
            .or(fields.input)
            .unwrap_or(0);
        let cached = fields
            .cached_input_tokens
            .or(fields.cache_read_input_tokens)
            .or(fields.cached_tokens)
            .unwrap_or(0);
        let output = fields
            .output_tokens
            .or(fields.completion_tokens)
            .or(fields.output)
            .unwrap_or(0);
        let reasoning = fields
            .reasoning_output_tokens
            .or(fields.reasoning_tokens)
            .unwrap_or(0);
        let computed = input + output + reasoning;
        let reported = (u8::from(
            fields.input_tokens.is_some()
                || fields.prompt_tokens.is_some()
                || fields.input.is_some(),
        ) * INPUT_REPORTED)
            | (u8::from(
                fields.cached_input_tokens.is_some()
                    || fields.cache_read_input_tokens.is_some()
                    || fields.cached_tokens.is_some(),
            ) * CACHED_REPORTED)
            | (u8::from(
                fields.output_tokens.is_some()
                    || fields.completion_tokens.is_some()
                    || fields.output.is_some(),
            ) * OUTPUT_REPORTED)
            | u8::from(fields.total_tokens.is_some()) * TOTAL_REPORTED;
        Ok(Self {
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            total_tokens: fields
                .total_tokens
                .filter(|total| *total > 0 || computed == 0)
                .unwrap_or(computed),
            reported,
        })
    }
}

pub(crate) fn normalize_timestamp(timestamp: Option<&CodexTimestamp<'_>>) -> Option<String> {
    match timestamp? {
        CodexTimestamp::String(raw) => non_empty(Some(raw)).map(ToOwned::to_owned),
        CodexTimestamp::Number(raw) => {
            let millis = if *raw > 10_000_000_000 {
                *raw
            } else {
                raw.checked_mul(1_000)?
            };
            Some(millis_to_rfc3339(millis))
        }
    }
}

pub(crate) fn millis_to_rfc3339(millis: u64) -> String {
    let secs = millis / 1_000;
    let frac_ms = millis % 1_000;
    let days = secs / 86_400;
    let time = secs % 86_400;
    let h = time / 3_600;
    let m = (time % 3_600) / 60;
    let s = time % 60;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{frac_ms:03}Z")
}

fn codex_timestamp(timestamp: Option<&CodexTimestamp<'_>>) -> Option<Timestamp> {
    match timestamp? {
        CodexTimestamp::String(raw) => raw.trim().parse().ok(),
        CodexTimestamp::Number(raw) => {
            let (seconds, nanos) = if *raw > 10_000_000_000 {
                (raw / 1_000, (raw % 1_000) * 1_000_000)
            } else {
                (*raw, 0)
            };
            Timestamp::new(i64::try_from(seconds).ok()?, nanos as i32).ok()
        }
    }
}

fn schema_error<'a>(raw: Option<&'a RawValue>) -> Option<RolloutError<'a>> {
    let error = parse_raw::<ErrorObject<'a>>(raw)?;
    let kinds = error_kinds(error.error_info());
    let label = raw_string(error.message.raw());
    (label.is_some() || !kinds.is_empty()).then_some(RolloutError { label, kinds })
}

fn error_is_present(raw: Option<&RawValue>) -> bool {
    let Some(raw) = raw else { return false };
    if raw.get() == "null" || raw.get() == "false" {
        return false;
    }
    if let Some(text) = raw_string(Some(raw)) {
        return !text.trim().is_empty();
    }
    parse_raw::<ErrorObject<'_>>(Some(raw)).is_none_or(|object| !object.is_empty())
}

fn error_kinds<'a>(raw: Option<&'a RawValue>) -> Vec<Cow<'a, str>> {
    let Some(raw) = raw else { return Vec::new() };
    if let Some(kind) = raw_string(Some(raw)) {
        return vec![kind];
    }
    serde_json::from_str::<BTreeMap<Cow<'a, str>, IgnoredAny>>(raw.get())
        .map(BTreeMap::into_keys)
        .map(Iterator::collect)
        .unwrap_or_default()
}

fn raw_string<'a>(raw: Option<&'a RawValue>) -> Option<Cow<'a, str>> {
    let mut deserializer = serde_json::Deserializer::from_str(raw?.get());
    deserialize_cow_str(&mut deserializer).ok()
}

fn parse_raw<'a, T>(raw: Option<&'a RawValue>) -> Option<T>
where
    T: Deserialize<'a>,
{
    serde_json::from_str(raw?.get()).ok()
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn owned_non_empty(value: Option<&str>) -> Option<String> {
    non_empty(value).map(ToOwned::to_owned)
}

fn normalize_agent_path(path: &str) -> Option<String> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let first = segments.next()?;
    let normalized = if first == "root" {
        segments.collect::<Vec<_>>()
    } else {
        std::iter::once(first).chain(segments).collect::<Vec<_>>()
    };
    (!normalized.is_empty()).then(|| normalized.join("/"))
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn deserialize_cow_str<'de, D>(deserializer: D) -> Result<Cow<'de, str>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CowVisitor;

    impl<'de> serde::de::Visitor<'de> for CowVisitor {
        type Value = Cow<'de, str>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string")
        }

        fn visit_borrowed_str<E: serde::de::Error>(
            self,
            value: &'de str,
        ) -> Result<Self::Value, E> {
            Ok(Cow::Borrowed(value))
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Cow::Owned(value.to_owned()))
        }

        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(Cow::Owned(value))
        }
    }

    deserializer.deserialize_str(CowVisitor)
}

fn deserialize_optional_cow_str<'de, D>(deserializer: D) -> Result<Option<Cow<'de, str>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct OptionalCowVisitor;

    impl<'de> serde::de::Visitor<'de> for OptionalCowVisitor {
        type Value = Option<Cow<'de, str>>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an optional string")
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            deserialize_cow_str(d).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalCowVisitor)
}

fn deserialize_optional_u64_lossy<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct U64Visitor;

    impl<'de> serde::de::Visitor<'de> for U64Visitor {
        type Value = Option<u64>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an optional unsigned integer")
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(Some(value))
        }

        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(value.trim().parse().ok())
        }

        fn visit_borrowed_str<E: serde::de::Error>(
            self,
            value: &'de str,
        ) -> Result<Self::Value, E> {
            self.visit_str(value)
        }

        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            self.visit_str(&value)
        }

        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            deserialize_optional_u64_lossy(d)
        }
    }

    deserializer.deserialize_any(U64Visitor)
}

#[cfg(test)]
#[path = "rollout/tests.rs"]
mod tests;
