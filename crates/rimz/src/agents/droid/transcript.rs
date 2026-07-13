//! Droid 0.170.0 provider-native conversation normalization.
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
use crate::agents::{read_transcript_tail, sanitize_user_prompt};

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
    if !supported_file(path) {
        return (None, None);
    }
    let settings = settings_path(path)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<Settings>(&raw).ok())
        .unwrap_or_default();
    let (tail_model, tail_effort) = latest_assistant_identity(path);
    (
        non_empty_owned(settings.model).or(tail_model),
        non_empty_owned(settings.reasoning_effort).or(tail_effort),
    )
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

fn latest_assistant_identity(path: &Path) -> (Option<String>, Option<String>) {
    let Some(tail) = read_transcript_tail(path) else {
        return (None, None);
    };
    for record in tail.lines().rev().filter_map(parse_record) {
        if record.record_type.as_deref() != Some("message")
            || visible_role(&record) != Some(TranscriptRole::Assistant)
        {
            continue;
        }
        let Some(message) = record.message else {
            continue;
        };
        return (
            non_empty_owned(message.model_id),
            non_empty_owned(message.reasoning_effort),
        );
    }
    (None, None)
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

fn settings_path(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    Some(path.with_file_name(format!("{stem}.settings.json")))
}

fn non_empty(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

fn non_empty_owned(raw: Option<String>) -> Option<String> {
    raw.and_then(|value| non_empty(Some(&value)).map(ToOwned::to_owned))
}
