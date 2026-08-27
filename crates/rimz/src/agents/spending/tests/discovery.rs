use std::collections::HashMap;
use std::fs::File;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::tempdir;

use super::super::cache::{FileCacheEntry, SpendingDiskCache};
use super::super::discovery::{SpendingDiscoveryIndex, SpendingSource, SpendingSourceTree};
use super::super::{SKIP_PARSE_MARGIN_SECS, WIDEST_SPEND_WINDOW_SECS};
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn recursive(root: &std::path::Path) -> SpendingSource {
    SpendingSource::group(vec![SpendingSourceTree::new(root, "**/*.jsonl").unwrap()])
}

#[test]
fn warm_discovery_reuses_unchanged_directories_and_finds_changed_frontier() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("sessions");
    let first = root.join("project/session/first.jsonl");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::write(&first, "{}\n").unwrap();
    let source = recursive(&root);
    let now = now_secs();
    let mut index = SpendingDiscoveryIndex::default();

    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now),
        std::slice::from_ref(&first)
    );
    let cold = index.stats_for_test();
    assert!(cold.read_dirs >= 3);
    assert_eq!(cold.candidate_stats, 1);
    assert_eq!(cold.materializations, 1);

    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now),
        std::slice::from_ref(&first)
    );
    let warm = index.stats_for_test();
    assert_eq!(warm.read_dirs, 0);
    assert_eq!(warm.candidate_stats, 0);
    assert_eq!(warm.materializations, 0);

    let second = root.join("project/session/deeper/second.jsonl");
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();
    std::fs::write(&second, "{}\n").unwrap();
    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now),
        [second.clone(), first.clone()]
    );
    assert!(index.stats_for_test().read_dirs > 0);
    assert_eq!(index.stats_for_test().materializations, 1);

    std::fs::remove_file(&first).unwrap();
    File::open(first.parent().unwrap())
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    assert_eq!(index.discover_sources_for_test(vec![source], now), [second]);
}

#[test]
fn transient_root_failure_retains_prior_subtree_for_retry() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("sessions");
    let file = root.join("session.jsonl");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&file, "{}\n").unwrap();
    let source = recursive(&root);
    let mut index = SpendingDiscoveryIndex::default();
    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now_secs()),
        std::slice::from_ref(&file)
    );

    let parked = dir.path().join("sessions-parked");
    std::fs::rename(&root, &parked).unwrap();
    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now_secs()),
        std::slice::from_ref(&file),
        "an unreadable provider root is not authoritative empty discovery"
    );
    std::fs::rename(&parked, &root).unwrap();
    assert_eq!(
        index.discover_sources_for_test(vec![source], now_secs()),
        [file]
    );
}

#[test]
fn exact_source_deletion_is_authoritative() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("spend.jsonl");
    std::fs::write(&file, "{}\n").unwrap();
    let source = SpendingSource::exact(&file);
    let mut index = SpendingDiscoveryIndex::default();
    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now_secs()),
        std::slice::from_ref(&file)
    );

    std::fs::remove_file(&file).unwrap();
    index.force_complete_for_test();
    assert!(
        index
            .discover_sources_for_test(vec![source], now_secs())
            .is_empty()
    );
    assert!(index.last_scan_authoritative());
}

#[test]
fn retired_file_stays_pruned_until_complete_reconciliation() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("sessions");
    let file = root.join("project/session.jsonl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "old\n").unwrap();
    let now = now_secs();
    let old = UNIX_EPOCH
        + Duration::from_secs(
            now.saturating_sub(WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS + 10),
        );
    File::open(&file).unwrap().set_modified(old).unwrap();
    let source = recursive(&root);
    let mut index = SpendingDiscoveryIndex::default();

    assert!(
        index
            .discover_sources_for_test(vec![source.clone()], now)
            .is_empty()
    );
    assert_eq!(index.stats_for_test().candidate_stats, 1);

    std::fs::write(&file, "recent and longer\n").unwrap();
    assert!(
        index
            .discover_sources_for_test(vec![source.clone()], now)
            .is_empty(),
        "an in-place write below a pruned branch waits for reconciliation"
    );
    assert_eq!(index.stats_for_test().candidate_stats, 0);

    index.force_complete_for_test();
    assert_eq!(index.discover_sources_for_test(vec![source], now), [file]);
}

#[test]
fn cursor_mtime_retires_active_files_without_rediscovery_stats() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("sessions");
    let file = root.join("session.jsonl");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&file, "{}\n").unwrap();
    let source = recursive(&root);
    let now = now_secs();
    let mut index = SpendingDiscoveryIndex::default();
    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now),
        std::slice::from_ref(&file)
    );

    index.reconcile(
        &SpendingDiskCache {
            files: HashMap::from([(
                file.to_string_lossy().into_owned(),
                FileCacheEntry {
                    stat: crate::agents::TranscriptStat {
                        mtime_secs: i64::try_from(
                            now.saturating_sub(
                                WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS + 1,
                            ),
                        )
                        .unwrap(),
                        ..Default::default()
                    },
                    cursor: Default::default(),
                    origin_path: None,
                    entries: Vec::new(),
                    unknown_models: Default::default(),
                },
            )]),
            ..Default::default()
        },
        now,
    );
    assert!(
        index
            .discover_sources_for_test(vec![source.clone()], now)
            .is_empty()
    );
    assert_eq!(index.stats_for_test().candidate_stats, 0);
    assert_eq!(index.stats_for_test().materializations, 1);
    assert!(
        index
            .discover_sources_for_test(vec![source], now)
            .is_empty()
    );
    assert_eq!(index.stats_for_test().materializations, 0);
}

#[test]
fn ordered_roots_dedup_by_relative_path_with_first_root_precedence() {
    let dir = tempdir().unwrap();
    let active = dir.path().join("active");
    let archive = dir.path().join("archive");
    let relative = std::path::Path::new("2026/01/02/rollout.jsonl");
    for root in [&active, &archive] {
        std::fs::create_dir_all(root.join(relative).parent().unwrap()).unwrap();
        std::fs::write(root.join(relative), "{}\n").unwrap();
    }
    let source = SpendingSource::group(vec![
        SpendingSourceTree::new(&active, "**/*.jsonl").unwrap(),
        SpendingSourceTree::new(&archive, "**/*.jsonl").unwrap(),
    ]);
    let mut index = SpendingDiscoveryIndex::default();
    assert_eq!(
        index.discover_sources_for_test(vec![source], now_secs()),
        [active.join(relative)]
    );
}

#[test]
fn directory_filter_prunes_unrelated_subtrees() {
    fn outside_sessions(path: &std::path::Path) -> bool {
        path.components()
            .next()
            .is_none_or(|component| component.as_os_str().to_str() != Some("sessions"))
    }

    let dir = tempdir().unwrap();
    let root = dir.path().join("codex");
    let legacy = root.join("legacy/rollout.jsonl");
    let modern = root.join("sessions/2026/01/02/rollout.jsonl");
    for file in [&legacy, &modern] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "{}\n").unwrap();
    }
    let source = SpendingSource::group(vec![
        SpendingSourceTree::new(&root, "**/*.jsonl")
            .unwrap()
            .filtered("outside-sessions", outside_sessions)
            .descend_filtered("outside-session-dirs", outside_sessions),
    ]);
    let mut index = SpendingDiscoveryIndex::default();

    assert_eq!(
        index.discover_sources_for_test(vec![source], now_secs()),
        [legacy]
    );
    assert_eq!(index.stats_for_test().read_dirs, 2);
}

#[test]
fn codex_old_date_partition_is_pruned_until_complete_scan() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("sessions");
    let old = root.join("2001/01/01/rollout.jsonl");
    let recent = root.join("2099/01/01/rollout.jsonl");
    for file in [&old, &recent] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "{}\n").unwrap();
    }
    let source = SpendingSource::group(vec![
        SpendingSourceTree::new(&root, "**/*.jsonl")
            .unwrap()
            .codex_dates(),
    ]);
    let mut index = SpendingDiscoveryIndex::default();
    assert_eq!(
        index.discover_sources_for_test(vec![source.clone()], now_secs()),
        std::slice::from_ref(&recent)
    );
    index.force_complete_for_test();
    assert_eq!(
        index.discover_sources_for_test(vec![source], now_secs()),
        [old, recent]
    );
}

#[test]
fn complete_enumeration_keeps_history_beyond_the_warm_horizon() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("sessions");
    let old = root.join("2001/01/01/rollout.jsonl");
    let recent = root.join("2099/01/01/rollout.jsonl");
    for file in [&old, &recent] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "{}\n").unwrap();
    }
    let source = SpendingSource::group(vec![
        SpendingSourceTree::new(&root, "**/*.jsonl")
            .unwrap()
            .codex_dates(),
    ]);

    assert_eq!(source.complete_files(), [old, recent]);
}

#[test]
fn complete_enumeration_preserves_selection_filters_and_symlink_policy() {
    fn admitted(relative: &std::path::Path) -> bool {
        relative
            .file_name()
            .is_some_and(|name| name == "keep.jsonl")
    }
    fn sessions_only(relative: &std::path::Path) -> bool {
        relative
            .components()
            .next()
            .is_none_or(|component| component.as_os_str() == "sessions")
    }

    let dir = tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    for root in [&first, &second] {
        let keep = root.join("sessions/keep.jsonl");
        std::fs::create_dir_all(keep.parent().unwrap()).unwrap();
        std::fs::write(&keep, "{}\n").unwrap();
        std::fs::write(root.join("sessions/drop.jsonl"), "{}\n").unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("other/keep.jsonl"), "{}\n").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        first.join("sessions/keep.jsonl"),
        first.join("sessions/link.jsonl"),
    )
    .unwrap();

    let all = SpendingSource::group(vec![
        SpendingSourceTree::new(&first, "**/*.jsonl")
            .unwrap()
            .filtered("keep", admitted)
            .descend_filtered("sessions", sessions_only),
        SpendingSourceTree::new(&second, "**/*.jsonl")
            .unwrap()
            .filtered("keep", admitted)
            .descend_filtered("sessions", sessions_only),
    ]);
    assert_eq!(all.complete_files(), [first.join("sessions/keep.jsonl")]);

    #[cfg(unix)]
    assert_eq!(
        SpendingSource::group(vec![
            SpendingSourceTree::new(first.join("sessions"), "*.jsonl").unwrap(),
        ])
        .complete_files(),
        [
            first.join("sessions/drop.jsonl"),
            first.join("sessions/keep.jsonl"),
        ]
    );
}

#[test]
fn source_fingerprint_is_binary_canonical_and_declaration_complete() {
    fn filter(_: &std::path::Path) -> bool {
        true
    }

    let plain = SpendingSource::group(vec![
        SpendingSourceTree::new("/tmp/root/./sessions", "**/*.jsonl").unwrap(),
    ]);
    let normalized = SpendingSource::group(vec![
        SpendingSourceTree::new("/tmp/root/sessions", "**/*.jsonl").unwrap(),
    ]);
    assert_eq!(plain.fingerprint(), normalized.fingerprint());

    let filtered = SpendingSource::group(vec![
        SpendingSourceTree::new("/tmp/root/sessions", "**/*.jsonl")
            .unwrap()
            .filtered("named-filter", filter),
    ]);
    assert_ne!(normalized.fingerprint(), filtered.fingerprint());
    let empty_named_filter = SpendingSource::group(vec![
        SpendingSourceTree::new("/tmp/root/sessions", "**/*.jsonl")
            .unwrap()
            .filtered("", filter),
    ]);
    assert_ne!(normalized.fingerprint(), empty_named_filter.fingerprint());

    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let left = SpendingSource::exact(std::path::PathBuf::from(OsString::from_vec(vec![0x80])));
        let right = SpendingSource::exact(std::path::PathBuf::from(OsString::from_vec(vec![0x81])));
        assert_ne!(left.fingerprint(), right.fingerprint());
    }
}

#[test]
fn transcript_only_kiro_declares_no_historical_spend_source() {
    assert!(
        crate::agents::definition_by_kind("kiro")
            .expect("Kiro definition")
            .spending_sources()
            .is_empty()
    );
}
