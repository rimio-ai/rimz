//! Reclaim superseded Cargo units without racing profile builds.

use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const STAMP: &str = ".xtask-prune";

#[derive(Debug, Default, PartialEq, Eq)]
struct Report {
    files: u64,
    bytes: u64,
}

#[expect(
    clippy::print_stdout,
    reason = "manual prune reports reclaimed file totals"
)]
pub(super) fn run(root: &Path, args: &[String]) -> Result<()> {
    let dry_run = match args {
        [] => false,
        [arg] if arg == "--dry-run" => true,
        _ => bail!("usage: cargo xtask prune [--dry-run]"),
    };
    let target = crate::files::target_dir(root);
    let now = SystemTime::now();
    if !dry_run {
        stamp(&target, now)?;
    }
    let report = sweep(&target, dry_run, now)?;
    let verb = if dry_run { "would remove" } else { "removed" };
    println!(
        "prune: {verb} {} files, {} bytes",
        report.files, report.bytes
    );
    Ok(())
}

#[expect(
    clippy::print_stderr,
    reason = "automatic cleanup is best-effort contributor feedback"
)]
pub(super) fn automatic(root: &Path) {
    match automatic_at(&crate::files::target_dir(root), SystemTime::now()) {
        Ok(Some(report)) if report.files > 0 => {
            eprintln!(
                "prune: removed {} files, {} bytes",
                report.files, report.bytes
            );
        }
        Ok(_) => {}
        Err(error) => eprintln!("warning: target prune failed: {error:#}"),
    }
}

fn stamp(target: &Path, now: SystemTime) -> Result<()> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(target.join(STAMP))?;
    file.set_modified(now)?;
    Ok(())
}

fn automatic_at(target: &Path, now: SystemTime) -> Result<Option<Report>> {
    let (file, created) = match File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(target.join(STAMP))
    {
        Ok(file) => (file, true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => (
            File::options()
                .read(true)
                .write(true)
                .open(target.join(STAMP))?,
            false,
        ),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(error)) => return Err(error.into()),
    }
    if !created && !older_than(file.metadata()?.modified()?, now, DAY) {
        return Ok(None);
    }
    file.set_modified(now)?;
    sweep(target, false, now).map(Some)
}

fn older_than(modified: SystemTime, now: SystemTime, age: Duration) -> bool {
    now.duration_since(modified)
        .is_ok_and(|elapsed| elapsed >= age)
}

#[derive(Default)]
struct Tree {
    report: Report,
    newest: Option<SystemTime>,
}

fn measure(path: &Path) -> Result<Tree> {
    let metadata = fs::symlink_metadata(path)?;
    let mut tree = Tree {
        newest: Some(metadata.modified()?),
        ..Tree::default()
    };
    if !metadata.is_dir() {
        tree.report = Report {
            files: 1,
            bytes: metadata.len(),
        };
        return Ok(tree);
    }
    for entry in fs::read_dir(path)? {
        let child = measure(&entry?.path())?;
        tree.report.files += child.report.files;
        tree.report.bytes += child.report.bytes;
        tree.newest = tree.newest.max(child.newest);
    }
    Ok(tree)
}

fn directories(path: &Path) -> Result<Vec<PathBuf>> {
    fs::read_dir(path)?
        .filter_map(|entry| match entry {
            Ok(entry) => match entry.file_type() {
                Ok(kind) if kind.is_dir() => Some(Ok(entry.path())),
                Ok(_) => None,
                Err(error) => Some(Err(error.into())),
            },
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

fn is_profile(path: &Path) -> bool {
    fs::symlink_metadata(path.join(".fingerprint")).is_ok_and(|meta| meta.is_dir())
}

fn sweep(target: &Path, dry_run: bool, now: SystemTime) -> Result<Report> {
    let mut report = Report::default();
    for top in directories(target)? {
        let mut profiles = Vec::new();
        if is_profile(&top) {
            profiles.push(top.clone());
        }
        for child in directories(&top)? {
            if is_profile(&child) {
                profiles.push(child);
            }
        }
        if profiles.is_empty() {
            continue;
        }
        let mut locked = Vec::new();
        for profile in &profiles {
            // Cargo creates this file. An incomplete profile without it is left alone,
            // including on dry runs, which must not create lock files.
            let lock = match File::options()
                .read(true)
                .write(true)
                .open(profile.join(".cargo-lock"))
            {
                Ok(lock) => lock,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            match lock.try_lock() {
                Ok(()) => locked.push((profile, lock)),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        if locked.len() == profiles.len() && top.file_name().is_some_and(|name| name != "debug") {
            let tree = measure(&top)?;
            if tree
                .newest
                .is_some_and(|mtime| older_than(mtime, now, DAY * 14))
            {
                remove(&top, tree.report, dry_run, &mut report)?;
                continue;
            }
        }
        for (profile, _lock) in &locked {
            for dir in ["deps", "build", ".fingerprint", "incremental"] {
                let path = profile.join(dir);
                if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                    prune_units(&path, dry_run, now, &mut report)
                        .with_context(|| format!("pruning {}", path.display()))?;
                }
            }
        }
    }
    Ok(report)
}

struct Entry {
    path: PathBuf,
    tree: Tree,
    executable: bool,
}

fn unit_name(name: &str) -> Option<(&str, &str)> {
    let (stem, suffix) = name.rsplit_once('-')?;
    let hash = suffix.split_once('.').map_or(suffix, |(hash, _)| hash);
    (!stem.is_empty()
        && hash.len() == 16
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some((stem, hash))
}

fn executable(path: &Path, metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file()
            && path.extension().is_none()
            && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata);
        false
    }
}

fn prune_units(dir: &Path, dry_run: bool, now: SystemTime, report: &mut Report) -> Result<()> {
    let mut stems: BTreeMap<String, BTreeMap<String, Vec<Entry>>> = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some((stem, hash)) = name.to_str().and_then(unit_name) else {
            continue;
        };
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let executable = executable(&path, &metadata);
        let tree = measure(&path)?;
        stems
            .entry(stem.to_owned())
            .or_default()
            .entry(hash.to_owned())
            .or_default()
            .push(Entry {
                path,
                tree,
                executable,
            });
    }
    for hashes in stems.into_values() {
        let mut units: Vec<_> = hashes
            .into_values()
            .map(|entries| {
                let newest = entries.iter().filter_map(|entry| entry.tree.newest).max();
                (newest, entries)
            })
            .collect();
        units.sort_by_key(|unit| std::cmp::Reverse(unit.0));
        let Some(Some(stem_newest)) = units.first().map(|unit| unit.0) else {
            continue;
        };
        // Check builds leave `.d`-only hashes under an executable's stem, so
        // executables rank only among the hashes that link one.
        let mut executable_rank = 0;
        for (newest, entries) in units {
            let Some(newest) = newest else { continue };
            // Variants of one stem that are all live (a lib beside its build
            // script, feature splits) are built together; a superseded hash
            // predates its replacement by at least one build.
            let superseded = older_than(newest, stem_newest, DAY);
            let mut expired = superseded && older_than(newest, now, DAY * 14);
            if entries.iter().any(|entry| entry.executable) {
                expired |= executable_rank >= 3 && older_than(newest, now, DAY);
                executable_rank += 1;
            }
            if !expired {
                continue;
            }
            for entry in entries {
                remove(&entry.path, entry.tree.report, dry_run, report)?;
            }
        }
    }
    Ok(())
}

fn remove(path: &Path, removed: Report, dry_run: bool, report: &mut Report) -> Result<()> {
    if !dry_run {
        if fs::symlink_metadata(path)?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    report.files += removed.files;
    report.bytes += removed.bytes;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::path::PathBuf;

    fn age(path: &Path, days: u32) {
        File::open(path)
            .unwrap()
            .set_modified(SystemTime::now() - DAY * days)
            .unwrap();
    }

    fn file(root: &Path, name: &str, days: u32) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"unit").unwrap();
        age(&path, days);
        path
    }

    fn profile(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.join(".fingerprint")).unwrap();
        File::create(path.join(".cargo-lock")).unwrap();
        path
    }

    fn age_tree(path: &Path, days: u32) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                age_tree(&entry.unwrap().path(), days);
            }
        }
        age(path, days);
    }

    #[test]
    fn superseded_units_expire_in_all_four_directories() {
        let target = tempfile::tempdir().unwrap();
        let debug = profile(target.path(), "debug");
        for dir in ["deps", ".fingerprint", "build", "incremental"] {
            let suffix = if dir == "deps" { ".rlib" } else { "/output" };
            file(&debug, &format!("{dir}/foo-0000000000000001{suffix}"), 20);
            age_tree(&debug.join(dir), 20);
            file(&debug, &format!("{dir}/foo-0000000000000002{suffix}"), 0);
            file(&debug, &format!("{dir}/unrecognized"), 30);
        }
        assert_eq!(
            sweep(target.path(), false, SystemTime::now()).unwrap(),
            Report {
                files: 4,
                bytes: 16
            }
        );
        for dir in ["deps", ".fingerprint", "build", "incremental"] {
            assert!(
                !debug
                    .join(dir)
                    .join(if dir == "deps" {
                        "foo-0000000000000001.rlib"
                    } else {
                        "foo-0000000000000001"
                    })
                    .exists()
            );
            assert!(debug.join(dir).join("unrecognized").exists());
        }
    }

    #[test]
    fn newest_hash_survives_and_sidecars_share_the_newest_age() {
        let target = tempfile::tempdir().unwrap();
        let debug = profile(target.path(), "debug");
        let oldest = file(&debug, "deps/libfoo-0000000000000001.rlib", 40);
        let newest = file(&debug, "deps/libfoo-0000000000000002.rlib", 30);
        let refreshed = file(&debug, "deps/libfoo-0000000000000003.rlib", 50);
        file(&debug, "deps/libfoo-0000000000000003.rmeta", 0);
        let only = file(&debug, "deps/only-0000000000000001.d", 50);
        sweep(target.path(), false, SystemTime::now()).unwrap();
        assert!(!oldest.exists());
        assert!(!newest.exists());
        assert!(refreshed.exists());
        assert!(only.exists());
    }

    #[test]
    fn variants_built_together_survive_however_old() {
        let target = tempfile::tempdir().unwrap();
        let debug = profile(target.path(), "debug");
        let lib = file(&debug, ".fingerprint/serde-0000000000000001/lib", 30);
        let script = file(&debug, ".fingerprint/serde-0000000000000002/build", 30);
        age_tree(&debug.join(".fingerprint"), 30);
        sweep(target.path(), false, SystemTime::now()).unwrap();
        assert!(lib.exists());
        assert!(script.exists());
    }

    #[cfg(unix)]
    #[test]
    fn executables_keep_three_hashes_and_everything_under_a_day() {
        use std::os::unix::fs::PermissionsExt;
        let target = tempfile::tempdir().unwrap();
        let debug = profile(target.path(), "debug");
        for (hash, days) in [(1, 4), (2, 3), (3, 2), (4, 0), (5, 0), (6, 0), (7, 0)] {
            let path = file(&debug, &format!("deps/rimz-{hash:016x}"), days);
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // An expired executable hash goes whole, sidecars included.
        file(&debug, "deps/rimz-0000000000000001.d", 4);
        // Check builds leave newer `.d`-only hashes that must not outrank
        // the live executables of the same stem.
        let mut live = Vec::new();
        for hash in 1..=3 {
            let path = file(&debug, &format!("deps/probe-{hash:016x}"), 2);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            live.push(path);
        }
        for hash in 4..=7 {
            file(&debug, &format!("deps/probe-{hash:016x}.d"), 1);
        }
        // A separate stem with four older executables retains exactly three.
        for hash in 1..=4 {
            let path = file(&debug, &format!("deps/tool-{hash:016x}"), 6 - hash);
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            sweep(target.path(), false, SystemTime::now())
                .unwrap()
                .files,
            5
        );
        assert!(!debug.join("deps/rimz-0000000000000001.d").exists());
        assert!(live.iter().all(|path| path.exists()));
        assert!(debug.join("deps/rimz-0000000000000004").exists());
        assert!(debug.join("deps/tool-0000000000000002").exists());
        assert!(!debug.join("deps/tool-0000000000000001").exists());
    }

    #[test]
    fn stale_profiles_expire_but_debug_and_non_cargo_output_survive() {
        let target = tempfile::tempdir().unwrap();
        for name in [
            "debug",
            "release",
            "llvm-cov-target/debug",
            "wasm32-wasip1/release",
        ] {
            let path = profile(target.path(), name);
            file(&path, "deps/only-0000000000000001.d", 30);
        }
        for name in [
            "dist/archive",
            "xtask/install/rimz",
            "ci/coverage/report",
            "CACHEDIR.TAG",
            ".rustc_info.json",
        ] {
            file(target.path(), name, 30);
        }
        age_tree(target.path(), 30);
        sweep(target.path(), false, SystemTime::now()).unwrap();
        for name in ["release", "llvm-cov-target", "wasm32-wasip1"] {
            assert!(!target.path().join(name).exists(), "{name}");
        }
        for name in [
            "debug",
            "dist",
            "xtask",
            "ci",
            "CACHEDIR.TAG",
            ".rustc_info.json",
        ] {
            assert!(target.path().join(name).exists(), "{name}");
        }
    }

    #[test]
    fn held_profile_lock_prevents_unit_and_enclosing_tree_removal() {
        let target = tempfile::tempdir().unwrap();
        let path = profile(target.path(), "llvm-cov-target/debug");
        let old = file(&path, "deps/foo-0000000000000001.d", 30);
        file(&path, "deps/foo-0000000000000002.d", 20);
        age_tree(target.path(), 20);
        let lock = File::open(path.join(".cargo-lock")).unwrap();
        lock.lock().unwrap();
        assert_eq!(
            sweep(target.path(), false, SystemTime::now()).unwrap(),
            Report::default()
        );
        assert!(old.exists());
        drop(lock);
        assert!(
            sweep(target.path(), false, SystemTime::now())
                .unwrap()
                .files
                > 0
        );
        assert!(!path.exists());
    }

    #[test]
    fn automatic_prune_is_gated_for_twenty_four_hours() {
        let target = tempfile::tempdir().unwrap();
        let debug = profile(target.path(), "debug");
        let now = SystemTime::now();
        assert!(automatic_at(target.path(), now).unwrap().is_some());
        File::create(target.path().join(STAMP)).unwrap();
        assert!(automatic_at(target.path(), now).unwrap().is_none());
        age(&target.path().join(STAMP), 2);
        assert!(automatic_at(target.path(), now).unwrap().is_some());
        let old = file(&debug, "deps/foo-0000000000000001.d", 30);
        file(&debug, "deps/foo-0000000000000002.d", 0);
        assert!(
            automatic_at(target.path(), now + DAY - Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
        assert!(old.exists());
        assert_eq!(
            automatic_at(target.path(), now + DAY)
                .unwrap()
                .unwrap()
                .files,
            1
        );
        assert!(!old.exists());
        stamp(target.path(), now + DAY * 2).unwrap();
        assert!(
            automatic_at(target.path(), now + DAY * 2)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn dry_run_preserves_tree_and_matches_real_totals() {
        let target = tempfile::tempdir().unwrap();
        let debug = profile(target.path(), "debug");
        let old = file(&debug, "deps/foo-0000000000000001.d", 30);
        file(&debug, "deps/foo-0000000000000002.d", 0);
        let release = profile(target.path(), "release");
        file(&release, "deps/foo-0000000000000001.d", 30);
        age_tree(&release, 30);
        let preview = sweep(target.path(), true, SystemTime::now()).unwrap();
        assert_eq!(preview, Report { files: 3, bytes: 8 });
        assert!(old.exists());
        assert!(release.exists());
        assert!(!target.path().join(STAMP).exists());
        assert_eq!(
            preview,
            sweep(target.path(), false, SystemTime::now()).unwrap()
        );
    }
}
