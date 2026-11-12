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
