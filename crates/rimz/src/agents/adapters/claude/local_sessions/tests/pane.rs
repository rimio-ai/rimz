use super::*;
use crate::agents::capabilities::SessionCapability as _;
use crate::agents::local_session_cache::testkit;
use crate::agents::session::{NamedSession, PaneSessionDemand, PaneSessionNeed};

const OWNER: &str = "11111111-1111-4111-8111-111111111111";
const RECENT: &str = "22222222-2222-4222-8222-222222222222";
const NEXT: &str = "33333333-3333-4333-8333-333333333333";

fn demand(own: &[&str], named: &[&str], since: Option<&str>) -> PaneSessionDemand {
    let names = |ids: &[&str]| {
        ids.iter()
            .map(|id| NamedSession {
                id: AgentSessionId::from(*id),
                transcript_path: None,
            })
            .collect()
    };
    PaneSessionDemand {
        workspace: PathBuf::from("/workspace/project"),
        panes: vec![PaneSessionNeed {
            own: names(own),
            named: names(named),
            fresh_since: since.map(|at| at.parse().unwrap()),
        }],
    }
}

fn key(roots: &[PathBuf]) -> DiscoveryKey {
    DiscoveryKey {
        config_dirs: roots.to_vec(),
        workspaces: vec![PathBuf::from("/workspace/project")],
        project_dir_name: None,
    }
}

fn session(root: &Path, id: &str, created: &str, activity: &str) -> PathBuf {
    let project = root.join("projects/-workspace-project");
    let records = [created, activity]
        .map(|at| serde_json::json!({"cwd": "/workspace/project", "timestamp": at}).to_string());
    write_session(&project, id, &[&records[0], &records[1]]);
    project.join(format!("{id}.jsonl"))
}

/// An empty session file last written in 2024, before every test window.
fn abandoned(path: &Path) {
    fs::write(path, "").unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_717_200_000)),
        )
        .unwrap();
}

fn ids(found: &[LocalSessionObservation]) -> Vec<String> {
    let mut ids = found
        .iter()
        .map(|row| row.session_id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn pane_warm_work_is_independent_of_history_for_settled_and_open_windows() {
    let mut actual = Vec::new();
    for history in [50, 500] {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..history {
            session(
                temp.path(),
                &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
                "2024-01-01T00:00:00Z",
                "2025-01-01T00:00:00Z",
            );
            abandoned(&temp.path().join(format!(
                "projects/-workspace-project/{index:08x}-bbbb-4bbb-8bbb-{index:012x}.jsonl"
            )));
        }
        session(
            temp.path(),
            OWNER,
            "2024-01-01T00:00:00Z",
            "2024-01-01T00:00:00Z",
        );
        session(
            temp.path(),
            RECENT,
            "2025-01-02T00:00:00Z",
            "2025-01-02T00:00:00Z",
        );
        for open in [false, true] {
            let mut demand = demand(&[OWNER], &[], Some("2025-01-01T00:00:00Z"));
            if open {
                demand.panes.push(PaneSessionNeed {
                    own: vec![],
                    named: vec![],
                    fresh_since: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                });
            }
            let key = key(&[temp.path().to_path_buf()]);
            let mut cache = ClaudePaneDiscovery::default();
            let now = Instant::now();
            cache.refresh(key.clone(), std::slice::from_ref(&demand), now);
            let before = testkit::read_counts();
            let found = cache.refresh(key, &[demand], now);
            let after = testkit::read_counts();
            actual.push((
                history,
                open,
                ids(&found),
                (after.0 - before.0, after.1 - before.1, after.2 - before.2),
            ));
        }
    }
    let mut expected = Vec::new();
    for history in [50, 500] {
        expected.push((history, false, vec![OWNER.to_owned()], (1, 0, 0)));
        expected.push((
            history,
            true,
            vec![OWNER.to_owned(), RECENT.to_owned()],
            (3, 0, 0),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn pane_window_reopen_keeps_learned_history_until_a_listing_removes_it() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("projects/-workspace-project");
    for index in 0..50 {
        let cwd = if index % 2 == 0 {
            "/workspace/project"
        } else {
            "/another/workspace"
        };
        let record = serde_json::json!({"cwd": cwd, "timestamp": "2024-01-01T00:00:00Z"});
        write_session(
            &project,
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            &[&record.to_string()],
        );
    }
    let owner = session(
        temp.path(),
        OWNER,
        "2025-01-02T00:00:00Z",
        "2025-01-02T00:00:00Z",
    );
    let key = key(&[temp.path().to_path_buf()]);
    let now = Instant::now();
    let mut cache = ClaudePaneDiscovery::default();
    let open = demand(&[], &[], Some("2025-01-02T00:00:00Z"));
    assert_eq!(ids(&cache.refresh(key.clone(), &[open], now)), [OWNER]);
    let settled = demand(&[OWNER], &[], Some("2025-01-02T00:00:00Z"));
    for _ in 0..2 {
        assert_eq!(
            ids(&cache.refresh(key.clone(), std::slice::from_ref(&settled), now)),
            [OWNER]
        );
    }
    session(
        temp.path(),
        RECENT,
        "2025-01-03T00:00:00Z",
        "2025-01-03T00:00:00Z",
    );
    let reopen = demand(&[], &[], Some("2025-01-03T00:00:00Z"));
    let before = testkit::read_counts();
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&reopen), now)),
        [RECENT]
    );
    let after = testkit::read_counts();
    assert_eq!(after.2 - before.2, 2, "only the new head and tail are read");
    assert!(
        after.1 - before.1 <= 1,
        "each folder is listed at most once"
    );

    fs::remove_file(owner).unwrap();
    cache.refresh(key.clone(), std::slice::from_ref(&reopen), now);
    session(
        temp.path(),
        OWNER,
        "2025-01-03T00:00:00Z",
        "2025-01-03T00:00:00Z",
    );
    let before = testkit::read_counts();
    assert_eq!(ids(&cache.refresh(key, &[reopen], now)), [OWNER, RECENT]);
    assert_eq!(testkit::read_counts().2 - before.2, 2);
}

#[test]
fn pane_workspace_changes_keep_the_account_history_warm() {
    let temp = tempfile::tempdir().unwrap();
    for id in [OWNER, RECENT, NEXT] {
        session(
            temp.path(),
            id,
            "2024-01-01T00:00:00Z",
            "2024-01-01T00:00:00Z",
        );
        let created = if id == RECENT {
            "2025-01-02T00:00:00Z"
        } else {
            "2024-01-01T00:00:00Z"
        };
        let record = serde_json::json!({"cwd": "/workspace/other", "timestamp": created});
        write_session(
            &temp.path().join("projects/-workspace-other"),
            id,
            &[&record.to_string()],
        );
    }
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_owned(),
            temp.path().display().to_string(),
        ),
    ]);
    let first = demand(&[], &[], Some("2025-01-02T00:00:00Z"));
    let mut second = first.clone();
    second.workspace = PathBuf::from("/workspace/other");
    assert!(discover_panes(std::slice::from_ref(&first), &env).is_empty());
    let before = testkit::read_counts();
    let found = discover_panes(&[first.clone(), second.clone()], &env);
    assert_eq!(ids(&found), [RECENT]);
    assert_eq!(found[0].workspace, second.workspace);
    assert_eq!(
        testkit::read_counts().2 - before.2,
        6,
        "only B's heads are read"
    );
    let before = testkit::read_counts();
    assert!(discover_panes(std::slice::from_ref(&first), &env).is_empty());
    assert_eq!(testkit::read_counts().2, before.2);
    assert_eq!(ids(&discover_panes(&[first, second], &env)), [RECENT]);
    assert_eq!(testkit::read_counts().2, before.2);
}

#[test]
fn pane_window_rebinds_cold_and_sees_new_and_completed_files_on_next_pass() {
    let temp = tempfile::tempdir().unwrap();
    let key = key(&[temp.path().to_path_buf()]);
    let demand = demand(&[], &[], Some("2025-01-02T00:00:00Z"));
    session(
        temp.path(),
        OWNER,
        "2025-01-01T00:00:00Z",
        "2025-01-03T00:00:00Z",
    );
    session(
        temp.path(),
        RECENT,
        "2025-01-02T00:00:00Z",
        "2025-01-02T00:00:00Z",
    );
    let now = Instant::now();
    let mut cache = ClaudePaneDiscovery::default();
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&demand), now)),
        [RECENT]
    );
    assert_eq!(
        ids(&ClaudePaneDiscovery::default().refresh(
            key.clone(),
            std::slice::from_ref(&demand),
            now
        )),
        [RECENT]
    );
    let pending = temp
        .path()
        .join(format!("projects/-workspace-project/{NEXT}.jsonl"));
    fs::write(&pending, "").unwrap();
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&demand), now)),
        [RECENT]
    );
    session(
        temp.path(),
        NEXT,
        "2025-01-03T00:00:00Z",
        "2025-01-03T00:00:00Z",
    );
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&demand), now)),
        [RECENT, NEXT]
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&pending)
        .unwrap()
        .write_all(b"\n{\"cwd\":\"/workspace/project\",\"timestamp\":\"2025-01-04T00:00:00Z\"}\n")
        .unwrap();
    let found = cache.refresh(key.clone(), std::slice::from_ref(&demand), now);
    assert_eq!(
        found
            .iter()
            .find(|row| row.session_id == NEXT)
            .unwrap()
            .last_activity,
        "2025-01-04T00:00:00Z".parse::<Timestamp>().unwrap()
    );
    // A head arriving in a file last written before the window waits for the
    // backstop's listing.
    let late = "44444444-4444-4444-8444-444444444444";
    let stale = pending.with_file_name(format!("{late}.jsonl"));
    abandoned(&stale);
    cache.refresh(key.clone(), std::slice::from_ref(&demand), now);
    fs::OpenOptions::new()
        .append(true)
        .open(&stale)
        .unwrap()
        .write_all(b"{\"cwd\":\"/workspace/project\",\"timestamp\":\"2025-01-05T00:00:00Z\"}\n")
        .unwrap();
    let found = cache.refresh(key, &[demand], now + Duration::from_secs(30));
    assert!(ids(&found).contains(&late.to_owned()));
}

#[test]
fn pane_named_sessions_bypass_history_cap_and_deduplicate_by_newest_activity() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    for index in 0..=MAX_DISCOVERED_SESSIONS {
        session(
            &first,
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            "2025-01-02T00:00:00Z",
            "2025-01-02T00:00:00Z",
        );
    }
    session(
        &first,
        OWNER,
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
    );
    let newest = session(
        &second,
        OWNER,
        "2024-01-01T00:00:00Z",
        "2024-01-02T00:00:00Z",
    );
    let demand = demand(&[OWNER], &[OWNER], Some("2025-01-01T00:00:00Z"));
    let found =
        ClaudePaneDiscovery::default().refresh(key(&[first, second]), &[demand], Instant::now());
    assert_eq!(ids(&found), [OWNER]);
    assert_eq!(found[0].transcript_path, newest);
}

#[test]
fn pane_window_keeps_newest_creation_cap_and_only_top_level_roots() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..=MAX_DISCOVERED_SESSIONS {
        let at = Timestamp::from_second(1_735_689_600 + index as i64)
            .unwrap()
            .to_string();
        session(
            temp.path(),
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            &at,
            &at,
        );
    }
    let nested = temp.path().join("projects/-workspace-project/child");
    write_session(
        &nested,
        NEXT,
        &[r#"{"cwd":"/workspace/project","timestamp":"2026-01-01T00:00:00Z"}"#],
    );
    let demand = demand(&[], &[], Some("2025-01-01T00:00:00Z"));
    let found = ClaudePaneDiscovery::default().refresh(
        key(&[temp.path().to_path_buf()]),
        &[demand],
        Instant::now(),
    );
    assert_eq!(found.len(), MAX_DISCOVERED_SESSIONS);
    assert!(found.iter().all(
        |row| row.session_id != "00000000-aaaa-4aaa-8aaa-000000000000" && row.session_id != NEXT
    ));
}

#[test]
fn pane_account_isolation_ignores_foreign_hints_and_wrong_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    let named = temp.path().join("named");
    let foreign = session(
        &temp.path().join(".claude"),
        OWNER,
        "2025-01-01T00:00:00Z",
        "2025-01-01T00:00:00Z",
    );
    let local = session(
        &named,
        RECENT,
        "2025-01-01T00:00:00Z",
        "2025-01-01T00:00:00Z",
    );
    let mut demand = demand(&[OWNER, RECENT], &[], None);
    demand.panes[0].own[0].transcript_path = Some(foreign);
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        ("CLAUDE_CONFIG_DIR".to_owned(), named.display().to_string()),
    ]);
    let found = super::super::super::ClaudeAdapter
        .discover_pane_sessions(std::slice::from_ref(&demand), &env);
    assert!(found.is_some(), "Claude must implement per-pane discovery");
    let found = found.unwrap();
    assert_eq!(ids(&found), [RECENT]);
    assert_eq!(found[0].transcript_path, local);
    demand.workspace = PathBuf::from("/other");
    assert!(
        super::super::super::ClaudeAdapter
            .discover_pane_sessions(&[demand], &env)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pane_named_sessions_do_not_consume_the_window_cap() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..MAX_DISCOVERED_SESSIONS {
        session(
            temp.path(),
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
        );
    }
    session(
        temp.path(),
        OWNER,
        "2025-01-02T00:00:00Z",
        "2025-01-02T00:00:00Z",
    );
    let demand = demand(&[], &[OWNER], Some("2025-01-01T00:00:00Z"));
    let found = ClaudePaneDiscovery::default().refresh(
        key(&[temp.path().to_path_buf()]),
        &[demand],
        Instant::now(),
    );
    assert_eq!(found.len(), MAX_DISCOVERED_SESSIONS + 1);
}

#[test]
fn pane_shared_bucket_rejections_do_not_add_warm_metadata() {
    let mut actual = Vec::new();
    for history in [50, 500] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("projects/-workspace-project");
        for index in 0..history {
            write_session(
                &project,
                &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
                &[r#"{"cwd":"/another/workspace","timestamp":"2024-01-01T00:00:00Z"}"#],
            );
        }
        session(
            temp.path(),
            RECENT,
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
        );
        let demand = demand(&[], &[], Some("2025-01-01T00:00:00Z"));
        let key = key(&[temp.path().to_path_buf()]);
        let mut cache = ClaudePaneDiscovery::default();
        let now = Instant::now();
        cache.refresh(key.clone(), std::slice::from_ref(&demand), now);
        let before = testkit::read_counts();
        let found = cache.refresh(key, &[demand], now);
        let after = testkit::read_counts();
        actual.push((
            ids(&found),
            (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        ));
    }
    assert_eq!(actual, vec![(vec![RECENT.to_owned()], (2, 0, 0)); 2]);
}

#[test]
fn pane_missing_named_home_never_falls_back_to_the_native_account() {
    let temp = tempfile::tempdir().unwrap();
    session(
        &temp.path().join(".claude"),
        OWNER,
        "2025-01-01T00:00:00Z",
        "2025-01-01T00:00:00Z",
    );
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_owned(),
            temp.path().join("missing-account").display().to_string(),
        ),
    ]);
    let demand = demand(&[OWNER], &[], None);
    let found = super::super::super::ClaudeAdapter
        .discover_pane_sessions(&[demand], &env)
        .unwrap();
    assert!(
        found.is_empty(),
        "an unavailable account must not read the native account"
    );
}
