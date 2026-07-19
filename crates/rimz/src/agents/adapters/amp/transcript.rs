//! Whole-object transcript reads for Amp's rewritten private thread cache.

use std::io;
use std::path::Path;

use crate::agents::{TranscriptMessage, TranscriptPage, TranscriptPosition};
use crate::ids::AgentSessionId;

use super::thread::AmpThread;

pub(super) fn read_messages(
    path: &Path,
    session_id: Option<&AgentSessionId>,
) -> io::Result<Vec<TranscriptMessage>> {
    let thread = AmpThread::read(path)?;
    validate_session(&thread, session_id)?;
    Ok(thread.transcript_messages())
}

pub(super) fn position(
    path: &Path,
    session_id: Option<&AgentSessionId>,
) -> Option<TranscriptPosition> {
    let thread = AmpThread::read(path).ok()?;
    validate_session(&thread, session_id).ok()?;
    Some(TranscriptPosition::new(
        thread.completed_assistant_messages().len() as u64,
    ))
}

pub(super) fn read_assistant_page(
    path: &Path,
    session_id: Option<&AgentSessionId>,
    position: TranscriptPosition,
) -> Option<TranscriptPage> {
    let thread = AmpThread::read(path).ok()?;
    validate_session(&thread, session_id).ok()?;
    let messages = thread.completed_assistant_messages();
    let current = messages.len() as u64;
    let start = if position.get() > current {
        0
    } else {
        position.get() as usize
    };
    Some(TranscriptPage {
        next: TranscriptPosition::new(current),
        messages: messages.into_iter().skip(start).collect(),
    })
}

fn validate_session(thread: &AmpThread, session_id: Option<&AgentSessionId>) -> io::Result<()> {
    if session_id.is_some_and(|session_id| session_id.as_str() != thread.id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Amp transcript thread id does not match the requested session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn whole_file_growth_partial_completion_and_replacement_use_logical_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-a.json");
        write(
            &path,
            r#"{"id":"T-a","messages":[{"role":"assistant","messageId":"a","content":"old","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","outputTokens":1}},{"role":"assistant","messageId":"b","content":"partial"}]}"#,
        );
        assert_eq!(position(&path, None), Some(TranscriptPosition::new(1)));
        assert_eq!(
            read_assistant_page(&path, None, TranscriptPosition::START)
                .unwrap()
                .messages,
            vec!["old"]
        );

        write(
            &path,
            r#"{"id":"T-a","messages":[{"role":"assistant","messageId":"a","content":"old","usage":{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","outputTokens":1}},{"role":"assistant","messageId":"b","content":"new","usage":{"timestamp":"2026-01-01T00:00:01Z","model":"gpt-5","outputTokens":1}}]}"#,
        );
        let page = read_assistant_page(&path, None, TranscriptPosition::new(1)).unwrap();
        assert_eq!(page.next, TranscriptPosition::new(2));
        assert_eq!(page.messages, vec!["new"]);

        write(
            &path,
            r#"{"id":"T-a","messages":[{"role":"assistant","messageId":"z","content":"replacement","usage":{"timestamp":"2026-01-01T00:00:02Z","model":"gpt-5","outputTokens":1}}]}"#,
        );
        assert_eq!(
            read_assistant_page(&path, None, TranscriptPosition::new(2))
                .unwrap()
                .messages,
            vec!["replacement"]
        );
    }

    #[test]
    fn explicit_session_mismatch_and_malformed_roots_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thread.json");
        write(&path, r#"{"id":"T-a","messages":[]}"#);
        let other = AgentSessionId::from("T-other");
        assert_eq!(
            read_messages(&path, Some(&other)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(position(&path, Some(&other)), None);

        write(&path, "{");
        assert_eq!(
            read_messages(&path, None).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
