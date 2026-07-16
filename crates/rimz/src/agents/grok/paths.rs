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
) -> Option<PathBuf> {
    current
        .and_then(|path| validate_transcript(path, session_id))
        .or_else(|| prior.and_then(|path| validate_transcript(path, session_id)))
        .or_else(|| transcript_for_session(session_id))
}

pub(super) fn transcript_for_session(session_id: &str) -> Option<PathBuf> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    transcript_files().into_iter().find(|path| {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            == Some(session_id)
    })
}

pub(super) fn transcript_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_updates(&sessions_root(), &mut files);
    files.sort();
    files.dedup();
    files
}

fn collect_updates(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_updates(&path, files);
        } else if kind.is_file() && path.file_name().is_some_and(|name| name == "updates.jsonl") {
            files.push(path);
        }
    }
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
}
