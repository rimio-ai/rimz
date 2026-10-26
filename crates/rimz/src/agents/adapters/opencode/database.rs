//! OpenCode storage discovery and read-only SQLite access.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::agents::transcript_fs::deserialize_optional_u64_lossy;
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

pub(super) fn newest_file() -> Option<PathBuf> {
    files()
        .into_iter()
        .filter_map(|path| logical_stat(&path).map(|stat| (path, logical_mtime(stat))))
        .max_by_key(|(_, stamp)| *stamp)
        .map(|(path, _)| path)
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

fn file_in_dir(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join("opencode.db");
    let mut candidates = primary
        .is_file()
        .then_some(primary.clone())
        .into_iter()
        .chain(
            std::fs::read_dir(dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(is_channel_filename)
                }),
        )
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_primary = left == &primary;
        let right_primary = right == &primary;
        right_primary
            .cmp(&left_primary)
            .then_with(|| left.cmp(right))
    });
    candidates
        .into_iter()
        .filter_map(|path| logical_stat(&path).map(|stat| (path, logical_mtime(stat))))
        .fold(None, |selected, candidate| match selected {
            Some((_, selected_mtime)) if candidate.1 <= selected_mtime => selected,
            _ => Some(candidate),
        })
        .map(|(path, _)| path)
}

fn logical_mtime(stat: TranscriptStat) -> (i64, u32) {
    stat.companion
        .map(|companion| {
            (stat.mtime_secs, stat.mtime_nanos).max((companion.mtime_secs, companion.mtime_nanos))
        })
        .unwrap_or((stat.mtime_secs, stat.mtime_nanos))
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
        .prepare("SELECT time_created, json_extract(data, '$.model.providerID') FROM session_message WHERE type = 'assistant' AND json_valid(data) ORDER BY time_created DESC, rowid DESC LIMIT 100")
        .ok()?;
    let mut rows = statement.query([]).ok()?;
    while let Ok(Some(row)) = rows.next() {
        if let (Ok(created), Ok(provider)) = (row.get::<_, i64>(0), row.get::<_, String>(1)) {
            return Some((created.max(0) as u64, provider));
        }
    }
    None
}

fn credentials_in_file(
    path: &Path,
) -> Result<(BTreeMap<String, serde_json::Value>, Option<u64>), rusqlite::Error> {
    let mut credentials = BTreeMap::new();
    let mut stamp = None;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    if !conn.table_exists(None, "credential")? {
        return Ok((credentials, stamp));
    }
    let mut statement = conn.prepare("SELECT integration_id, value, time_updated FROM credential WHERE integration_id IS NOT NULL ORDER BY active DESC, time_created DESC, id DESC")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (Ok(provider), Ok(value)) = (row.get::<_, String>(0), row.get::<_, String>(1)) else {
            continue;
        };
        if credentials.contains_key(&provider) {
            continue;
        }
        if let Ok(value) = serde_json::from_str(&value) {
            stamp = stamp.max(u64::try_from(row.get::<_, i64>(2)?).ok());
            credentials.insert(provider, value);
        }
    }
    Ok((credentials, stamp))
}

pub(super) fn load_credentials() -> crate::agents::delegated_account::Credentials {
    use crate::agents::delegated_account::{Credentials, Error};

    let Some(path) = newest_file() else {
        return Credentials {
            stamp: None,
            entries: Err(Error::NoCredentials {
                adapter: "opencode",
            }),
        };
    };
    load_credentials_at(&path)
}

fn load_credentials_at(path: &Path) -> crate::agents::delegated_account::Credentials {
    use crate::agents::delegated_account::{Credentials, Error};
    let (credentials, stamp) = match credentials_in_file(path) {
        Ok(loaded) => loaded,
        Err(source) => {
            return Credentials {
                stamp: None,
                entries: Err(if !path.try_exists().unwrap_or(true) {
                    Error::NoCredentials {
                        adapter: "opencode",
                    }
                } else {
                    Error::Io {
                        adapter: "opencode",
                        source: std::io::Error::other(source),
                    }
                }),
            };
        }
    };
    let entries = credentials
        .into_iter()
        .map(|(provider, value)| {
            serde_json::from_value(value)
                .map(|credential| (provider, credential))
                .map_err(|source| Error::Parse {
                    adapter: "opencode",
                    source,
                })
        })
        .collect();
    Credentials { stamp, entries }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_stamp_ignores_messages_and_tracks_selected_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let db = crate::testkit::opencode::OpenCodeDb::new(&path, false, true);
        db.credential(
            "selected",
            "openai",
            None,
            (100, 100),
            r#"{"type":"oauth","access":"first"}"#,
        );
        set_mtime(&path, 1);
        let before = load_credentials_at(&path).stamp;
        db.message("m", "s", "assistant", 200, "{}");
        set_mtime(&path, 2);
        assert_eq!(
            load_credentials_at(&path).stamp,
            before,
            "session writes must not invalidate OAuth usage"
        );
        db.connection().execute("UPDATE credential SET time_updated = 300, value = '{\"type\":\"oauth\",\"access\":\"second\"}' WHERE id = 'selected'", []).unwrap();
        assert_ne!(load_credentials_at(&path).stamp, before);
    }

    #[test]
    fn unreadable_credentials_are_unavailable_not_logged_out() {
        use crate::agents::account::{AccountProbe, AccountProbeError};
        use crate::agents::delegated_account::{Adapter, Config, probe_account};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let load_path = path.clone();
        let config = Config {
            adapter: Adapter::OpenCode,
            load_credentials: Box::new(move || load_credentials_at(&load_path)),
            used_provider: Box::new(|| crate::agents::delegated_account::UsedProvider::Silent),
            api_key_types: &["key"],
            account_key_domain: b"test",
        };
        assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
        std::fs::write(&path, b"not a SQLite database").unwrap();
        assert!(matches!(
            probe_account(&config),
            AccountProbe::Unavailable(AccountProbeError::ReadFailed)
        ));
    }

    #[test]
    fn credentials_follow_opencodes_active_creation_and_id_order() {
        use crate::testkit::opencode::OpenCodeDb;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let db = OpenCodeDb::new(&path, false, true);
        db.credential(
            "older-refreshed",
            "openai",
            None,
            (100, 300),
            r#"{"type":"key","key":"old"}"#,
        );
        db.credential(
            "newer-created",
            "openai",
            None,
            (200, 200),
            r#"{"type":"oauth","metadata":{"accountID":"account"}}"#,
        );
        let selected = credentials_in_file(&path).unwrap().0;
        assert_eq!(
            selected
                .get("openai")
                .and_then(|value| value["metadata"]["accountID"].as_str()),
            Some("account")
        );

        db.credential(
            "active",
            "openai",
            Some(true),
            (50, 50),
            r#"{"type":"external","methodID":"selected"}"#,
        );
        assert_eq!(
            credentials_in_file(&path).unwrap().0["openai"]["methodID"],
            "selected"
        );

        db.credential(
            "a",
            "anthropic",
            None,
            (200, 400),
            r#"{"type":"key","key":"a"}"#,
        );
        db.credential(
            "b",
            "anthropic",
            None,
            (200, 200),
            r#"{"type":"key","key":"b"}"#,
        );
        assert_eq!(
            credentials_in_file(&path).unwrap().0["anthropic"]["key"],
            "b"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        assert!(credentials_in_file(&path).is_err());
        let _db = crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        assert!(credentials_in_file(&path).unwrap().0.is_empty());
    }

    fn create_db(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        path
    }

    fn insert_message(path: &Path, data: &str) {
        let db = crate::testkit::opencode::OpenCodeDb::new(path, false, false);
        let index: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM session_message", [], |row| row.get(0))
            .unwrap();
        db.message(&format!("msg-{index}"), "ses", "assistant", index, data);
    }

    #[test]
    fn discovery_selects_the_most_recent_logical_database() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("opencode.db");
        let alpha = dir.path().join("opencode-alpha.db");
        let beta = dir.path().join("opencode-beta.db");
        std::fs::write(&primary, "").unwrap();
        std::fs::write(&alpha, "").unwrap();
        std::fs::write(&beta, "").unwrap();
        set_mtime(&primary, 100);
        set_mtime(&alpha, 200);
        set_mtime(&beta, 200);
        assert_eq!(
            file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode-alpha.db"
        );

        set_mtime(&primary, 200);
        assert_eq!(
            file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode.db"
        );

        set_mtime(&primary, 300);
        set_mtime(&alpha, 100);
        let alpha_wal = dir.path().join("opencode-alpha.db-wal");
        std::fs::write(&alpha_wal, "").unwrap();
        set_mtime(&alpha_wal, 400);
        assert_eq!(
            file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode-alpha.db"
        );

        std::fs::write(dir.path().join("opencode-bad!.db"), "").unwrap();
        assert!(!is_channel_filename("opencode-bad!.db"));
        assert_eq!(
            file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode-alpha.db"
        );
    }

    fn set_mtime(path: &Path, seconds: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn latest_provider_uses_tolerant_minimal_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{"model":{"providerID":"anthropic"},"tokens":false}"#,
        );
        insert_message(
            &path,
            r#"{"model":{"providerID":"openai"},"cost":"unknown"}"#,
        );
        insert_message(&path, "not json");
        crate::testkit::opencode::OpenCodeDb::new(&path, false, false).message(
            "compaction",
            "ses",
            "compaction",
            1000,
            r#"{"model":{"providerID":"anthropic"},"cost":0.5}"#,
        );
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
