use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::store::gc::TmpHold;
use std::cell::Cell;
use std::fs;
use std::time::{Duration, SystemTime};

fn candidate(root: &Path, handle: &str, seconds: u64, owned: bool) -> Candidate {
    let paths = StatePaths::under(crate::WorkspaceId::from_project_root(root), root).unwrap();
    let path = paths.temp_unit_dir(Some(handle));
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("data"), b"scratch").unwrap();
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    let hold = if owned {
        TmpHold::Ended(jiff::Timestamp::from_second(seconds as i64).unwrap())
    } else {
        TmpHold::Unowned
    };
    Candidate {
        paths,
        unit: TmpUnit {
            handle: handle.into(),
            path,
            modified,
            hold,
        },
    }
}

fn config() -> GcConfig {
    GcConfig {
        tmp_floor_percent: 10,
        tmp_floor_min: "1GiB".into(),
        ..GcConfig::default()
    }
}

#[test]
fn pressure_orders_mixed_candidates_and_stops_at_the_floor() {
    let temp = tempfile::tempdir().unwrap();
    let candidates = vec![
        candidate(temp.path(), "new", 30, true),
        candidate(temp.path(), "old", 10, true),
        candidate(temp.path(), "orphan", 20, false),
    ];
    let oldest = candidates[1].unit.path.clone();
    let mut records = Vec::new();
    let probes = Cell::new(0);
    let report = enforce(
        candidates,
        &GcConfig {
            tmp_floor_min: "0".into(),
            ..config()
        },
        || {
            probes.set(probes.get() + 1);
            Ok(DiskSpace {
                free: if oldest.exists() { 1 } else { 10 },
                total: 100,
            })
        },
        |candidate| Ok(Some(candidate.unit.clone())),
        |record| records.push(record.clone()),
    )
    .unwrap();
    assert!(
        !oldest.exists(),
        "oldest is removed before considering the newer units"
    );
    assert_eq!(report.evictions.len(), 1);
    assert_eq!(records.len(), 1);
    assert_eq!(report.free_bytes, 10);
    assert!(
        probes.get() >= 3,
        "recheck before removal and measure after it"
    );
}

#[test]
fn pressure_stops_at_allocated_estimate_when_space_probe_stays_stale() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir().unwrap();
    let candidates = vec![
        candidate(temp.path(), "new", 30, true),
        candidate(temp.path(), "old", 10, true),
        candidate(temp.path(), "orphan", 20, false),
    ];
    let paths: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.unit.path.clone())
        .collect();
    let allocated = |path: &Path| {
        [path.to_owned(), path.join("data")]
            .into_iter()
            .map(|path| fs::symlink_metadata(path).unwrap().blocks() * 512)
            .sum::<u64>()
    };
    let removed_bytes = allocated(&paths[1]) + allocated(&paths[2]);
    assert!(removed_bytes > 0);
    let config = GcConfig {
        tmp_floor_percent: 0,
        tmp_floor_min: (1 + removed_bytes).to_string(),
        ..config()
    };
    let report = enforce(
        candidates,
        &config,
        || {
            Ok(DiskSpace {
                free: 1,
                total: 1_000_000_000,
            })
        },
        |candidate| Ok(Some(candidate.unit.clone())),
        |_| {},
    )
    .unwrap();

    assert!(
        paths[0].exists(),
        "stale statvfs must not authorize a third eviction"
    );
    assert!(!paths[1].exists());
    assert!(!paths[2].exists());
    assert_eq!(report.evictions.len(), 2);
    assert_eq!(
        report.free_bytes, 1,
        "the report must keep the measured reading"
    );
    let reclaimed: u64 = report
        .evictions
        .iter()
        .map(|record| match &record.assist {
            Assist::TmpEvict {
                reclaimed_bytes,
                free_bytes_after,
                ..
            } => {
                assert_eq!(*free_bytes_after, 1);
                *reclaimed_bytes
            }
            other => panic!("unexpected {other:?}"),
        })
        .sum();
    assert_eq!(reclaimed, removed_bytes);
}

#[test]
fn pressure_rejudges_holds_and_mtime_and_continues_after_errors() {
    let temp = tempfile::tempdir().unwrap();
    let candidates: Vec<_> = ["live", "open", "touched", "unreadable", "orphan", "ended"]
        .into_iter()
        .enumerate()
        .map(|(index, handle)| candidate(temp.path(), handle, index as u64, handle != "orphan"))
        .collect();
    let paths: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.unit.path.clone())
        .collect();
    let report = enforce(
        candidates,
        &config(),
        || {
            Ok(DiskSpace {
                free: 1,
                total: 100,
            })
        },
        |candidate| {
            let mut unit = candidate.unit.clone();
            match unit.handle.as_str() {
                "live" => unit.hold = TmpHold::Live,
                "open" => unit.hold = TmpHold::Open,
                "touched" => unit.modified += Duration::from_secs(1),
                "unreadable" => return Err(io::Error::other("unreadable rollup").into()),
                _ => {}
            }
            Ok(Some(unit))
        },
        |_| {},
    )
    .unwrap();
    assert!(paths[..4].iter().all(|path| path.exists()));
    assert!(
        paths[4..].iter().all(|path| !path.exists()),
        "one failed rejudgment does not stop the pass"
    );
    let handles: Vec<_> = report
        .evictions
        .iter()
        .map(|record| match &record.assist {
            super::super::assist_log::Assist::TmpEvict { handle, .. } => handle.as_str(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(handles, ["orphan", "ended"]);
}

#[test]
fn pressure_does_not_remove_after_a_failed_probe() {
    let temp = tempfile::tempdir().unwrap();
    let unit = candidate(temp.path(), "keep", 1, true);
    let path = unit.unit.path.clone();
    let mut calls = 0;
    let result = enforce(
        vec![unit],
        &config(),
        || {
            calls += 1;
            if calls == 1 {
                Ok(DiskSpace {
                    free: 1,
                    total: 100,
                })
            } else {
                Err(io::Error::other("probe failed"))
            }
        },
        |candidate| Ok(Some(candidate.unit.clone())),
        |_| {},
    );
    assert!(
        result.is_err(),
        "a stale free-space reading cannot authorize removal"
    );
    assert!(path.exists());
}

#[test]
fn pressure_keeps_the_assist_after_a_post_removal_probe_failure() {
    let temp = tempfile::tempdir().unwrap();
    let oldest = candidate(temp.path(), "old", 1, true);
    let newest = candidate(temp.path(), "new", 2, true);
    let oldest_path = oldest.unit.path.clone();
    let newest_path = newest.unit.path.clone();
    let mut calls = 0;
    let mut records = Vec::new();
    let result = enforce(
        vec![oldest, newest],
        &config(),
        || {
            calls += 1;
            if calls >= 3 {
                return Err(io::Error::other("space probe unavailable"));
            }
            Ok(DiskSpace {
                free: 1,
                total: 100,
            })
        },
        |candidate| Ok(Some(candidate.unit.clone())),
        |record| records.push(record.clone()),
    );
    assert!(
        result.is_err(),
        "the next pre-removal decision requires a fresh reading"
    );
    assert!(!oldest_path.exists());
    assert!(newest_path.exists());
    assert_eq!(
        records.len(),
        1,
        "a completed removal keeps its audit even when the next probe fails"
    );
    match &records[0].assist {
        super::super::assist_log::Assist::TmpEvict {
            free_bytes_after,
            error,
            ..
        } => {
            assert_eq!(*free_bytes_after, 1, "last successful reading is retained");
            assert!(error.is_none(), "the removal itself succeeded");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn pressure_lock_elects_only_one_helper() {
    let temp = tempfile::tempdir().unwrap();
    let lock = temp.path().join("tmp-floor.lock");
    let guard = claim(&lock).unwrap().expect("first helper elected");
    assert!(claim(&lock).unwrap().is_none());
    drop(guard);
    assert!(claim(&lock).unwrap().is_some());
    assert!(WorkspaceLock::try_acquire(&lock).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn pressure_records_failed_removals_and_keeps_evicting() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let bad = candidate(temp.path(), "bad", 1, true);
    let good = candidate(temp.path(), "good", 2, true);
    let protected = bad.unit.path.join("protected");
    fs::create_dir(&protected).unwrap();
    fs::write(protected.join("data"), b"keep").unwrap();
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o500)).unwrap();
    // Permissions do not bind root, so there is nothing to prove.
    if fs::write(protected.join("probe"), b"").is_ok() {
        return;
    }
    let report = enforce(
        vec![bad, good],
        &config(),
        || {
            Ok(DiskSpace {
                free: 1,
                total: 100,
            })
        },
        |candidate| Ok(Some(candidate.unit.clone())),
        |_| {},
    )
    .unwrap();
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(report.evictions.len(), 2);
    match &report.evictions[0].assist {
        super::super::assist_log::Assist::TmpEvict {
            error,
            reclaimed_bytes,
            ..
        } => {
            assert!(error.is_some());
            assert_eq!(*reclaimed_bytes, 0);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        !StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path()
        )
        .unwrap()
        .temp_unit_dir(Some("good"))
        .exists()
    );
}

#[test]
fn earliest_reclaim_counts_only_units_inside_the_grace() {
    let grace = Duration::from_secs(100);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let unit = |seconds: u64, hold| TmpUnit {
        handle: format!("u{seconds}"),
        path: std::path::PathBuf::from(format!("/nonexistent/u{seconds}")),
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        hold,
    };
    let ended = |seconds: i64| TmpHold::Ended(jiff::Timestamp::from_second(seconds).unwrap());

    let mut report = TempUnits::default();
    report.tally(&unit(100, TmpHold::Unowned), 1, now, grace);
    report.tally(&unit(200, ended(200)), 1, now, grace);
    assert_eq!(report.past_grace_units, 2);
    assert_eq!(report.earliest_reclaim, None, "no past time is reported");

    report.tally(&unit(950, ended(950)), 1, now, grace);
    report.tally(&unit(920, TmpHold::Unowned), 1, now, grace);
    report.tally(&unit(10, TmpHold::Live), 5, now, grace);
    assert_eq!(report.reclaimable_units, 4);
    assert_eq!(report.past_grace_units, 2);
    assert_eq!(report.in_use_units, 1);
    assert_eq!(
        report.earliest_reclaim,
        Some(jiff::Timestamp::from_second(1_020).unwrap()),
        "the earliest is the soonest unit still inside the grace"
    );
}
