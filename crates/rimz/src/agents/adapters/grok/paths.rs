//! Grok Build per-user paths and session transcript validation.

use std::path::{Component, Path, PathBuf};

use crate::agents::{AgentErr, Result};

pub(super) fn home() -> PathBuf {
    std::env::var_os("GROK_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::agents::transcript_fs::home_dir().join(".grok"))
}

pub(super) fn sessions_root() -> PathBuf {
    home().join("sessions")
}

pub(super) fn auth_path() -> PathBuf {
    home().join("auth.json")
}

pub(super) fn hooks_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RIMZ_GROK_HOOKS").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let root = home();
    if !root.is_absolute() {
        return Err(AgentErr::Install {
            agent: "grok",
            reason: format!("GROK_HOME must be absolute (got {})", root.display()),
        });
    }
    Ok(root.join("hooks/rimz.json"))
}

/// Accept only Grok's durable main-session update file for this exact id.
pub(super) fn validate_transcript(path: &Path, session_id: &str) -> Option<PathBuf> {
    validate_transcript_under(path, session_id, &sessions_root())
}

/// Resolve the optional event sidecar beside an already validated transcript.
pub(super) fn events_companion(path: &Path, session_id: &str) -> Option<PathBuf> {
    events_companion_under(path, session_id, &sessions_root())
}

fn events_companion_under(path: &Path, session_id: &str, sessions_root: &Path) -> Option<PathBuf> {
    let transcript = validate_transcript_under(path, session_id, sessions_root)?;
    let session_dir = transcript.parent()?;
    if session_dir.file_name()?.to_str()? != session_id.trim() {
        return None;
    }
    let events = session_dir.join("events.jsonl").canonicalize().ok()?;
    (events.is_file() && events.parent() == Some(session_dir)).then_some(events)
}

fn validate_transcript_under(
    path: &Path,
    session_id: &str,
    sessions_root: &Path,
) -> Option<PathBuf> {
    let session_id = session_id.trim();
    if session_id.is_empty()
        || !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || path.file_name()? != "updates.jsonl"
        || path.parent()?.file_name()?.to_str()? != session_id
    {
        return None;
    }
    let path = path.canonicalize().ok()?;
    let root = sessions_root.canonicalize().ok()?;
    (path.is_file() && path.starts_with(root)).then_some(path)
}

pub(super) fn resolve_transcript(
    session_id: &str,
    current: Option<&Path>,
    prior: Option<&Path>,
    files: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    current
        .and_then(|path| validate_transcript(path, session_id))
        .or_else(|| prior.and_then(|path| validate_transcript(path, session_id)))
        .or_else(|| transcript_for_session(session_id, files))
}

pub(super) fn transcript_for_session(
    session_id: &str,
    files: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    files.into_iter().find(|path| {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            == Some(session_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_validation_requires_matching_session_directory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grok");
        let path = home.join("sessions/%2Fworkspace/session-1/updates.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        assert_eq!(
            validate_transcript_under(&path, "session-1", &home.join("sessions")),
            path.canonicalize().ok()
        );
        assert!(validate_transcript_under(&path, "session-2", &home.join("sessions")).is_none());
        assert!(
            validate_transcript_under(&home.join("auth.json"), "session-1", &home.join("sessions"))
                .is_none()
        );
    }

    #[test]
    fn events_companion_requires_a_regular_sibling_in_the_validated_session() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let session = root.join("%2Fworkspace/session-1");
        let updates = session.join("updates.jsonl");
        let events = session.join("events.jsonl");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(&updates, "").unwrap();

        assert!(events_companion_under(&updates, "session-1", &root).is_none());
        std::fs::write(&events, "").unwrap();
        assert_eq!(
            events_companion_under(&updates, "session-1", &root),
            events.canonicalize().ok()
        );
        assert!(events_companion_under(&updates, "session-2", &root).is_none());
        assert!(events_companion_under(&events, "session-1", &root).is_none());

        std::fs::remove_file(&events).unwrap();
        std::fs::create_dir(&events).unwrap();
        assert!(events_companion_under(&updates, "session-1", &root).is_none());

        let outside = temp.path().join("outside/session-1/updates.jsonl");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, "").unwrap();
        std::fs::write(outside.parent().unwrap().join("events.jsonl"), "").unwrap();
        assert!(events_companion_under(&outside, "session-1", &root).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn events_companion_rejects_symlinks_outside_the_session() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let session = root.join("%2Fworkspace/session-1");
        let updates = session.join("updates.jsonl");
        let outside = temp.path().join("outside-events.jsonl");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(&updates, "").unwrap();
        std::fs::write(&outside, "").unwrap();
        symlink(&outside, session.join("events.jsonl")).unwrap();

        assert!(events_companion_under(&updates, "session-1", &root).is_none());
    }
}
