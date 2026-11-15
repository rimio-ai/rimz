use super::*;

fn binding(name: &str) -> Binding {
    Binding {
        scope: Scope::Machine,
        name: name.to_owned(),
    }
}

fn mark(scope: crate::store::workflow::ScopeKind, result: Option<LoopRunResult>) -> FireMark {
    FireMark {
        fire: FireId::new(&TaskId::new()),
        at: Timestamp::UNIX_EPOCH,
        booked_by: Vec::new(),
        once: true,
        run: None,
        scope,
        result,
    }
}

#[test]
fn a_missing_record_is_empty_and_an_unreadable_or_newer_one_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    assert_eq!(load_from(&path).unwrap(), TaskRecords::empty());

    std::fs::write(&path, b"not json").unwrap();
    let error = load_from(&path).unwrap_err();
    assert!(matches!(error, TaskRecordErr::Decode { .. }), "{error:?}");
    let text = error.to_string();
    assert!(text.contains(&path.display().to_string()), "{text}");
    assert!(text.contains("move it aside"), "{text}");

    std::fs::write(&path, br#"{"version": 2, "tasks": {"later": "shape"}}"#).unwrap();
    let error = load_from(&path).unwrap_err();
    assert!(
        matches!(error, TaskRecordErr::Newer { version: 2, .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("upgraded"), "{error}");
}

#[test]
fn records_round_trip_and_look_up_by_binding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    let (old, live) = (TaskId::new(), TaskId::new());
    let mut records = TaskRecords::empty();
    let mut retired = TaskRecord::new(binding("nightly"));
    retired.retired_at = Some(Timestamp::UNIX_EPOCH);
    records.tasks.insert(old.clone(), retired);
    records
        .tasks
        .insert(live.clone(), TaskRecord::new(binding("nightly")));
    std::fs::write(&path, serde_json::to_vec(&records).unwrap()).unwrap();

    let loaded = load_from(&path).unwrap();
    assert_eq!(loaded, records);
    assert_eq!(
        loaded.live("machine::nightly").map(|(id, _)| id),
        Some(&live)
    );
    assert_eq!(
        loaded.named("machine::nightly").map(|(id, _)| id),
        Some(&live)
    );
    assert_eq!(loaded.by_key().len(), 1);

    records.tasks.remove(&live);
    assert!(records.live("machine::nightly").is_none());
    assert_eq!(
        records.named("machine::nightly").map(|(id, _)| id),
        Some(&old),
        "a consumed one-off is still found by name"
    );
}

#[test]
fn a_checkout_is_held_by_a_project_mark_or_its_own_unfailed_one() {
    use crate::store::workflow::ScopeKind;
    let mut record = TaskRecord::new(binding("once"));
    let (here, there) = (PathBuf::from("/here"), PathBuf::from("/there"));
    record
        .marks
        .insert(here.clone(), mark(ScopeKind::Worktree, None));
    assert!(record.launched(&here).is_some(), "an open fire holds it");
    assert!(record.launched(&there).is_none());

    record.marks.insert(
        here.clone(),
        mark(ScopeKind::Worktree, Some(LoopRunResult::Failed)),
    );
    assert!(record.launched(&here).is_none(), "a failed fire frees it");
    record.marks.insert(
        here.clone(),
        mark(ScopeKind::Worktree, Some(LoopRunResult::Completed)),
    );
    assert!(record.launched(&here).is_some());

    record
        .marks
        .insert(there.clone(), mark(ScopeKind::Project, None));
    assert_eq!(
        record
            .launched(&PathBuf::from("/elsewhere"))
            .map(|mark| mark.scope),
        Some(ScopeKind::Project)
    );

    let mut uncounted = mark(ScopeKind::Project, None);
    uncounted.once = false;
    record.marks = BTreeMap::from([(there, uncounted)]);
    assert!(
        record.launched(&here).is_none(),
        "a fire that does not count holds nothing"
    );
}
