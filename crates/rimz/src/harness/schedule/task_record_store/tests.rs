use super::*;
use crate::harness::schedule::task_record::Scope;

fn binding(name: &str) -> Binding {
    Binding {
        scope: Scope::Machine,
        name: name.to_owned(),
    }
}

#[test]
fn add_mints_rename_rebinds_and_remove_retires() {
    let mut records = TaskRecords::empty();
    let now = Timestamp::UNIX_EPOCH;
    let (bound, _) = records.bind(&binding("nightly"));
    assert_eq!(
        records.bind(&binding("nightly")).0,
        bound,
        "binding is stable"
    );

    let minted = records.mint(&binding("nightly"), now);
    assert_ne!(minted, bound, "a re-add is a new task");
    assert!(records.get(&bound).is_some_and(|record| !record.is_live()));
    assert_eq!(
        records.live("machine::nightly").map(|(id, _)| id),
        Some(&minted)
    );

    assert!(records.rebind("machine::nightly", binding("weekly"), now));
    assert_eq!(
        records.live("machine::weekly").map(|(id, _)| id),
        Some(&minted)
    );
    assert!(records.live("machine::nightly").is_none());
    assert!(!records.rebind("machine::nightly", binding("other"), now));

    assert!(records.retire("machine::weekly", now));
    assert!(!records.retire("machine::weekly", now));
    assert_eq!(
        records.named("machine::weekly").map(|(id, _)| id),
        Some(&minted)
    );

    let (delivery, _) = records.bind(&binding("deliver"));
    assert!(records.delete("machine::deliver"));
    assert!(records.get(&delivery).is_none());

    // A rename onto a name another live record holds retires that record, as a re-add does.
    let (taken, _) = records.bind(&binding("taken"));
    let (moving, _) = records.bind(&binding("moving"));
    assert!(records.rebind("machine::moving", binding("taken"), now));
    assert!(records.get(&taken).is_some_and(|record| !record.is_live()));
    assert_eq!(
        records.live("machine::taken").map(|(id, _)| id),
        Some(&moving)
    );
}

#[test]
fn an_unchanged_edit_writes_nothing_and_an_unreadable_record_refuses_writes() {
    if super::super::test_homes::reexec_with_private_homes(
        "harness::schedule::task_record_store::tests::an_unchanged_edit_writes_nothing_and_an_unreadable_record_refuses_writes",
    ) {
        return;
    }
    let path = task_record::path();
    mutate(|_| ()).unwrap();
    assert!(
        !path.exists(),
        "a no-op neither creates nor rewrites the record"
    );

    mutate(|records| {
        records.bind(&binding("nightly"));
    })
    .unwrap();
    assert!(path.is_file());

    std::fs::write(&path, b"not json").unwrap();
    let error = mutate(|records| {
        records.bind(&binding("other"));
    })
    .unwrap_err();
    assert!(matches!(error, TaskStoreErr::Read(_)), "{error:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"not json");
}
