//! Grok Build per-user paths and session transcript validation.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::agents::capabilities::LaunchCapability;
use crate::agents::{AgentErr, Result};

pub(super) fn home_from(
    configured: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".grok")))
}

fn home(login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    super::GrokAdapter.config_home(login_env)
}

pub(super) fn sessions_root(login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    home(login_env).map(|home| home.join("sessions"))
}

pub(super) fn auth_path(login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    login_env
        .get("GROK_AUTH_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home(login_env).map(|home| home.join("auth.json")))
}

pub(super) fn hooks_path(login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RIMZ_GROK_HOOKS").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let Some(root) = home(login_env) else {
        return Err(AgentErr::Install {
            agent: "grok",
            reason: "$GROK_HOME and $HOME are not set; cannot resolve Grok hooks".to_owned(),
        });
    };
    if !root.is_absolute() {
        return Err(AgentErr::Install {
            agent: "grok",
            reason: format!("GROK_HOME must be absolute (got {})", root.display()),
        });
    }
    Ok(root.join("hooks/rimz.json"))
}

/// Accept only Grok's durable main-session update file for this exact id.
pub(super) fn validate_transcript(
    path: &Path,
    session_id: &str,
    login_env: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    validate_transcript_under(path, session_id, &sessions_root(login_env)?)
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
    login_env: &BTreeMap<String, String>,
    files: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    current
        .and_then(|path| validate_transcript(path, session_id, login_env))
        .or_else(|| prior.and_then(|path| validate_transcript(path, session_id, login_env)))
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
}
