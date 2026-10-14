//! Codex's session-title index.
//!
//! Codex appends provisional and generated names to `session_index.jsonl`;
//! the newest valid row for a session is authoritative.
//!
//! The sidebar looks a title up per live Codex session on every refresh, so
//! the process keeps one parsed id→name map per file version, keyed by the
//! full [`FileStamp`] (length, mtime, device, inode) taken before the read:
//! an unchanged index costs one `stat`. The stamp is sufficient for this
//! file because every write that can change a title changes it: renames
//! append and grow the file, and deleting a thread rewrites it without the
//! thread's rows, which shrinks or replaces it. Account homes whose index
//! is one file (symlinks to `~/.codex`) share one parse.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use serde::Deserialize;

use crate::disk::parse_cache::FileStamp;

#[derive(Deserialize)]
struct SessionIndexRow {
    id: String,
    thread_name: String,
}

struct IndexVersion {
    stamp: FileStamp,
    names: HashMap<String, String>,
}

#[derive(Default)]
struct IndexCache {
    by_home: HashMap<PathBuf, Arc<IndexVersion>>,
    parses: usize,
}

static INDEX_CACHE: LazyLock<Mutex<IndexCache>> = LazyLock::new(Mutex::default);

pub(super) fn session_name_under(home: &Path, session_id: &str) -> Option<String> {
    INDEX_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .session_name(home, session_id)
}

impl IndexCache {
    fn session_name(&mut self, home: &Path, session_id: &str) -> Option<String> {
        self.current(home)?.names.get(session_id).cloned()
    }

    fn current(&mut self, home: &Path) -> Option<Arc<IndexVersion>> {
        let path = home.join("session_index.jsonl");
        let Ok(meta) = std::fs::metadata(&path) else {
            self.by_home.remove(home);
            return None;
        };
        let stamp = FileStamp::from_metadata(&meta);
        if let Some(cached) = self.by_home.get(home)
            && cached.stamp == stamp
        {
            return Some(Arc::clone(cached));
        }
        let shared = self
            .by_home
            .values()
            .find(|version| version.stamp == stamp)
            .cloned();
        let version = match shared {
            Some(version) => version,
            None => {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    self.by_home.remove(home);
                    return None;
                };
                self.parses += 1;
                Arc::new(IndexVersion {
                    stamp,
                    names: parse_names(&text),
                })
            }
        };
        self.by_home
            .insert(home.to_path_buf(), Arc::clone(&version));
        Some(version)
    }
}

fn parse_names(text: &str) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for row in text
        .lines()
        .filter_map(|line| serde_json::from_str::<SessionIndexRow>(line).ok())
    {
        let name = row.thread_name.trim();
        if !name.is_empty() {
            names.insert(row.id, name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "session_index.jsonl";

    fn row(id: &str, name: &str) -> String {
        format!("{{\"id\":\"{id}\",\"thread_name\":\"{name}\"}}\n")
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn newest_valid_title_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            "{\"id\":\"session\",\"thread_name\":\"Provisional title\",\"updated_at\":\"first\"}\n\
             {\"id\":\"other\",\"thread_name\":\"Foreign title\"}\n\
             {\"id\":\"session\",\"thread_name\":\" Generated title \"}\n\
             {truncated\n\
             {\"id\":\"session\",\"thread_name\":\"   \"}\n",
        )
        .unwrap();

        assert_eq!(
            session_name_under(dir.path(), "session").as_deref(),
            Some("Generated title")
        );
    }

    #[test]
    fn missing_index_or_session_has_no_title() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(session_name_under(dir.path(), "session"), None);
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            "{\"id\":\"other\",\"thread_name\":\"Foreign title\"}\n",
        )
        .unwrap();
        assert_eq!(session_name_under(dir.path(), "session"), None);
    }

    #[test]
    fn unchanged_index_parses_once_and_appends_reparse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX);
        std::fs::write(&path, row("a", "Alpha") + &row("b", "Beta")).unwrap();
        let mut cache = IndexCache::default();

        for _ in 0..3 {
            assert_eq!(
                cache.session_name(dir.path(), "a").as_deref(),
                Some("Alpha")
            );
            assert_eq!(cache.session_name(dir.path(), "b").as_deref(), Some("Beta"));
        }
        assert_eq!(cache.parses, 1);

        append(&path, &row("a", "Renamed"));
        assert_eq!(
            cache.session_name(dir.path(), "a").as_deref(),
            Some("Renamed")
        );
        assert_eq!(cache.session_name(dir.path(), "b").as_deref(), Some("Beta"));
        assert_eq!(cache.parses, 2);
    }

    #[test]
    fn delete_rewrite_drops_the_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX);
        std::fs::write(&path, row("a", "Alpha") + &row("b", "Beta")).unwrap();
        let mut cache = IndexCache::default();
        assert_eq!(
            cache.session_name(dir.path(), "a").as_deref(),
            Some("Alpha")
        );

        let tmp = dir.path().join("session_index.jsonl.tmp");
        std::fs::write(&tmp, row("b", "Beta")).unwrap();
        std::fs::rename(&tmp, &path).unwrap();

        assert_eq!(cache.session_name(dir.path(), "a"), None);
        assert_eq!(cache.session_name(dir.path(), "b").as_deref(), Some("Beta"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_homes_share_one_parse() {
        let shared = tempfile::tempdir().unwrap();
        let target = shared.path().join(INDEX);
        std::fs::write(&target, row("a", "Alpha")).unwrap();
        let homes = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        for home in &homes {
            std::os::unix::fs::symlink(&target, home.path().join(INDEX)).unwrap();
        }
        let mut cache = IndexCache::default();

        for _ in 0..2 {
            for home in &homes {
                assert_eq!(
                    cache.session_name(home.path(), "a").as_deref(),
                    Some("Alpha")
                );
            }
        }
        assert_eq!(cache.parses, 1);
    }

    #[test]
    fn index_created_later_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = IndexCache::default();
        assert_eq!(cache.session_name(dir.path(), "a"), None);

        std::fs::write(dir.path().join(INDEX), row("a", "Alpha")).unwrap();
        assert_eq!(
            cache.session_name(dir.path(), "a").as_deref(),
            Some("Alpha")
        );
    }
}
