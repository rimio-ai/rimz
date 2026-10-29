use super::*;
use crate::agents::capabilities::SessionCapability as _;
use crate::agents::local_session_cache::testkit;
use crate::agents::session::{NamedSession, PaneSessionDemand, PaneSessionNeed};

const OWNER: &str = "11111111-1111-4111-8111-111111111111";
const RECENT: &str = "22222222-2222-4222-8222-222222222222";
const NEXT: &str = "33333333-3333-4333-8333-333333333333";

fn demand(
    own: &[(&str, Option<PathBuf>)],
    named: &[&str],
    since: Option<Timestamp>,
) -> PaneSessionDemand {
    PaneSessionDemand {
        workspace: PathBuf::from("/workspace/project"),
        panes: vec![PaneSessionNeed {
            own: own
                .iter()
                .map(|(id, path)| NamedSession {
                    id: AgentSessionId::from(*id),
                    transcript_path: path.clone(),
                })
                .collect(),
            named: named
                .iter()
                .map(|id| NamedSession {
                    id: AgentSessionId::from(*id),
                    transcript_path: None,
                })
                .collect(),
            fresh_since: since,
        }],
    }
}

fn key(home: &Path) -> DiscoveryKey {
    DiscoveryKey {
        home: home.to_path_buf(),
        workspaces: vec![PathBuf::from("/workspace/project")],
        today: Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
    }
}

fn at() -> Timestamp {
    Timestamp::now()
}

fn session(home: &Path, id: &str, days: usize, created: Timestamp) -> PathBuf {
    let dir = date_path(home, false, days);
    let path = write_rollout(
        &dir,
        id,
        "/workspace/project",
        &created.to_string(),
        created.as_second() as u64,
    );
    let mut date = key(home).today;
    for _ in 0..days {
        date = date.yesterday().unwrap();
    }
    let dated = dir.join(format!("rollout-{date}T00-00-00-{id}.jsonl"));
    fs::rename(path, &dated).unwrap();
    dated
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
fn pane_named_paths_match_full_scan_across_revisions_and_archive_moves() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let created = at();
    let key = key(home);
    let workspace = key.workspaces[0].clone();
    session(home, OWNER, 2, created);
    let initial = session(home, OWNER, 1, created);
    let revision = initial.with_file_name(format!(
        "rollout-{}T12-00-00-{OWNER}_66666666-6666-4666-8666-666666666666.jsonl",
        key.today.yesterday().unwrap()
    ));
    fs::rename(initial, &revision).unwrap();
    let moving = session(home, RECENT, 0, created);
    let archive = date_path(home, true, 0);
    write_rollout(
        &archive,
        NEXT,
        workspace.to_str().unwrap(),
        &created.to_string(),
        created.as_second() as u64,
    );
    fs::copy(&revision, archive.join(revision.file_name().unwrap())).unwrap();
    let tomorrow = key.today.tomorrow().unwrap();
    let next_dir = home.join("sessions").join(format!(
        "{:04}/{:02}/{:02}",
        tomorrow.year(),
        tomorrow.month(),
        tomorrow.day()
    ));
    let future_id = "44444444-4444-4444-8444-444444444444";
    let future = write_rollout(
        &next_dir,
        future_id,
        workspace.to_str().unwrap(),
        &created.to_string(),
        created.as_second() as u64,
    );
    fs::rename(
        &future,
        future.with_file_name(format!("rollout-{tomorrow}T00-00-00-{future_id}.jsonl")),
    )
    .unwrap();
    let abandoned = session(home, "55555555-5555-4555-8555-555555555555", 0, created);
    fs::write(abandoned, "").unwrap();
    let paths = |found: Vec<LocalSessionObservation>| {
        found
            .into_iter()
            .map(|row| ((row.workspace, row.session_id), row.transcript_path))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let full = discover_under(home, &[&workspace]);
    assert_eq!(ids(&full), [OWNER, RECENT, NEXT, future_id]);
    let names = full
        .iter()
        .map(|row| row.session_id.as_str())
        .collect::<Vec<_>>();
    let request = demand(&[], &names, None);
    let full = paths(full);
    assert_eq!(full[&(workspace.clone(), OWNER.into())], revision);
    let now = Instant::now();
    let mut cache = CodexPaneDiscovery::default();
    assert_eq!(
        paths(cache.refresh(key.clone(), std::slice::from_ref(&request), now)),
        full
    );

    let written = session(home, OWNER, 0, created);
    let revision = written.with_file_name(format!(
        "rollout-{}T12-00-00-{OWNER}_77777777-7777-4777-8777-777777777777.jsonl",
        key.today
    ));
    fs::rename(written, &revision).unwrap();
    let archived = archive.join(moving.file_name().unwrap());
    fs::rename(moving, &archived).unwrap();
    let full = discover_under(home, &[&workspace]);
    assert_eq!(ids(&full), [OWNER, RECENT, NEXT, future_id]);
    let full = paths(full);
    assert_eq!(full[&(workspace.clone(), OWNER.into())], revision);
    assert_eq!(full[&(workspace, RECENT.into())], archived);
    assert_eq!(
        paths(cache.refresh(key, &[request], now + Duration::from_secs(31))),
        full
    );
}

#[test]
fn pane_warm_work_is_independent_of_history_for_settled_and_open_windows() {
    let mut actual = Vec::new();
    let created = at();
    for history in [50, 500] {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..history {
            session(
                temp.path(),
                &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
                0,
                "2024-01-01T00:00:00Z".parse().unwrap(),
            );
        }
        let owner = session(
            temp.path(),
            OWNER,
            40,
            "2024-01-01T00:00:00Z".parse().unwrap(),
        );
        session(temp.path(), RECENT, 0, created);
        for open in [false, true] {
            let mut demand = demand(&[(OWNER, Some(owner.clone()))], &[], Some(created));
            if open {
                demand.panes.push(PaneSessionNeed {
                    own: vec![],
                    named: vec![],
                    fresh_since: Some(created),
                });
            }
            let mut cache = CodexPaneDiscovery::default();
            let key = key(temp.path());
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
        // Yesterday, today, tomorrow, and the flat archive are watched.
        expected.push((
            history,
            true,
            vec![OWNER.to_owned(), RECENT.to_owned()],
            (6, 0, 0),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn pane_window_reopen_keeps_learned_history_until_a_listing_removes_it() {
    let temp = tempfile::tempdir().unwrap();
    let created = at();
    for index in 0..50 {
        let cwd = if index % 2 == 0 {
            "/workspace/project"
        } else {
            "/another/workspace"
        };
        write_rollout(
            &date_path(temp.path(), false, 0),
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            cwd,
            "2024-01-01T00:00:00Z",
            1,
        );
    }
    let owner = session(temp.path(), OWNER, 0, created);
    let key = key(temp.path());
    let now = Instant::now();
    let mut cache = CodexPaneDiscovery::default();
    let open = demand(&[], &[], Some(created));
    assert_eq!(ids(&cache.refresh(key.clone(), &[open], now)), [OWNER]);
    let settled = demand(&[(OWNER, Some(owner.clone()))], &[], Some(created));
    for _ in 0..2 {
        assert_eq!(
            ids(&cache.refresh(key.clone(), std::slice::from_ref(&settled), now)),
            [OWNER]
        );
    }
    let next = created.checked_add(jiff::Span::new().seconds(1)).unwrap();
    session(temp.path(), RECENT, 0, next);
    let reopen = demand(&[], &[], Some(next));
    let before = testkit::read_counts();
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&reopen), now)),
        [RECENT]
    );
    let after = testkit::read_counts();
    assert_eq!(after.2 - before.2, 1, "only the new header is read");
    assert!(
        after.1 - before.1 <= 4,
        "each folder is listed at most once"
    );

    fs::remove_file(owner).unwrap();
    cache.refresh(key.clone(), std::slice::from_ref(&reopen), now);
    session(temp.path(), OWNER, 0, next);
    let before = testkit::read_counts();
    assert_eq!(ids(&cache.refresh(key, &[reopen], now)), [OWNER, RECENT]);
    assert_eq!(testkit::read_counts().2 - before.2, 1);
}

#[test]
fn pane_workspace_changes_keep_the_account_history_warm() {
    let temp = tempfile::tempdir().unwrap();
    let created = at();
    let old = created.checked_sub(jiff::Span::new().hours(1)).unwrap();
    for (id, workspace, at) in [
        (OWNER, "/workspace/project", old),
        (RECENT, "/workspace/other", created),
        (NEXT, "/workspace/other", old),
    ] {
        write_rollout(
            &date_path(temp.path(), false, 0),
            id,
            workspace,
            &at.to_string(),
            created.as_second() as u64,
        );
    }
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        ("CODEX_HOME".to_owned(), temp.path().display().to_string()),
    ]);
    let first = demand(&[], &[], Some(created));
    let mut second = first.clone();
    second.workspace = PathBuf::from("/workspace/other");
    assert!(discover_panes(std::slice::from_ref(&first), &env).is_empty());
    let new_id = "44444444-4444-4444-8444-444444444444";
    write_rollout(
        &date_path(temp.path(), false, 0),
        new_id,
        second.workspace.to_str().unwrap(),
        &created.to_string(),
        created.as_second() as u64,
    );
    let before = testkit::read_counts();
    let found = discover_panes(&[first.clone(), second.clone()], &env);
    assert_eq!(ids(&found), [RECENT, new_id]);
    assert!(found.iter().all(|row| row.workspace == second.workspace));
    assert_eq!(
        testkit::read_counts().2 - before.2,
        1,
        "only the new head is read"
    );
    let before = testkit::read_counts();
    assert!(discover_panes(std::slice::from_ref(&first), &env).is_empty());
    assert_eq!(testkit::read_counts().2, before.2);
    assert_eq!(
        ids(&discover_panes(&[first, second], &env)),
        [RECENT, new_id]
    );
    assert_eq!(testkit::read_counts().2, before.2);
}

#[test]
fn pane_window_rebinds_cold_and_sees_new_and_completed_files_on_next_pass() {
    let temp = tempfile::tempdir().unwrap();
    let created = at();
    session(
        temp.path(),
        OWNER,
        0,
        "2024-01-01T00:00:00Z".parse().unwrap(),
    );
    session(temp.path(), RECENT, 0, created);
    let demand = demand(&[], &[], Some(created));
    let key = key(temp.path());
    let now = Instant::now();
    let mut cache = CodexPaneDiscovery::default();
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&demand), now)),
        [RECENT]
    );
    assert_eq!(
        ids(&CodexPaneDiscovery::default().refresh(
            key.clone(),
            std::slice::from_ref(&demand),
            now
        )),
        [RECENT]
    );
    let pending = session(temp.path(), NEXT, 0, created);
    fs::write(&pending, "").unwrap();
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&demand), now)),
        [RECENT]
    );
    session(temp.path(), NEXT, 0, created);
    assert_eq!(
        ids(&cache.refresh(key.clone(), std::slice::from_ref(&demand), now)),
        [RECENT, NEXT]
    );
    let changed = created.as_second() as u64 + 60;
    fs::File::options()
        .write(true)
        .open(pending)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(changed)))
        .unwrap();
    let found = cache.refresh(key, &[demand], now);
    assert_eq!(
        found
            .iter()
            .find(|row| row.session_id == NEXT)
            .unwrap()
            .last_activity,
        Timestamp::from_second(changed as i64).unwrap()
    );
}

#[test]
fn pane_old_named_rollouts_use_hints_or_the_bounded_resume_resolver() {
    let mut actual = Vec::new();
    for hint in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = session(
            temp.path(),
            OWNER,
            40,
            "2024-01-01T00:00:00Z".parse().unwrap(),
        );
        if hint {
            for days in 0..20 {
                fs::create_dir_all(date_path(temp.path(), false, days)).unwrap();
            }
        }
        let demand = if hint {
            demand(&[(OWNER, Some(path.clone()))], &[], Some(at()))
        } else {
            demand(&[], &[OWNER], None)
        };
        let found =
            CodexPaneDiscovery::default().refresh(key(temp.path()), &[demand], Instant::now());
        actual.push((
            ids(&found),
            found.first().map(|row| row.transcript_path.clone()),
        ));
        assert!(path.exists());
    }
    assert_eq!(
        actual
            .iter()
            .map(|(ids, path)| (ids.clone(), path.is_some()))
            .collect::<Vec<_>>(),
        vec![(vec![OWNER.to_owned()], true); 2]
    );
}

#[test]
fn pane_named_rollout_moves_to_a_reverted_revision_by_hint_or_backstop() {
    for hinted in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let first = session(
            temp.path(),
            OWNER,
            2,
            "2024-01-01T00:00:00Z".parse().unwrap(),
        );
        let mut request = if hinted {
            demand(&[(OWNER, Some(first.clone()))], &[], None)
        } else {
            demand(&[], &[OWNER], None)
        };
        let now = Instant::now();
        let mut cache = CodexPaneDiscovery::default();
        let mut pass = |request: &PaneSessionDemand, after: u64| {
            cache.refresh(
                key(temp.path()),
                std::slice::from_ref(request),
                now + Duration::from_secs(after),
            )[0]
            .transcript_path
            .clone()
        };
        assert_eq!(pass(&request, 0), first);
        let written = write_rollout(
            &date_path(temp.path(), false, 0),
            OWNER,
            "/workspace/project",
            "2024-01-01T00:00:00Z",
            1,
        );
        let revision = written.with_file_name(format!(
            "rollout-{}T12-00-00-{OWNER}_{NEXT}.jsonl",
            key(temp.path()).today
        ));
        fs::rename(written, &revision).unwrap();
        assert_eq!(pass(&request, 29), first, "inside the backstop");
        let before = testkit::read_counts();
        let backstop = pass(&request, 30);
        if hinted {
            assert_eq!(backstop, first, "a hooked row follows its hook's path");
            assert_eq!(testkit::read_counts().1, before.1);
            request.panes[0].own[0].transcript_path = Some(revision.clone());
            assert_eq!(pass(&request, 30), revision);
        } else {
            assert_eq!(backstop, revision, "a remembered path is a cache");
        }
    }
}

#[test]
fn pane_named_rollout_follows_archive_moves_in_both_directions_without_listing() {
    let temp = tempfile::tempdir().unwrap();
    let active = session(
        temp.path(),
        OWNER,
        40,
        "2024-01-01T00:00:00Z".parse().unwrap(),
    );
    let archive = temp
        .path()
        .join("archived_sessions")
        .join(active.file_name().unwrap());
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    let demand = demand(&[(OWNER, Some(active.clone()))], &[], None);
    let now = Instant::now();
    let mut cache = CodexPaneDiscovery::default();
    assert_eq!(
        ids(&cache.refresh(key(temp.path()), std::slice::from_ref(&demand), now)),
        [OWNER]
    );
    for (from, to) in [(&active, &archive), (&archive, &active)] {
        fs::rename(from, to).unwrap();
        let before = testkit::read_counts();
        let found = cache.refresh(key(temp.path()), std::slice::from_ref(&demand), now);
        let after = testkit::read_counts();
        assert_eq!(found[0].transcript_path, *to);
        assert_eq!(after.1 - before.1, 0, "archive twins need no tree walk");
    }
}

#[test]
fn pane_and_full_scan_cover_local_dates_ahead_of_and_behind_utc() {
    let temp = tempfile::tempdir().unwrap();
    let now = at();
    let today = key(temp.path()).today;
    let tomorrow = today.tomorrow().unwrap();
    let next_dir = temp.path().join("sessions").join(format!(
        "{:04}/{:02}/{:02}",
        tomorrow.year(),
        tomorrow.month(),
        tomorrow.day()
    ));
    write_rollout(
        &next_dir,
        RECENT,
        "/workspace/project",
        &now.to_string(),
        now.as_second() as u64,
    );
    let request = demand(&[], &[], Some(now));
    let pane = CodexPaneDiscovery::default().refresh(key(temp.path()), &[request], Instant::now());
    let full = discover_under(temp.path(), &[Path::new("/workspace/project")]);
    assert_eq!(
        (ids(&pane), ids(&full)),
        (vec![RECENT.to_owned()], vec![RECENT.to_owned()])
    );
    let earliest = today.yesterday().unwrap().yesterday().unwrap();
    let early_dir = temp.path().join("sessions").join(format!(
        "{:04}/{:02}/{:02}",
        earliest.year(),
        earliest.month(),
        earliest.day()
    ));
    let since = now.checked_sub(jiff::Span::new().hours(24)).unwrap();
    write_rollout(
        &early_dir,
        OWNER,
        "/workspace/project",
        &since.to_string(),
        now.as_second() as u64,
    );
    let archive = session(temp.path(), NEXT, 0, now);
    let archive_target = temp
        .path()
        .join("archived_sessions")
        .join(archive.file_name().unwrap());
    fs::create_dir_all(archive_target.parent().unwrap()).unwrap();
    fs::rename(archive, archive_target).unwrap();
    let demand = demand(&[], &[], Some(since));
    assert_eq!(
        ids(&CodexPaneDiscovery::default().refresh(key(temp.path()), &[demand], Instant::now())),
        [OWNER, RECENT, NEXT]
    );
}

#[test]
fn pane_unresolved_ids_are_backstopped_and_window_paths_are_remembered() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = demand(&[], &[OWNER, "--last"], None);
    let now = Instant::now();
    let mut cache = CodexPaneDiscovery::default();
    let before = testkit::read_counts();
    assert!(
        cache
            .refresh(key(temp.path()), std::slice::from_ref(&request), now)
            .is_empty()
    );
    let initial = testkit::read_counts();
    assert!(
        initial.1 > before.1,
        "a UUID gets one bounded resolution attempt"
    );
    let invalid = demand(&[], &["--last"], None);
    assert!(
        CodexPaneDiscovery::default()
            .refresh(key(temp.path()), &[invalid], now)
            .is_empty()
    );
    assert_eq!(
        testkit::read_counts(),
        initial,
        "non-UUID ids are never looked up"
    );
    assert!(
        cache
            .refresh(
                key(temp.path()),
                std::slice::from_ref(&request),
                now + Duration::from_secs(29)
            )
            .is_empty()
    );
    assert_eq!(testkit::read_counts(), initial);
    let created = at();
    session(temp.path(), OWNER, 0, created);
    request.panes[0].fresh_since = Some(created);
    assert_eq!(
        ids(&cache.refresh(
            key(temp.path()),
            std::slice::from_ref(&request),
            now + Duration::from_secs(29)
        )),
        [OWNER]
    );
    request.panes[0].fresh_since = None;
    let before = testkit::read_counts();
    assert_eq!(
        ids(&cache.refresh(key(temp.path()), &[request], now + Duration::from_secs(29))),
        [OWNER]
    );
    let after = testkit::read_counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (1, 0, 0)
    );
    let absent = demand(&[], &[NEXT], None);
    cache.refresh(key(temp.path()), std::slice::from_ref(&absent), now);
    let before = testkit::read_counts();
    cache.refresh(key(temp.path()), &[absent], now + Duration::from_secs(30));
    assert!(testkit::read_counts().1 > before.1);
}

#[test]
fn pane_account_isolation_rejects_foreign_mismatched_and_traversing_hints() {
    let temp = tempfile::tempdir().unwrap();
    let named = temp.path().join("named");
    let foreign = session(&temp.path().join(".codex"), OWNER, 0, at());
    let local = session(&named, RECENT, 0, at());
    let mut demand = demand(
        &[(OWNER, Some(foreign)), (RECENT, None)],
        &[NEXT, "--last"],
        None,
    );
    demand.panes[0].named[0].transcript_path = Some(local.clone());
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        ("CODEX_HOME".to_owned(), named.display().to_string()),
    ]);
    let found = super::super::super::CodexAdapter
        .discover_pane_sessions(std::slice::from_ref(&demand), &env);
    assert!(found.is_some(), "Codex must implement per-pane discovery");
    let found = found.unwrap();
    assert_eq!(ids(&found), [RECENT]);
    assert_eq!(found[0].transcript_path, local);
    demand.panes[0].own[0].transcript_path = Some(
        named
            .join("../.codex")
            .join(local.strip_prefix(&named).unwrap()),
    );
    let found = super::super::super::CodexAdapter
        .discover_pane_sessions(&[demand], &env)
        .unwrap();
    assert_eq!(ids(&found), [RECENT]);
}

#[test]
fn pane_window_prefers_active_twins_and_keeps_newest_creation_per_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let created = at();
    let active = session(temp.path(), OWNER, 0, created);
    write_rollout(
        &date_path(temp.path(), true, 0),
        OWNER,
        "/workspace/project",
        &created.to_string(),
        created.as_second() as u64 + 60,
    );
    let request = demand(&[], &[], Some(created));
    let found = CodexPaneDiscovery::default().refresh(key(temp.path()), &[request], Instant::now());
    assert_eq!(ids(&found), [OWNER]);
    assert_eq!(found[0].transcript_path, active);
    let revision = write_rollout(
        &date_path(temp.path(), true, 0),
        OWNER,
        "/workspace/project",
        &created.to_string(),
        created.as_second() as u64,
    );
    let archived = revision.with_file_name(format!(
        "rollout-{}T12-00-00-{OWNER}_{NEXT}.jsonl",
        key(temp.path()).today
    ));
    fs::rename(revision, &archived).unwrap();
    let request = demand(&[], &[], Some(created));
    let found = CodexPaneDiscovery::default().refresh(key(temp.path()), &[request], Instant::now());
    assert_eq!(
        found[0].transcript_path, archived,
        "the newest revision wins"
    );
    for index in 0..=MAX_EXAMINED_FILES {
        let id = format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}");
        session(
            temp.path(),
            &id,
            0,
            created
                .checked_add(jiff::Span::new().seconds(index as i64 + 1))
                .unwrap(),
        );
    }
    let demand = demand(&[], &[], Some(created));
    let found = CodexPaneDiscovery::default().refresh(key(temp.path()), &[demand], Instant::now());
    assert_eq!(found.len(), MAX_EXAMINED_FILES);
    assert!(
        found.iter().all(|row| row.session_id != OWNER
            && row.session_id != "00000000-aaaa-4aaa-8aaa-000000000000")
    );
}

#[test]
fn pane_named_sessions_do_not_consume_the_window_cap() {
    let temp = tempfile::tempdir().unwrap();
    let created = at();
    for index in 0..MAX_EXAMINED_FILES {
        session(
            temp.path(),
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            0,
            created,
        );
    }
    session(
        temp.path(),
        OWNER,
        0,
        created.checked_add(jiff::Span::new().seconds(1)).unwrap(),
    );
    let demand = demand(&[], &[OWNER], Some(created));
    let found = CodexPaneDiscovery::default().refresh(key(temp.path()), &[demand], Instant::now());
    assert_eq!(found.len(), MAX_EXAMINED_FILES + 1);
}
