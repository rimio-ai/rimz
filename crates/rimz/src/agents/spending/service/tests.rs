use super::*;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::sync::{Arc, Barrier};

#[test]
fn request_rejects_relative_paths_and_normalizes_absolute_roots() {
    let runtime = RuntimePaths::under(
        WorkspaceId::from_project_root(Path::new("/tmp/project")),
        Path::new("/tmp/rimz-spending-service-test"),
    )
    .unwrap();
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/project"));
    let invalid = SpendingServiceRequest::workspace(
        &runtime,
        workspace_id.clone(),
        Some(PathBuf::from("relative")),
        Vec::new(),
        None,
        HashMap::new(),
        HeadlineSpec::default(),
    );
    assert_eq!(
        invalid.validate(&namespace).unwrap_err().code,
        SpendingServiceErrorCode::InvalidPath
    );

    let valid = SpendingServiceRequest::workspace(
        &runtime,
        workspace_id,
        Some(PathBuf::from("/tmp/a/../project")),
        vec![PathBuf::from("/tmp/project/./worktree")],
        None,
        HashMap::new(),
        HeadlineSpec::default(),
    )
    .validate(&namespace)
    .unwrap();
    assert_eq!(valid.project_root, Some(PathBuf::from("/tmp/project")));
    assert_eq!(
        valid.worktree_roots,
        vec![PathBuf::from("/tmp/project/worktree")]
    );

    let mut wrong_protocol = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
    wrong_protocol.protocol_version += 1;
    assert_eq!(
        wrong_protocol.validate(&namespace).unwrap_err().code,
        SpendingServiceErrorCode::VersionMismatch
    );
    let mut wrong_cache = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
    wrong_cache.cache_version += 1;
    assert_eq!(
        wrong_cache.validate(&namespace).unwrap_err().code,
        SpendingServiceErrorCode::VersionMismatch
    );
    let mut wrong_provider = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
    wrong_provider.provider_version += 1;
    assert_eq!(
        wrong_provider.validate(&namespace).unwrap_err().code,
        SpendingServiceErrorCode::VersionMismatch
    );
    let mut wrong_workspace = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
    wrong_workspace.workspace_version += 1;
    assert_eq!(
        wrong_workspace.validate(&namespace).unwrap_err().code,
        SpendingServiceErrorCode::VersionMismatch
    );
    let other_namespace = SpendingServiceNamespace::from_declarations(
        Path::new("/tmp/other-state/rimz/shared"),
        Vec::new(),
    );
    assert_eq!(
        SpendingServiceRequest::global(&runtime, HeadlineSpec::default())
            .validate(&other_namespace)
            .unwrap_err()
            .code,
        SpendingServiceErrorCode::NamespaceMismatch
    );
}

#[test]
fn framed_protocol_round_trips_result() {
    let frame = SpendingServiceFrame::Complete(Box::default());
    let mut bytes = Vec::new();
    write_json_line(&mut bytes, &frame).unwrap();
    let decoded: SpendingServiceFrame =
        read_json_line(&mut BufReader::new(bytes.as_slice())).unwrap();
    assert!(matches!(decoded, SpendingServiceFrame::Complete(_)));
}

#[test]
fn write_json_line_buffers_large_frames() {
    #[derive(Default)]
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let message = "\\\"\n".repeat(WRITE_BUFFER_BYTES);
    let frame = SpendingServiceFrame::Error(SpendingServiceFailure::new(
        SpendingServiceErrorCode::Internal,
        message.clone(),
    ));
    let mut writer = CountingWriter::default();

    write_json_line(&mut writer, &frame).unwrap();

    let mut expected = serde_json::to_vec(&frame).unwrap();
    expected.push(b'\n');
    assert_eq!(writer.bytes, expected);
    assert!(writer.bytes.len() > WRITE_BUFFER_BYTES * 3);
    assert!(
        writer.writes <= writer.bytes.len() / WRITE_BUFFER_BYTES + 2,
        "{} writes for {} encoded bytes",
        writer.writes,
        writer.bytes.len()
    );
    let decoded: SpendingServiceFrame =
        read_json_line(&mut BufReader::new(writer.bytes.as_slice())).unwrap();
    let SpendingServiceFrame::Error(decoded) = decoded else {
        panic!("large error frame changed variants");
    };
    assert_eq!(decoded.code, SpendingServiceErrorCode::Internal);
    assert_eq!(decoded.message, message);
}

#[test]
fn namespace_tracks_sorted_canonical_source_declarations() {
    let copilot_a =
        super::super::SpendingSourceTree::new("/home/a/.copilot/session-state", "*/events.jsonl")
            .map(|tree| super::super::SpendingSource::group(vec![tree]))
            .unwrap()
            .fingerprint();
    let copilot_b =
        super::super::SpendingSourceTree::new("/home/b/.copilot/session-state", "*/events.jsonl")
            .map(|tree| super::super::SpendingSource::group(vec![tree]))
            .unwrap()
            .fingerprint();
    let grok_a =
        super::super::SpendingSourceTree::new("/home/a/.grok/sessions", "**/updates.jsonl")
            .map(|tree| super::super::SpendingSource::group(vec![tree]))
            .unwrap()
            .fingerprint();
    let grok_b =
        super::super::SpendingSourceTree::new("/home/b/.grok/sessions", "**/updates.jsonl")
            .map(|tree| super::super::SpendingSource::group(vec![tree]))
            .unwrap()
            .fingerprint();
    let plugin = super::super::SpendingSourceTree::new("/plugins/history", "**/*.jsonl")
        .map(|tree| super::super::SpendingSource::group(vec![tree]))
        .unwrap()
        .fingerprint();
    let base = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/shared"),
        vec![
            ("copilot@default".to_owned(), copilot_a.clone()),
            ("grok@default".to_owned(), grok_a.clone()),
            ("plugin@default".to_owned(), plugin.clone()),
        ],
    );
    let reordered = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/./shared"),
        vec![
            ("plugin@default".to_owned(), plugin.clone()),
            ("grok@default".to_owned(), grok_a.clone()),
            ("copilot@default".to_owned(), copilot_a.clone()),
        ],
    );
    let other_state = SpendingServiceNamespace::from_declarations(
        Path::new("/state-b/rimz/shared"),
        vec![
            ("copilot@default".to_owned(), copilot_a.clone()),
            ("grok@default".to_owned(), grok_a.clone()),
            ("plugin@default".to_owned(), plugin.clone()),
        ],
    );
    let other_copilot_root = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/shared"),
        vec![
            ("copilot@default".to_owned(), copilot_b),
            ("grok@default".to_owned(), grok_a.clone()),
            ("plugin@default".to_owned(), plugin.clone()),
        ],
    );
    let other_grok_root = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/shared"),
        vec![
            ("copilot@default".to_owned(), copilot_a.clone()),
            ("grok@default".to_owned(), grok_b),
            ("plugin@default".to_owned(), plugin.clone()),
        ],
    );
    let other_plugin = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/shared"),
        vec![
            ("copilot@default".to_owned(), copilot_a),
            ("grok@default".to_owned(), grok_a),
            ("plugin@default".to_owned(), [plugin, vec![1]].concat()),
        ],
    );

    assert_eq!(base, reordered);
    assert_ne!(base, other_state);
    assert_ne!(base, other_copilot_root);
    assert_ne!(base, other_grok_root);
    assert_ne!(base, other_plugin);
    let default = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/shared"),
        vec![("claude@default".to_owned(), vec![1])],
    );
    let named = SpendingServiceNamespace::from_declarations(
        Path::new("/state-a/rimz/shared"),
        vec![("claude@work".to_owned(), vec![1])],
    );
    assert_ne!(default, named);
}

#[test]
fn framed_service_matches_direct_global_and_workspace_aggregation() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let workspace_id = WorkspaceId::from_project_root(&project);
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = super::super::unix_secs_now();
    let tod = now_secs % 86_400;
    std::fs::write(
        &transcript,
        format!(
            r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","cwd":"{}","costUSD":2.5,"requestId":"req-service","message":{{"id":"msg-service","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#,
            super::super::utc_date(now_secs),
            tod / 3_600,
            (tod % 3_600) / 60,
            tod % 60,
            project.display()
        ),
    )
    .unwrap();
    let _discovery = super::super::override_discovered_spending_files_for_test(vec![
        super::super::SpendingFile {
            adapter: crate::agents::definition_by_kind("claude").unwrap(),
            login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
                "claude",
            )),
            path: transcript.clone(),
        },
    ]);
    let request = SpendingServiceRequest::workspace(
        &runtime,
        workspace_id,
        Some(project.clone()),
        Vec::new(),
        None,
        HashMap::from([(transcript, project.clone())]),
        HeadlineSpec::default(),
    );

    let mut direct_walker = SpendingWalker::new();
    let mut ignore_progress = |_| {};
    let direct = super::super::engine::serve_request(
        &mut direct_walker,
        &runtime,
        &request,
        &mut ignore_progress,
    );
    std::fs::remove_file(runtime.shared_provider_spending_path()).unwrap();
    let scope = super::super::SpendScope::from_roots(Some(&project), &[]);
    std::fs::remove_file(runtime.workspace_spending_path(&scope.hash())).unwrap();

    let (client, server) = UnixStream::pair().unwrap();
    let service_walker = Mutex::new(SpendingWalker::new());
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);
    let actual = std::thread::scope(|scope| {
        let client = scope.spawn(|| transact(client, &request));
        serve_connection(server, &runtime, &namespace, &service_walker).unwrap();
        client.join().unwrap().unwrap()
    });

    assert_eq!(actual.provider.spending, direct.provider.spending);
    assert_eq!(actual.provider.days, direct.provider.days);
    assert_eq!(actual.workspace.tally, direct.workspace.tally);
    assert!((actual.workspace.tally.year.usd - 2.5).abs() < 1e-9);
}

#[test]
fn concurrent_clients_elect_one_private_socket_owner() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    super::super::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &super::super::ProviderSpendingCache {
            refreshed_at_ms: crate::utils::time::unix_now_ms(),
            ..Default::default()
        },
    );

    let clients = 6;
    let barrier = Arc::new(Barrier::new(clients));
    let handles = (0..clients)
        .map(|_| {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                request(
                    &runtime,
                    SpendingServiceRequest::global(&runtime, HeadlineSpec::default()),
                    SpendingServiceStartup::HostEligible,
                )
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let namespace = SpendingServiceNamespace::for_runtime(&runtime);
    let socket = runtime.shared_spending_service_socket_path(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
    let owner_lock = runtime.shared_spending_service_owner_lock(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
    );
    assert!(matches!(
        crate::disk::single_flight::coordinate::<()>(&owner_lock, CONNECT_WAIT_STEP, 0, || {
            None
        }),
        crate::disk::single_flight::Coordination::ContentionTimeout
    ));
}

#[test]
fn stale_socket_is_unlinked_only_after_winning_owner_lock() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);
    let socket = runtime.shared_spending_service_socket_path(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
    );
    std::fs::write(&socket, b"stale").unwrap();
    let owner_lock = runtime.shared_spending_service_owner_lock(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
    );
    let held = match crate::disk::single_flight::coordinate::<()>(
        &owner_lock,
        CONNECT_WAIT_STEP,
        0,
        || None,
    ) {
        crate::disk::single_flight::Coordination::Produce(guard) => guard,
        _ => panic!("test owns the lifetime lock"),
    };

    assert!(connect_or_start(&runtime, &namespace, SpendingServiceStartup::HostEligible).is_err());
    assert_eq!(std::fs::read(&socket).unwrap(), b"stale");

    drop(held);
    drop(connect_or_start(&runtime, &namespace, SpendingServiceStartup::HostEligible).unwrap());
    assert!(std::fs::metadata(&socket).unwrap().file_type().is_socket());
}

#[test]
fn busy_walker_returns_immediately_instead_of_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);
    let request = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
    let walker = Mutex::new(SpendingWalker::new());
    let held = walker.lock().unwrap();
    let (client, server) = UnixStream::pair().unwrap();

    let error = std::thread::scope(|scope| {
        let client = scope.spawn(|| transact(client, &request));
        serve_connection(server, &runtime, &namespace, &walker).unwrap();
        client.join().unwrap().unwrap_err()
    });
    drop(held);

    assert!(matches!(
        error,
        SpendingServiceClientError::Service(SpendingServiceFailure {
            code: SpendingServiceErrorCode::Busy,
            ..
        })
    ));
}

#[test]
fn fresh_publication_bypasses_busy_walker() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_shared_dirs().unwrap();
    super::super::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &super::super::ProviderSpendingCache {
            refreshed_at_ms: crate::utils::time::unix_now_ms(),
            ..Default::default()
        },
    );
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);
    let request = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
    let walker = Mutex::new(SpendingWalker::new());
    let held = walker.lock().unwrap();
    let (client, server) = UnixStream::pair().unwrap();

    let caches = std::thread::scope(|scope| {
        let client = scope.spawn(|| transact(client, &request));
        serve_connection(server, &runtime, &namespace, &walker).unwrap();
        client.join().unwrap().unwrap()
    });
    drop(held);

    assert!(caches.provider.is_fresh(crate::utils::time::unix_now_ms()));
}

#[test]
fn service_election_prepares_shared_dirs_without_workspace_tree() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);

    drop(connect_or_start(&runtime, &namespace, SpendingServiceStartup::HostEligible).unwrap());

    assert!(runtime.shared_root.is_dir());
    assert!(!runtime.root.exists());
}

#[test]
fn one_shot_client_does_not_start_a_service_or_create_directories() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let namespace = SpendingServiceNamespace::for_runtime(&runtime);

    assert!(connect_or_start(&runtime, &namespace, SpendingServiceStartup::OneShot).is_err());
    assert!(!runtime.shared_root.exists());
    assert!(!runtime.root.exists());
}
