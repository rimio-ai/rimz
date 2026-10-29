mod pane;

use std::fs::FileTimes;
use std::io::Write as _;
use std::time::{Duration, UNIX_EPOCH};

use super::*;

#[test]
fn exact_locator_finds_old_and_archived_sessions_only_in_named_home() {
    use crate::agents::capabilities::SessionCapability as _;
    use crate::agents::session::LocalSessionLookup;
    let temp = tempfile::tempdir().unwrap();
    let named = temp.path().join("named");
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        ("CODEX_HOME".to_owned(), named.display().to_string()),
    ]);
    let id = AgentSessionId::from("22222222-2222-4222-8222-222222222222");
    write_rollout(
        &date_path(&temp.path().join(".codex"), false, 40),
        &id,
        "/original/cwd",
        "2025-01-01T00:00:00Z",
        1,
    );
    assert!(matches!(
        super::super::CodexAdapter.locate_session(&id, &env),
        LocalSessionLookup::NotFound
    ));
    for (archived, reverted) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut path = write_rollout(
            &date_path(&named, archived, 40),
            &id,
            "/original/cwd",
            "2025-01-01T00:00:00Z",
            1,
        );
        if reverted {
            let suffixed = path.with_file_name(format!(
                "{}_33333333-3333-4333-8333-333333333333.jsonl",
                path.file_stem().unwrap().to_str().unwrap()
            ));
            fs::rename(&path, &suffixed).unwrap();
            path = suffixed;
        }
        for days in 0..20 {
            fs::create_dir_all(date_path(&named, false, days)).unwrap();
        }
        let LocalSessionLookup::Found(found) = super::super::CodexAdapter.locate_session(&id, &env)
        else {
            panic!("must find old and archived sessions without a day budget");
        };
        assert_eq!(found.workspace, Path::new("/original/cwd"));
        assert_eq!(found.session_id, id);
        assert_eq!(found.transcript_path, path);
        fs::remove_file(path).unwrap();
    }
}

fn date_path(root: &Path, archived: bool, days_ago: usize) -> PathBuf {
    let mut date = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    for _ in 0..days_ago {
        date = date.yesterday().unwrap();
    }
    let root = root.join(if archived {
        "archived_sessions"
    } else {
        "sessions"
    });
    if archived {
        root
    } else {
        root.join(format!("{:04}", date.year()))
            .join(format!("{:02}", date.month()))
            .join(format!("{:02}", date.day()))
    }
}

fn write_rollout(dir: &Path, id: &str, cwd: &str, created_at: &str, modified_secs: u64) -> PathBuf {
    write_rollout_with_extra(dir, id, cwd, created_at, modified_secs, "")
}

fn write_rollout_with_extra(
    dir: &Path,
    id: &str,
    cwd: &str,
    created_at: &str,
    modified_secs: u64,
    extra: &str,
) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let today = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let path = dir.join(format!(
        "rollout-{:04}-{:02}-{:02}T00-00-00-{id}.jsonl",
        today.year(),
        today.month(),
        today.day()
    ));
    fs::write(
        &path,
        format!(
            r#"{{"timestamp":"{created_at}","type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}"{extra}}}}}"#
        ),
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(modified_secs)))
        .unwrap();
    path
}

#[test]
fn named_login_discovers_only_its_own_sessions() {
    use crate::agents::capabilities::SessionCapability as _;

    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture_observation();
    let named_home = temp.path().join("named");
    let home_env = std::collections::BTreeMap::from([(
        "HOME".to_owned(),
        temp.path().to_string_lossy().into_owned(),
    )]);
    let work = crate::agents::ProviderLogin::named(
        fixture.kind.clone(),
        "work".parse().unwrap(),
        named_home.clone(),
    )
    .unwrap();
    let named_env = work.env(&home_env);
    for (home, id) in [
        (
            temp.path().join(".codex"),
            "22222222-2222-4222-8222-222222222222",
        ),
        (named_home, fixture.session_id.as_str()),
    ] {
        write_rollout(
            &date_path(&home, false, 0),
            id,
            fixture.workspace.to_str().unwrap(),
            &fixture.created_at.to_string(),
            1_735_689_600,
        );
    }
    let adapter = super::super::CodexAdapter;
    let default = adapter.discover_local_sessions(&[&fixture.workspace], &home_env);
    assert_eq!(default.len(), 1);
    assert_ne!(default[0].session_id, fixture.session_id);
    let named = adapter.discover_local_sessions(&[&fixture.workspace], &named_env);
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].session_id, fixture.session_id);

    let definition = crate::agents::find_definition("codex").unwrap();
    let workspaces = [fixture.workspace.as_path()];
    let stamped = work.local_sessions(definition, &workspaces, &home_env);
    assert_eq!(stamped[0].session_id, fixture.session_id);
    assert_eq!(stamped[0].login.as_ref(), Some(work.name()));
    let native = crate::agents::ProviderLogin::default_for(fixture.kind.clone()).local_sessions(
        definition,
        &workspaces,
        &home_env,
    );
    assert_eq!(native[0].session_id, default[0].session_id);
    assert_eq!(native[0].login, None);
}

#[test]
fn discovers_recent_matching_rollouts_and_prefers_active_twins() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let active_today = date_path(temp.path(), false, 0);
    let active_yesterday = date_path(temp.path(), false, 1);
    let archive_today = date_path(temp.path(), true, 0);
    let first = "11111111-1111-4111-8111-111111111111";
    let second = "22222222-2222-4222-8222-222222222222";
    let archived = "33333333-3333-4333-8333-333333333333";
    let active_first = write_rollout(
        &active_today,
        first,
        "/workspace/project",
        "2025-01-03T00:00:00Z",
        1_736_035_500,
    );
    write_rollout(
        &active_yesterday,
        second,
        "/workspace/project",
        "2025-01-02T00:00:00Z",
        1_735_949_000,
    );
    write_rollout(
        &active_today,
        "44444444-4444-4444-8444-444444444444",
        "/other",
        "2025-01-04T00:00:00Z",
        1_736_121_600,
    );
    write_rollout(
        &archive_today,
        archived,
        "/workspace/project",
        "2025-01-01T00:00:00Z",
        1_735_689_700,
    );
    write_rollout(
        &archive_today,
        first,
        "/workspace/project",
        "2025-01-03T00:00:00Z",
        1_736_200_000,
    );

    let observations = discover_under(temp.path(), &[workspace]);

    assert_eq!(
        observations
            .iter()
            .map(|observation| observation.session_id.as_str())
            .collect::<Vec<_>>(),
        [first, second, archived]
    );
    assert_eq!(observations[0].transcript_path, active_first);
    assert!(
        observations
            .iter()
            .all(|observation| observation.projection == LocalSessionProjection::IdentityOnly)
    );
}

#[test]
fn discovers_requested_workspaces_in_one_rollout_batch() {
    let temp = tempfile::tempdir().unwrap();
    let active = date_path(temp.path(), false, 0);
    let first = "11111111-1111-4111-8111-111111111111";
    let second = "22222222-2222-4222-8222-222222222222";
    write_rollout(
        &active,
        first,
        "/workspace/first/.",
        "2025-01-01T00:00:00Z",
        1_735_689_600,
    );
    write_rollout(
        &active,
        second,
        "/workspace/second",
        "2025-01-02T00:00:00Z",
        1_735_776_000,
    );
    write_rollout(
        &active,
        "33333333-3333-4333-8333-333333333333",
        "/workspace/unrequested",
        "2025-01-03T00:00:00Z",
        1_735_862_400,
    );

    let observations = discover_under(
        temp.path(),
        &[
            Path::new("/workspace/first"),
            Path::new("/workspace/second"),
        ],
    );

    assert_eq!(
        observations
            .iter()
            .map(|observation| (
                observation.session_id.as_str(),
                observation.workspace.as_path()
            ))
            .collect::<Vec<_>>(),
        [
            (second, Path::new("/workspace/second")),
            (first, Path::new("/workspace/first")),
        ]
    );
    assert_eq!(
        observations[1].workspace.as_os_str().as_encoded_bytes(),
        b"/workspace/first"
    );
}

#[test]
fn cached_discovery_reuses_headers_and_forces_the_backstop() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = PathBuf::from("/workspace/project");
    let today = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let active = date_path(temp.path(), false, 0);
    let id = "11111111-1111-4111-8111-111111111111";
    let path = write_rollout(
        &active,
        id,
        "/workspace/project",
        "2025-01-01T00:00:00Z",
        1_735_689_600,
    );
    let key = DiscoveryKey {
        home: temp.path().to_path_buf(),
        workspaces: vec![workspace],
        today,
    };
    let start = Instant::now();
    let mut snapshot = CodexDiscoverySnapshot::default();

    assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
    assert_eq!(snapshot.work.full_scans, 1);
    assert_eq!(snapshot.work.header_reads, 1);
    assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
    assert_eq!(snapshot.work.header_reads, 1);

    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"\n{\"type\":\"event_msg\"}\n")
        .unwrap();
    assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
    assert_eq!(snapshot.work.full_scans, 1);
    assert_eq!(snapshot.work.header_reads, 2);

    assert_eq!(
        snapshot.refresh(key, start + Duration::from_secs(30)).len(),
        1
    );
    assert_eq!(snapshot.work.full_scans, 2);
    assert_eq!(snapshot.work.header_reads, 3);
}

#[test]
fn examines_at_most_the_bounded_number_of_rollouts() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let active = date_path(temp.path(), false, 0);
    for index in 0..=MAX_EXAMINED_FILES {
        let id = format!("{index:08x}-1111-4111-8111-{index:012x}");
        write_rollout(
            &active,
            &id,
            "/workspace/project",
            "2025-01-01T00:00:00Z",
            1_735_689_600 + index as u64,
        );
    }

    assert_eq!(
        discover_under(temp.path(), &[workspace]).len(),
        MAX_EXAMINED_FILES
    );
}

#[test]
fn excludes_internal_threads_but_keeps_roots_and_user_forks() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let active = date_path(temp.path(), false, 0);
    let root = "11111111-1111-4111-8111-111111111111";
    let fork = "22222222-2222-4222-8222-222222222222";
    let fixtures = [
        (root, r#","source":"cli","thread_source":"user""#.to_owned()),
        (fork, format!(r#","forked_from_id":"{root}""#)),
        (
            "33333333-3333-4333-8333-333333333333",
            r#","source":{"subagent":{"other":"guardian"}},"thread_source":"guardian_review""#
                .to_owned(),
        ),
        (
            "44444444-4444-4444-8444-444444444444",
            r#","source":{"subagent":"review"},"thread_source":"user""#.to_owned(),
        ),
        (
            "55555555-5555-4555-8555-555555555555",
            r#","source":{"internal":"memory_consolidation"},"thread_source":"user""#.to_owned(),
        ),
        (
            "66666666-6666-4666-8666-666666666666",
            format!(r#","source":{{"future":{{"x":1}}}},"parent_thread_id":"{root}""#),
        ),
    ];
    let now = Timestamp::now();
    for (index, (id, extra)) in fixtures.into_iter().enumerate() {
        write_rollout_with_extra(
            &active,
            id,
            "/workspace/project",
            &now.to_string(),
            now.as_second() as u64 + index as u64,
            &extra,
        );
    }

    let ids = discover_under(temp.path(), &[workspace])
        .into_iter()
        .map(|observation| observation.session_id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, [fork, root]);
}

#[test]
fn excludes_v2_and_structured_subagents_but_keeps_user_forks() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let active = date_path(temp.path(), false, 0);
    let archived = date_path(temp.path(), true, 0);
    let root = "11111111-1111-4111-8111-111111111111";
    let fork = "22222222-2222-4222-8222-222222222222";
    let v2_child = "33333333-3333-4333-8333-333333333333";
    let structured_child = "44444444-4444-4444-8444-444444444444";
    write_rollout(
        &active,
        root,
        "/workspace/project",
        "2026-01-01T00:00:00Z",
        100,
    );
    write_rollout_with_extra(
        &active,
        fork,
        "/workspace/project",
        "2026-01-01T00:00:01Z",
        200,
        &format!(r#","forked_from_id":"{root}","thread_source":"user""#),
    );
    write_rollout_with_extra(
        &active,
        v2_child,
        "/workspace/project",
        "2026-01-01T00:00:02Z",
        300,
        &format!(r#","thread_source":"subagent","parent_thread_id":"{root}""#),
    );
    write_rollout_with_extra(
        &archived,
        structured_child,
        "/workspace/project",
        "2026-01-01T00:00:03Z",
        400,
        &format!(
            r#","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{root}","depth":1}}}}}}"#
        ),
    );

    let ids = discover_under(temp.path(), &[workspace])
        .into_iter()
        .map(|observation| observation.session_id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, [fork, root]);
}
