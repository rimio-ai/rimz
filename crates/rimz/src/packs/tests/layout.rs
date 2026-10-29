use super::*;

fn git_source(home: &Path, commit: char) -> PathBuf {
    let checkout = source(
        &home.join("packs/.checkouts"),
        &format!("repo-{}", commit.to_string().repeat(40)),
        &[("crew", ".")],
    );
    let mut record = seed(home, &[(&checkout, "repo", &["crew"])]);
    record.sources[0].source = "https://example.test/crew".to_owned();
    record.sources[0].linked = false;
    record.sources[0].reference = Some("main".to_owned());
    record.sources[0].commit = Some(commit.to_string().repeat(40));
    write_record(&home.join("packs"), &record).unwrap();
    checkout
}

#[test]
fn legacy_records_default_to_unpinned_and_serialize_the_pin() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    seed(temp.path(), &[(&source, "repo", &["crew"])]);
    let path = temp.path().join("packs/installed.json");
    let mut legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("pinned");
    write(&path, &serde_json::to_string(&legacy).unwrap());
    let record = Record::read(temp.path());
    assert!(record.is_ok(), "legacy records must still load: {record:?}");
    let encoded = serde_json::to_value(record.unwrap()).unwrap();
    assert_eq!(encoded["sources"][0]["pinned"], false);
    legacy["sources"][0]["pinned"] = true.into();
    write(&path, &serde_json::to_string(&legacy).unwrap());
    let encoded = serde_json::to_value(Record::read(temp.path()).unwrap()).unwrap();
    assert_eq!(encoded["sources"][0]["pinned"], true);
}

#[test]
fn git_records_preserve_the_transport_url() {
    let temp = tempfile::tempdir().unwrap();
    git_source(temp.path(), 'a');
    let mut encoded = serde_json::to_value(Record::read(temp.path()).unwrap()).unwrap();
    encoded["sources"][0]["url"] = "https://example.test/crew.git".into();
    let decoded: Record = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
}

#[test]
fn absolute_recorded_git_targets_are_repaired_to_relative_links() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let checkout = git_source(home, 'a');
    let entry = home.join("packs/repo");
    assert_eq!(fs::read_link(&entry).unwrap(), checkout);
    include(home, &name("crew"), &item(ItemType::Teams, "one")).unwrap();
    assert_eq!(
        fs::read_link(&entry).unwrap(),
        Path::new(".checkouts").join(format!("repo-{}", "a".repeat(40)))
    );
    assert!(entry.join("rimz-packs.toml").is_file());
}

#[test]
fn removal_survives_broken_git_maintenance_and_reports_the_warning() {
    for missing_commit in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let checkout = git_source(home, 'a');
        let entry = home.join("packs/repo");
        if missing_commit {
            let mut record = Record::read(home).unwrap();
            record.sources[0].commit = None;
            write_record(&home.join("packs"), &record).unwrap();
        } else {
            fs::remove_file(&entry).unwrap();
            fs::create_dir(&entry).unwrap();
            write(&entry.join("unexpected"), "Keep me.");
        }
        let result = remove(home, &name("crew"));
        assert!(
            result.is_ok(),
            "broken sources must be removable: {result:?}"
        );
        let outcome = result.unwrap();
        assert!(outcome.cleanup_warning.is_some(), "{outcome:?}");
        assert_eq!(Record::read(home).unwrap(), Record::default());
        assert!(checkout.is_dir());
        if missing_commit {
            assert_eq!(outcome.removed_entry, Some(entry));
        } else {
            assert!(outcome.removed_entry.is_none());
            assert!(entry.join("unexpected").is_file());
        }
    }
}

#[test]
fn checkout_holds_stay_addressable_after_collection() {
    use crate::disk::lock::{lock_file, try_lock_file};
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("packs");
    let checkout = root
        .join(".checkouts")
        .join(format!("repo-{}", "a".repeat(40)));
    write(&checkout.join("README.md"), "Staged.");
    let path = checkout.with_extension("lock");
    let hold = WorkspaceLock::acquire(&path).unwrap();
    let mut waiter = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let old_inode = waiter.metadata().unwrap().ino();
    assert!(matches!(
        try_lock_file(&mut waiter, &path),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    super::super::layout::maintain(&root, &Record::default()).unwrap();
    assert!(checkout.is_dir(), "GC must respect the addressable hold");
    drop(hold);
    super::super::layout::maintain(&root, &Record::default()).unwrap();
    assert!(!checkout.exists());
    assert!(!path.exists(), "GC must collect the checkout's lock too");
    lock_file(&mut waiter, &path).unwrap();
    assert_ne!(waiter.metadata().unwrap().ino(), old_inode);
    assert_eq!(
        waiter.metadata().unwrap().ino(),
        fs::metadata(&path).unwrap().ino()
    );
    assert!(WorkspaceLock::try_acquire(&path).unwrap().is_none());
    write(&checkout.join("README.md"), "Restaged.");
    super::super::layout::maintain(&root, &Record::default()).unwrap();
    assert!(checkout.is_dir(), "the reopened waiter must exclude GC");
    drop(waiter);
    super::super::layout::maintain(&root, &Record::default()).unwrap();
    assert!(!checkout.exists() && !path.exists());

    drop(WorkspaceLock::acquire(&path).unwrap());
    super::super::layout::maintain(&root, &Record::default()).unwrap();
    assert!(!path.exists(), "GC must collect an unheld orphan lock");
}

#[test]
fn every_mutation_repairs_entries_and_collects_only_unheld_unreferenced_checkouts() {
    let temp = tempfile::tempdir().unwrap();
    let mut results = Vec::new();
    for operation in ["install", "remove", "exclude", "include"] {
        let home = temp.path().join(operation);
        let current = git_source(&home, 'a');
        let root = home.join("packs/.checkouts");
        let ahead = root.join(format!("repo-{}", "b".repeat(40)));
        let held = root.join(format!("repo-{}", "c".repeat(40)));
        let abandoned = root.join(format!("repo-{}", "d".repeat(40)));
        for path in [&ahead, &held, &abandoned] {
            write(&path.join("skills/review/SKILL.md"), "Review.");
        }
        let hold = WorkspaceLock::acquire(&held.with_extension("lock")).unwrap();
        let entry = home.join("packs/repo");
        fs::remove_file(&entry).unwrap();
        symlink(&ahead, &entry).unwrap();
        let other = source(
            temp.path(),
            &format!("other-{operation}"),
            &[("other", ".")],
        );
        if operation == "remove" {
            install(&home, &other).unwrap();
            fs::remove_file(&entry).unwrap();
            symlink(&ahead, &entry).unwrap();
            write(&abandoned.join("skills/review/SKILL.md"), "Review.");
        }
        let result = match operation {
            "install" => install(&home, &other).map(|_| ()),
            "remove" => remove(&home, &name("other")).map(|_| ()),
            "exclude" => exclude(&home, &name("crew"), &item(ItemType::Teams, "one")).map(|_| ()),
            "include" => include(&home, &name("crew"), &item(ItemType::Teams, "one")).map(|_| ()),
            _ => unreachable!(),
        };
        results.push((
            operation,
            result.is_ok(),
            fs::read_link(&entry).ok()
                == Some(current.strip_prefix(home.join("packs")).unwrap().to_owned()),
            current.is_dir(),
            ahead.is_dir(),
            held.is_dir(),
            !abandoned.exists(),
        ));
        include(&home, &name("crew"), &item(ItemType::Teams, "one")).unwrap();
        results.push((
            operation,
            true,
            fs::read_link(&entry).ok()
                == Some(current.strip_prefix(home.join("packs")).unwrap().to_owned()),
            current.is_dir(),
            !ahead.exists(),
            held.is_dir(),
            !abandoned.exists(),
        ));
        drop(hold);
        include(&home, &name("crew"), &item(ItemType::Teams, "one")).unwrap();
        results.push((operation, true, true, true, true, !held.exists(), true));
    }
    assert!(
        results
            .iter()
            .all(|(_, a, b, c, d, e, f)| *a && *b && *c && *d && *e && *f),
        "repair, record root, pre-repair target, live hold, abandoned checkout: {results:?}"
    );
}
