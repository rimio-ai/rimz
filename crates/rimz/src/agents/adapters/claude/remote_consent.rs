//! Claude global-config consent for the remote-control host.
//!
//! `claude remote-control` asks `Enable Remote Control? (y/n)` once per machine
//! and records that the dialog ran as `remoteDialogSeen` in the global config.
//! An unattended host pane blocks on that prompt with nobody to answer it, so
//! RimZ seeds the flag when `[remote_control] claude = true` supplies the
//! operator's intent. Seeding fills an unset value only: an explicit `false`
//! belongs to the operator and RimZ reports it instead of overwriting it.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::store::atomic::{self, AtomicErr};

const CONSENT_KEY: &str = "remoteDialogSeen";
const GLOBAL_CONFIG_FILE: &str = ".claude.json";
const OVERRIDE_ENV: &str = "RIMZ_CLAUDE_GLOBAL_CONFIG";

/// The consent flag as the global config currently records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsentState {
    /// The dialog flag is set; the host starts without prompting.
    Seeded,
    /// No flag yet; the host prompts and an unattended pane stalls.
    Unseeded,
    /// An explicit non-`true` value that RimZ leaves to the operator.
    Refused,
    /// The global config could not be read or parsed as an object.
    Unreadable,
}

/// Where Claude keeps the global config: `$CLAUDE_CONFIG_DIR/.claude.json`,
/// falling back to `$HOME/.claude.json`. Claude reads the first entry of a
/// comma-separated `CLAUDE_CONFIG_DIR`, so this resolves the same one.
pub(crate) fn global_config_path() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(OVERRIDE_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(raw));
    }
    config_base().map(|base| base.join(GLOBAL_CONFIG_FILE))
}

fn config_base() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("CLAUDE_CONFIG_DIR")
        && let Some(first) = configured
            .split(',')
            .map(str::trim)
            .find(|part| !part.is_empty())
    {
        return Some(PathBuf::from(first));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The consent flag and the config recording it, mirroring the shape of
/// [`super::remote_control::read_rc_settings`]. `None` when no home or
/// `CLAUDE_CONFIG_DIR` resolves a path to read.
pub(crate) fn read_consent() -> Option<(PathBuf, ConsentState)> {
    let path = global_config_path()?;
    let state = consent_state(&path);
    Some((path, state))
}

/// Read the consent flag without writing anything.
pub(crate) fn consent_state(path: &Path) -> ConsentState {
    match std::fs::read_to_string(path) {
        Ok(text) => consent_state_from(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ConsentState::Unseeded,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "Claude global config unreadable for the remote-control consent read",
            );
            ConsentState::Unreadable
        }
    }
}

fn consent_state_from(text: &str) -> ConsentState {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else {
        return ConsentState::Unreadable;
    };
    match root.get(CONSENT_KEY) {
        None => ConsentState::Unseeded,
        Some(Value::Bool(true)) => ConsentState::Seeded,
        Some(_) => ConsentState::Refused,
    }
}

/// Set `remoteDialogSeen` when the global config leaves it unset, and report the
/// state the config holds afterwards. Already-seeded configs cost one read and
/// no write, so the steady state never touches a file Claude owns.
pub(crate) fn seed(path: &Path) -> Result<ConsentState, AtomicErr> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(AtomicErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let next = match existing.as_deref() {
        None => format!("{{\n  \"{CONSENT_KEY}\": true\n}}\n"),
        Some(text) => match consent_state_from(text) {
            ConsentState::Seeded => return Ok(ConsentState::Seeded),
            ConsentState::Refused => return Ok(ConsentState::Refused),
            ConsentState::Unreadable => return Ok(ConsentState::Unreadable),
            ConsentState::Unseeded => match insert_consent(text) {
                Some(next) => next,
                None => return Ok(ConsentState::Unreadable),
            },
        },
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|source| AtomicErr::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    atomic::write_bytes_atomically(path, next.as_bytes())?;
    Ok(ConsentState::Seeded)
}

/// Insert the flag as the root object's first member, leaving every other byte
/// in place — the global config is large, Claude owns its key order, and a
/// serde round trip would rewrite the whole file. The result is accepted only
/// when it parses back to the same object plus this one key.
fn insert_consent(text: &str) -> Option<String> {
    let before = root_len(text)?;
    let open = text.find('{')?;
    let (head, tail) = text.split_at(open + 1);
    let entry = if tail.trim_start().starts_with('}') {
        format!("\n  \"{CONSENT_KEY}\": true\n")
    } else {
        format!("\n  \"{CONSENT_KEY}\": true,")
    };
    let next = format!("{head}{entry}{tail}");
    (root_len(&next) == Some(before + 1) && consent_state_from(&next) == ConsentState::Seeded)
        .then_some(next)
}

fn root_len(text: &str) -> Option<usize> {
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Object(root) => Some(root.len()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_state_reads_each_recorded_shape() {
        assert_eq!(
            consent_state_from(r#"{"remoteDialogSeen": true}"#),
            ConsentState::Seeded
        );
        assert_eq!(
            consent_state_from(r#"{"remoteDialogSeen": false}"#),
            ConsentState::Refused
        );
        assert_eq!(
            consent_state_from(r#"{"numStartups": 3}"#),
            ConsentState::Unseeded
        );
        assert_eq!(consent_state_from("{"), ConsentState::Unreadable);
        assert_eq!(consent_state_from("[]"), ConsentState::Unreadable);
    }

    #[test]
    fn insert_keeps_every_other_byte_and_key_order() {
        let original = "{\n  \"numStartups\": 3571,\n  \"installMethod\": \"native\"\n}\n";
        let next = insert_consent(original).expect("insertable");
        assert_eq!(
            next,
            "{\n  \"remoteDialogSeen\": true,\n  \"numStartups\": 3571,\n  \"installMethod\": \"native\"\n}\n"
        );
        assert!(next.ends_with("\"installMethod\": \"native\"\n}\n"));
        assert_eq!(consent_state_from(&next), ConsentState::Seeded);
    }

    #[test]
    fn insert_handles_an_empty_root_object() {
        let next = insert_consent("{}").expect("insertable");
        assert_eq!(consent_state_from(&next), ConsentState::Seeded);
        assert_eq!(root_len(&next), Some(1));
    }

    #[test]
    fn insert_refuses_a_non_object_root() {
        assert_eq!(insert_consent("[1, 2]"), None);
        assert_eq!(insert_consent("not json"), None);
    }

    #[test]
    fn seed_fills_an_unseeded_config_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, "{\n  \"numStartups\": 7\n}\n").expect("write");

        assert_eq!(seed(&path).expect("seed"), ConsentState::Seeded);
        let seeded = std::fs::read_to_string(&path).expect("read");
        assert!(seeded.contains("\"remoteDialogSeen\": true"));
        assert!(seeded.contains("\"numStartups\": 7"));

        assert_eq!(seed(&path).expect("reseed"), ConsentState::Seeded);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), seeded);
    }

    #[test]
    fn seed_creates_a_missing_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join(".claude.json");
        assert_eq!(seed(&path).expect("seed"), ConsentState::Seeded);
        assert_eq!(consent_state(&path), ConsentState::Seeded);
    }

    #[test]
    fn seed_leaves_an_explicit_refusal_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".claude.json");
        let original = "{\n  \"remoteDialogSeen\": false\n}\n";
        std::fs::write(&path, original).expect("write");

        assert_eq!(seed(&path).expect("seed"), ConsentState::Refused);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);
    }

    #[test]
    fn seed_leaves_an_unparseable_config_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, "{ broken").expect("write");

        assert_eq!(seed(&path).expect("seed"), ConsentState::Unreadable);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{ broken");
    }
}
