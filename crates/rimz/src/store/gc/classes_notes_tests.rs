use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::store::{event::EventEnvelope, notes, workflow};
use serde_json::json;

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
            "version":3,"id":id,"workflow":"test","spec_dir":home.path(),
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
