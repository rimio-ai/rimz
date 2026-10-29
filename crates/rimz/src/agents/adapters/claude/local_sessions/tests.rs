mod pane;

use std::io::Write as _;
use std::time::Duration;

use super::*;

fn write_session(dir: &Path, id: &str, records: &[&str]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{id}.jsonl")), records.join("\n")).unwrap();
}

#[test]
fn exact_locator_stays_in_named_home_and_reads_cwd() {
    use crate::agents::capabilities::SessionCapability as _;
    use crate::agents::session::LocalSessionLookup;
    let temp = tempfile::tempdir().unwrap();
    let id = AgentSessionId::from("22222222-2222-4222-8222-222222222222");
    let named = temp.path().join("named");
    let env = std::collections::BTreeMap::from([
        ("HOME".to_owned(), temp.path().display().to_string()),
        ("CLAUDE_CONFIG_DIR".to_owned(), named.display().to_string()),
    ]);
    let record = r#"{"cwd":"/original/cwd","timestamp":"2025-01-01T00:00:00Z"}"#;
    write_session(
        &temp.path().join(".claude/projects/-original-cwd"),
        &id,
        &[record],
    );
    assert!(matches!(
        super::super::ClaudeAdapter.locate_session(&id, &env),
        LocalSessionLookup::NotFound
    ));
    for prefix in [
        String::new(),
        serde_json::json!({"snapshot": "x".repeat(128 * 1024)}).to_string(),
        "{}\n".repeat(40),
    ] {
        write_session(
            &named.join("projects/-original-cwd"),
            &id,
            &[&prefix, record],
        );
        let LocalSessionLookup::Found(found) =
            super::super::ClaudeAdapter.locate_session(&id, &env)
        else {
            panic!("must find the named account's session past leading records");
        };
        assert_eq!(found.workspace, Path::new("/original/cwd"));
        assert_eq!(found.session_id, id);
        assert!(found.transcript_path.starts_with(&named));
    }
    assert!(matches!(
        super::super::ClaudeAdapter.locate_session(&"../../escape".into(), &env),
        LocalSessionLookup::NotFound
    ));
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
    let named_env = crate::agents::ProviderLogin::named(
        fixture.kind.clone(),
        "work".parse().unwrap(),
        named_home.clone(),
    )
    .unwrap()
    .env(&home_env);
    let record = serde_json::json!({
        "timestamp": fixture.created_at.to_string(), "cwd": fixture.workspace,
    })
    .to_string();
    for (home, id) in [
        (
            temp.path().join(".claude"),
            "22222222-2222-4222-8222-222222222222",
        ),
        (named_home, fixture.session_id.as_str()),
    ] {
        write_session(&home.join("projects/-workspace-project"), id, &[&record]);
    }
    let adapter = super::super::ClaudeAdapter;
    let default = adapter.discover_local_sessions(&[&fixture.workspace], &home_env);
    assert_eq!(default.len(), 1);
    assert_ne!(default[0].session_id, fixture.session_id);
    let named = adapter.discover_local_sessions(&[&fixture.workspace], &named_env);
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].session_id, fixture.session_id);
}

#[test]
fn conversation_presence_follows_the_workspace_the_resume_would_open() {
    let temp = tempfile::tempdir().unwrap();
    let config_dirs = vec![temp.path().to_path_buf()];
    let workspace = Path::new("/repo/account-change");
    let id = AgentSessionId::from("06e78f43-ecc1-486b-b50d-3c1f7770a5ae");
    let project = temp.path().join("projects").join("-repo-account-change");

    assert!(!conversation_present_under(&config_dirs, &id, workspace));

    write_session(&project, id.as_str(), &[r#"{"type":"user"}"#]);
    assert!(conversation_present_under(&config_dirs, &id, workspace));

    // The same id under another workspace is not this workspace's session.
    assert!(!conversation_present_under(
        &config_dirs,
        &id,
        Path::new("/repo/other")
    ));

    // An unwritten session carries an id and an empty file.
    let unwritten = AgentSessionId::from("11111111-1111-4111-8111-111111111111");
    write_session(&project, unwritten.as_str(), &[]);
    assert!(!conversation_present_under(
        &config_dirs,
        &unwritten,
        workspace
    ));
}

#[test]
fn conversation_presence_normalizes_the_workspace_path() {
    let temp = tempfile::tempdir().unwrap();
    let config_dirs = vec![temp.path().to_path_buf()];
    let id = AgentSessionId::from("06e78f43-ecc1-486b-b50d-3c1f7770a5ae");
    write_session(
        &temp.path().join("projects").join("-repo-worktrees-lane"),
        id.as_str(),
        &[r#"{"type":"user"}"#],
    );
    assert!(conversation_present_under(
        &config_dirs,
        &id,
        Path::new("/repo/nested/../worktrees/lane")
    ));
}

#[test]
fn discovers_only_root_sessions_for_the_exact_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Path::new("/home/marvin/.agents");
    let project = temp.path().join("projects").join("-home-marvin--agents");
    write_session(
        &project,
        "11111111-1111-4111-8111-111111111111",
        &[
            r#"{"type":"mode","mode":"default"}"#,
            r#"{"type":"permission-mode","mode":"default"}"#,
            r#"{"type":"bridge-session","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            r#"{"timestamp":"2025-01-01T00:00:00Z","cwd":"/home/marvin/.agents"}"#,
            r#"{"timestamp":"2025-01-01T00:05:00Z","cwd":"/home/marvin/.agents"}"#,
        ],
    );
    write_session(
        &project,
        "22222222-2222-4222-8222-222222222222",
        &[r#"{"timestamp":"2025-01-02T00:00:00Z","cwd":"/home/marvin/.agents"}"#],
    );
    write_session(
        &project,
        "33333333-3333-4333-8333-333333333333",
        &[r#"{"timestamp":"2025-01-03T00:00:00Z","cwd":"/other"}"#],
    );
    write_session(
        &project,
        "44444444-4444-4444-8444-444444444444",
        &[
            r#"{"timestamp":"2025-01-04T00:00:00Z","cwd":"/home/marvin/.agents","isSidechain":true}"#,
        ],
    );
    write_session(&project, "55555555-5555-4555-8555-555555555555", &[]);
    write_session(
        &project,
        "agent-not-a-root-session",
        &[r#"{"timestamp":"2025-01-05T00:00:00Z","cwd":"/home/marvin/.agents"}"#],
    );

    let observations = discover_under(temp.path(), workspace);

    assert_eq!(
        observations
            .iter()
            .map(|observation| observation.session_id.as_str())
            .collect::<Vec<_>>(),
        [
            "22222222-2222-4222-8222-222222222222",
            "11111111-1111-4111-8111-111111111111"
        ]
    );
    assert_eq!(
        observations[1].last_activity,
        "2025-01-01T00:05:00Z".parse::<Timestamp>().unwrap()
    );
    assert_eq!(
        observations[1].created_at,
        "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap()
    );
    assert!(
        observations
            .iter()
            .all(|observation| observation.projection == LocalSessionProjection::IdentityOnly)
    );
}

#[test]
fn cached_discovery_reparses_only_changed_candidates_and_backstops() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = PathBuf::from("/workspace/project");
    let project = temp.path().join("projects").join("-workspace-project");
    let id = "11111111-1111-4111-8111-111111111111";
    write_session(
        &project,
        id,
        &[r#"{"timestamp":"2025-01-01T00:00:00Z","cwd":"/workspace/project"}"#],
    );
    let key = DiscoveryKey {
        config_dirs: vec![temp.path().to_path_buf()],
        workspaces: vec![workspace],
        project_dir_name: None,
    };
    let start = Instant::now();
    let mut snapshot = ClaudeDiscoverySnapshot::default();

    assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
    assert_eq!(snapshot.work.full_scans, 1);
    assert_eq!(snapshot.work.candidate_parses, 1);
    assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
    assert_eq!(snapshot.work.candidate_parses, 1);

    fs::OpenOptions::new()
        .append(true)
        .open(project.join(format!("{id}.jsonl")))
        .unwrap()
        .write_all(b"\n{\"timestamp\":\"2025-01-01T00:01:00Z\",\"cwd\":\"/workspace/project\"}\n")
        .unwrap();
    assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
    assert_eq!(snapshot.work.full_scans, 1);
    assert_eq!(snapshot.work.candidate_parses, 2);

    assert_eq!(
        snapshot.refresh(key, start + Duration::from_secs(30)).len(),
        1
    );
    assert_eq!(snapshot.work.full_scans, 2);
    assert_eq!(snapshot.work.candidate_parses, 3);
}

#[test]
fn batched_discovery_retains_the_cap_for_each_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let config_dir = temp.path().to_path_buf();
    let first = PathBuf::from("/workspace/first");
    let second = PathBuf::from("/workspace/second");
    let key = DiscoveryKey {
        config_dirs: vec![config_dir.clone()],
        workspaces: vec![first.clone(), second.clone()],
        project_dir_name: None,
    };
    let start = Instant::now();
    let mut snapshot = ClaudeDiscoverySnapshot::default();
    let mut fixtures = Vec::new();

    for (workspace, count, timestamp) in [(&first, MAX_DISCOVERED_SESSIONS, 2), (&second, 2, 1)] {
        for index in 0..count {
            let path = temp.path().join(format!("{}-{index}.jsonl", timestamp));
            fs::write(&path, []).unwrap();
            let entry = CatalogEntry {
                config_dir: config_dir.clone(),
                workspace: workspace.clone(),
                path: path.clone(),
            };
            let at = Timestamp::from_second(timestamp).unwrap();
            let observation = LocalSessionObservation {
                login: None,
                kind: AgentKind::new_unchecked("claude"),
                session_id: AgentSessionId::from(format!("{timestamp}-{index}")),
                workspace: workspace.clone(),
                transcript_path: path,
                created_at: at,
                fresh_binding_at: Some(at),
                first_event_at: Some(at),
                last_activity: at,
                projection: LocalSessionProjection::IdentityOnly,
            };
            fixtures.push((entry, observation));
        }
    }
    snapshot.catalog.refresh(key.clone(), start, |_| {
        fixtures.iter().map(|(entry, _)| entry.clone()).collect()
    });
    for (entry, observation) in fixtures {
        snapshot.candidates.refresh(
            entry.clone(),
            StampedPaths::exact([entry.path.clone()]),
            false,
            |_| Some(observation),
        );
    }

    let observations = snapshot.refresh(key, start);
    assert_eq!(observations.len(), MAX_DISCOVERED_SESSIONS + 2);
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation.workspace == first)
            .count(),
        MAX_DISCOVERED_SESSIONS
    );
    assert_eq!(
        observations
            .iter()
            .filter(|observation| observation.workspace == second)
            .count(),
        2
    );
}

#[test]
fn project_directory_override_precedes_the_flattened_fallback() {
    assert_eq!(
        project_directory_names_from(Path::new("/repo/main"), Some("short-name")),
        ["short-name", "-repo-main"]
    );
    assert_eq!(
        project_directory_names_from(Path::new("/repo/main"), Some("")),
        ["-repo-main"]
    );
    assert_eq!(
        project_directory_names_from(Path::new("/repo/main"), Some("-repo-main")),
        ["-repo-main"]
    );
}

#[test]
fn override_bucket_discovery_keeps_transcript_cwd_authoritative() {
    let temp = tempfile::tempdir().unwrap();
    let first = PathBuf::from("/workspace/first");
    let second = PathBuf::from("/workspace/second");
    let shared = temp.path().join("projects/shared");
    write_session(
        &shared,
        "11111111-1111-4111-8111-111111111111",
        &[r#"{"timestamp":"2025-01-01T00:00:00Z","cwd":"/workspace/first"}"#],
    );
    write_session(
        &shared,
        "22222222-2222-4222-8222-222222222222",
        &[r#"{"timestamp":"2025-01-02T00:00:00Z","cwd":"/workspace/second"}"#],
    );
    let key = DiscoveryKey {
        config_dirs: vec![temp.path().to_path_buf()],
        workspaces: vec![first.clone(), second.clone()],
        project_dir_name: Some("shared".to_owned()),
    };

    let observations = ClaudeDiscoverySnapshot::default().refresh(key, Instant::now());
    assert_eq!(observations.len(), 2);
    assert!(observations.iter().any(|observation| {
        observation.session_id == "11111111-1111-4111-8111-111111111111"
            && observation.workspace == first
    }));
    assert!(observations.iter().any(|observation| {
        observation.session_id == "22222222-2222-4222-8222-222222222222"
            && observation.workspace == second
    }));
}

#[test]
fn conversation_presence_checks_override_and_flattened_buckets() {
    let temp = tempfile::tempdir().unwrap();
    let config_dirs = vec![temp.path().to_path_buf()];
    let workspace = Path::new("/repo/main");
    let override_id = AgentSessionId::from("11111111-1111-4111-8111-111111111111");
    let fallback_id = AgentSessionId::from("22222222-2222-4222-8222-222222222222");
    write_session(
        &temp.path().join("projects/short-name"),
        override_id.as_str(),
        &[r#"{"type":"user"}"#],
    );
    write_session(
        &temp.path().join("projects/-repo-main"),
        fallback_id.as_str(),
        &[r#"{"type":"user"}"#],
    );

    assert!(conversation_present_under_with(
        &config_dirs,
        &override_id,
        workspace,
        Some("short-name")
    ));
    assert!(conversation_present_under_with(
        &config_dirs,
        &fallback_id,
        workspace,
        Some("short-name")
    ));
}
