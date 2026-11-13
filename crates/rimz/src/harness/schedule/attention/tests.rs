use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

thread_local! {
    /// How many times this thread's [`read_room`] calls listed the room's workflow runs.
    pub(super) static RUN_LISTINGS: Cell<usize> = const { Cell::new(0) };
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.clone(), None);
                pending.push(path);
            } else {
                files.insert(path.clone(), Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    files
}

/// A booking waiting on `pr[feature]` and `pr[other]`, its room's forge cache holding an
/// open pull request for `feature` and nothing for `other`.
#[test]
fn read_room_reads_a_seeded_room_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let home = dir.path().join("home");
    let state = StatePaths::for_project_root_under(&root, &home).unwrap();
    let runtime = RuntimePaths::for_state_under(&state, &dir.path().join("run"));
    let name = "attention-reader-booking";
    let entry = TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("start the next stage".to_owned()),
        stay: true,
        when: Some(vec!["pr[feature]=merged && pr[other]=merged".to_owned()]),
        root: root.clone(),
        ..TaskEntry::default()
    };
    let workspace = crate::workspace::WorkspaceResolver::resolve(&root, None).unwrap();
    crate::workspace::record::write(
        &state,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    super::super::instances::insert(&state, name, &entry).unwrap();
    let cache = runtime.pr_state_path();
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let scope = root.to_string_lossy();
    std::fs::write(
        &cache,
        serde_json::to_vec(&serde_json::json!({
            "branch_ci": {},
            "path_repos": {scope.as_ref(): "repo"},
            "repos": {"repo": {"ok": true, "refreshed_at_ms": 1_000}},
            "branches": {"repo": {"feature": {"pr": {"state": "open", "number": 1640}}}},
        }))
        .unwrap(),
    )
    .unwrap();
    let before = tree(dir.path());
    let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let stats = BTreeMap::new();
    let read = |open| {
        read_room(
            &root,
            &state,
            &runtime,
            ReadContext {
                open,
                stats: &stats,
                now: &now,
            },
        )
        .remove(name)
        .expect("the room's task")
    };

    let open = read(true);
    let closed = read(false);

    assert_eq!(
        (open.attention, open.fired, open.blocking),
        (
            None,
            false,
            Some(BlockingTerm {
                key: "pr[feature]".to_owned(),
                reading: Some("open".to_owned()),
                pr: Some(1640),
                more: 1,
            })
        ),
        "an unread `pr[other]` raises no cause"
    );
    assert_eq!(
        (closed.attention, closed.blocking),
        (
            None,
            Some(BlockingTerm {
                key: "pr[feature]".to_owned(),
                reading: None,
                pr: None,
                more: 1,
            })
        ),
        "a closed room reads every forge key unread"
    );
    assert!(
        !read_room(
            &root,
            &state,
            &runtime,
            ReadContext {
                open: true,
                stats: &stats,
                now: &now,
            },
        )
        .contains_key("no-such-task")
    );
    assert_eq!(tree(dir.path()), before, "the reader wrote nothing");
}

#[test]
fn parked_replaces_only_a_lost_watcher() {
    assert_eq!(
        [
            None,
            Some(Attention::Lost),
            Some(Attention::Failing),
            Some(Attention::CheckoutGone),
        ]
        .map(parked_overrides),
        [true, true, false, false]
    );
}

/// A room at `dir/project` holding `entry` as task `name`, its forge cache holding `branches`.
fn seeded_room(
    dir: &Path,
    name: &str,
    entry: impl FnOnce(&Path) -> TaskEntry,
    branches: serde_json::Value,
) -> (PathBuf, StatePaths, RuntimePaths) {
    let root = dir.join("project");
    std::fs::create_dir(&root).unwrap();
    let state = StatePaths::for_project_root_under(&root, &dir.join("home")).unwrap();
    let runtime = RuntimePaths::for_state_under(&state, &dir.join("run"));
    let workspace = crate::workspace::WorkspaceResolver::resolve(&root, None).unwrap();
    crate::workspace::record::write(
        &state,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    super::super::instances::insert(&state, name, &entry(&root)).unwrap();
    // The clock pass's first sight, before the forge probe, so the readings count.
    let stamps = runtime.lane_path("loop-fire.json");
    std::fs::create_dir_all(stamps.parent().unwrap()).unwrap();
    std::fs::write(
        &stamps,
        serde_json::to_vec(&serde_json::json!({name: Timestamp::UNIX_EPOCH})).unwrap(),
    )
    .unwrap();
    let cache = runtime.pr_state_path();
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(
        &cache,
        serde_json::to_vec(&serde_json::json!({
            "branch_ci": {},
            "path_repos": {root.to_string_lossy().as_ref(): "repo"},
            "repos": {"repo": {"ok": true, "refreshed_at_ms": 1_000}},
            "branches": {"repo": branches},
        }))
        .unwrap(),
    )
    .unwrap();
    (root, state, runtime)
}

fn read_seeded(room: &(PathBuf, StatePaths, RuntimePaths), name: &str, open: bool) -> TaskReading {
    let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    read_room(
        &room.0,
        &room.1,
        &room.2,
        ReadContext {
            open,
            stats: &BTreeMap::new(),
            now: &now,
        },
    )
    .remove(name)
    .expect("the room's task")
}

#[test]
fn read_room_lists_the_rooms_runs_once_for_all_its_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let room = seeded_room(
        dir.path(),
        "first-booking",
        booking("pr[x]=merged"),
        serde_json::json!({"x": pr("open")}),
    );
    for name in ["second-booking", "third-booking"] {
        super::super::instances::insert(&room.1, name, &booking("pr[x]=merged")(&room.0)).unwrap();
    }
    let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let before = RUN_LISTINGS.get();

    let read = read_room(
        &room.0,
        &room.1,
        &room.2,
        ReadContext {
            open: true,
            stats: &BTreeMap::new(),
            now: &now,
        },
    );

    assert_eq!(
        read.keys().map(String::as_str).collect::<Vec<_>>(),
        ["first-booking", "second-booking", "third-booking"]
    );
    assert_eq!(RUN_LISTINGS.get() - before, 1);
}

#[test]
fn a_blocking_term_counts_each_further_unmet_term() {
    let name = "repeated-term-booking";
    let dir = tempfile::tempdir().unwrap();
    let room = seeded_room(
        dir.path(),
        name,
        booking("pr[x]=merged && pr[x]=merged"),
        serde_json::json!({"x": pr("open")}),
    );
    let blocking = read_seeded(&room, name, true)
        .blocking
        .expect("a blocking term");
    assert_eq!((blocking.key.as_str(), blocking.more), ("pr[x]", 1));
}

fn booking(when: &str) -> impl FnOnce(&Path) -> TaskEntry {
    let when = when.to_owned();
    move |root| TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("start the next stage".to_owned()),
        stay: true,
        when: Some(vec![when]),
        root: root.to_path_buf(),
        ..TaskEntry::default()
    }
}

fn pr(state: &str) -> serde_json::Value {
    serde_json::json!({"pr": {"state": state, "number": 1640}})
}

#[test]
fn a_closed_pull_request_needs_you_only_while_nothing_else_can_meet_the_condition() {
    let name = "closed-pr-booking";
    let cause = |when: &str, branches, open| {
        let dir = tempfile::tempdir().unwrap();
        let room = seeded_room(dir.path(), name, booking(when), branches);
        read_seeded(&room, name, open).attention
    };
    let closed = || serde_json::json!({"x": pr("closed"), "y": pr("open")});
    let expected = Some((
        Attention::PrClosed,
        "pr[x] closed without merging".to_owned(),
    ));
    assert_eq!(cause("pr[x]=merged", closed(), true), expected);
    assert_eq!(
        cause("pr[y]=merged && pr[x]=merged", closed(), true),
        expected
    );
    for (when, branches, open) in [
        ("pr[x]=merged", closed(), false),
        ("pr[x]=merged", serde_json::json!({"x": pr("open")}), true),
        ("pr[x]=merged", serde_json::json!({}), true),
        ("pr[x]=merged || pr[y]=merged", closed(), true),
        ("!(pr[x]=merged) && pr[y]=merged", closed(), true),
    ] {
        assert_eq!(cause(when, branches, open), None, "{when}, open={open}");
    }
    let dir = tempfile::tempdir().unwrap();
    let routine = seeded_room(
        dir.path(),
        name,
        |root| TaskEntry {
            stay: false,
            ..booking("pr[x]=merged")(root)
        },
        closed(),
    );
    assert_eq!(
        read_seeded(&routine, name, true).attention,
        None,
        "a routine never raises it"
    );
}

#[test]
fn a_fired_booking_with_a_gone_checkout_is_finished() {
    let name = "gone-checkout-booking";
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path().join("gone");
    let room = seeded_room(
        dir.path(),
        name,
        |root| TaskEntry {
            dir: Some(checkout.clone()),
            ..booking("pr[x]=merged")(root)
        },
        serde_json::json!({}),
    );
    let unfired = read_seeded(&room, name, true);
    assert_eq!(
        (unfired.attention, unfired.fired),
        (
            Some((Attention::CheckoutGone, "checkout gone is gone".to_owned())),
            false
        )
    );
    let ledger = launch_ledger::workflow_path(&room.1);
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(
        &ledger,
        serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "tasks": {name: {checkout.to_string_lossy().as_ref(): {
                "at": Timestamp::now(), "scope": "worktree",
            }}},
        }))
        .unwrap(),
    )
    .unwrap();
    let fired = read_seeded(&room, name, true);
    assert_eq!((fired.attention, fired.fired), (None, true));
}

type Room = (PathBuf, StatePaths, RuntimePaths);

/// A workflow run of `task` scoped to `project_root`, in `state` since `created_at`.
fn run_record(task: &str, project_root: &Path, state: &str, created_at: Timestamp) -> RunRecord {
    serde_json::from_value(serde_json::json!({
        "version": crate::store::workflow::VERSION, "id": crate::ids::WorkflowRunId::new(),
        "workflow": "review", "task": task, "spec_dir": "/definitions/review",
        "spec_digest": "abc", "scope": {"kind": "worktree", "path": project_root},
        "project_root": project_root, "created_at": created_at, "entered_at": created_at,
        "state": state, "node": "check", "visits": {}, "visit_grant": 0,
        "deadline_fired": false, "next_seq": 1,
    }))
    .unwrap()
}

/// A shell decline's resident retry, waiting inside its run.
fn sleeping(mut run: RunRecord) -> RunRecord {
    run.wait = Some(
        serde_json::from_value(serde_json::json!({
            "node": "recheck-check", "outcome": "sleeping", "first": run.created_at,
            "last": run.created_at, "count": 1, "recheck_at": null,
        }))
        .unwrap(),
    );
    run
}

fn seed_run(room: &Room, run: RunRecord) {
    let guard = crate::disk::lock::WorkspaceLock::acquire(&room.1.workspace_lock).unwrap();
    crate::store::workflow::initialize(&room.1, &run, b"spec", &guard).unwrap();
}

#[test]
fn a_run_in_progress_holds_back_only_the_failure_it_follows() {
    let name = "retried-check";
    let failed_at = Timestamp::now() - jiff::SignedDuration::from_mins(5);
    let mut failed = super::super::run_log::LoopRunRecord::new(
        name,
        super::super::run_log::LoopRunResult::Failed,
        super::super::run_log::LoopRunMode::Manual,
        0,
    );
    failed.at = failed_at;
    let stats = BTreeMap::from([(
        name.to_owned(),
        LoopRunStats {
            runs: 1,
            last: Some(failed.clone()),
            last_kind: None,
            spend_today_usd: 0.0,
            acting: super::super::run_log::acting_run(&[failed]),
            heard: None,
        },
    )]);
    let cause = |seed: &dyn Fn(&Room)| {
        let dir = tempfile::tempdir().unwrap();
        let room = seeded_room(
            dir.path(),
            name,
            |root| TaskEntry {
                root: root.to_path_buf(),
                check: Some("false".into()),
                every: Some("1h".to_owned()),
                max_strikes: Some(0),
                ..TaskEntry::default()
            },
            serde_json::json!({}),
        );
        seed(&room);
        let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
        read_room(
            &room.0,
            &room.1,
            &room.2,
            ReadContext {
                open: false,
                stats: &stats,
                now: &now,
            },
        )
        .remove(name)
        .expect("the room's task")
        .attention
        .map(|(cause, _)| cause)
    };
    let later = failed_at + jiff::SignedDuration::from_mins(1);
    let earlier = failed_at - jiff::SignedDuration::from_mins(1);
    assert_eq!(
        cause(&|room| seed_run(room, run_record(name, &room.0, "active", later))),
        None,
        "the retry is in progress"
    );
    assert_eq!(
        cause(&|room| seed_run(room, sleeping(run_record(name, &room.0, "active", later)))),
        None,
        "the retry waits inside its run"
    );
    for (case, seed) in [
        ("no run", Box::new(|_: &Room| {}) as Box<dyn Fn(&Room)>),
        (
            "the retry ended",
            Box::new(|room: &Room| seed_run(room, run_record(name, &room.0, "failed", later))),
        ),
        (
            "a sibling started before the failure",
            Box::new(|room: &Room| seed_run(room, run_record(name, &room.0, "active", earlier))),
        ),
        (
            "a run started at the failure's instant",
            Box::new(|room: &Room| seed_run(room, run_record(name, &room.0, "active", failed_at))),
        ),
        (
            "a parked run",
            Box::new(|room: &Room| seed_run(room, run_record(name, &room.0, "parked", later))),
        ),
        (
            "another task's run",
            Box::new(|room: &Room| {
                seed_run(room, run_record("other-task", &room.0, "active", later))
            }),
        ),
        (
            "a run under another root",
            Box::new(|room: &Room| {
                seed_run(
                    room,
                    run_record(name, &room.0.join("elsewhere"), "active", later),
                )
            }),
        ),
    ] {
        assert_eq!(cause(&*seed), Some(Attention::Failing), "{case}");
    }
}

#[test]
fn a_run_in_progress_is_the_lock_or_an_active_run_after_the_acting_result() {
    let root = Path::new("/repo");
    let acting_at = Timestamp::now() - jiff::SignedDuration::from_mins(5);
    let after = |minutes| acting_at + jiff::SignedDuration::from_mins(minutes);
    let waiting = sleeping(run_record("task", root, "active", after(1)));
    let executing = [
        run_record("task", root, "active", after(3)),
        run_record("task", root, "active", after(2)),
    ];
    let read = |lock: Option<u32>, runs: &[RunRecord], acting_at| {
        run_in_progress(lock, runs, "task", root, acting_at)
    };
    assert_eq!(
        read(None, std::slice::from_ref(&waiting), Some(acting_at)),
        Some(InProgress::Run {
            since: after(1),
            executing: false,
        }),
        "a run that only waits is in progress but not executing"
    );
    let runs = [&[waiting][..], &executing[..]].concat();
    assert_eq!(
        read(None, &runs, Some(acting_at)),
        Some(InProgress::Run {
            since: after(2),
            executing: true,
        }),
        "the earliest executing run outranks a waiting one"
    );
    assert_eq!(read(None, &runs, Some(after(3))), None);
    assert_eq!(
        read(Some(7), &[], Some(after(3))),
        Some(InProgress::Locked(7)),
        "the held run lock is a run in progress, with or without a run record"
    );
    assert_eq!(read(Some(7), &runs, None), Some(InProgress::Locked(7)));
    // With no acting result (a first fire), every active run of the task counts, and the
    // exclusions that need no cutoff still hold.
    let first = [
        run_record("task", root, "parked", after(-9)),
        run_record("other-task", root, "active", after(-9)),
        run_record("task", Path::new("/elsewhere"), "active", after(-9)),
        run_record("task", root, "active", after(-8)),
    ];
    assert_eq!(
        read(None, &first, None),
        Some(InProgress::Run {
            since: after(-8),
            executing: true,
        }),
        "a first fire's run reads in progress"
    );
    assert_eq!(
        read(None, &first[..3], None),
        None,
        "parked, foreign, elsewhere"
    );
    assert!(InProgress::<u32>::Locked(7).executing());
    assert!(
        !InProgress::<u32>::Run {
            since: acting_at,
            executing: false
        }
        .executing()
    );
}
