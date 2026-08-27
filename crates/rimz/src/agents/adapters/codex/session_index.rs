//! Codex's append-only session-title index.
//!
//! Codex appends provisional and generated names to `session_index.jsonl`;
//! the newest valid row for a session is authoritative.

use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct SessionIndexRow {
    id: String,
    thread_name: String,
}

pub(super) fn session_name_under(home: &Path, session_id: &str) -> Option<String> {
    std::fs::read_to_string(home.join("session_index.jsonl"))
        .ok()?
        .lines()
        .filter_map(|line| serde_json::from_str::<SessionIndexRow>(line).ok())
        .filter(|row| row.id == session_id)
        .filter_map(|row| {
            let name = row.thread_name.trim();
            (!name.is_empty()).then(|| name.to_owned())
        })
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_valid_title_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            "{\"id\":\"session\",\"thread_name\":\"Provisional title\",\"updated_at\":\"first\"}\n\
             {\"id\":\"other\",\"thread_name\":\"Foreign title\"}\n\
             {\"id\":\"session\",\"thread_name\":\" Generated title \"}\n\
             {truncated\n\
             {\"id\":\"session\",\"thread_name\":\"   \"}\n",
        )
        .unwrap();

        assert_eq!(
            session_name_under(dir.path(), "session").as_deref(),
            Some("Generated title")
        );
    }

    #[test]
    fn missing_index_or_session_has_no_title() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(session_name_under(dir.path(), "session"), None);
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            "{\"id\":\"other\",\"thread_name\":\"Foreign title\"}\n",
        )
        .unwrap();
        assert_eq!(session_name_under(dir.path(), "session"), None);
    }
}
