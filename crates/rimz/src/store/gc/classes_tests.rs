use super::*;
use std::fs;
use std::time::{Duration, SystemTime};

fn temp_owner(agent: &crate::agents::AgentState) -> Option<String> {
    match agent.name.as_deref() {
        Some("child") => Some("parent".to_owned()),
        _ => agent.name.clone(),
    }
}

#[test]
fn empty_dir_gc_does_not_follow_parent_symlinks() {
    for machine_level in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let target = temp.path().join("elsewhere");
        let foreign = if machine_level {
            target.join(paths.dir_name.as_str()).join("saved")
        } else {
            target.join("saved")
        };
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("report"), "keep").unwrap();
        fs::File::open(&foreign)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
            .unwrap();
        let link = if machine_level {
            paths.empty_dir_root()
        } else {
            paths.empty_room_dir()
        };
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, link).unwrap();
        collect_empty(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        assert!(
            foreign.join("report").exists(),
            "GC must not follow a symlinked empty-dir parent"
        );
    }
}

#[test]
fn empty_dirs_wait_for_record_pruning_and_use_cwd_not_handle() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let paths = crate::StatePaths::under(crate::WorkspaceId::from_project_root(temp.path()), &link)
        .unwrap();
    let dir = home
        .join("empty")
        .join(paths.dir_name.as_str())
        .join("original-name");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("report"), "saved").unwrap();
    fs::File::open(&dir)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
        .unwrap();
    let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    let mut retained = crate::testkit::agent_state("claude", "ended", now);
    retained.name = Some("new-handle".into());
    retained.worktree_path = Some(dir.display().to_string());
    retained.ended_at = Some(now);
    collect_empty(
        &paths,
        &vec![retained].into(),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    assert!(dir.exists());
    let recent = dir.with_file_name("recent");
    fs::create_dir(&recent).unwrap();
    let mut preview = GcReport::default();
    collect_empty(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(true),
        &mut preview,
    )
    .unwrap();
    assert_eq!(preview.sidecar_files_removed, 1);
    assert!(dir.exists());
    let mut actual = GcReport::default();
    collect_empty(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut actual,
    )
    .unwrap();
    assert_eq!(preview, actual);
    assert!(!dir.exists());
    assert!(recent.exists() && dir.parent().unwrap().exists());
}

#[test]
fn output_sweep_does_not_hold_the_store_lock() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let reader = paths.out_reader_dir(Some("armer"));
    fs::create_dir_all(&reader).unwrap();
    let pane_lock = paths.lock_path("pane-write/free.lock");
    let held_path = paths.lock_path("sidebar-launch.lock");
    let held = crate::disk::lock::WorkspaceLock::acquire(&held_path).unwrap();
    drop(crate::disk::lock::WorkspaceLock::acquire(&pane_lock).unwrap());
    let output = reader.join("wait-old.output");
    fs::File::create(&output)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
        .unwrap();
    collect_state(
        &paths,
        GcPolicy {
            older_than: retention::DEFAULT_OLDER_THAN,
            tmp_grace: retention::DEFAULT_TMP_GRACE,
            transcript_retention: TranscriptRetention::Forever,
        },
        false,
        &|_| {
            assert!(
                crate::disk::lock::WorkspaceLock::try_acquire(&paths.workspace_lock)
                    .unwrap()
                    .is_some(),
                "a hook writer can acquire the store lock during output collection"
            );
            Ok(false)
        },
        &|_| temp_owner,
    )
    .unwrap();
    assert!(!output.exists());
    assert!(
        !paths.workspace_lock.exists(),
        "idle workspace lock has no exemption"
    );
    assert!(!pane_lock.exists(), "nested room locks are swept too");
    assert!(held_path.exists(), "busy state lock survives");
    drop(held);
}

#[test]
fn out_files_keep_recent_live_and_recorded_owners_and_preview_expiry() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let reader = paths.out_reader_dir(Some("parent"));
    let emptied = paths.out_reader_dir(Some("gone"));
    fs::create_dir_all(&reader).unwrap();
    fs::create_dir_all(&emptied).unwrap();
    let file = |dir: &Path, name: &str, aged: bool| {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        if aged {
            file.set_modified(old).unwrap();
        }
        path
    };
    let kept = [
        file(&reader, "wait-recent.output", false),
        file(&reader, "wait-live.output", true),
        file(&reader, "fresh.output", false),
        file(&reader, "recorded.output", true),
        file(&reader, "recorded.2.output", true),
        file(&reader, "unrelated.txt", true),
    ];
    let removed = [
        file(&reader, "wait-old.output", true),
        file(&reader, "orphan.output", true),
        file(&emptied, "orphan.1234.output", true),
    ];
    let mut run = crate::store::run::RunRecord::new(
        paths.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        String::new(),
        temp.path().to_owned(),
    );
    run.agent_name = Some("recorded".to_owned());
    run.status = crate::store::run::RunStatus::Completed;
    crate::store::run::write(&paths.runs_dir, &run).unwrap();
    let watcher = |name: &str| Ok(name == "wait-live");
    let mut preview = GcReport::default();
    collect_out(&paths, &watcher, &mut Sweep::new(true), &mut preview).unwrap();
    assert_eq!(preview.sidecar_files_removed, 3);
    assert_eq!(preview.wait_outputs_removed, 1);
    assert_eq!(preview.dirs_removed, 1);
    assert!(removed.iter().all(|path| path.exists()));
    let mut actual = GcReport::default();
    collect_out(&paths, &watcher, &mut Sweep::new(false), &mut actual).unwrap();
    assert_eq!(preview, actual);
    assert!(kept.iter().all(|path| path.exists()));
    assert!(removed.iter().all(|path| !path.exists()));
    assert!(!emptied.exists(), "an emptied reader dir goes");
    assert!(reader.exists());
}

#[test]
fn tmp_units_follow_the_grace_rule_and_keep_the_unnamed_unit() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(25 * 3_600);
    let unit = |owner: Option<&str>| {
        let dir = paths.temp_unit_dir(owner);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("note"), b"temp").unwrap();
        fs::File::open(dir.join("note"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::File::open(&dir).unwrap().set_modified(old).unwrap();
        dir
    };
    let (ended, live, orphan, unnamed) = (
        unit(Some("ended")),
        unit(Some("live")),
        unit(Some("orphan")),
        unit(None),
    );
    let recent = paths.temp_unit_dir(Some("recent"));
    fs::create_dir_all(&recent).unwrap();
    fs::File::open(&recent)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 3_600))
        .unwrap();
    let mut ended_row = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
    ended_row.name = Some("ended".to_owned());
    ended_row.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    let mut live_row = ended_row.clone();
    live_row.name = Some("live".to_owned());
    live_row.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::pane::RuntimeOwnerKind::Agent,
        "live",
    ));
    let mut report = GcReport::default();
    collect_tmp(
        &paths,
        &vec![ended_row, live_row].into(),
        retention::DEFAULT_TMP_GRACE,
        &|agent, _| temp_owner(agent),
        &mut Sweep::new(false),
        &mut report,
    )
    .unwrap();
    assert!(!ended.exists() && !orphan.exists());
    assert!(live.exists(), "a live owner keeps its unit past the grace");
    assert!(unnamed.exists() && recent.exists());
}

#[test]
fn tmp_shared_unit_keeps_live_or_open_children() {
    for live in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let dir = paths.temp_unit_dir(Some("parent"));
        fs::create_dir_all(&dir).unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        fs::File::open(&dir).unwrap().set_modified(old).unwrap();
        let mut parent = crate::testkit::agent_state("claude", "parent-id", jiff::Timestamp::now());
        parent.name = Some("parent".into());
        parent.ended_at = Some(old.try_into().unwrap());
        let mut child = crate::testkit::agent_state("claude", "child-id", jiff::Timestamp::now());
        child.name = Some("child".into());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        if live {
            child.ended_at = parent.ended_at;
            child.runtime_owner = Some(crate::store::runtime::current_process_owner(
                crate::pane::RuntimeOwnerKind::Agent,
                "child",
            ));
        }
        collect_tmp(
            &paths,
            &vec![parent, child].into(),
            retention::DEFAULT_TMP_GRACE,
            &|agent, _| temp_owner(agent),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        assert!(
            dir.exists(),
            "child hold must keep the shared unit (live={live})"
        );
    }
}

#[cfg(unix)]
#[test]
fn tmp_errors_do_not_abort_other_units() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let blocked = paths.temp_unit_dir(Some("blocked"));
    let readable = blocked.join("nested");
    let unreadable = paths.temp_unit_dir(Some("unreadable"));
    let removable = paths.temp_unit_dir(Some("removable"));
    for dir in [&readable, &unreadable, &removable] {
        fs::create_dir_all(dir).unwrap();
        fs::File::open(dir).unwrap().set_modified(old).unwrap();
    }
    fs::write(readable.join("file"), b"blocked").unwrap();
    for dir in [&readable, &blocked] {
        fs::File::open(dir).unwrap().set_modified(old).unwrap();
    }
    fs::set_permissions(&readable, fs::Permissions::from_mode(0o500)).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    // Permissions do not bind root, so there is nothing to prove.
    if fs::write(readable.join("probe"), b"").is_ok() {
        return;
    }
    let result = collect_tmp(
        &paths,
        &Vec::new().into(),
        retention::DEFAULT_TMP_GRACE,
        &|agent, _| temp_owner(agent),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    );
    fs::set_permissions(&readable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        result.is_ok(),
        "one bad unit must not abort tmp: {result:?}"
    );
    assert!(!removable.exists());
    assert!(blocked.exists() && unreadable.exists());
}

#[test]
fn owned_units_and_terminal_runs_expire_together() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let orphan = paths.agents_dir.join("orphan");
    fs::create_dir_all(orphan.join("scratch")).unwrap();
    fs::write(orphan.join("scratch/note"), b"keep together").unwrap();
    fs::File::open(orphan.join("scratch"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    fs::File::open(&orphan).unwrap().set_modified(old).unwrap();
    let mut run = crate::store::run::RunRecord::new(
        paths.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        String::new(),
        temp.path().to_owned(),
    );
    run.status = crate::store::run::RunStatus::Completed;
    crate::store::run::write(&paths.runs_dir, &run).unwrap();
    let terminal = paths.runs_dir.join(format!("{}.json", run.run_id));
    fs::File::open(&terminal)
        .unwrap()
        .set_modified(old)
        .unwrap();
    run.run_id = crate::ids::RunId::new();
    run.status = crate::store::run::RunStatus::Running;
    crate::store::run::write(&paths.runs_dir, &run).unwrap();
    let running = paths.runs_dir.join(format!("{}.json", run.run_id));
    fs::File::open(&running).unwrap().set_modified(old).unwrap();
    let mut preview = GcReport::default();
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(true),
        &mut preview,
    )
    .unwrap();
    assert_eq!(preview.sidecar_files_removed, 2);
    assert!(orphan.exists() && terminal.exists() && running.exists());
    let mut actual = GcReport::default();
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut actual,
    )
    .unwrap();
    assert_eq!(preview, actual);
    assert!(!orphan.exists() && !terminal.exists() && running.exists());
}

#[test]
fn owned_grace_uses_ended_at_but_a_live_owner_always_wins() {
    let now = SystemTime::now();
    let old = now - Duration::from_secs(8 * 86_400);
    let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
    agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    assert!(owned_expired(Some(&agent), old, now));
    assert!(!owned_expired(Some(&agent), now, now));
    agent.ended_at =
        Some(jiff::Timestamp::try_from(now - Duration::from_secs(6 * 86_400)).unwrap());
    assert!(!owned_expired(Some(&agent), old, now));
    agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    agent.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::pane::RuntimeOwnerKind::Agent,
        "ended",
    ));
    assert!(!owned_expired(Some(&agent), old, now));
    assert!(owned_expired(None, old, now));
    assert!(!owned_expired(None, now, now));
    agent.runtime_owner = None;
    agent.ended_at = None;
    assert!(!owned_expired(Some(&agent), old, now));
}

#[test]
fn terminal_workflow_homes_expire_but_live_parked_and_recent_runs_stay() {
    use crate::store::workflow::{self, RunRecord};
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let mut homes = Vec::new();
    for (state, age) in [
        ("done", true),
        ("failed", true),
        ("canceled", true),
        ("active", true),
        ("parked", true),
        ("done", false),
    ] {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":1, "id":crate::ids::WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/definitions/review", "spec_digest":"abc", "prompt":null,
            "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
            "created_at":"2026-01-01T00:00:00Z", "state":state, "park":null, "node":"pr",
            "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
            "wait":null, "visits":{}, "visit_grant":0, "deadline":null,
            "deadline_fired":false, "next_seq":1, "attempt":null, "prev_attempt":null,
            "terminal_at":null
        }))
        .unwrap();
        let guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
        workflow::initialize(&paths, &record, b"spec", &guard).unwrap();
        let home = paths.workflow(&record.id);
        if age {
            fs::File::open(&home.record)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        homes.push(home);
    }
    for contents in [None, Some("corrupt")] {
        let home = paths.workflow(&crate::ids::WorkflowRunId::new());
        fs::create_dir_all(&home.home).unwrap();
        if let Some(contents) = contents {
            fs::write(&home.record, contents).unwrap();
        }
        homes.push(home);
    }
    let orphan = paths.workflow(&crate::ids::WorkflowRunId::new());
    fs::create_dir_all(&orphan.work).unwrap();
    fs::write(&orphan.spec, "spec").unwrap();
    fs::File::open(&orphan.home)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mut preview = GcReport::default();
    let collected = collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(true),
        &mut preview,
    );
    assert!(
        collected.is_ok(),
        "unreadable workflow homes must not block GC: {collected:?}"
    );
    assert_eq!(
        preview.sidecar_files_removed, 7,
        "old terminal and old recordless workflow homes must be collected together"
    );
    assert!(homes.iter().all(|p| p.home.exists()) && orphan.home.exists());
    let mut actual = GcReport::default();
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut actual,
    )
    .unwrap();
    assert_eq!(actual, preview);
    assert!(homes[..3].iter().all(|p| !p.home.exists()));
    assert!(homes[3..].iter().all(|p| p.home.exists()));
    assert!(!orphan.home.exists(), "an old recordless home must go");
}

#[test]
fn owned_sweep_keeps_a_relaunch_with_a_fresh_direct_child() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let unit = paths.agents_dir.join("resumed");
    fs::create_dir_all(unit.join("scratch")).unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    fs::File::open(&unit).unwrap().set_modified(old).unwrap();
    let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
    agent.name = Some("resumed".to_owned());
    agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    collect_owned(
        &paths,
        &vec![agent].into(),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    assert!(unit.join("scratch").exists());
}

#[test]
fn audit_expires_then_trims_oldest_with_dry_run_parity() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    fs::create_dir_all(paths.audit_path("")).unwrap();
    let now = SystemTime::now();
    let expired = paths.audit_path("expired.jsonl");
    let oldest = paths.audit_path("a-oldest.jsonl");
    let newest = paths.audit_path("newest.jsonl");
    for (path, days, bytes) in [
        (&expired, 31, 1),
        (&oldest, 29, crate::disk::retention::AUDIT_MAX_BYTES),
        (&newest, 29, 1),
    ] {
        let file = fs::File::create(path).unwrap();
        file.set_len(bytes).unwrap();
        file.set_modified(now - Duration::from_secs(days * 86_400))
            .unwrap();
    }
    let mut preview = GcReport::default();
    collect_audit(&paths, &mut Sweep::new(true), &mut preview).unwrap();
    assert_eq!(preview.sidecar_files_removed, 2);
    assert!(expired.exists() && oldest.exists() && newest.exists());
    let mut actual = GcReport::default();
    collect_audit(&paths, &mut Sweep::new(false), &mut actual).unwrap();
    assert_eq!(preview, actual);
    assert!(!expired.exists() && !oldest.exists() && newest.exists());
}

#[test]
fn default_sweep_preserves_transcript_and_messages_outside_the_audit_cap() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let expired = paths.audit_path("expired.jsonl");
    let oldest = paths.audit_path("a-oldest.jsonl");
    let newest = paths.audit_path("newest.jsonl");
    let transcript = paths.transcript_dir.join("old.jsonl");
    let messages = paths.message_history_dir.join("old.jsonl");
    let history_size = 40 * 1024 * 1024;
    for dir in [&paths.transcript_dir, &paths.message_history_dir] {
        fs::create_dir_all(dir.join("empty")).unwrap();
    }
    let now = SystemTime::now();
    for (path, days, bytes) in [
        (&expired, 31, 1),
        (&oldest, 29, retention::AUDIT_MAX_BYTES),
        (&newest, 29, 1),
        (&transcript, 60, history_size),
        (&messages, 60, history_size),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(path).unwrap();
        file.set_len(bytes).unwrap();
        file.set_modified(now - Duration::from_secs(days * 86_400))
            .unwrap();
    }
    let sweep = |dry_run| {
        collect_state(
            &paths,
            GcPolicy {
                older_than: retention::DEFAULT_OLDER_THAN,
                tmp_grace: retention::DEFAULT_TMP_GRACE,
                transcript_retention: TranscriptRetention::Forever,
            },
            dry_run,
            &|_| Ok(false),
            &|_| temp_owner,
        )
        .unwrap()
        .0
    };
    let preview = sweep(true);
    assert!(transcript.exists() && messages.exists() && expired.exists() && oldest.exists());
    let actual = sweep(false);
    assert!(
        transcript.exists() && messages.exists(),
        "history has its own retention"
    );
    assert!(
        paths.transcript_dir.join("empty").exists()
            && paths.message_history_dir.join("empty").exists()
    );
    assert!(!expired.exists() && !oldest.exists() && newest.exists());
    for (class, removed, retained) in [("audit", 2, 1), ("transcript", 0, 2 * history_size)] {
        let row = actual
            .iter()
            .find(|row| row.class == class)
            .expect("class row");
        assert_eq!(preview.iter().find(|row| row.class == class), Some(row));
        assert_eq!(row.files_removed, removed);
        if class == "transcript" {
            assert_eq!(row.bytes_removed, 0);
        }
        assert_eq!(
            serde_json::to_value(row).unwrap()["bytes_retained"],
            retained
        );
    }
}
