use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::store::{event::EventEnvelope, notes, workflow};
use serde_json::json;
use std::path::PathBuf;

fn expire(run: &Path) {
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    for entry in fs::read_dir(run).unwrap() {
        fs::File::open(entry.unwrap().path())
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    fs::File::open(run).unwrap().set_modified(old).unwrap();
}

fn workflow_record(
    paths: &crate::StatePaths,
    home: &Path,
    state: &str,
) -> (workflow::RunRecord, PathBuf) {
    let id = crate::ids::WorkflowRunId::new();
    let at = jiff::Timestamp::now();
    let relative = notes::allocate(
        paths,
        notes::Kind::Workflow {
            workflow: "test".into(),
            run: id.clone(),
        },
        home,
        at,
    )
    .unwrap();
    let record = serde_json::from_value(json!({
        "version":crate::store::workflow::VERSION,"id":id,"workflow":"test","spec_dir":home,
        "spec_digest":"test","project_root":home,"notes":relative,
        "scope":{"kind":"worktree","path":home},"state":state,"node":"build",
        "created_at":at,"entered_at":at,"terminal_at":at,
        "deadline_fired":false,"next_seq":1,"visits":{},"visit_grant":0,
    }))
    .unwrap();
    let guard = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    workflow::initialize(paths, &record, b"spec", &guard).unwrap();
    (record, relative)
}

fn age(path: &Path) {
    fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
        .unwrap();
}

#[test]
fn finished_homes_expire_by_record_age_after_their_history_reaches_the_notes() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut actual = Vec::new();
    // Each case: whether the notes archive refuses the copy, and whether the record is aged.
    for (blocked, aged) in [(false, true), (true, true), (false, false)] {
        let (record, relative) = workflow_record(&paths, home.path(), "done");
        let live = paths.workflow(&record.id);
        if aged {
            age(&live.record);
        }
        let archive = paths.notes_run(Class::AutomationNotes, &relative);
        if blocked {
            fs::create_dir_all(archive.join("history.txt").join("blocker")).unwrap();
        }
        let guard = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
        // The move keeps the record's mtime: the grace runs from the run's last write.
        let moved = workflow::finish(&paths, &record.id, Some(b"history"), &guard).unwrap();
        assert_eq!(moved, workflow::Finished::Moved);
        drop(guard);
        collect_owned(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        actual.push((
            paths.finished_workflow(&record.id).home.exists(),
            fs::read(archive.join("history.txt")).ok(),
        ));
    }
    assert_eq!(
        actual,
        [
            (false, Some(b"history".to_vec())),
            (true, None),
            (true, None),
        ]
    );
}

#[test]
fn live_homes_stay_unless_they_never_got_a_record() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let (current, _) = workflow_record(&paths, home.path(), "done");
    let (stranded, _) = workflow_record(&paths, home.path(), "active");
    let (newer, _) = workflow_record(&paths, home.path(), "active");
    for (record, version) in [(&stranded, 3), (&newer, workflow::VERSION + 1)] {
        let live = paths.workflow(&record.id);
        let mut value = serde_json::to_value(record).unwrap();
        value["version"] = json!(version);
        fs::write(&live.record, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let recordless = paths.workflow(&crate::ids::WorkflowRunId::new());
    fs::create_dir_all(&recordless.home).unwrap();
    for record in [&current, &stranded, &newer] {
        age(&paths.workflow(&record.id).record);
    }
    age(&recordless.home);
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    assert_eq!(
        [&current, &stranded, &newer]
            .map(|record| paths.workflow(&record.id).home.exists())
            .into_iter()
            .chain([recordless.home.exists()])
            .collect::<Vec<_>>(),
        [true, true, true, false]
    );
}

#[test]
fn notes_stay_held_by_stranded_and_newer_live_runs_but_not_by_finished_ones() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut actual = Vec::new();
    for case in ["stranded", "newer", "finished", "gone"] {
        let state = if case == "finished" { "done" } else { "active" };
        let (record, relative) = workflow_record(&paths, home.path(), state);
        let live = paths.workflow(&record.id);
        let mut value = serde_json::to_value(&record).unwrap();
        match case {
            "stranded" => value["version"] = json!(workflow::VERSION - 1),
            "newer" => value["version"] = json!(workflow::VERSION + 1),
            _ => {}
        }
        fs::write(&live.record, serde_json::to_vec(&value).unwrap()).unwrap();
        if case == "finished" {
            let guard = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
            let moved = workflow::finish(&paths, &record.id, None, &guard).unwrap();
            assert_eq!(moved, workflow::Finished::Moved);
        }
        if case == "gone" {
            fs::remove_dir_all(&live.home).unwrap();
        }
        let run = paths.notes_run(Class::AutomationNotes, &relative);
        expire(&run);
        collect_automation_notes(
            &paths,
            HistoryRetention::Duration(Duration::from_secs(1)),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        actual.push(run.exists());
    }
    assert_eq!(actual, [true, true, false, false]);
}

#[test]
fn automation_notes_sweep_reads_workflow_ownership_and_live_launch_records() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut actual = Vec::new();
    for state in ["active", "parked", "done", "failed", "canceled"] {
        let id = crate::ids::WorkflowRunId::new();
        let at = jiff::Timestamp::now();
        let relative = notes::allocate(
            &paths,
            notes::Kind::Workflow {
                workflow: "test".into(),
                run: id.clone(),
            },
            home.path(),
            at,
        )
        .unwrap();
        let record = serde_json::from_value(json!({
            "version":crate::store::workflow::VERSION,"id":id,"workflow":"test","spec_dir":home.path(),
            "spec_digest":"test","project_root":home.path(),"notes":relative,
            "scope":{"kind":"worktree","path":home.path()},"state":state,"node":"build",
            "created_at":at,"entered_at":at,"deadline_fired":false,"next_seq":1,"visits":{},"visit_grant":0,
        })).unwrap();
        let guard = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
        workflow::initialize(&paths, &record, b"spec", &guard).unwrap();
        drop(guard);
        let run = paths.notes_run(Class::AutomationNotes, &relative);
        expire(&run);
        collect_automation_notes(
            &paths,
            HistoryRetention::Duration(Duration::from_secs(1)),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        actual.push(run.exists());
    }
    let at = jiff::Timestamp::now();
    let relative = notes::allocate(
        &paths,
        notes::Kind::Loop {
            task: "review".into(),
        },
        home.path(),
        at,
    )
    .unwrap();
    let run = paths.notes_run(Class::AutomationNotes, &relative);
    expire(&run);
    let mut observation = crate::agents::AgentLifecycleObservation::new(
        Some("loop-agent".into()),
        crate::agents::LifecycleSignal::Registered,
    );
    observation.launch.record = Some(Box::new(
        serde_json::from_value(json!({
            "model":null,"effort":null,"agent":null,"notes":relative,
        }))
        .unwrap(),
    ));
    let event = EventEnvelope::agent_lifecycle(
        paths.workspace_id.clone(),
        "room",
        "claude",
        "test",
        &observation,
    );
    crate::store::event_log::append(&paths.events_log, &event).unwrap();
    collect_automation_notes(
        &paths,
        HistoryRetention::Duration(Duration::from_secs(1)),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    actual.push(run.exists());
    observation.signal = crate::agents::LifecycleSignal::Ended;
    let event = EventEnvelope::agent_lifecycle(
        paths.workspace_id.clone(),
        "room",
        "claude",
        "test",
        &observation,
    );
    crate::store::event_log::append(&paths.events_log, &event).unwrap();
    collect_automation_notes(
        &paths,
        HistoryRetention::Duration(Duration::from_secs(1)),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    actual.push(run.exists());
    assert_eq!(actual, [true, true, false, false, false, true, false]);
}

#[test]
fn unreadable_rollup_keeps_automation_notes() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let relative = notes::allocate(
        &paths,
        notes::Kind::Loop {
            task: "test".into(),
        },
        home.path(),
        jiff::Timestamp::now(),
    )
    .unwrap();
    let run = paths.notes_run(Class::AutomationNotes, &relative);
    expire(&run);
    fs::create_dir_all(&paths.events_log).unwrap();
    collect_automation_notes(
        &paths,
        HistoryRetention::Duration(Duration::from_secs(1)),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    assert!(
        run.exists(),
        "no ownership decision without a readable rollup"
    );
}
