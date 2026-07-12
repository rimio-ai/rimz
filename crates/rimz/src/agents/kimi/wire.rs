//! Kimi Code's flat durable agent-record log and session-index lookup.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::agents::transcript_fs::{home_dir, read_spend_lines};

#[derive(Clone, Debug, Deserialize)]
pub struct WireRecord {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub time: f64,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
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
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UsageRecord {
    pub model: String,
    pub usage: TokenUsage,
    #[serde(rename = "usageScope", alias = "scope")]
    pub scope: Option<Value>,
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

pub fn transcript_files() -> Vec<PathBuf> {
    transcript_files_under(&kimi_home())
}

pub(crate) fn transcript_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(work_dirs) = std::fs::read_dir(root.join("sessions")) else {
        return files;
    };
    for work_dir in work_dirs.filter_map(Result::ok) {
        let Ok(sessions) = std::fs::read_dir(work_dir.path()) else {
            continue;
        };
        for session in sessions.filter_map(Result::ok) {
            let path = session.path().join("agents/main/wire.jsonl");
            if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

pub fn records_from_bytes(bytes: &[u8]) -> Vec<WireRecord> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<WireRecord>(line).ok())
        .filter(|record| record.kind != "metadata")
        .collect()
}

pub fn read_records(path: &Path, offset: u64) -> Option<(Vec<WireRecord>, u64)> {
    let (bytes, next) = read_spend_lines(path, offset)?;
    Some((records_from_bytes(&bytes), next))
}

pub fn usage_records(records: &[WireRecord]) -> Vec<(f64, UsageRecord)> {
    records
        .iter()
        .filter(|record| record.kind == "usage.record")
        .filter_map(|record| {
            serde_json::from_value::<UsageRecord>(Value::Object(record.fields.clone()))
                .ok()
                .map(|usage| (record.time, usage))
        })
        .collect()
}

pub fn latest_context_tokens(records: &[WireRecord]) -> Option<u64> {
    records
        .iter()
        .fold(None, |latest, record| match record.kind.as_str() {
            "usage.record" => {
                serde_json::from_value::<UsageRecord>(Value::Object(record.fields.clone()))
                    .ok()
                    .map(|record| record.usage.input_total())
                    .or(latest)
            }
            "context.apply_compaction" => record
                .fields
                .get("tokensAfter")
                .and_then(Value::as_u64)
                .or(latest),
            _ => latest,
        })
}

pub fn record_message(record: &WireRecord) -> Option<&Value> {
    (record.kind == "context.append_message").then(|| record.fields.get("message"))?
}
