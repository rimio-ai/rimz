//! OpenCode SQLite conversation normalization.
//!
//! OpenCode stores text and ordered content in each session-message row.
//! Reads stay scoped to one session because every OpenCode agent on a
//! machine shares the same database. Incremental output advances only across
//! completed assistant messages, so an in-place update that completes a live
//! message remains visible to the next poll.

use std::io;
use std::path::Path;

use jiff::Timestamp;
use rusqlite::{Connection, Rows, params};
use serde::Deserialize;

use super::database::{MessageTime, open_readonly};
use crate::agents::sanitize_user_prompt;
use crate::agents::transcript::{
    TranscriptMessage, TranscriptPage, TranscriptPosition, TranscriptRole,
};
use crate::agents::transcript_fs::{
    deserialize_optional_object_lossy, deserialize_optional_string_lossy,
};
use crate::ids::AgentSessionId;

const COMPLETE_ASSISTANT: &str = "m.type = 'assistant' AND json_valid(m.data) AND (json_extract(m.data, '$.time.completed') IS NOT NULL OR json_extract(m.data, '$.finish') IS NOT NULL OR json_extract(m.data, '$.error') IS NOT NULL)";

#[derive(Deserialize)]
struct MessageData {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    time: Option<MessageTime>,
    #[serde(default)]
    content: Vec<PartData>,
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
    let sql = concat!(
        "SELECT m.rowid, m.time_created, m.type, m.data ",
        "FROM session_message m WHERE m.session_id = ?1 AND m.type IN ('user', 'assistant') ",
        "ORDER BY m.time_created, m.rowid"
    );
    let mut stmt = conn.prepare(sql).map_err(sqlite_io)?;
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
        "SELECT COALESCE(MAX(m.rowid), 0) FROM session_message m WHERE m.session_id = ?1 AND {COMPLETE_ASSISTANT}"
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
    let sql = format!(
        concat!(
            "SELECT m.rowid, m.time_created, m.type, m.data ",
            "FROM session_message m ",
            "WHERE m.session_id = ?1 AND m.rowid > ?2 AND {} ",
            "ORDER BY m.rowid"
        ),
        COMPLETE_ASSISTANT
    );
    let mut stmt = conn.prepare(&sql).ok()?;
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
    let mut max_rowid = 0;

    while let Some(row) = rows.next().map_err(sqlite_io)? {
        let rowid = row.get::<_, i64>(0).map_err(sqlite_io)?.max(0) as u64;
        max_rowid = max_rowid.max(rowid);
        let table_created = row.get::<_, Option<i64>>(1).map_err(sqlite_io)?;
        let kind = row.get::<_, String>(2).map_err(sqlite_io)?;
        let data = row.get::<_, String>(3).map_err(sqlite_io)?;
        push_message(&mut messages, parse_message(&data, table_created, &kind));
    }
    Ok((messages, max_rowid))
}

fn parse_message(data: &str, table_created: Option<i64>, kind: &str) -> Option<MessageBuilder> {
    let data = serde_json::from_str::<MessageData>(data).ok()?;
    let role = match kind {
        "user" => TranscriptRole::User,
        "assistant" => TranscriptRole::Assistant,
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
        text_parts: match role {
            TranscriptRole::User => data.text.into_iter().collect(),
            TranscriptRole::Assistant => data
                .content
                .into_iter()
                .filter(|part| part.part_type.as_deref() == Some("text"))
                .filter_map(|part| part.text)
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
                .collect(),
        },
    })
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
    use crate::agents::PriceBook;
    use crate::agents::capabilities::{
        HookCapability as _, SpendingCapability as _, TranscriptCapability as _,
    };

    use super::super::OpencodeAdapter;

    fn create_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        (dir, path)
    }

    fn insert_message(path: &Path, id: &str, session_id: &str, kind: &str, at: i64, data: &str) {
        crate::testkit::opencode::OpenCodeDb::new(path, false, false)
            .message(id, session_id, kind, at, data);
    }

    #[test]
    fn reads_one_sessions_visible_conversation_in_part_order() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-user",
            "ses-main",
            "user",
            1_780_590_149_000,
            r#"{"time":{"created":"1780590149000"},"text":"  fix the parser  "}"#,
        );
        insert_message(
            &path,
            "msg-assistant",
            "ses-main",
            "assistant",
            1_780_590_150_000,
            r#"{"time":{"created":1780590150000,"completed":1780590151000},"finish":"stop","content":[{"type":"text","text":"First line"},{"type":"reasoning","text":"hidden"},{"type":"text","text":"Second line"}]}"#,
        );
        insert_message(
            &path,
            "msg-other",
            "ses-other",
            "assistant",
            1_780_590_151_000,
            r#"{"time":{"created":1780590151000,"completed":1780590152000},"content":[{"type":"text","text":"other session"}]}"#,
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
            "assistant",
            1_000,
            r#"{"time":{"created":1000,"completed":1100},"content":[{"type":"text","text":"old"}]}"#,
        );
        let session_id = AgentSessionId::from("ses-main");
        let path_text = path.to_string_lossy().into_owned();
        let mut cursor = crate::agents::transcript::TranscriptCursor::new(false);
        assert!(
            cursor
                .messages(
                    Some(&path_text),
                    Some(&session_id),
                    crate::agents::definition_by_kind("opencode").unwrap()
                )
                .is_empty()
        );

        insert_message(&path, "msg-malformed", "ses-main", "assistant", 1_500, "{");

        insert_message(
            &path,
            "msg-live",
            "ses-main",
            "assistant",
            2_000,
            r#"{"time":{"created":2000},"content":[{"type":"text","text":"partial"}]}"#,
        );
        assert!(
            cursor
                .messages(
                    Some(&path_text),
                    Some(&session_id),
                    crate::agents::definition_by_kind("opencode").unwrap()
                )
                .is_empty()
        );

        crate::testkit::opencode::OpenCodeDb::new(&path, false, false).update_message("msg-live", 2100, r#"{"time":{"created":2000,"completed":2100},"finish":"stop","content":[{"type":"text","text":"final answer"}]}"#);

        assert_eq!(
            cursor.messages(
                Some(&path_text),
                Some(&session_id),
                crate::agents::definition_by_kind("opencode").unwrap()
            ),
            ["final answer"]
        );
        assert!(
            cursor
                .messages(
                    Some(&path_text),
                    Some(&session_id),
                    crate::agents::definition_by_kind("opencode").unwrap()
                )
                .is_empty()
        );

        insert_message(
            &path,
            "msg-other",
            "ses-other",
            "assistant",
            3_000,
            r#"{"time":{"created":3000,"completed":3100},"content":[{"type":"text","text":"other final"}]}"#,
        );
        assert_eq!(
            cursor.messages(
                Some(&path_text),
                Some(&AgentSessionId::from("ses-other")),
                crate::agents::definition_by_kind("opencode").unwrap(),
            ),
            ["other final"],
            "a session change resets the cursor even though the database path is shared"
        );
    }

    #[test]
    fn full_reads_follow_timestamps_while_incremental_reads_follow_rowids() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-newer",
            "ses-main",
            "assistant",
            2_000,
            r#"{"time":{"created":2000,"completed":2100},"finish":"stop","content":[{"type":"text","text":"newer by timestamp"}]}"#,
        );
        insert_message(
            &path,
            "msg-imported",
            "ses-main",
            "assistant",
            1_000,
            r#"{"time":{"created":1000,"completed":1100},"finish":"stop","content":[{"type":"text","text":"older timestamp, higher rowid"}]}"#,
        );
        let session_id = AgentSessionId::from("ses-main");

        let messages = read_messages(&path, Some(&session_id)).unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["older timestamp, higher rowid", "newer by timestamp"]
        );

        assert_eq!(position(&path, Some(&session_id)).unwrap().get(), 2);
        let page = read_assistant_page(
            &path,
            Some(&session_id),
            crate::agents::transcript::TranscriptPosition::START,
        )
        .unwrap();
        assert_eq!(
            page.messages,
            ["newer by timestamp", "older timestamp, higher rowid"]
        );
        assert_eq!(page.next.get(), 2);
        assert_eq!(
            last_assistant_message(&path, &session_id).as_deref(),
            Some("older timestamp, higher rowid")
        );
    }

    #[test]
    fn normalized_conversation_and_spend_produce_one_history_turn() {
        let (_dir, path) = create_db();
        insert_message(
            &path,
            "msg-user",
            "ses-main",
            "user",
            1_780_590_149_000,
            r#"{"time":{"created":1780590149000},"text":"fix history"}"#,
        );
        insert_message(
            &path,
            "msg-assistant",
            "ses-main",
            "assistant",
            1_780_590_150_000,
            r#"{"cost":0.42,"tokens":{"input":100,"output":50},"time":{"created":1780590150000,"completed":1780590151000},"finish":"stop","model":{"id":"gpt-priced","providerID":"openai"},"content":[{"type":"text","text":"done"}]}"#,
        );
        let adapter = OpencodeAdapter;
        let session_id = AgentSessionId::from("ses-main");
        let messages = adapter
            .read_transcript_messages(&path, Some(&session_id))
            .unwrap();
        let spend = adapter.parse_spend(&path, None, &PriceBook::fixture());

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
            "assistant",
            2_000,
            r#"{"time":{"created":2000,"completed":2100},"finish":"stop","content":[{"type":"text","text":"completed reply"}]}"#,
        );
        assert_eq!(
            OpencodeAdapter
                .decode_hook(
                    "session_idle",
                    &serde_json::json!({"session_id":"ses-main","transcript_path":path}),
                )
                .expect("test hook decodes")
                .final_message(),
            Some("completed reply")
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

    #[test]
    fn v2_transcript_reads_text_and_waits_for_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let db = crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        db.message(
            "user",
            "ses",
            "user",
            1000,
            r#"{"text":"question","time":{"created":1000}}"#,
        );
        db.message(
            "synthetic",
            "ses",
            "synthetic",
            1100,
            r#"{"text":"hidden"}"#,
        );
        db.message(
            "assistant",
            "ses",
            "assistant",
            2000,
            r#"{"content":[{"type":"text","text":"partial"}],"time":{"created":2000}}"#,
        );
        let session = AgentSessionId::from("ses");
        let messages = read_messages(&path, Some(&session)).expect("v2 conversation is readable");
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["question", "partial"]
        );
        assert_eq!(
            position(&path, Some(&session)),
            Some(TranscriptPosition::START)
        );
        db.update_message("assistant", 3000, r#"{"content":[{"type":"reasoning","text":"hidden"},{"type":"text","text":"final"},{"type":"text","text":"answer"}],"time":{"created":2000,"completed":3000}}"#);
        let page = read_assistant_page(&path, Some(&session), TranscriptPosition::START).unwrap();
        assert_eq!(page.messages, ["final\nanswer"]);
        assert_eq!(
            last_assistant_message(&path, &session).as_deref(),
            Some("final\nanswer")
        );
        assert!(
            read_assistant_page(&path, Some(&session), page.next)
                .unwrap()
                .messages
                .is_empty()
        );
    }
}
