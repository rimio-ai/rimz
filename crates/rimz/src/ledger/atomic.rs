//! Disk-write primitives.
//!
//! Two helpers cover every durable write in the project:
//!
//! - [`write_temp_then_rename`] for feed files, snapshots, and heartbeats.
//! - [`append_framed_record`] for the event log, with `fsync` per record.
//!
//! No module hand-rolls its own atomic dance.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AtomicErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AtomicErr>;

/// Write raw bytes to `path` via a same-directory temp file followed by an
/// atomic rename. fsync is applied to the temp file before the rename.
/// Used by writers (TOML, anything pre-serialised) that own their own
/// encoding; JSON callers prefer [`write_temp_then_rename`].
#[must_use = "durability barrier; check the result"]
pub fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = temp_sibling(path);
    {
        let mut file = File::create(&tmp).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        file.write_all(bytes).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        file.sync_all().map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    sync_parent_dir(path)?;
    Ok(())
}

/// Write `value` as pretty JSON to `path` via a same-directory temp file
/// followed by an atomic rename. fsync is applied to the temp file before
/// the rename. Caller has already created `path.parent()`.
#[must_use = "durability barrier; check the result"]
pub fn write_temp_then_rename<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = temp_sibling(path);
    {
        let mut file = File::create(&tmp).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n").map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        file.sync_all().map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    sync_parent_dir(path)?;
    Ok(())
}

/// Append a single length-prefixed JSON record to `path`, fsync, return.
///
/// Wire format per record: `<decimal byte length> <space> <json>\n`. Recovery
/// in [`crate::ledger::event_log::read_all`] tolerates a torn trailing
/// record, since that's what a SIGKILL between write and fsync leaves
/// behind.
#[must_use = "durability barrier; check the result"]
pub fn append_framed_record<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let first_create = !path.exists();
    let bytes = serde_json::to_vec(value)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| AtomicErr::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    // One write() call per record so EWOULDBLOCK on partial doesn't fragment.
    let mut line = Vec::with_capacity(bytes.len() + 16);
    line.extend_from_slice(bytes.len().to_string().as_bytes());
    line.push(b' ');
    line.extend_from_slice(&bytes);
    line.push(b'\n');
    file.write_all(&line).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    file.sync_data().map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if first_create {
        sync_parent_dir(path)?;
    }
    Ok(())
}

fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let dir = File::open(parent).map_err(|e| AtomicErr::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;
    dir.sync_all().map_err(|e| AtomicErr::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn temp_sibling(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let nonce = uuid::Uuid::now_v7().simple();
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp.{pid}.{nonce}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn temp_rename_writes_pretty_json_with_trailing_newline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/file.json");
        write_temp_then_rename(&path, &json!({ "a": 1, "b": "two" })).unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert!(read.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&read).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn appended_records_round_trip_framing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        append_framed_record(&path, &json!({ "a": 1 })).unwrap();
        append_framed_record(&path, &json!({ "b": 2 })).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let mut lines = text.lines();
        let first = lines.next().unwrap();
        let (len_str, rest) = first.split_once(' ').unwrap();
        let len: usize = len_str.parse().unwrap();
        assert_eq!(rest.len(), len);
        let second = lines.next().unwrap();
        assert!(second.contains("\"b\""));
    }
}
