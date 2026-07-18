//! OpenCode storage discovery and read-only SQLite access.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::agents::transcript_fs::{
    deserialize_optional_object_lossy, deserialize_optional_string_lossy,
    deserialize_optional_u64_lossy,
};
use crate::agents::{TranscriptCompanionStat, TranscriptStat};

/// Durable identity of the logical SQLite store read through `path`.
///
/// SQLite merges committed frames from the optional WAL when opening the main
/// database, so the WAL participates in invalidation without becoming a
/// separately discovered parse source. The shared-memory file carries only
/// coordination state and intentionally stays outside this stamp.
pub(super) fn logical_stat(path: &Path) -> Option<TranscriptStat> {
    let mut stat = TranscriptStat::from_path(path)?;
    let mut wal_path = path.as_os_str().to_os_string();
    wal_path.push("-wal");
    stat.companion =
        TranscriptStat::from_path(Path::new(&wal_path)).map(TranscriptCompanionStat::from);
    Some(stat)
}

pub(super) fn files() -> Vec<PathBuf> {
    let mut files = data_dirs()
        .into_iter()
        .filter_map(|dir| file_in_dir(&dir))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

pub(super) fn data_dirs() -> Vec<PathBuf> {
    if let Ok(value) = std::env::var("RIMZ_OPENCODE_DATA_DIR") {
        return value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect();
    }

    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("/").join(".local/share"));
    vec![data_home.join("opencode")]
}

pub(super) fn auth_path() -> Option<PathBuf> {
    data_dirs()
        .into_iter()
        .map(|dir| dir.join("auth.json"))
        .find(|path| path.exists())
}

fn file_in_dir(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join("opencode.db");
    if primary.is_file() {
        return Some(primary);
    }

    let mut candidates = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_channel_filename)
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().next()
}

fn is_channel_filename(name: &str) -> bool {
    let Some(channel) = name
        .strip_prefix("opencode-")
        .and_then(|rest| rest.strip_suffix(".db"))
    else {
        return false;
    };
    !channel.is_empty()
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn open_readonly(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

#[derive(Deserialize)]
pub(super) struct MessageTime {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub(super) created: Option<u64>,
}

#[derive(Deserialize)]
struct ProviderMetadata {
    #[serde(
        rename = "providerID",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    provider_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    time: Option<MessageTime>,
}

pub(super) fn latest_message_provider() -> Option<String> {
    files()
        .into_iter()
        .filter_map(|path| latest_provider_in_file(&path))
        .max_by_key(|(created, _)| *created)
        .map(|(_, provider)| provider)
}

fn latest_provider_in_file(path: &Path) -> Option<(u64, String)> {
    let conn = open_readonly(path)?;
    let mut statement = conn
        .prepare("SELECT data FROM message ORDER BY rowid DESC LIMIT 100")
        .ok()?;
    let mut rows = statement.query([]).ok()?;
    while let Ok(Some(row)) = rows.next() {
        let Ok(data) = row.get::<_, String>(0) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_str::<ProviderMetadata>(&data) else {
            continue;
        };
        if let Some(provider) = metadata.provider_id {
            return Some((
                metadata.time.and_then(|time| time.created).unwrap_or(0),
                provider,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_db(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)",
                [],
            )
            .unwrap();
        path
    }

    fn insert_message(path: &Path, data: &str) {
        Connection::open(path)
            .unwrap()
            .execute(
                "INSERT INTO message (id, session_id, data) VALUES ('msg', 'ses', ?1)",
                [data],
            )
            .unwrap();
    }

    #[test]
    fn discovery_prefers_primary_then_sorted_channel_database() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("opencode-beta.db"), "").unwrap();
        std::fs::write(dir.path().join("opencode-alpha.db"), "").unwrap();
        assert_eq!(
            file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode-alpha.db"
        );
        std::fs::write(dir.path().join("opencode.db"), "").unwrap();
        assert_eq!(
            file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode.db"
        );
        std::fs::write(dir.path().join("opencode-bad!.db"), "").unwrap();
        assert!(!is_channel_filename("opencode-bad!.db"));
    }

    #[test]
    fn latest_provider_uses_tolerant_minimal_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{"providerID":"anthropic","time":{"created":"1000"},"tokens":false}"#,
        );
        insert_message(
            &path,
            r#"{"providerID":"openai","time":{"created":2000},"cost":"unknown"}"#,
        );
        insert_message(&path, "not json");
        assert_eq!(
            latest_provider_in_file(&path).map(|(_, provider)| provider),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn opencode_logical_stat_tracks_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode-channel.db");
        std::fs::write(&path, b"main").unwrap();

        let primary = TranscriptStat::from_path(&path).unwrap();
        assert_eq!(logical_stat(&path).unwrap(), primary);

        let wal_path = dir.path().join("opencode-channel.db-wal");
        std::fs::write(&wal_path, b"a").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&wal_path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(12_345, 100))
            .unwrap();
        let appeared = logical_stat(&path).unwrap();
        assert_eq!(
            TranscriptStat {
                companion: None,
                ..appeared
            },
            primary
        );
        assert!(appeared.companion.is_some());

        std::fs::write(&wal_path, b"b").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&wal_path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(12_345, 200))
            .unwrap();
        let changed = logical_stat(&path).unwrap();
        assert_ne!(changed, appeared);
        assert_eq!(changed.companion.unwrap().mtime_nanos, 200);

        std::fs::write(dir.path().join("opencode-channel.db-shm"), b"coordination").unwrap();
        assert_eq!(logical_stat(&path).unwrap(), changed);

        std::fs::remove_file(&wal_path).unwrap();
        assert_eq!(logical_stat(&path).unwrap(), primary);
    }
}
