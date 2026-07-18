//! OpenCode SQLite conversation normalization.
//!
//! OpenCode stores message metadata and ordered content parts in separate
//! tables. Reads stay scoped to one session because every OpenCode agent on a
//! machine shares the same database. Incremental output advances only across
//! completed assistant messages, so an in-place update that completes a live
//! message remains visible to the next poll.

use std::io;
use std::path::Path;

use jiff::Timestamp;
use rusqlite::{Connection, Rows, params};
use serde::Deserialize;
use serde_json::Value;

use super::database::{MessageTime, open_readonly};
use crate::agents::sanitize_user_prompt;
use crate::agents::transcript::{
    TranscriptMessage, TranscriptPage, TranscriptPosition, TranscriptRole,
};
use crate::agents::transcript_fs::{
    deserialize_optional_object_lossy, deserialize_optional_string_lossy,
};
use crate::ids::AgentSessionId;

const COMPLETE_ASSISTANT: &str = "json_valid(m.data) AND json_extract(m.data, '$.role') = 'assistant' AND (json_extract(m.data, '$.time.completed') IS NOT NULL OR json_extract(m.data, '$.finish') IS NOT NULL OR json_extract(m.data, '$.error') IS NOT NULL)";

#[derive(Deserialize)]
struct MessageData {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    role: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    time: Option<MessageTime>,
    summary: Option<Value>,
}

#[derive(Deserialize)]
struct PartData {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    part_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    text: Option<String>,
    synthetic: Option<Value>,
    ignored: Option<Value>,
}

struct MessageBuilder {
    role: TranscriptRole,
    at: Option<Timestamp>,
    text_parts: Vec<String>,
}

pub(super) fn read_messages(
    path: &Path,
    session_id: Option<&AgentSessionId>,
) -> io::Result<Vec<TranscriptMessage>> {
    let session_id = require_session_id(session_id)?;
    let conn = connection(path)?;
    let primary = concat!(
        "SELECT m.rowid, m.time_created, m.data, p.data ",
        "FROM message m LEFT JOIN part p ON p.message_id = m.id ",
        "WHERE m.session_id = ?1 ",
        "ORDER BY m.time_created, m.rowid, p.time_created, p.rowid"
    );
    let fallback = concat!(
        "SELECT m.rowid, NULL, m.data, p.data ",
        "FROM message m LEFT JOIN part p ON p.message_id = m.id ",
        "WHERE m.session_id = ?1 ORDER BY m.rowid, p.rowid"
    );
    let mut stmt = conn
        .prepare(primary)
        .or_else(|_| conn.prepare(fallback))
        .map_err(sqlite_io)?;
    let rows = stmt.query([session_id.as_str()]).map_err(sqlite_io)?;
    collect_messages(rows).map(|(messages, _)| messages)
}

pub(super) fn position(
    path: &Path,
    session_id: Option<&AgentSessionId>,
) -> Option<TranscriptPosition> {
    let session_id = session_id?;
    let conn = open_readonly(path)?;
    let sql = format!(
        "SELECT COALESCE(MAX(m.rowid), 0) FROM message m WHERE m.session_id = ?1 AND {COMPLETE_ASSISTANT}"
    );
    let rowid = conn
        .query_row(&sql, [session_id.as_str()], |row| row.get::<_, i64>(0))
        .ok()?;
    Some(TranscriptPosition::new(rowid.max(0) as u64))
}

pub(super) fn read_assistant_page(
    path: &Path,
    session_id: Option<&AgentSessionId>,
    position: TranscriptPosition,
) -> Option<TranscriptPage> {
    let session_id = session_id?;
    let conn = open_readonly(path)?;
    let primary = format!(
        concat!(
            "SELECT m.rowid, m.time_created, m.data, p.data ",
            "FROM message m LEFT JOIN part p ON p.message_id = m.id ",
            "WHERE m.session_id = ?1 AND m.rowid > ?2 AND {} ",
            "ORDER BY m.rowid, p.time_created, p.rowid"
        ),
        COMPLETE_ASSISTANT
    );
    let fallback = format!(
        concat!(
            "SELECT m.rowid, NULL, m.data, p.data ",
            "FROM message m LEFT JOIN part p ON p.message_id = m.id ",
            "WHERE m.session_id = ?1 AND m.rowid > ?2 AND {} ",
            "ORDER BY m.rowid, p.rowid"
        ),
        COMPLETE_ASSISTANT
    );
    let mut stmt = conn
        .prepare(&primary)
        .or_else(|_| conn.prepare(&fallback))
        .ok()?;
    let rows = stmt
        .query(params![session_id.as_str(), position.get() as i64])
        .ok()?;
    let (messages, max_rowid) = collect_messages(rows).ok()?;
    Some(TranscriptPage {
        next: TranscriptPosition::new(max_rowid.max(position.get())),
        messages: messages.into_iter().map(|message| message.text).collect(),
    })
}

pub(super) fn last_assistant_message(path: &Path, session_id: &AgentSessionId) -> Option<String> {
    let end = position(path, Some(session_id))?;
    if end == TranscriptPosition::START {
        return None;
    }
    read_assistant_page(
        path,
        Some(session_id),
        TranscriptPosition::new(end.get().saturating_sub(1)),
    )?
    .messages
    .pop()
}

fn collect_messages(mut rows: Rows<'_>) -> io::Result<(Vec<TranscriptMessage>, u64)> {
    let mut messages = Vec::new();
    let mut current_rowid = None;
    let mut current = None;
    let mut max_rowid = 0;

    while let Some(row) = rows.next().map_err(sqlite_io)? {
        let rowid = row.get::<_, i64>(0).map_err(sqlite_io)?.max(0) as u64;
        max_rowid = max_rowid.max(rowid);
        if current_rowid != Some(rowid) {
            push_message(&mut messages, current.take());
            current_rowid = Some(rowid);
            let table_created = row.get::<_, Option<i64>>(1).map_err(sqlite_io)?;
            let data = row.get::<_, String>(2).map_err(sqlite_io)?;
            current = parse_message(&data, table_created);
        }
        let part = row.get::<_, Option<String>>(3).map_err(sqlite_io)?;
        if let (Some(builder), Some(part)) = (&mut current, part.as_deref())
            && let Some(text) = parse_text_part(part)
        {
            builder.text_parts.push(text);
        }
    }
    push_message(&mut messages, current);
    Ok((messages, max_rowid))
}

fn parse_message(data: &str, table_created: Option<i64>) -> Option<MessageBuilder> {
    let data = serde_json::from_str::<MessageData>(data).ok()?;
    if matches!(data.summary, Some(Value::Bool(true))) {
        return None;
    }
    let role = match data.role.as_deref() {
        Some("user") => TranscriptRole::User,
        Some("assistant") => TranscriptRole::Assistant,
        _ => return None,
    };
    let created = data
        .time
        .and_then(|time| time.created)
        .and_then(|millis| i64::try_from(millis).ok())
        .or(table_created);
    Some(MessageBuilder {
        role,
        at: created.and_then(|millis| Timestamp::from_millisecond(millis).ok()),
        text_parts: Vec::new(),
    })
}

fn parse_text_part(data: &str) -> Option<String> {
    let part = serde_json::from_str::<PartData>(data).ok()?;
    if part.part_type.as_deref() != Some("text")
        || matches!(part.synthetic, Some(Value::Bool(true)))
        || matches!(part.ignored, Some(Value::Bool(true)))
    {
        return None;
    }
    part.text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn push_message(messages: &mut Vec<TranscriptMessage>, builder: Option<MessageBuilder>) {
    let Some(builder) = builder else {
        return;
    };
    let visible = builder.text_parts.join("\n");
    let text = match builder.role {
        TranscriptRole::User => sanitize_user_prompt(Some(&visible)),
        TranscriptRole::Assistant => (!visible.is_empty()).then_some(visible),
    };
    if let Some(text) = text {
        messages.push(TranscriptMessage {
            role: builder.role,
            at: builder.at,
            text,
        });
    }
}

fn require_session_id(session_id: Option<&AgentSessionId>) -> io::Result<&AgentSessionId> {
    session_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "OpenCode transcript reads require an agent session id",
        )
    })
}

fn connection(path: &Path) -> io::Result<Connection> {
    open_readonly(path).ok_or_else(|| {
        io::Error::other(format!(
            "opening OpenCode transcript database `{}` read-only",
            path.display()
        ))
    })
}

fn sqlite_io(error: rusqlite::Error) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentAdapter as _, OpencodeAdapter, PriceBook};

    fn create_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        (dir, path)
    }

    fn insert_message(path: &Path, id: &str, session_id: &str, at: i64, data: &str) {
        Connection::open(path)
            .unwrap()
            .execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?3, ?4)",
                params![id, session_id, at, data],
            )
            .unwrap();
    }

    fn insert_part(path: &Path, id: &str, message_id: &str, session_id: &str, at: i64, data: &str) {
        Connection::open(path)
            .unwrap()
            .execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                params![id, message_id, session_id, at, data],
            )
            .unwrap();
    }

    #[test]
    fn reads_one_sessions_visible_conversation_in_part_order() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-user",
            "ses-main",
            1_780_590_149_000,
            r#"{"role":"user","time":{"created":"1780590149000"},"summary":{"title":"Fix parser"}}"#,
        );
        insert_part(
            &path,
            "part-user",
            "msg-user",
            "ses-main",
            1_780_590_149_001,
            r#"{"type":"text","text":"  fix the parser  "}"#,
        );
        insert_message(
            &path,
            "msg-assistant",
            "ses-main",
            1_780_590_150_000,
            r#"{"role":"assistant","time":{"created":1780590150000,"completed":1780590151000},"finish":"stop"}"#,
        );
        insert_part(
            &path,
            "part-answer-1",
            "msg-assistant",
            "ses-main",
            1_780_590_150_001,
            r#"{"type":"text","text":"First line"}"#,
        );
        insert_part(
            &path,
            "part-reasoning",
            "msg-assistant",
            "ses-main",
            1_780_590_150_002,
            r#"{"type":"reasoning","text":"hidden"}"#,
        );
        insert_part(
            &path,
            "part-answer-2",
            "msg-assistant",
            "ses-main",
            1_780_590_150_003,
            r#"{"type":"text","text":"Second line"}"#,
        );
        insert_part(
            &path,
            "part-synthetic",
            "msg-assistant",
            "ses-main",
            1_780_590_150_004,
            r#"{"type":"text","text":"control","synthetic":true}"#,
        );
        insert_message(
            &path,
            "msg-other",
            "ses-other",
            1_780_590_151_000,
            r#"{"role":"assistant","time":{"created":1780590151000,"completed":1780590152000}}"#,
        );
        insert_part(
            &path,
            "part-other",
            "msg-other",
            "ses-other",
            1_780_590_151_001,
            r#"{"type":"text","text":"other session"}"#,
        );

        let messages = read_messages(&path, Some(&AgentSessionId::from("ses-main"))).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, TranscriptRole::User);
        assert_eq!(messages[0].text, "fix the parser");
        assert_eq!(messages[1].role, TranscriptRole::Assistant);
        assert_eq!(messages[1].text, "First line\nSecond line");
        assert!(messages.iter().all(|message| message.at.is_some()));
    }

    #[test]
    fn cursor_waits_for_completion_and_reads_updated_final_text_once() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-old",
            "ses-main",
            1_000,
            r#"{"role":"assistant","time":{"created":1000,"completed":1100}}"#,
        );
        insert_part(
            &path,
            "part-old",
            "msg-old",
            "ses-main",
            1_001,
            r#"{"type":"text","text":"old"}"#,
        );
        let session_id = AgentSessionId::from("ses-main");
        let path_text = path.to_string_lossy().into_owned();
        let mut cursor = crate::agents::transcript::TranscriptCursor::new(false);
        assert!(
            cursor
                .messages(Some(&path_text), Some(&session_id), &OpencodeAdapter)
                .is_empty()
        );

        insert_message(&path, "msg-malformed", "ses-main", 1_500, "{");

        insert_message(
            &path,
            "msg-live",
            "ses-main",
            2_000,
            r#"{"role":"assistant","time":{"created":2000}}"#,
        );
        insert_part(
            &path,
            "part-live",
            "msg-live",
            "ses-main",
            2_001,
            r#"{"type":"text","text":"partial"}"#,
        );
        assert!(
            cursor
                .messages(Some(&path_text), Some(&session_id), &OpencodeAdapter)
                .is_empty()
        );

        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE message SET data = ?1, time_updated = 2100 WHERE id = 'msg-live'",
            [r#"{"role":"assistant","time":{"created":2000,"completed":2100},"finish":"stop"}"#],
        )
        .unwrap();
        conn.execute(
            "UPDATE part SET data = ?1, time_updated = 2100 WHERE id = 'part-live'",
            [r#"{"type":"text","text":"final answer"}"#],
        )
        .unwrap();

        assert_eq!(
            cursor.messages(Some(&path_text), Some(&session_id), &OpencodeAdapter),
            ["final answer"]
        );
        assert!(
            cursor
                .messages(Some(&path_text), Some(&session_id), &OpencodeAdapter)
                .is_empty()
        );

        insert_message(
            &path,
            "msg-other",
            "ses-other",
            3_000,
            r#"{"role":"assistant","time":{"created":3000,"completed":3100}}"#,
        );
        insert_part(
            &path,
            "part-other",
            "msg-other",
            "ses-other",
            3_001,
            r#"{"type":"text","text":"other final"}"#,
        );
        assert_eq!(
            cursor.messages(
                Some(&path_text),
                Some(&AgentSessionId::from("ses-other")),
                &OpencodeAdapter,
            ),
            ["other final"],
            "a session change resets the cursor even though the database path is shared"
        );
    }

    #[test]
    fn normalized_conversation_and_spend_produce_one_history_turn() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-user",
            "ses-main",
            1_780_590_149_000,
            r#"{"role":"user","time":{"created":1780590149000}}"#,
        );
        insert_part(
            &path,
            "part-user",
            "msg-user",
            "ses-main",
            1_780_590_149_001,
            r#"{"type":"text","text":"fix history"}"#,
        );
        insert_message(
            &path,
            "msg-assistant",
            "ses-main",
            1_780_590_150_000,
            r#"{"role":"assistant","modelID":"gpt-priced","providerID":"openai","cost":0.42,"tokens":{"input":100,"output":50},"time":{"created":1780590150000,"completed":1780590151000},"finish":"stop"}"#,
        );
        insert_part(
            &path,
            "part-assistant",
            "msg-assistant",
            "ses-main",
            1_780_590_150_001,
            r#"{"type":"text","text":"done"}"#,
        );
        let adapter = OpencodeAdapter;
        let session_id = AgentSessionId::from("ses-main");
        let messages = adapter
            .read_transcript_messages(&path, Some(&session_id))
            .unwrap();
        let spend = adapter.parse_spend(&path, None, &PriceBook::embedded());

        let turns = crate::agents::turns::session_turns(
            &messages,
            &spend.entries,
            session_id.as_str(),
            false,
        );

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt, "fix history");
        assert_eq!(turns[0].fresh_input, 100);
        assert_eq!(turns[0].output, 50);
        assert_eq!(turns[0].cost_usd, Some(0.42));
        assert_eq!(turns[0].outcome, crate::agents::turns::TurnOutcome::Done);
    }

    #[test]
    fn adapter_reads_the_last_completed_reply_for_turn_end() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-assistant",
            "ses-main",
            2_000,
            r#"{"role":"assistant","time":{"created":2000,"completed":2100},"finish":"stop"}"#,
        );
        insert_part(
            &path,
            "part-assistant",
            "msg-assistant",
            "ses-main",
            2_001,
            r#"{"type":"text","text":"completed reply"}"#,
        );
        assert_eq!(
            OpencodeAdapter
                .decode_hook(
                    "session_idle",
                    &serde_json::json!({"session_id":"ses-main","transcript_path":path}),
                )
                .expect("test hook decodes")
                .final_message(),
            Some("completed reply".to_owned())
        );
        assert_eq!(
            OpencodeAdapter
                .decode_hook(
                    "chat_message",
                    &serde_json::json!({"session_id":"ses-main","transcript_path":path}),
                )
                .expect("test hook decodes")
                .final_message(),
            None
        );
    }

    #[test]
    fn complete_read_requires_a_session_id() {
        let (_dir, path) = create_db();
        let error = read_messages(&path, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
