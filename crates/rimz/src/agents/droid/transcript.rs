//! Droid 0.171.0 provider-native conversation normalization.
//!
//! Factory's private session format is a parent-linked JSONL tree. RimZ pins
//! this reader to version 2, follows the active leaf for complete history, and
//! reads appended assistant records physically for streaming.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::transcript::{TranscriptMessage, TranscriptRole};
use crate::agents::transcript_fs::deserialize_optional_string_lossy;
use crate::agents::transcript_fs::{
    deserialize_optional_object_lossy, deserialize_optional_u64_lossy,
};
use crate::agents::{
    AgentCurrentUsage, AgentSessionUsage, TranscriptCompanionStat, TranscriptStat,
    read_transcript_tail, sanitize_user_prompt,
};
use jiff::Timestamp;

const TRANSCRIPT_VERSION: u64 = 2;
const MAX_PARENT_DEPTH: usize = 16_384;

#[derive(Debug, Deserialize)]
struct Record {
    #[serde(rename = "type")]
    record_type: Option<String>,
    version: Option<u64>,
    id: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    timestamp: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    role: Option<String>,
    visibility: Option<String>,
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(rename = "hookEventName")]
    hook_event_name: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    #[serde(rename = "reasoningEffort")]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Settings {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    model: Option<String>,
    #[serde(
        rename = "reasoningEffort",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    reasoning_effort: Option<String>,
    #[serde(
        rename = "tokenUsage",
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    token_usage: Option<TokenUsage>,
    #[serde(
        rename = "lastCallTokenUsage",
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    last_call_token_usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct TokenUsage {
    #[serde(
        rename = "inputTokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    input_tokens: Option<u64>,
    #[serde(
        rename = "outputTokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    output_tokens: Option<u64>,
    #[serde(
        rename = "cacheCreationTokens",
        alias = "cacheCreationInputTokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    cache_creation_tokens: Option<u64>,
    #[serde(
        rename = "cacheReadTokens",
        alias = "cacheReadInputTokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    cache_read_tokens: Option<u64>,
    #[serde(
        rename = "thinkingTokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    thinking_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SessionTelemetry {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub session_usage: Option<AgentSessionUsage>,
    pub current_usage: Option<AgentCurrentUsage>,
    pub native_permission_wait: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TelemetryRefresh {
    pub transcript_path: PathBuf,
    pub settings_path: PathBuf,
    pub session_cwd: Option<PathBuf>,
    transcript_header_valid: bool,
    pub stat: TranscriptStat,
    pub telemetry: SessionTelemetry,
}

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    if !supported_lines(lines) {
        return Vec::new();
    }

    let records = lines
        .lines()
        .filter_map(parse_record)
        .filter(|record| record.record_type.as_deref() == Some("message"))
        .collect::<Vec<_>>();
    let Some(leaf_id) = records
        .iter()
        .rev()
        .find(|record| visible_role(record).is_some() && non_empty(record.id.as_deref()).is_some())
        .and_then(|record| non_empty(record.id.as_deref()))
    else {
        return Vec::new();
    };
    let by_id = records
        .iter()
        .filter_map(|record| non_empty(record.id.as_deref()).map(|id| (id, record)))
        .collect::<HashMap<_, _>>();

    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(leaf_id);
    while let Some(id) = current {
        if chain.len() >= MAX_PARENT_DEPTH || !seen.insert(id) {
            return Vec::new();
        }
        let Some(record) = by_id.get(id).copied() else {
            return Vec::new();
        };
        chain.push(record);
        current = non_empty(record.parent_id.as_deref());
    }
    chain.reverse();
    chain.into_iter().filter_map(normalize_record).collect()
}

pub(super) fn parse_assistant_suffix(lines: &str) -> Vec<String> {
    lines
        .lines()
        .filter_map(parse_record)
        .filter(|record| record.record_type.as_deref() == Some("message"))
        .filter(|record| visible_role(record) == Some(TranscriptRole::Assistant))
        .filter_map(|record| visible_text(&record))
        .collect()
}

pub(super) fn supported_file(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut first = String::new();
    if BufReader::new(file).read_line(&mut first).is_err() {
        return false;
    }
    supported_lines(&first)
}

pub(super) fn last_assistant_message(path: &Path) -> Option<String> {
    if !supported_file(path) {
        return None;
    }
    let tail = read_transcript_tail(path)?;
    tail.lines()
        .rev()
        .filter_map(parse_record)
        .find(|record| {
            record.record_type.as_deref() == Some("message")
                && visible_role(record) == Some(TranscriptRole::Assistant)
                && visible_text(record).is_some()
        })
        .and_then(|record| visible_text(&record))
}

pub(super) fn identity(path: &Path) -> (Option<String>, Option<String>) {
    let Some(transcript) = transcript_path(path) else {
        return (None, None);
    };
    if !supported_file(&transcript) {
        return (None, None);
    }
    let settings = settings_path(path)
        .and_then(|path| read_settings(&path))
        .unwrap_or_default();
    let (tail_model, tail_effort) = latest_assistant_identity(&transcript);
    (
        non_empty_owned(settings.model).or(tail_model),
        non_empty_owned(settings.reasoning_effort).or(tail_effort),
    )
}

/// Read the public session snapshot after validating its sibling version-2
/// transcript header. The settings file is the watched/stat-gated source;
/// callers may pass either the hook JSONL path or a prior settings sidecar path.
pub(super) fn telemetry(
    path: &Path,
    prior_stat: Option<&TranscriptStat>,
) -> Option<TelemetryRefresh> {
    let transcript = transcript_path(path)?;
    let mut refresh = settings_snapshot(path, None)?;
    if !refresh.transcript_header_valid {
        return None;
    }
    refresh.stat.companion =
        TranscriptStat::from_path(&transcript).map(TranscriptCompanionStat::from);
    if prior_stat == Some(&refresh.stat) {
        return None;
    }
    let tail = tail_projection(&transcript);
    refresh.telemetry.model = refresh.telemetry.model.or(tail.model);
    refresh.telemetry.reasoning_effort = refresh.telemetry.reasoning_effort.or(tail.effort);
    refresh.telemetry.native_permission_wait = tail.native_permission_wait;
    Some(refresh)
}

/// Typed settings read used by the spend-parser seam after the caller has
/// selected a session snapshot. The sibling header is optional for exact
/// built-in pricing; custom identity still requires its validated cwd.
pub(super) fn settings_snapshot(
    path: &Path,
    prior_stat: Option<&TranscriptStat>,
) -> Option<TelemetryRefresh> {
    let settings_path = settings_path(path)?;
    let transcript_path = transcript_path(path)?;
    let header = session_header(&transcript_path);
    let transcript_header_valid = header.is_some();
    let session_cwd = header.and_then(|header| header.cwd);
    let stat = TranscriptStat::from_path(&settings_path)?;
    if prior_stat == Some(&stat) {
        return None;
    }
    let settings = read_settings(&settings_path)?;
    let token_usage = settings.token_usage.and_then(|usage| {
        let usage = AgentSessionUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_tokens,
            cache_read_input_tokens: usage.cache_read_tokens,
            thinking_tokens: usage.thinking_tokens,
        };
        (usage.input_tokens.is_some()
            || usage.output_tokens.is_some()
            || usage.cache_creation_input_tokens.is_some()
            || usage.cache_read_input_tokens.is_some()
            || usage.thinking_tokens.is_some())
        .then_some(usage)
    });
    let current_usage = settings.last_call_token_usage.and_then(|usage| {
        let usage = AgentCurrentUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_tokens,
            cache_read_input_tokens: usage.cache_read_tokens,
        };
        (!usage.is_zero()).then_some(usage)
    });
    Some(TelemetryRefresh {
        transcript_path,
        settings_path,
        session_cwd,
        transcript_header_valid,
        stat,
        telemetry: SessionTelemetry {
            model: non_empty_owned(settings.model),
            reasoning_effort: non_empty_owned(settings.reasoning_effort),
            session_usage: token_usage,
            current_usage,
            native_permission_wait: None,
        },
    })
}

/// Absolute session cwd from the validated version-2 header. Settings sidecar
/// inputs derive only their exact sibling JSONL for this bounded first-line read.
#[cfg(test)]
pub(super) fn session_cwd(path: &Path) -> Option<PathBuf> {
    let transcript = transcript_path(path)?;
    session_header(&transcript)?.cwd
}

struct SessionHeader {
    cwd: Option<PathBuf>,
}

fn session_header(transcript: &Path) -> Option<SessionHeader> {
    let file = File::open(transcript).ok()?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first).ok()?;
    let record = parse_record(first.trim())?;
    if record.record_type.as_deref() != Some("session_start")
        || record.version != Some(TRANSCRIPT_VERSION)
    {
        return None;
    }
    let cwd = non_empty_owned(record.cwd)
        .map(PathBuf::from)
        .filter(|cwd| cwd.is_absolute());
    Some(SessionHeader { cwd })
}

fn supported_lines(lines: &str) -> bool {
    lines
        .lines()
        .find(|line| !line.trim().is_empty())
        .and_then(parse_record)
        .is_some_and(|record| {
            record.record_type.as_deref() == Some("session_start")
                && record.version == Some(TRANSCRIPT_VERSION)
        })
}

#[derive(Default)]
struct TailProjection {
    model: Option<String>,
    effort: Option<String>,
    native_permission_wait: Option<Timestamp>,
}

fn tail_projection(path: &Path) -> TailProjection {
    let Some(tail) = read_transcript_tail(path) else {
        return TailProjection::default();
    };
    let mut projection = TailProjection::default();
    let mut found_visible_leaf = false;
    let mut found_assistant_identity = false;
    for record in tail.lines().rev().filter_map(parse_record) {
        let role = visible_role(&record);
        if !found_visible_leaf && role.is_some() {
            found_visible_leaf = true;
            projection.native_permission_wait = (role == Some(TranscriptRole::Assistant)
                && record.message.as_ref().is_some_and(|message| {
                    message.content.iter().any(|block| {
                        block.content_type.as_deref() == Some("tool_use")
                            && block.name.as_deref() == Some("AskUser")
                    })
                }))
            .then(|| record.timestamp.as_deref()?.parse().ok())
            .flatten();
        }
        if !found_assistant_identity
            && role == Some(TranscriptRole::Assistant)
            && let Some(message) = record.message
        {
            found_assistant_identity = true;
            projection.model = non_empty_owned(message.model_id);
            projection.effort = non_empty_owned(message.reasoning_effort);
        }
    }
    projection
}

fn latest_assistant_identity(path: &Path) -> (Option<String>, Option<String>) {
    let projection = tail_projection(path);
    (projection.model, projection.effort)
}

fn normalize_record(record: &Record) -> Option<TranscriptMessage> {
    let role = visible_role(record)?;
    let visible = visible_text(record)?;
    let text = match role {
        TranscriptRole::User => sanitize_user_prompt(Some(&visible))?,
        TranscriptRole::Assistant => visible,
    };
    Some(TranscriptMessage {
        role,
        at: record.timestamp.as_deref().and_then(|raw| raw.parse().ok()),
        text,
    })
}

fn visible_role(record: &Record) -> Option<TranscriptRole> {
    let message = record.message.as_ref()?;
    if message.visibility.is_some() || message.hook_event_name.is_some() {
        return None;
    }
    match message.role.as_deref() {
        Some("user") => Some(TranscriptRole::User),
        Some("assistant") => Some(TranscriptRole::Assistant),
        _ => None,
    }
}

fn visible_text(record: &Record) -> Option<String> {
    let text = record
        .message
        .as_ref()?
        .content
        .iter()
        .filter(|block| block.content_type.as_deref() == Some("text"))
        .filter_map(|block| block.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn parse_record(line: &str) -> Option<Record> {
    serde_json::from_str(line.trim()).ok()
}

pub(super) fn settings_path(path: &Path) -> Option<PathBuf> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".settings.json"))
    {
        return Some(path.to_path_buf());
    }
    let stem = path.file_stem()?.to_str()?;
    Some(path.with_file_name(format!("{stem}.settings.json")))
}

fn transcript_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if let Some(stem) = name.strip_suffix(".settings.json") {
        return (!stem.is_empty()).then(|| path.with_file_name(format!("{stem}.jsonl")));
    }
    Some(path.to_path_buf())
}

fn read_settings(path: &Path) -> Option<Settings> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn non_empty(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

fn non_empty_owned(raw: Option<String>) -> Option<String> {
    raw.and_then(|value| non_empty(Some(&value)).map(ToOwned::to_owned))
}
