//! Copilot home, transcript, hook, and optional telemetry path resolution.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use super::super::{AgentErr, Result};

const EVENTS_FILE: &str = "events.jsonl";

pub(super) fn copilot_home() -> Option<PathBuf> {
    copilot_home_from(
        std::env::var_os("COPILOT_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(super) fn hooks_path() -> Result<PathBuf> {
    hooks_path_from(
        std::env::var_os("RIMZ_COPILOT_HOOKS").as_deref(),
        std::env::var_os("COPILOT_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(super) fn settings_path() -> Result<PathBuf> {
    settings_path_from(
        std::env::var_os("RIMZ_COPILOT_SETTINGS").as_deref(),
        std::env::var_os("COPILOT_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(super) fn session_transcript_path(session_id: &str) -> Option<PathBuf> {
    session_transcript_path_from(copilot_home().as_deref(), session_id)
}

pub(super) fn validated_transcript_path(path: &Path, session_id: &str) -> Option<PathBuf> {
    validated_transcript_path_from(path, session_id)
}

pub(super) fn otel_source(prior_path: Option<&Path>) -> Option<PathBuf> {
    let exporter_path = std::env::var_os("COPILOT_OTEL_FILE_EXPORTER_PATH");
    if prior_path.is_none_or(|path| !path.is_file())
        && non_empty_path(exporter_path.as_deref()).is_none()
        && otlp_only_config(
            std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").as_deref(),
            std::env::var_os("COPILOT_OTEL_EXPORTER_TYPE").as_deref(),
        )
    {
        return None;
    }
    otel_source_from(
        prior_path,
        exporter_path.as_deref(),
        copilot_home().as_deref(),
    )
}

pub(super) fn otlp_only_config(endpoint: Option<&OsStr>, exporter_type: Option<&OsStr>) -> bool {
    let exporter_type = exporter_type
        .and_then(OsStr::to_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    exporter_type.is_some_and(|value| !value.eq_ignore_ascii_case("file"))
        || (non_empty_path(endpoint).is_some()
            && !exporter_type.is_some_and(|value| value.eq_ignore_ascii_case("file")))
}

fn non_empty_path(raw: Option<&OsStr>) -> Option<PathBuf> {
    raw.filter(|value| !value.is_empty()).map(PathBuf::from)
}

pub(super) fn copilot_home_from(
    copilot_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    non_empty_path(copilot_home).or_else(|| non_empty_path(home).map(|home| home.join(".copilot")))
}

pub(super) fn hooks_path_from(
    override_path: Option<&OsStr>,
    copilot_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf> {
    if let Some(path) = non_empty_path(override_path) {
        return Ok(path);
    }
    copilot_home_from(copilot_home, home)
        .map(|home| home.join("hooks/rimz.json"))
        .ok_or_else(|| AgentErr::Install {
            agent: "copilot",
            reason: "$COPILOT_HOME and $HOME are not set; cannot resolve Copilot hooks".to_owned(),
        })
}

pub(super) fn settings_path_from(
    override_path: Option<&OsStr>,
    copilot_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf> {
    if let Some(path) = non_empty_path(override_path) {
        return Ok(path);
    }
    copilot_home_from(copilot_home, home)
        .map(|home| home.join("settings.json"))
        .ok_or_else(|| AgentErr::Install {
            agent: "copilot",
            reason: "$COPILOT_HOME and $HOME are not set; cannot resolve Copilot settings"
                .to_owned(),
        })
}

pub(super) fn session_transcript_path_from(
    copilot_home: Option<&Path>,
    session_id: &str,
) -> Option<PathBuf> {
    safe_session_component(session_id)?;
    Some(
        copilot_home?
            .join("session-state")
            .join(session_id)
            .join(EVENTS_FILE),
    )
}

pub(super) fn validated_transcript_path_from(path: &Path, session_id: &str) -> Option<PathBuf> {
    safe_session_component(session_id)?;
    (path.file_name() == Some(OsStr::new(EVENTS_FILE))
        && path.parent().and_then(Path::file_name) == Some(OsStr::new(session_id)))
    .then(|| path.to_path_buf())
}

fn safe_session_component(session_id: &str) -> Option<()> {
    let mut components = Path::new(session_id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(value)), None) if !value.is_empty() => Some(()),
        _ => None,
    }
}

pub(super) fn otel_source_from(
    prior_path: Option<&Path>,
    exporter_path: Option<&OsStr>,
    copilot_home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = prior_path.filter(|path| path.is_file()) {
        return Some(path.to_path_buf());
    }
    if let Some(path) = non_empty_path(exporter_path) {
        return Some(path);
    }
    newest_direct_jsonl(&copilot_home?.join("otel"))
}

fn newest_direct_jsonl(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            (metadata.is_file() && path.extension() == Some(OsStr::new("jsonl")))
                .then(|| (metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path))
        })
        .max_by(|left, right| left.cmp(right))
        .map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    #[test]
    fn home_and_hook_precedence_ignore_empty_values() {
        assert_eq!(
            copilot_home_from(
                Some(OsStr::new("/alt/copilot")),
                Some(OsStr::new("/home/user"))
            ),
            Some(PathBuf::from("/alt/copilot"))
        );
        assert_eq!(
            copilot_home_from(Some(OsStr::new("")), Some(OsStr::new("/home/user"))),
            Some(PathBuf::from("/home/user/.copilot"))
        );
        assert_eq!(
            hooks_path_from(
                Some(OsStr::new("/override/rimz.json")),
                Some(OsStr::new("/alt/copilot")),
                None,
            )
            .unwrap(),
            PathBuf::from("/override/rimz.json")
        );
        assert_eq!(
            hooks_path_from(None, Some(OsStr::new("/alt/copilot")), None).unwrap(),
            PathBuf::from("/alt/copilot/hooks/rimz.json")
        );
        assert!(hooks_path_from(None, Some(OsStr::new("")), None).is_err());
        assert_eq!(
            settings_path_from(
                Some(OsStr::new("/override/settings.json")),
                Some(OsStr::new("/alt/copilot")),
                None,
            )
            .unwrap(),
            PathBuf::from("/override/settings.json")
        );
        assert_eq!(
            settings_path_from(None, Some(OsStr::new("/alt/copilot")), None).unwrap(),
            PathBuf::from("/alt/copilot/settings.json")
        );
        assert_eq!(
            settings_path_from(None, None, Some(OsStr::new("/home/user"))).unwrap(),
            PathBuf::from("/home/user/.copilot/settings.json")
        );
        assert!(settings_path_from(None, Some(OsStr::new("")), None).is_err());
    }

    #[test]
    fn transcript_paths_require_one_safe_component_and_matching_native_parent() {
        let home = Path::new("/alt/copilot");
        assert_eq!(
            session_transcript_path_from(Some(home), "session-1"),
            Some(PathBuf::from(
                "/alt/copilot/session-state/session-1/events.jsonl"
            ))
        );
        for unsafe_id in ["", ".", "..", "a/b", "/absolute"] {
            assert!(
                session_transcript_path_from(Some(home), unsafe_id).is_none(),
                "{unsafe_id}"
            );
        }

        let native = Path::new("/elsewhere/session-1/events.jsonl");
        assert_eq!(
            validated_transcript_path_from(native, "session-1"),
            Some(native.to_path_buf())
        );
        assert!(validated_transcript_path_from(native, "session-2").is_none());
        assert!(
            validated_transcript_path_from(
                Path::new("/elsewhere/session-1/other.jsonl"),
                "session-1"
            )
            .is_none()
        );
    }

    #[test]
    fn otel_source_prefers_live_prior_then_explicit_then_newest_direct_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("copilot");
        let otel = home.join("otel");
        std::fs::create_dir_all(otel.join("nested")).unwrap();
        let older = otel.join("older.jsonl");
        let newer = otel.join("newer.jsonl");
        std::fs::write(&older, "{}\n").unwrap();
        std::fs::write(&newer, "{}\n").unwrap();
        std::fs::write(otel.join("ignored.txt"), "{}\n").unwrap();
        std::fs::write(otel.join("nested/ignored.jsonl"), "{}\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&newer)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(2))
            .unwrap();

        assert_eq!(
            otel_source_from(None, None, Some(&home)),
            Some(newer.clone())
        );
        assert_eq!(
            otel_source_from(None, Some(OsStr::new("/explicit.jsonl")), Some(&home)),
            Some(PathBuf::from("/explicit.jsonl"))
        );
        assert_eq!(
            otel_source_from(
                Some(&older),
                Some(OsStr::new("/explicit.jsonl")),
                Some(&home)
            ),
            Some(older)
        );
    }

    #[test]
    fn otlp_only_configuration_is_not_redirected_to_a_home_file() {
        assert!(otlp_only_config(Some(OsStr::new("http://otel")), None));
        assert!(otlp_only_config(None, Some(OsStr::new("otlp-http"))));
        assert!(!otlp_only_config(
            Some(OsStr::new("http://unused")),
            Some(OsStr::new("file"))
        ));
    }
}
