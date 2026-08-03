use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::{collections::HashMap, os::unix::fs::MetadataExt};

use crate::store::atomic;

/// Recursively remove orphaned whole-file-write temps under `root`.
///
/// A hard kill can leave these same-directory siblings behind before rename.
/// Only files older than `min_age` are removed, so an in-flight write stays
/// intact. Reclaimed bytes count a hardlinked payload only when the sweep
/// removes its final name.
pub(super) fn sweep_orphan_temps_under(
    root: &Path,
    min_age: Duration,
    dry_run: bool,
) -> (usize, u64) {
    let mut stack = vec![root.to_path_buf()];
    let now = SystemTime::now();
    let mut candidates = Vec::new();

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !atomic::is_orphan_temp_name(name) {
                continue;
            }
            let Some(metadata) = std::fs::symlink_metadata(&path).ok() else {
                continue;
            };
            let old_enough = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= min_age);
            if old_enough {
                candidates.push((path, metadata));
            }
        }
    }

    let removed: Vec<_> = if dry_run {
        candidates
    } else {
        candidates
            .into_iter()
            .filter(|(path, _)| std::fs::remove_file(path).is_ok())
            .collect()
    };
    (removed.len(), removed_payload_bytes(&removed))
}

#[cfg(unix)]
fn removed_payload_bytes(files: &[(PathBuf, std::fs::Metadata)]) -> u64 {
    #[derive(Clone, Copy)]
    struct Links {
        removed: u64,
        total: u64,
        len: u64,
    }

    let mut links_by_file = HashMap::new();
    for (_, metadata) in files {
        let links = links_by_file
            .entry((metadata.dev(), metadata.ino()))
            .or_insert(Links {
                removed: 0,
                total: metadata.nlink(),
                len: metadata.len(),
            });
        links.removed = links.removed.saturating_add(1);
        links.total = links.total.max(metadata.nlink());
    }
    links_by_file.values().fold(0_u64, |bytes, links| {
        if links.removed >= links.total {
            bytes.saturating_add(links.len)
        } else {
            bytes
        }
    })
}

#[cfg(not(unix))]
fn removed_payload_bytes(files: &[(PathBuf, std::fs::Metadata)]) -> u64 {
    files.iter().fold(0_u64, |bytes, (_, metadata)| {
        bytes.saturating_add(metadata.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sweep_orphan_temps_removes_matching_recursively() {
        let dir = tempdir().unwrap();
        let nonce = "00000000000000000000000000000000";
        let stale_root = dir.path().join(format!("spending.json.tmp.1.{nonce}"));
        let subdir = dir.path().join("nested");
        std::fs::create_dir_all(&subdir).unwrap();
        let stale_nested = subdir.join(format!("rollup.json.tmp.2.{nonce}"));
        let fresh = subdir.join(format!("workspace.json.tmp.3.{nonce}"));
        let keep = subdir.join("workspace.json");
        for path in [&stale_root, &stale_nested, &fresh, &keep] {
            std::fs::write(path, b"temp").unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [&stale_root, &stale_nested] {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        let (files, bytes) = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), false);

        assert_eq!(files, 2);
        assert_eq!(bytes, 8);
        assert!(!stale_root.exists());
        assert!(!stale_nested.exists());
        assert!(fresh.exists());
        assert!(keep.exists());
    }

    #[test]
    fn sweep_orphan_temps_dry_run_counts_without_removing() {
        let dir = tempdir().unwrap();
        let nonce = "00000000000000000000000000000000";
        let stale = dir.path().join(format!("spending.json.tmp.1.{nonce}"));
        std::fs::write(&stale, b"temp").unwrap();
        std::fs::File::open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let (files, bytes) = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), true);

        assert_eq!((files, bytes), (1, 4));
        assert!(stale.exists(), "dry-run leaves temp file in place");
    }

    #[cfg(unix)]
    #[test]
    fn sweep_orphan_temps_counts_hardlinked_payload_once() {
        let dir = tempdir().unwrap();
        let first = dir
            .path()
            .join("rimz.tmp.1.00000000000000000000000000000000");
        let second = dir
            .path()
            .join("rimz.tmp.2.11111111111111111111111111111111");
        std::fs::write(&first, b"stable build").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(&first)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let preview = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), true);
        let removed = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), false);

        assert_eq!(preview, (2, 12));
        assert_eq!(removed, preview);
    }

    #[cfg(unix)]
    #[test]
    fn sweep_orphan_temps_does_not_charge_retained_hardlink() {
        let dir = tempdir().unwrap();
        let stable = dir.path().join("rimz");
        let temp = dir
            .path()
            .join("rimz.tmp.1.00000000000000000000000000000000");
        std::fs::write(&stable, b"stable build").unwrap();
        std::fs::hard_link(&stable, &temp).unwrap();
        std::fs::File::open(&temp)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let preview = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), true);
        let removed = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), false);

        assert_eq!(preview, (1, 0));
        assert_eq!(removed, preview);
        assert_eq!(std::fs::read(stable).unwrap(), b"stable build");
    }

    #[test]
    fn sweep_orphan_temps_rejects_near_misses() {
        let dir = tempdir().unwrap();
        let hex_31 = "0000000000000000000000000000000";
        let hex_32 = "00000000000000000000000000000000";
        let no_nonce = dir.path().join("foo.json.tmp.12");
        let non_digit_pid = dir.path().join(format!("foo.json.tmp.ab.{hex_32}"));
        let short_nonce = dir.path().join(format!("foo.json.tmp.1.{hex_31}"));
        for path in [&no_nonce, &non_digit_pid, &short_nonce] {
            std::fs::write(path, b"keep").unwrap();
        }

        let (files, bytes) = sweep_orphan_temps_under(dir.path(), Duration::ZERO, false);

        assert_eq!((files, bytes), (0, 0));
        assert!(no_nonce.exists());
        assert!(non_digit_pid.exists());
        assert!(short_nonce.exists());
    }
}
