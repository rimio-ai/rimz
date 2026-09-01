//! Durable RimZ-owned cross-provider conversation log.
//!
//! The log is append-only JSONL under `transcript/<bucket-start>.jsonl` in the
//! workspace state root. This transcript is distinct from provider-native
//! transcript and session files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::AskId;
use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::store::{StatePaths, atomic, lock};

const FILE_DAYS: u32 = 7;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, thiserror::Error)]
pub enum TranscriptLogErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error("cannot access {path}: {source}")]
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
    SubagentReport,
    Assistant,
    Ask,
    Answer,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub at: Timestamp,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AskId>,
    /// Queue record that opened this prompt/message turn, when delivery was
    /// confirmed against a RimZ-authored message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// Messages whose receiving turn authored this entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<MessageId>,
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
    pub from: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<AskAnswer>,
}

impl TranscriptEntry {
    pub fn new(
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        entry: TranscriptKind,
        text: String,
    ) -> Self {
        Self {
            at,
            kind,
            agent_id,
            id: None,
            message_id: None,
            reply_to: Vec::new(),
            channel: None,
            name: None,
            profile: None,
            role: None,
            entry,
            from: None,
            text,
            questions: Vec::new(),
            answers: Vec::new(),
        }
    }
}

/// Chat transcript lane: launch-stamped channel when known, else worktree
/// basename fallback for older agent payloads that carry only a path.
pub fn entry_channel(stamped: Option<&str>, worktree_path: Option<&str>) -> Option<String> {
    crate::store::snapshot::compose_channel(
        stamped,
        worktree_path.and_then(|path| path.rsplit('/').next().filter(|value| !value.is_empty())),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<AskOption>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
    /// Claude uses a different keyboard contract when any option carries a
    /// rich preview: digits move focus and Enter selects. Persist the bit so a
    /// later `rimz answer` does not guess from lossy option labels.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_option_previews: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AskOptionWire", into = "AskOptionWire")]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caution: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AskOptionWire {
    Label(String),
    Detailed {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caution: Option<String>,
    },
}

impl From<AskOptionWire> for AskOption {
    fn from(value: AskOptionWire) -> Self {
        match value {
            AskOptionWire::Label(label) => Self {
                label,
                description: None,
                caution: None,
            },
            AskOptionWire::Detailed {
                label,
                description,
                caution,
            } => Self {
                label,
                description,
                caution,
            },
        }
    }
}

impl From<AskOption> for AskOptionWire {
    fn from(value: AskOption) -> Self {
        match (value.description, value.caution) {
            (Some(description), caution) => AskOptionWire::Detailed {
                label: value.label,
                description: Some(description),
                caution,
            },
            (None, Some(caution)) => AskOptionWire::Detailed {
                label: value.label,
                description: None,
                caution: Some(caution),
            },
            (None, None) => AskOptionWire::Label(value.label),
        }
    }
}

impl From<String> for AskOption {
    fn from(label: String) -> Self {
        Self {
            label,
            description: None,
            caution: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn answers_text(answers: &[AskAnswer]) -> String {
    answers
        .iter()
        .filter_map(|answer| {
            let mut line = answer
                .chosen
                .iter()
                .filter_map(|choice| non_empty(choice))
                .collect::<Vec<_>>()
                .join(", ");
            if line.is_empty() {
                return None;
            }
            if let Some(note) = answer.note.as_deref().and_then(non_empty) {
                line.push_str(" (note: ");
                line.push_str(note);
                line.push(')');
            }
            Some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

/// Append one transcript entry. Callers must not already hold the workspace
/// lock; append takes it to serialize hook children writing long JSONL lines.
#[must_use = "durability barrier; check the result"]
pub fn append(paths: &StatePaths, entry: &TranscriptEntry) -> Result<()> {
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    append_locked(paths, entry)
}

/// Append an id-stamped answer exactly once across the native hook and CLI
/// confirmation writers. The workspace lock covers both the duplicate check
/// and append, so a PostToolUse racing `rimz answer` still yields one record.
pub fn append_answer_if_missing(paths: &StatePaths, entry: &TranscriptEntry) -> Result<bool> {
    debug_assert_eq!(entry.entry, TranscriptKind::Answer);
    let Some(id) = entry.id.as_ref() else {
        append(paths, entry)?;
        return Ok(true);
    };
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    if read_all(paths)?.into_iter().any(|existing| {
        existing.entry == TranscriptKind::Answer && existing.id.as_ref() == Some(id)
    }) {
        return Ok(false);
    }
    append_locked(paths, entry)?;
    Ok(true)
}

fn append_locked(paths: &StatePaths, entry: &TranscriptEntry) -> Result<()> {
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

/// The agent's latest open native ask: the newest `Ask` entry for
/// `(kind, agent_id)` with no later `Answer` entry.
///
/// Bucket files and their entries are walked newest-first. Answer ids accumulate
/// across buckets until the scan reaches the newest ask they do not close.
pub fn latest_open_ask(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<Option<TranscriptEntry>> {
    let mut files = transcript_files(&paths.transcript_dir)?;
    files.sort_by(|left, right| right.cmp(left));
    let mut closed = std::collections::BTreeSet::new();

    for path in files {
        let text = fs::read_to_string(&path).map_err(|source| TranscriptLogErr::Io {
            path: path.clone(),
            source,
        })?;
        let entries = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<TranscriptEntry>(line).ok())
            .collect::<Vec<_>>();
        for entry in entries.into_iter().rev() {
            if &entry.kind != kind || &entry.agent_id != agent_id {
                continue;
            }
            match entry.entry {
                TranscriptKind::Ask => match entry.id.as_ref() {
                    Some(id) if closed.contains(id.as_str()) => {}
                    _ => return Ok(Some(entry)),
                },
                TranscriptKind::Answer => match entry.id.as_ref() {
                    Some(id) => {
                        closed.insert(id.as_str().to_owned());
                    }
                    None => return Ok(None),
                },
                _ => {}
            }
        }
    }
    Ok(None)
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
    paths.transcript_dir.join(bucket_file_name(at, FILE_DAYS))
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
    use serde_json::json;
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
        TranscriptEntry::new(
            ts(at),
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("sess-1"),
            entry,
            text.to_owned(),
        )
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
    fn chat_entry_round_trips_and_skips_empty_optionals() {
        for kind in [
            TranscriptKind::Prompt,
            TranscriptKind::Message,
            TranscriptKind::SubagentReport,
            TranscriptKind::Assistant,
            TranscriptKind::Ask,
            TranscriptKind::Answer,
            TranscriptKind::Error,
        ] {
            let entry = entry(kind, "hello", "2026-06-01T00:00:00Z");
            let json = serde_json::to_string(&entry).expect("serialize");
            assert!(!json.contains("request_id"));
            assert!(!json.contains("message_id"));
            assert!(!json.contains("reply_to"));
            assert!(!json.contains("channel"));
            assert!(!json.contains("questions"));
            assert!(!json.contains("answers"));
            let decoded: TranscriptEntry = serde_json::from_str(&json).expect("decode");
            assert_eq!(decoded, entry);
        }
    }

    #[test]
    fn chat_entry_round_trips_message_causality_and_decodes_legacy_lines() {
        let mut entry = entry(TranscriptKind::Message, "hello", "2026-06-01T00:00:00Z");
        entry.message_id = Some(MessageId::parse("msg_0123456789abcdef").unwrap());
        entry.reply_to = vec![MessageId::parse("msg_123456789abcdef0").unwrap()];

        let json = serde_json::to_value(&entry).expect("serialize");
        let decoded: TranscriptEntry = serde_json::from_value(json.clone()).expect("decode");
        assert_eq!(decoded, entry);

        let mut legacy = json;
        legacy.as_object_mut().unwrap().remove("message_id");
        legacy.as_object_mut().unwrap().remove("reply_to");
        let decoded: TranscriptEntry = serde_json::from_value(legacy).expect("legacy decode");
        assert_eq!(decoded.message_id, None);
        assert!(decoded.reply_to.is_empty());
    }

    #[test]
    fn chat_entry_round_trips_structured_asks_and_answers() {
        let mut entry = entry(TranscriptKind::Ask, "lead-in", "2026-06-01T00:00:00Z");
        entry.questions = vec![AskQuestion {
            question: "Choose deployment path?".to_owned(),
            options: vec![
                AskOption::from("safe".to_owned()),
                AskOption::from("fast".to_owned()),
            ],
            multi_select: false,
            has_option_previews: false,
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
    fn read_all_decodes_mixed_option_shapes() {
        let (_dir, paths) = paths();
        fs::create_dir_all(&paths.transcript_dir).expect("mkdir transcript");
        fs::write(
            paths.transcript_dir.join("2026-06-01.jsonl"),
            serde_json::json!({
                "at": "2026-06-01T00:00:00Z",
                "kind": "claude",
                "agent_id": "sess-1",
                "entry": "ask",
                "text": "",
                "questions": [{
                    "question": "Choose deployment path?",
                    "options": [
                        {
                            "label": "safe",
                            "description": "Use staged rollout."
                        },
                        "fast"
                    ]
                }]
            })
            .to_string(),
        )
        .expect("write log");

        let entries = read_all(&paths).expect("read log");

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].questions[0].options,
            vec![
                AskOption {
                    label: "safe".to_owned(),
                    description: Some("Use staged rollout.".to_owned()),
                    caution: None,
                },
                AskOption::from("fast".to_owned()),
            ]
        );
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
    fn latest_open_ask_finds_unanswered_ask() {
        let (_dir, paths) = paths();
        let ask = entry(TranscriptKind::Ask, "approve?", "2026-06-01T00:00:00Z");
        append(&paths, &ask).expect("append ask");

        let open = latest_open_ask(&paths, &ask.kind, &ask.agent_id).expect("read ask");

        assert_eq!(
            open.as_ref().map(|entry| entry.text.as_str()),
            Some("approve?")
        );
    }

    #[test]
    fn latest_open_ask_treats_later_answer_as_closed() {
        let (_dir, paths) = paths();
        let ask = entry(TranscriptKind::Ask, "approve?", "2026-06-01T00:00:00Z");
        let answer = entry(TranscriptKind::Answer, "yes", "2026-06-01T00:00:01Z");
        append(&paths, &ask).expect("append ask");
        append(&paths, &answer).expect("append answer");

        let open = latest_open_ask(&paths, &ask.kind, &ask.agent_id).expect("read ask");

        assert_eq!(open, None);
    }

    #[test]
    fn latest_open_ask_pairs_answers_by_id() {
        let (_dir, paths) = paths();
        let mut old_ask = entry(TranscriptKind::Ask, "old?", "2026-06-01T00:00:00Z");
        old_ask.id = Some(AskId::parse("ask_0123456789abcdea").unwrap());
        let mut new_ask = entry(TranscriptKind::Ask, "new?", "2026-06-01T00:00:01Z");
        new_ask.id = Some(AskId::parse("ask_0123456789abcdeb").unwrap());
        let mut unrelated = entry(TranscriptKind::Answer, "old answer", "2026-06-01T00:00:02Z");
        unrelated.id = old_ask.id.clone();
        append(&paths, &old_ask).expect("append old ask");
        append(&paths, &new_ask).expect("append new ask");
        append(&paths, &unrelated).expect("append unrelated answer");

        let open = latest_open_ask(&paths, &new_ask.kind, &new_ask.agent_id)
            .expect("read ask")
            .expect("new ask stays open");
        assert_eq!(open.id, new_ask.id);

        let mut matching = entry(TranscriptKind::Answer, "new answer", "2026-06-01T00:00:03Z");
        matching.id = new_ask.id.clone();
        append(&paths, &matching).expect("append matching answer");
        assert!(
            latest_open_ask(&paths, &new_ask.kind, &new_ask.agent_id)
                .expect("read closed ask")
                .is_none()
        );
    }

    #[test]
    fn latest_open_ask_newest_bucket_decides() {
        let (_dir, paths) = paths();
        let older_answer = entry(TranscriptKind::Answer, "yes", "2026-06-01T00:00:00Z");
        let newer_ask = entry(TranscriptKind::Ask, "approve?", "2026-06-08T00:00:00Z");
        append(&paths, &older_answer).expect("append answer");
        append(&paths, &newer_ask).expect("append ask");

        let open = latest_open_ask(&paths, &newer_ask.kind, &newer_ask.agent_id).expect("read ask");

        assert_eq!(
            open.as_ref().map(|entry| entry.text.as_str()),
            Some("approve?")
        );
    }

    #[test]
    fn latest_open_ask_missing_dir_is_empty() {
        let (_dir, paths) = paths();

        let open = latest_open_ask(
            &paths,
            &AgentKind::new_unchecked("claude"),
            &AgentSessionId::from("sess-1"),
        )
        .expect("read missing dir");

        assert_eq!(open, None);
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
    fn answer_append_is_idempotent_by_ask_id() {
        let (_dir, paths) = paths();
        let mut answer = entry(TranscriptKind::Answer, "safe", "2026-06-01T00:00:00Z");
        answer.id = Some(AskId::parse("ask_0123456789abcdef").unwrap());

        assert!(append_answer_if_missing(&paths, &answer).expect("first append"));
        assert!(!append_answer_if_missing(&paths, &answer).expect("duplicate append"));
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

    #[test]
    fn ask_option_deserializes_legacy_string_shape() {
        let option: AskOption = serde_json::from_value(json!("safe")).expect("decode option");

        assert_eq!(
            option,
            AskOption {
                label: "safe".to_owned(),
                description: None,
                caution: None,
            }
        );
    }

    #[test]
    fn ask_option_deserializes_object_shape() {
        let option: AskOption =
            serde_json::from_value(json!({"label": "safe", "description": "Use staged rollout"}))
                .expect("decode option");

        assert_eq!(
            option,
            AskOption {
                label: "safe".to_owned(),
                description: Some("Use staged rollout".to_owned()),
                caution: None,
            }
        );
    }

    #[test]
    fn ask_option_deserializes_object_without_description() {
        let option: AskOption =
            serde_json::from_value(json!({"label": "safe"})).expect("decode option");

        assert_eq!(option, AskOption::from("safe".to_owned()));
    }

    #[test]
    fn ask_option_serializes_label_only_as_string_and_description_as_object() {
        assert_eq!(
            serde_json::to_value(AskOption::from("safe".to_owned())).expect("serialize option"),
            json!("safe")
        );
        assert_eq!(
            serde_json::to_value(AskOption {
                label: "safe".to_owned(),
                description: Some("Use staged rollout".to_owned()),
                caution: None,
            })
            .expect("serialize option"),
            json!({"label": "safe", "description": "Use staged rollout"})
        );
    }
}
