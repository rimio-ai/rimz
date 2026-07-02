//! Rolling Rimz-owned transcript log.
//!
//! The log is append-only JSONL under `transcript/<bucket-start>.jsonl`. File
//! buckets cap individual file size; reads return entries sorted by timestamp,
//! so bucket boundaries never carry ordering meaning.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::{AskAnswer, AskQuestion};
use crate::ids::{AgentKind, AgentSessionId, RequestId};
use crate::ledger::{StatePaths, atomic, lock};

const DEFAULT_FILE_DAYS: u32 = 7;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, thiserror::Error)]
pub enum TranscriptLogErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, TranscriptLogErr>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    Prompt,
    Message,
    Assistant,
    Ask,
    Answer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub at: Timestamp,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub entry: TranscriptKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<AskAnswer>,
}

/// Append one transcript entry. Callers must not already hold the workspace
/// lock; append takes it to serialize hook children writing long JSONL lines.
#[must_use = "durability barrier; check the result"]
pub fn append(paths: &StatePaths, entry: &TranscriptEntry) -> Result<()> {
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    append_locked(paths, entry)
}

/// Append one transcript entry while the caller holds the workspace lock.
#[must_use = "durability barrier; check the result"]
pub(crate) fn append_locked(paths: &StatePaths, entry: &TranscriptEntry) -> Result<()> {
    fs::create_dir_all(&paths.transcript_dir).map_err(|source| TranscriptLogErr::Io {
        path: paths.transcript_dir.clone(),
        source,
    })?;
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    atomic::append_record_bytes(&bucket_path(paths, entry.at), &line)?;
    Ok(())
}

pub fn read_all(paths: &StatePaths) -> Result<Vec<TranscriptEntry>> {
    let mut files = transcript_files(&paths.transcript_dir)?;
    files.sort();

    let mut entries = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|source| TranscriptLogErr::Io {
            path: path.clone(),
            source,
        })?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|entry| entry.at);
    Ok(entries)
}

pub fn answer_text(decision: &Value) -> String {
    if let Some(choice) = decision
        .get("choice")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return choice.to_owned();
    }
    for key in ["updatedInput", "answer", "message"] {
        if let Some(text) = decision.get(key).and_then(text_value) {
            return text;
        }
    }
    serde_json::to_string(decision).unwrap_or_else(|_| "null".to_owned())
}

fn text_value(value: &Value) -> Option<String> {
    if let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(text.to_owned());
    }
    (!value.is_null())
        .then(|| serde_json::to_string(value).ok())
        .flatten()
}

fn transcript_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(TranscriptLogErr::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| TranscriptLogErr::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(files)
}

fn bucket_path(paths: &StatePaths, at: Timestamp) -> PathBuf {
    paths.transcript_dir.join(bucket_file_name(at, file_days()))
}

fn file_days() -> u32 {
    std::env::var(crate::run::ENV_TRANSCRIPT_FILE_DAYS)
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|days| *days > 0)
        .unwrap_or(DEFAULT_FILE_DAYS)
}

fn bucket_file_name(at: Timestamp, file_days: u32) -> String {
    let days = at.as_second().div_euclid(SECONDS_PER_DAY);
    let window = i64::from(file_days.max(1));
    let start_days = days.div_euclid(window) * window;
    let start = Timestamp::from_second(start_days * SECONDS_PER_DAY)
        .expect("day-aligned unix timestamp is valid");
    format!("{}.jsonl", start.strftime("%Y-%m-%d"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use tempfile::tempdir;

    fn ts(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    fn paths() -> (tempfile::TempDir, StatePaths) {
        let dir = tempdir().expect("tempdir");
        let id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(id, dir.path()).expect("state paths");
        (dir, paths)
    }

    fn entry(entry: TranscriptKind, text: &str, at: &str) -> TranscriptEntry {
        TranscriptEntry {
            at: ts(at),
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            channel: None,
            name: None,
            profile: None,
            role: None,
            entry,
            request_id: None,
            from: None,
            text: text.to_owned(),
            questions: Vec::new(),
            answers: Vec::new(),
        }
    }

    #[test]
    fn bucket_names_align_to_file_day_windows() {
        assert_eq!(
            bucket_file_name(ts("1970-01-08T00:00:00Z"), 1),
            "1970-01-08.jsonl"
        );
        assert_eq!(
            bucket_file_name(ts("1970-01-07T23:59:59Z"), 7),
            "1970-01-01.jsonl"
        );
        assert_eq!(
            bucket_file_name(ts("1970-01-08T00:00:00Z"), 7),
            "1970-01-08.jsonl"
        );
        assert_eq!(
            bucket_file_name(ts("1970-01-30T23:59:59Z"), 30),
            "1970-01-01.jsonl"
        );
        assert_eq!(
            bucket_file_name(ts("1970-01-31T00:00:00Z"), 30),
            "1970-01-31.jsonl"
        );
    }

    #[test]
    fn transcript_entry_round_trips_and_skips_empty_optionals() {
        for kind in [
            TranscriptKind::Prompt,
            TranscriptKind::Message,
            TranscriptKind::Assistant,
            TranscriptKind::Ask,
            TranscriptKind::Answer,
        ] {
            let entry = entry(kind, "hello", "2026-06-01T00:00:00Z");
            let json = serde_json::to_string(&entry).expect("serialize");
            assert!(!json.contains("request_id"));
            assert!(!json.contains("channel"));
            assert!(!json.contains("questions"));
            assert!(!json.contains("answers"));
            let decoded: TranscriptEntry = serde_json::from_str(&json).expect("decode");
            assert_eq!(decoded, entry);
        }
    }

    #[test]
    fn transcript_entry_round_trips_structured_asks_and_answers() {
        let mut entry = entry(TranscriptKind::Ask, "lead-in", "2026-06-01T00:00:00Z");
        entry.questions = vec![AskQuestion {
            question: "Choose deployment path?".to_owned(),
            options: vec!["safe".to_owned(), "fast".to_owned()],
        }];
        entry.answers = vec![AskAnswer {
            question: Some("Choose deployment path?".to_owned()),
            chosen: vec!["safe".to_owned()],
            note: Some("use prod window".to_owned()),
        }];

        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("questions"));
        assert!(json.contains("answers"));
        let decoded: TranscriptEntry = serde_json::from_str(&json).expect("decode");

        assert_eq!(decoded, entry);
    }

    #[test]
    fn read_all_sorts_by_timestamp_and_skips_malformed_lines() {
        let (_dir, paths) = paths();
        fs::create_dir_all(&paths.transcript_dir).expect("mkdir transcript");
        let first = entry(TranscriptKind::Prompt, "first", "2026-06-01T00:00:02Z");
        let second = entry(TranscriptKind::Assistant, "second", "2026-06-01T00:00:01Z");
        fs::write(
            paths.transcript_dir.join("2026-06-01.jsonl"),
            format!("{}\nnot json\n", serde_json::to_string(&first).unwrap()),
        )
        .expect("write first");
        fs::write(
            paths.transcript_dir.join("2026-06-08.jsonl"),
            format!("{}\n", serde_json::to_string(&second).unwrap()),
        )
        .expect("write second");

        let entries = read_all(&paths).expect("read log");

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }

    #[test]
    fn append_creates_transcript_dir_lazily() {
        let (_dir, paths) = paths();
        let entry = entry(TranscriptKind::Prompt, "hello", "2026-06-01T00:00:00Z");

        append(&paths, &entry).expect("append");

        assert!(paths.transcript_dir.is_dir());
        assert_eq!(read_all(&paths).expect("read").len(), 1);
    }

    #[test]
    fn answer_text_prefers_choice_then_typed_fields_then_json() {
        assert_eq!(
            answer_text(&serde_json::json!({"choice": "allow"})),
            "allow"
        );
        assert_eq!(
            answer_text(&serde_json::json!({"updatedInput": "deploy now"})),
            "deploy now"
        );
        assert_eq!(answer_text(&serde_json::json!({"answer": "yes"})), "yes");
        assert_eq!(
            answer_text(&serde_json::json!({"message": {"ok": true}})),
            r#"{"ok":true}"#
        );
        assert_eq!(
            answer_text(&serde_json::json!({"other": 1})),
            r#"{"other":1}"#
        );
    }
}
