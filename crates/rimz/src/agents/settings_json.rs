//! Strict JSON-object settings I/O shared by hook installers.

use std::path::Path;

use serde_json::{Map, Value};

use crate::store::atomic;

use super::{AgentErr, Result};

pub(crate) fn read_json_object(agent: &'static str, path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent,
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            match value {
                Value::Object(root) => Ok(root),
                other => Err(AgentErr::Install {
                    agent,
                    reason: format!(
                        "expected JSON object at the top level of {}; found {}",
                        path.display(),
                        json_type_name(&other)
                    ),
                }),
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent,
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn render_json(agent: &'static str, root: &Map<String, Value>) -> Result<String> {
    let text = serde_json::to_string_pretty(&Value::Object(root.clone())).map_err(|source| {
        AgentErr::InstallSerialize {
            agent,
            source: Box::new(source),
        }
    })?;
    Ok(format!("{text}\n"))
}

pub(crate) fn write_json(
    agent: &'static str,
    path: &Path,
    root: &Map<String, Value>,
) -> Result<()> {
    atomic::write_bytes_atomically(path, render_json(agent, root)?.as_bytes())?;
    Ok(())
}

pub(crate) fn read_optional_bytes(agent: &'static str, path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentErr::InstallIo {
            agent,
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) struct PendingWrite<'a> {
    path: &'a Path,
    candidate: Option<&'a [u8]>,
}

impl<'a> PendingWrite<'a> {
    pub(crate) fn required(path: &'a Path, candidate: &'a str) -> Self {
        Self {
            path,
            candidate: Some(candidate.as_bytes()),
        }
    }

    pub(crate) fn optional(path: &'a Path, candidate: Option<&'a str>) -> Self {
        Self {
            path,
            candidate: candidate.map(str::as_bytes),
        }
    }
}

pub(crate) fn commit_pair(
    agent: &'static str,
    first: PendingWrite<'_>,
    second: PendingWrite<'_>,
    first_original: Option<&[u8]>,
    second_original: Option<&[u8]>,
) -> Result<()> {
    commit_pair_with(
        agent,
        first,
        second,
        first_original,
        second_original,
        atomic::write_bytes_atomically,
    )
}

fn commit_pair_with(
    agent: &'static str,
    first: PendingWrite<'_>,
    second: PendingWrite<'_>,
    first_original: Option<&[u8]>,
    second_original: Option<&[u8]>,
    mut write: impl FnMut(&Path, &[u8]) -> atomic::Result<()>,
) -> Result<()> {
    let first_written = if let Some(candidate) = first.candidate {
        write(first.path, candidate)?;
        true
    } else {
        false
    };
    if let Some(candidate) = second.candidate
        && let Err(error) = write(second.path, candidate)
    {
        let second_rollback = restore(second.path, second_original, &mut write);
        let first_rollback = first_written.then(|| restore(first.path, first_original, &mut write));
        let first_rollback_error = match first_rollback {
            Some(Err(error)) => Some(error),
            Some(Ok(())) | None => None,
        };
        let rollback_errors = [second_rollback.err(), first_rollback_error]
            .into_iter()
            .flatten()
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        return if rollback_errors.is_empty() {
            Err(error.into())
        } else {
            Err(AgentErr::Install {
                agent,
                reason: format!(
                    "writing {} failed ({error}); rollback also failed ({})",
                    second.path.display(),
                    rollback_errors.join("; "),
                ),
            })
        };
    }
    Ok(())
}

fn restore(
    path: &Path,
    original: Option<&[u8]>,
    write: &mut impl FnMut(&Path, &[u8]) -> atomic::Result<()>,
) -> atomic::Result<()> {
    if let Some(original) = original {
        write(path, original)
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(atomic::AtomicErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;

    #[test]
    fn second_write_failure_restores_first_file_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("hooks.json");
        let second_path = dir.path().join("cli-config.json");
        let original = b"{  \"user\": true }\n";
        std::fs::write(&first_path, original).unwrap();
        let mut writes = 0;
        let error = commit_pair_with(
            "cursor",
            PendingWrite {
                path: &first_path,
                candidate: Some(b"first candidate"),
            },
            PendingWrite {
                path: &second_path,
                candidate: Some(b"second candidate"),
            },
            Some(original),
            None,
            |path, bytes| {
                writes += 1;
                if writes == 2 {
                    return Err(atomic::AtomicErr::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::other("injected second-write failure"),
                    });
                }
                atomic::write_bytes_atomically(path, bytes)
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected second-write failure"));
        assert_eq!(std::fs::read(first_path).unwrap(), original);
        assert!(!second_path.exists());
    }

    #[test]
    fn uninstall_second_write_failure_restores_both_files_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("hooks.json");
        let second_path = dir.path().join("cli-config.json");
        let first_original = b"{  \"hooks\": {\"user\": []} }\n";
        let second_original = b"{\n  \"statusLine\": \"user status\"\n}\n";
        std::fs::write(&first_path, first_original).unwrap();
        std::fs::write(&second_path, second_original).unwrap();
        let mut writes = 0;
        let error = commit_pair_with(
            "cursor",
            PendingWrite {
                path: &first_path,
                candidate: Some(b"uninstalled hooks"),
            },
            PendingWrite {
                path: &second_path,
                candidate: Some(b"restored statusline"),
            },
            Some(first_original),
            Some(second_original),
            |path, bytes| {
                writes += 1;
                if writes == 2 {
                    return Err(atomic::AtomicErr::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::other("injected uninstall failure"),
                    });
                }
                atomic::write_bytes_atomically(path, bytes)
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected uninstall failure"));
        assert_eq!(std::fs::read(first_path).unwrap(), first_original);
        assert_eq!(std::fs::read(second_path).unwrap(), second_original);
    }
}
