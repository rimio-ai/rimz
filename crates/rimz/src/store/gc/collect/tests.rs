use super::*;
use crate::disk::paths::RuntimePaths;
use crate::ids::{MuxName, SidebarInstanceId};
use crate::wakeup::heartbeat::SidebarHeartbeat;
use tempfile::tempdir;

fn age_socket(path: &Path) {
    let at = nix::sys::time::TimeSpec::from_duration(
        (SystemTime::now() - Duration::from_secs(7200))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap(),
    );
    nix::sys::stat::utimensat(
        nix::fcntl::AT_FDCWD,
        path,
        &at,
        &at,
        nix::sys::stat::UtimensatFlags::NoFollowSymlink,
    )
    .unwrap();
}

#[test]
fn known_room_keeps_empty_class_roots() {
    let temp = tempdir().unwrap();
    let rt = RuntimePaths::under(WorkspaceId::from_project_root(temp.path()), temp.path()).unwrap();
    rt.ensure_dirs().unwrap();
    fs::write(rt.root.join("workspace.json"), br#"{"layout":2}"#).unwrap();
    drop(std::os::unix::net::UnixDatagram::bind(rt.sock_dir.join("dead.sock")).unwrap());
    fs::write(rt.lock_path("free.lock"), b"").unwrap();
    collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &rt.shared_root,
        Duration::ZERO,
        false,
    )
    .unwrap();
    for class in Class::RUNTIME {
        assert!(class.path_under(&rt.root).is_dir());
    }
    collect_claims(&rt).unwrap();
    assert!(rt.sock_dir.is_dir());
    assert!(rt.locks_dir.is_dir());
    assert!(
        rt.lock_path("free.lock").exists(),
        "runtime sweep leaves state locks alone"
    );
}

#[test]
fn runtime_classes_preserve_lanes_and_probe_unlistened_sockets() {
    let temp = tempdir().unwrap();
    let rt = RuntimePaths::under(WorkspaceId::from_project_root(temp.path()), temp.path()).unwrap();
    rt.ensure_dirs().unwrap();
    let lanes = [
        rt.lane_path("loop-fire.json"),
        rt.lane_path("message-wake.json"),
    ];
    for lane in &lanes {
        fs::write(lane, b"{}").unwrap();
        fs::File::open(lane)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(30 * 86_400))
            .unwrap();
    }
    let dead = rt.sock_dir.join("run.dead.sock");
    drop(std::os::unix::net::UnixDatagram::bind(&dead).unwrap());
    age_socket(&dead);
    let starting = rt.sock_dir.join("starting.sock");
    let socket = nix::sys::socket::socket(
        nix::sys::socket::AddressFamily::Unix,
        nix::sys::socket::SockType::Stream,
        nix::sys::socket::SockFlag::empty(),
        None,
    )
    .unwrap();
    use std::os::fd::AsRawFd;
    nix::sys::socket::bind(
        socket.as_raw_fd(),
        &nix::sys::socket::UnixAddr::new(&starting).unwrap(),
    )
    .unwrap();
    let live = rt.sock_dir.join("run.live.sock");
    let _listener = std::os::unix::net::UnixDatagram::bind(&live).unwrap();
    let mut report = GcReport::default();
    collect_runtime_classes(
        &rt.root,
        Duration::from_secs(3600),
        &mut Sweep::new(false),
        &mut report,
    )
    .unwrap();
    for lane in lanes {
        assert!(lane.exists(), "quiet lanes must not expire");
    }
    assert!(!dead.exists(), "unlistened run sockets are reclaimed");
    assert!(live.exists(), "listening sockets survive regardless of age");
    assert!(
        starting.exists(),
        "a socket between bind and listen survives"
    );
}

#[test]
fn lock_sweep_keeps_held_files_and_previews_unheld_files() {
    let temp = tempdir().unwrap();
    let held_path = temp.path().join("held.lock");
    let free_path = temp.path().join("free.lock");
    let held = crate::disk::lock::WorkspaceLock::acquire(&held_path).unwrap();
    fs::write(&free_path, "old holder").unwrap();
    let mut preview = GcReport::default();
    collect_locks(temp.path(), &mut Sweep::new(true), &mut preview).unwrap();
    assert!(held_path.exists());
    assert!(free_path.exists());
    let mut actual = GcReport::default();
    collect_locks(temp.path(), &mut Sweep::new(false), &mut actual).unwrap();
    assert_eq!(preview.sidecar_files_removed, 0);
    assert_eq!(preview.locks_would_check, 2);
    assert_eq!(actual.sidecar_files_removed, 1);
    assert!(held_path.exists());
    assert!(!free_path.exists());
    assert!(
        crate::disk::lock::WorkspaceLock::try_acquire(&held_path)
            .unwrap()
            .is_none()
    );
    drop(held);
    collect_locks(temp.path(), &mut Sweep::new(false), &mut actual).unwrap();
    assert!(!held_path.exists());
}

#[test]
fn lock_sweep_keeps_subdirs_so_a_queued_waiter_can_reopen() {
    let temp = tempdir().unwrap();
    let pane_dir = temp.path().join("pane-write");
    let lock_path = pane_dir.join("pane.lock");
    let held = crate::disk::lock::WorkspaceLock::acquire(&lock_path).unwrap();
    let mut waiter = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(matches!(
        crate::disk::lock::try_lock_file(&mut waiter, &lock_path),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(held);
    let mut report = GcReport::default();
    collect_locks(temp.path(), &mut Sweep::new(false), &mut report).unwrap();
    assert_eq!(report.sidecar_files_removed, 1);
    assert!(pane_dir.is_dir(), "the sweep keeps the emptied lock subdir");
    crate::disk::lock::try_lock_file(&mut waiter, &lock_path).unwrap();
    assert!(lock_path.exists());
}

#[test]
fn a_file_vanished_before_its_age_check_is_not_a_candidate() {
    let temp = tempdir().unwrap();

    assert!(!is_older_than(&temp.path().join("gone.json"), Duration::ZERO).unwrap());
}

#[test]
fn runtime_gc_reaps_sidecars_and_unblocks_the_workspace_root() {
    // Before the sweep covered them, a leftover per-session sidecar (an
    // activity heartbeat or a statusline context file) kept the workspace
    // root non-empty forever, so the root never reaped.
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
    rt.ensure_dirs().unwrap();

    let stale_read_marks = rt.sidebar_read_marks_path(&SidebarInstanceId::new());
    fs::write(&stale_read_marks, br#"{"marks":{"row-a":1000}}"#).unwrap();
    let stale_activity = rt.agent_activity_dir.join("deadbeefdeadbeef.json");
    fs::write(
        &stale_activity,
        br#"{"kind":"claude","agent_id":"sess-1","at":"1970-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let stale_active_time = rt.active_time_dir.join("active.deadbeef.json");
    fs::write(
        &stale_active_time,
        br#"{"kind":"claude","agent_id":"sess-1","credited_ms":1000,"last_progress":"1970-01-01T00:00:00Z","active":false}"#,
    )
    .unwrap();
    let stale_context = rt.agent_context_dir.join("cafef00dcafef00d.json");
    fs::write(&stale_context, b"{}").unwrap();
    let stale_subagent = rt.subagent_context_dir.join("sub.cafebabecafebabe.json");
    fs::write(&stale_subagent, b"{}").unwrap();
    let stale_telemetry = rt.copilot_otel_path();
    fs::write(&stale_telemetry, b"{}\n").unwrap();
    let stale_idle_compact = rt.live_path("idle-compact").join("deadbeef.json");
    fs::create_dir_all(stale_idle_compact.parent().unwrap()).unwrap();
    fs::write(&stale_idle_compact, b"{}").unwrap();
    let stale_prompt = rt.prompt_dir().join("sys.deadbeef.md");
    fs::create_dir_all(stale_prompt.parent().unwrap()).unwrap();
    fs::write(&stale_prompt, b"prompt").unwrap();
    let old = SystemTime::now() - Duration::from_secs(7200);
    for path in [
        &stale_read_marks,
        &stale_activity,
        &stale_active_time,
        &stale_context,
        &stale_subagent,
        &stale_telemetry,
        &stale_idle_compact,
        &stale_prompt,
    ] {
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }

    let report = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(report.sidecar_files_removed, 8);
    assert!(
        !rt.read_marks_dir.exists(),
        "the emptied read-marks dir is removed"
    );
    assert!(
        !rt.agent_activity_dir.exists(),
        "the emptied activity dir is removed"
    );
    assert!(
        !rt.active_time_dir.exists(),
        "the emptied active-time dir is removed"
    );
    assert!(
        !rt.agent_context_dir.exists(),
        "the emptied context dir is removed"
    );
    assert!(
        !rt.subagent_context_dir.exists(),
        "the emptied subagent-context dir is removed"
    );
    assert!(
        !rt.agent_telemetry_dir.exists(),
        "the emptied telemetry dir is removed"
    );
    assert!(
        !rt.live_path("idle-compact").exists(),
        "the emptied idle-compact dir is removed"
    );
    assert!(
        !rt.prompt_dir().exists(),
        "the emptied prompt dir is removed"
    );
    assert!(
        !rt.agent_activity_dir.parent().unwrap().exists(),
        "with no runtime files left, the workspace root is reaped too"
    );
}

#[test]
fn runtime_gc_expires_old_live_entries_even_with_a_fresh_sibling() {
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
    rt.ensure_dirs().unwrap();
    let context = rt.agent_context_dir.join("ctx.live.json");
    let lock = rt.agent_context_dir.join("ctx.live.lock");
    fs::write(&context, b"{}").unwrap();
    fs::write(&lock, b"").unwrap();
    fs::File::open(&lock)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();

    let report = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(report.sidecar_files_removed, 1);
    assert!(context.exists(), "fresh sidecar is kept");
    assert!(
        !lock.exists(),
        "the live class has no paired-file exception"
    );
}

#[test]
fn runtime_gc_dry_run_reports_apply_without_removing() {
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
    rt.ensure_dirs().unwrap();

    let stale_activity = rt.agent_activity_dir.join("deadbeefdeadbeef.json");
    fs::write(&stale_activity, b"{}").unwrap();
    fs::File::open(&stale_activity)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();

    let preview = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        true,
    )
    .unwrap();

    assert!(stale_activity.exists(), "dry-run keeps stale sidecar");
    assert!(rt.agent_activity_dir.exists(), "dry-run keeps emptied dirs");

    let applied = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(preview, applied);
    assert!(!stale_activity.exists(), "apply removes stale sidecar");
    assert!(
        !rt.agent_activity_dir.parent().unwrap().exists(),
        "apply removes the now-empty workspace root"
    );
}

#[test]
fn runtime_gc_accounts_for_stale_telemetry_and_keeps_fresh_files() {
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
    rt.ensure_dirs().unwrap();

    let stale = rt.copilot_otel_path();
    let fresh = rt.agent_telemetry_dir.join("fresh.jsonl");
    fs::write(&stale, b"stale telemetry\n").unwrap();
    fs::write(&fresh, b"fresh telemetry\n").unwrap();
    fs::File::open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();

    let preview = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        true,
    )
    .unwrap();
    assert_eq!(preview.sidecar_files_removed, 1);
    assert!(preview.bytes_removed >= b"stale telemetry\n".len() as u64);
    assert!(stale.exists(), "dry-run keeps stale telemetry");

    let applied = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();
    assert_eq!(preview, applied);
    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(rt.agent_telemetry_dir.exists());
}

#[test]
fn runtime_gc_does_not_unlink_a_live_room_exporter() {
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
    rt.ensure_dirs().unwrap();

    let telemetry = rt.copilot_otel_path();
    fs::write(&telemetry, b"open exporter\n").unwrap();
    fs::File::open(&telemetry)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();
    let instance_id = SidebarInstanceId::new();
    let heartbeat = SidebarHeartbeat::new(
        workspace_id,
        instance_id.clone(),
        MuxName::Tmux,
        "rimz-live",
        rt.sock_dir.join("sidebar.live.sock"),
        None,
    );
    write_json(&rt.sidebar_heartbeat_path(&instance_id), &heartbeat);

    let report = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(report.sidecar_files_removed, 0);
    assert!(telemetry.exists(), "live exporter inode remains linked");
    assert!(rt.agent_telemetry_dir.exists());
}

#[test]
fn runtime_gc_expires_all_live_entries_and_probes_socket_listeners() {
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
    rt.ensure_dirs().unwrap();

    let stale_socket = rt.sock_dir.join("sidebar.stale.sock");
    drop(std::os::unix::net::UnixDatagram::bind(&stale_socket).unwrap());
    age_socket(&stale_socket);
    let run_socket = rt.sock_dir.join("run.123456789abc.sock");
    let _listener = std::os::unix::net::UnixDatagram::bind(&run_socket).unwrap();

    let stale_sidebar = SidebarHeartbeat::new(
        workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test",
        stale_socket.clone(),
        None,
    );
    let stale_sidebar_path = rt.heartbeat_dir.join("sidebar.stale.json");
    write_json(&stale_sidebar_path, &stale_sidebar);

    let legacy_unknown_path = rt.heartbeat_dir.join("unknown.opus-policy.json");
    fs::write(&legacy_unknown_path, b"{}").unwrap();

    let old = SystemTime::now() - Duration::from_secs(7200);
    for path in [&stale_sidebar_path, &legacy_unknown_path] {
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }

    let report = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(report.runtime_roots_scanned, 1);
    assert_eq!(report.heartbeat_files_removed, 1);
    assert_eq!(report.sidebar_sockets_removed, 1);
    assert!(!stale_sidebar_path.exists());
    assert!(!stale_socket.exists());
    assert!(!legacy_unknown_path.exists());
    assert!(run_socket.exists(), "a listening run socket survives");
}

#[test]
fn runtime_gc_expires_read_marks_at_class_ttl_even_with_a_fresh_owner() {
    let temp = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(temp.path());
    let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
    rt.ensure_dirs().unwrap();

    let instance_id = SidebarInstanceId::new();
    let read_marks = rt.sidebar_read_marks_path(&instance_id);
    fs::write(&read_marks, br#"{"marks":{"row-a":1000}}"#).unwrap();
    fs::File::open(&read_marks)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();

    let socket = rt
        .sock_dir
        .join(format!("sidebar.{}.sock", instance_id.short()));
    let heartbeat = SidebarHeartbeat::new(
        workspace_id,
        instance_id.clone(),
        MuxName::Tmux,
        "rimz-test",
        socket,
        None,
    );
    write_json(&rt.sidebar_heartbeat_path(&instance_id), &heartbeat);

    let report = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(report.sidecar_files_removed, 1);
    assert!(
        !read_marks.exists(),
        "the class TTL applies even with a live owner"
    );
}

#[test]
fn runtime_gc_reaps_stale_probe_markers() {
    let temp = tempdir().unwrap();
    let shared = temp.path().join("rimz").join("shared");
    fs::create_dir_all(&shared).unwrap();
    let nonce = "00000000000000000000000000000000";
    let stale_session = shared.join(format!("{SESSION_PROBE_MARKER_PREFIX}{nonce}"));
    let recent_session = shared.join(format!(
        "{SESSION_PROBE_MARKER_PREFIX}11111111111111111111111111111111"
    ));
    let accounts = shared.join("accounts.json");
    let lock = shared.join("accounts.lock");
    let trace = shared.join("rate_limits_trace.jsonl");
    for path in [&stale_session, &recent_session, &accounts, &lock, &trace] {
        fs::write(path, b"probe").unwrap();
    }
    let old = SystemTime::now() - Duration::from_secs(7200);
    for path in [&stale_session, &accounts, &lock, &trace] {
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }
    let recently_dead = SystemTime::now() - (SESSION_PROBE_MARKER_TTL + Duration::from_secs(1));
    fs::File::open(&recent_session)
        .unwrap()
        .set_modified(recently_dead)
        .unwrap();

    let report = collect_runtime_under(
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/ws"),
        &temp.path().join("rimz/shared"),
        Duration::from_secs(3600),
        false,
    )
    .unwrap();

    assert_eq!(report.probe_markers_removed, 2);
    assert!(!stale_session.exists());
    assert!(!recent_session.exists());
    assert!(accounts.exists());
    assert!(lock.exists());
    assert!(trace.exists());
}

#[test]
fn runtime_gc_walks_directories_without_parsing_workspace_ids() {
    let temp = tempdir().unwrap();
    let workspaces = temp.path().join("rimz/ws");
    let shared = temp.path().join("rimz/shared");
    for name in ["project-abcd", "unfinished"] {
        fs::create_dir_all(workspaces.join(name)).unwrap();
    }
    let report =
        collect_runtime_under(&workspaces, &workspaces, &shared, Duration::ZERO, false).unwrap();
    assert_eq!(report.runtime_roots_scanned, 2);
    assert!(!workspaces.join("project-abcd").exists());
    assert!(!workspaces.join("unfinished").exists());
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn runtime_sweep_preserves_legacy_rooms() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let runtime = temp.path().join("runtime");
    fs::create_dir_all(state.join("old-abcd")).unwrap();
    fs::create_dir_all(runtime.join("old-abcd")).unwrap();
    fs::write(state.join("old-abcd/workspace.json"), b"{}").unwrap();
    let report = collect_runtime_under(
        &state,
        &runtime,
        &temp.path().join("shared"),
        Duration::ZERO,
        false,
    )
    .unwrap();
    assert_eq!(report.runtime_roots_scanned, 0);
    assert!(runtime.join("old-abcd").exists());
}
