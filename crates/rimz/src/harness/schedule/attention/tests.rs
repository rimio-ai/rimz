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
