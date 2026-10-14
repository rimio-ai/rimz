use super::*;

#[test]
fn managed_layouts_resolve_in_priority_order() {
    let home = tempfile::tempdir().expect("tempdir");
    let paths = [
        "packages/app-server-daemon/current/bin/codex",
        "packages/app-server-daemon/current/codex",
        "packages/standalone/current/bin/codex",
        "packages/standalone/current/codex",
    ];
    for path in paths.into_iter().rev() {
        let bin = home.path().join(path);
        std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
        std::fs::write(&bin, b"").expect("binary");
        assert_eq!(managed_bin_under(home.path()), Some(bin), "{path}");
    }
}

#[test]
fn dedicated_only_install_is_ready() {
    let home = tempfile::tempdir().expect("tempdir");
    let bin = home
        .path()
        .join("packages/app-server-daemon/current/bin/codex");
    std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
    std::fs::write(bin, b"").expect("binary");
    let env = BTreeMap::from([("CODEX_HOME".into(), home.path().display().to_string())]);
    assert!(matches!(
        readiness(true, &env),
        crate::agents::runtime_control::RuntimeControlReadiness::Ready { .. }
    ));
}

#[test]
fn updater_only_records_do_not_name_a_writer() {
    for name in ["daemon-updater.pid", "app-server-updater.pid"] {
        let home = tempfile::tempdir().expect("tempdir");
        let state = home.path().join("app-server-daemon");
        std::fs::create_dir_all(&state).expect("mkdir");
        let pid = std::process::id();
        std::fs::write(
            state.join(name),
            serde_json::json!({"pid": pid, "processStartTime": start_time(pid)}).to_string(),
        )
        .expect("record");
        assert!(!app_server_runs_under(home.path()));
    }
}

#[test]
fn dedicated_writer_is_not_clear() {
    let home = tempfile::tempdir().expect("tempdir");
    let pid = std::process::id();
    write_app_record(
        home.path(),
        &serde_json::json!({"pid": pid, "processStartTime": start_time(pid)}).to_string(),
    );
    std::fs::rename(
        home.path().join("app-server-daemon/app-server.pid"),
        home.path().join("app-server-daemon/daemon.pid"),
    )
    .expect("rename");
    assert!(app_server_runs_under(home.path()));
    let env = BTreeMap::from([("CODEX_HOME".into(), home.path().display().to_string())]);
    assert_eq!(writes_history(&env), DaemonSessions::Unknown);
    let _daemon = StandInDaemon::serve(home.path(), serde_json::json!({"data": ["thread"]}));
    assert_eq!(
        writes_history(&env),
        DaemonSessions::Live(NonZeroUsize::new(1).expect("one"))
    );
}

#[test]
fn pending_restore_refuses_without_opening_socket() {
    let home = tempfile::tempdir().expect("tempdir");
    let state = home.path().join("app-server-daemon");
    std::fs::create_dir_all(&state).expect("mkdir");
    std::fs::write(state.join("loaded-threads.json"), b"{}").expect("handoff");
    let daemon = StandInDaemon::serve(home.path(), serde_json::json!({"data": []}));
    assert_eq!(sessions_under(home.path()), DaemonSessions::Unknown);
    write_app_record(
        home.path(),
        r#"{"pid":4294967295,"processStartTime":"dead"}"#,
    );
    assert_eq!(sessions_under(home.path()), DaemonSessions::Unknown);
    assert_eq!(daemon.connections(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn native_pid_identity_takes_precedence_and_accepts_alias() {
    let pid = std::process::id();
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").expect("boot id");
    let ticks = crate::proc::stat_metrics(pid).expect("stat").start_ticks;
    for key in ["processIdentity", "linuxProcessIdentity"] {
        let record: PidRecord = serde_json::from_value(serde_json::json!({"pid": pid, "processStartTime": "garbage", key: {"bootId": boot.trim(), "startTicks": ticks}})).expect("record");
        assert!(pid_record_matches(&record), "{key}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_pid_identity_mismatch_is_final() {
    let pid = std::process::id();
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").expect("boot id");
    let ticks = crate::proc::stat_metrics(pid).expect("stat").start_ticks;
    for identity in [
        serde_json::json!({"bootId": boot.trim(), "startTicks": ticks + 1}),
        serde_json::json!({"bootId": "wrong", "startTicks": ticks}),
    ] {
        let record: PidRecord = serde_json::from_value(serde_json::json!({"pid": pid, "processStartTime": start_time(pid), "processIdentity": identity})).expect("record");
        assert!(!pid_record_matches(&record));
    }
}

#[test]
fn unfamiliar_pid_identity_falls_back_to_text() {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join("record");
    let pid = std::process::id();
    for identity in [
        serde_json::json!({"bootSessionUuid": "mac", "uniqueId": 1}),
        serde_json::json!({"future": true}),
    ] {
        std::fs::write(&path, serde_json::json!({"pid": pid, "processStartTime": start_time(pid), "processIdentity": identity, "executableIdentity": {}}).to_string()).expect("write");
        assert!(pid_record_matches(&read_pid_record(&path).expect("record")));
    }
}

#[test]
fn updater_restore_release_argv_is_recognised() {
    let home = Path::new("/home/u/.codex");
    let mut argv: Vec<OsString> = [
        "/home/u/.codex/packages/standalone/current/codex",
        "app-server",
        "daemon",
        "pid-update-loop",
        "--restore-release",
        "release",
    ]
    .map(OsString::from)
    .into();
    assert!(updater_argv(home, &argv));
    argv.pop();
    assert!(!updater_argv(home, &argv));
    argv[4] = "--other".into();
    assert!(!updater_argv(home, &argv));
    argv.push("release".into());
    assert!(!updater_argv(home, &argv));
}

#[cfg(unix)]
#[test]
fn dedicated_updater_can_recover() {
    let home = Path::new("/home/u/.codex");
    let mut snapshot = stale_snapshot(home);
    snapshot.updater_exe = home.join("packages/app-server-daemon/releases/0.162/bin/codex");
    snapshot.updater_argv[0] = snapshot.updater_exe.clone().into_os_string();
    assert_eq!(
        stale_updater_pid(home, &pid_record(42), &pid_record(41), &snapshot),
        Some(41)
    );
    assert!(!managed_executable(
        home,
        &home.join("packages/app-server-daemon/releases/other")
    ));
    assert!(!managed_executable(home, Path::new("/tmp/codex")));
}

#[test]
fn updater_skew_compares_release_not_launcher() {
    let root = Path::new("/home/u/.codex/packages/app-server-daemon/releases");
    let launcher = root.join("0.162/bin/codex");
    let bundle = root.join("0.162/CodexCLI.app/Contents/MacOS/codex");
    let binary = PathBuf::from("packages/app-server-daemon/current/bin/codex");
    assert_eq!(
        classify_updater_skew(42, bundle.clone(), launcher.clone(), false, binary.clone()),
        None
    );
    assert!(classify_updater_skew(42, bundle.clone(), launcher, true, binary.clone()).is_some());
    assert!(
        classify_updater_skew(42, bundle, root.join("0.163/bin/codex"), false, binary).is_some()
    );
}

#[test]
fn dedicated_updater_guidance_uses_dedicated_binary() {
    let skew = UpdaterSkew {
        updater_pid: 42,
        updater_exe: "/home/u/.codex/packages/app-server-daemon/releases/old/bin/codex".into(),
        managed_exe: "/home/u/.codex/packages/app-server-daemon/releases/new/bin/codex".into(),
        updater_exe_deleted: false,
        managed_bin_relative: "packages/app-server-daemon/current/bin/codex".into(),
    };
    insta::assert_snapshot!(skew.to_string(), @r###"
    Codex remote-control updater version skew:
    updater pid: 42
    updater exe: /home/u/.codex/packages/app-server-daemon/releases/old/bin/codex
    managed exe: /home/u/.codex/packages/app-server-daemon/releases/new/bin/codex
    The next successful hourly updater pass will restart the shared app-server, then replace the updater with the managed binary. This should clear the skew automatically, but it disconnects every daemon-backed Codex session.

    To choose the timing, run one provider-owned bootstrap while no valuable Codex turns are running:
        cd ~; "${CODEX_HOME:-$HOME/.codex}/packages/app-server-daemon/current/bin/codex" app-server daemon bootstrap --remote-control
    This restarts both provider processes and disconnects daemon-backed Codex sessions once; resume them afterwards. A `remote-control stop` / `start` pair restarts only the app-server and does not clear updater skew.
    "###);
}

#[test]
fn standalone_bin_resolves_only_when_install_exists() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(managed_bin_under(home.path()).is_none());

    let bin = home
        .path()
        .join("packages")
        .join("standalone")
        .join("current")
        .join("codex");
    std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
    std::fs::write(&bin, b"#!/bin/sh\n").expect("write");
    assert_eq!(managed_bin_under(home.path()), Some(bin));
}

#[test]
fn missing_standalone_guidance_uses_official_install_command() {
    let issue = managed_missing_guidance();
    assert!(issue.contains("managed Codex install is missing"));
    assert!(issue.contains("$CODEX_HOME/packages/app-server-daemon/current/bin/codex"));
    assert!(issue.contains("packages/standalone"));
    assert!(issue.contains(INSTALL_COMMAND));
    assert!(issue.contains("[remote_control] codex"));
}

#[test]
fn updater_skew_guidance_names_auto_refresh_and_provider_bootstrap() {
    let skew = UpdaterSkew {
        updater_pid: 1_643_110,
        updater_exe: "/home/u/.codex/packages/standalone/releases/0.144.4/bin/codex".into(),
        managed_exe: "/home/u/.codex/packages/standalone/releases/0.144.5/bin/codex".into(),
        updater_exe_deleted: false,
        managed_bin_relative: "packages/standalone/current/codex".into(),
    };

    insta::assert_snapshot!(skew.to_string(), @r###"
    Codex remote-control updater version skew:
    updater pid: 1643110
    updater exe: /home/u/.codex/packages/standalone/releases/0.144.4/bin/codex
    managed exe: /home/u/.codex/packages/standalone/releases/0.144.5/bin/codex
    The next successful hourly updater pass will restart the shared app-server, then replace the updater with the managed binary. This should clear the skew automatically, but it disconnects every daemon-backed Codex session.

    To choose the timing, run one provider-owned bootstrap while no valuable Codex turns are running:
        cd ~; "${CODEX_HOME:-$HOME/.codex}/packages/standalone/current/codex" app-server daemon bootstrap --remote-control
    This restarts both provider processes and disconnects daemon-backed Codex sessions once; resume them afterwards. A `remote-control stop` / `start` pair restarts only the app-server and does not clear updater skew.
    "###);
}

#[test]
fn updater_skew_recycle_expands_the_managed_home_without_path_lookup() {
    assert_eq!(
        recycle_command(Path::new("packages/standalone/current/codex")),
        "cd ~; \"${CODEX_HOME:-$HOME/.codex}/packages/standalone/current/codex\" app-server daemon bootstrap --remote-control"
    );
}

#[test]
fn ensure_requires_toggle_and_standalone() {
    assert!(!should_ensure(false, false));
    assert!(!should_ensure(false, true));
    assert!(!should_ensure(true, false));
    assert!(should_ensure(true, true));
}

#[test]
fn updater_skew_requires_different_or_deleted_executable_identities() {
    let temp = tempfile::tempdir().expect("tempdir");
    let managed = temp.path().join("managed-codex");
    let updater = temp.path().join("old-codex");
    std::fs::write(&managed, b"managed").expect("write managed executable");
    std::fs::write(&updater, b"old").expect("write updater executable");
    let managed = std::fs::canonicalize(managed).expect("canonical managed executable");
    let binary = PathBuf::from("packages/standalone/current/codex");

    assert_eq!(
        classify_updater_skew(42, managed.clone(), managed.clone(), false, binary.clone()),
        None
    );
    assert_eq!(
        classify_updater_skew(42, updater.clone(), managed.clone(), false, binary.clone()),
        Some(UpdaterSkew {
            updater_pid: 42,
            updater_exe: updater,
            managed_exe: managed.clone(),
            updater_exe_deleted: false,
            managed_bin_relative: binary.clone(),
        })
    );
    assert_eq!(
        classify_updater_skew(42, managed.clone(), managed.clone(), true, binary.clone()),
        Some(UpdaterSkew {
            updater_pid: 42,
            updater_exe: managed.clone(),
            managed_exe: managed,
            updater_exe_deleted: true,
            managed_bin_relative: binary,
        })
    );
}

#[test]
fn updater_skew_requires_control_socket_and_valid_pid_record() {
    let home = tempfile::tempdir().expect("tempdir");
    assert_eq!(updater_skew_under(home.path()), None);

    let control = home.path().join("app-server-control");
    std::fs::create_dir_all(&control).expect("control directory");
    std::fs::write(control.join("app-server-control.sock"), b"").expect("socket marker");
    assert_eq!(updater_skew_under(home.path()), None);

    let daemon = home.path().join("app-server-daemon");
    std::fs::create_dir_all(&daemon).expect("daemon directory");
    std::fs::write(daemon.join("app-server-updater.pid"), b"not json").expect("invalid pid record");
    assert_eq!(updater_skew_under(home.path()), None);
}

#[test]
fn toggle_uses_symmetric_start_and_stop_commands() {
    let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
    assert_eq!(
        command(bin, true),
        vec![
            bin.display().to_string(),
            "remote-control".to_owned(),
            "start".to_owned(),
        ]
    );
    assert_eq!(
        command(bin, false),
        vec![
            bin.display().to_string(),
            "remote-control".to_owned(),
            "stop".to_owned(),
        ]
    );
    assert_eq!(action(true), "start");
    assert_eq!(action(false), "stop");
}

#[test]
fn failed_commands_retry_recovery_first_and_settle_only_for_start() {
    assert_eq!(
        failed_command_retry(true, true),
        FailedCommandRetry::AfterStaleRecovery
    );
    assert_eq!(
        failed_command_retry(false, true),
        FailedCommandRetry::AfterStaleRecovery
    );
    assert_eq!(
        failed_command_retry(true, false),
        FailedCommandRetry::AfterStartSettle
    );
    assert_eq!(failed_command_retry(false, false), FailedCommandRetry::None);
}

#[test]
fn commands_anchor_descendants_to_codex_home() {
    let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
    let home = Path::new("/home/u/.codex");
    let login_env = BTreeMap::from([
        ("CODEX_HOME".to_owned(), home.to_string_lossy().into_owned()),
        ("CODEX_SQLITE_HOME".to_owned(), "/home/u/base".to_owned()),
        ("HOME".to_owned(), "/home/u".to_owned()),
    ]);
    for argv in [command(bin, true), command(bin, false)] {
        let command = control_command(&argv, home, &login_env).expect("non-empty Codex command");
        assert_eq!(command.get_current_dir(), Some(home));
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            vec![
                (OsStr::new("CODEX_HOME"), Some(home.as_os_str())),
                (
                    OsStr::new("CODEX_SQLITE_HOME"),
                    Some(OsStr::new("/home/u/base"))
                ),
            ]
        );
        assert_eq!(command.get_program(), argv[0].as_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            argv[1..].iter().map(OsStr::new).collect::<Vec<_>>()
        );
    }
}

#[cfg(unix)]
#[test]
fn run_command_reports_exit_status_and_stderr() {
    let home = tempfile::tempdir().expect("tempdir");
    let argv = [
        "sh".to_owned(),
        "-c".to_owned(),
        "echo boom >&2; exit 3".to_owned(),
    ];
    let Err(ControlError::Exit {
        action,
        status,
        stderr,
        ..
    }) = run_command(&argv, true, home.path(), &BTreeMap::new())
    else {
        panic!("nonzero command should report its exit");
    };
    assert_eq!(action, "start");
    assert_eq!(status.code(), Some(3));
    assert_eq!(stderr, "boom");

    assert!(run_command(&["true".to_owned()], false, home.path(), &BTreeMap::new()).is_ok());
}

#[cfg(unix)]
fn pid_record(pid: u32) -> PidRecord {
    PidRecord {
        pid,
        process_start_time: "Mon Jul 13 02:03:45 2026".to_owned(),
        #[cfg(target_os = "linux")]
        process_identity: None,
    }
}

#[cfg(unix)]
fn stale_snapshot(home: &Path) -> ProcessSnapshot {
    let uid = crate::proc::own_uid().expect("unix uid");
    let updater = home.join("packages/standalone/releases/0.144.3/bin/codex");
    ProcessSnapshot {
        app_state: 'Z',
        app_parent: 41,
        app_uid: uid,
        app_identity_matches: true,
        updater_state: 'S',
        updater_uid: uid,
        updater_identity_matches: true,
        updater_exe: updater.clone(),
        updater_argv: [
            updater.into_os_string(),
            OsString::from("app-server"),
            OsString::from("daemon"),
            OsString::from("pid-update-loop"),
        ]
        .into(),
        updater_children: vec![42],
    }
}

#[cfg(unix)]
#[test]
fn recovery_requires_exact_owned_zombie_tree() {
    let home = Path::new("/home/u/.codex");
    let app = pid_record(42);
    let updater = pid_record(41);
    let snapshot = stale_snapshot(home);
    assert_eq!(stale_updater_pid(home, &app, &updater, &snapshot), Some(41));

    let mut live_app = stale_snapshot(home);
    live_app.app_state = 'S';
    assert_eq!(stale_updater_pid(home, &app, &updater, &live_app), None);

    let mut reused_app_pid = stale_snapshot(home);
    reused_app_pid.app_identity_matches = false;
    assert_eq!(
        stale_updater_pid(home, &app, &updater, &reused_app_pid),
        None
    );

    let mut wrong_owner = stale_snapshot(home);
    wrong_owner.updater_uid = wrong_owner.updater_uid.saturating_add(1);
    assert_eq!(stale_updater_pid(home, &app, &updater, &wrong_owner), None);

    let mut reused_updater_pid = stale_snapshot(home);
    reused_updater_pid.updater_identity_matches = false;
    assert_eq!(
        stale_updater_pid(home, &app, &updater, &reused_updater_pid),
        None
    );

    let mut unrelated_parent = stale_snapshot(home);
    unrelated_parent.app_parent = 7;
    assert_eq!(
        stale_updater_pid(home, &app, &updater, &unrelated_parent),
        None
    );

    let mut extra_child = stale_snapshot(home);
    extra_child.updater_children.push(43);
    assert_eq!(stale_updater_pid(home, &app, &updater, &extra_child), None);

    let mut unrelated_executable = stale_snapshot(home);
    unrelated_executable.updater_exe = PathBuf::from("/tmp/codex");
    assert_eq!(
        stale_updater_pid(home, &app, &updater, &unrelated_executable),
        None
    );

    let mut unrelated_argv = stale_snapshot(home);
    unrelated_argv.updater_argv[3] = OsString::from("something-else");
    assert_eq!(
        stale_updater_pid(home, &app, &updater, &unrelated_argv),
        None
    );
}

#[test]
fn pid_record_requires_upstream_identity_fields() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("app-server.pid");
    std::fs::write(
        &path,
        r#"{"pid":42,"processStartTime":"Mon Jul 13 02:08:48 2026"}"#,
    )
    .expect("write valid record");
    let record = read_pid_record(&path).expect("valid record");
    assert_eq!(record.pid, 42);

    std::fs::write(&path, r#"{"pid":42}"#).expect("write incomplete record");
    assert!(read_pid_record(&path).is_none());

    std::fs::write(
        &path,
        r#"{"pid":0,"processStartTime":"Mon Jul 13 02:08:48 2026"}"#,
    )
    .expect("write zero pid record");
    assert!(read_pid_record(&path).is_none());
}

fn write_app_record(home: &Path, body: &str) {
    let state_dir = home.join("app-server-daemon");
    std::fs::create_dir_all(&state_dir).expect("mkdir");
    std::fs::write(state_dir.join("app-server.pid"), body).expect("write record");
}

fn start_time(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(unix)]
#[test]
fn an_app_server_runs_under_a_home_only_while_its_recorded_process_lives() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path();
    assert!(!app_server_runs_under(home), "no record");

    let mut process = Command::new("sleep").arg("60").spawn().expect("spawn");
    let pid = process.id();
    let record = |start: &str| format!(r#"{{"pid":{pid},"processStartTime":"{start}"}}"#);

    write_app_record(home, "{not json");
    assert!(!app_server_runs_under(home), "malformed record");
    write_app_record(home, &record("Thu Jan  1 00:00:00 1970"));
    assert!(!app_server_runs_under(home), "another process's start time");

    let started = start_time(pid);
    write_app_record(home, &record(&started));
    assert!(app_server_runs_under(home), "live process, true start time");
    // The updater's record alone names no writer.
    let other = tempfile::tempdir().expect("tempdir");
    let state_dir = other.path().join("app-server-daemon");
    std::fs::create_dir_all(&state_dir).expect("mkdir");
    std::fs::write(state_dir.join("app-server-updater.pid"), record(&started)).expect("write");
    assert!(!app_server_runs_under(other.path()), "updater record only");

    // Unreaped, the killed process keeps its pid and start time as a zombie.
    process.kill().expect("kill");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app_server_runs_under(home) {
        assert!(Instant::now() < deadline, "zombie");
        std::thread::sleep(Duration::from_millis(20));
    }
    process.wait().expect("wait");
    assert!(!app_server_runs_under(home), "dead pid");
}

/// A stand-in daemon on the control socket under `home`: it completes the
/// handshake and answers `thread/loaded/list` with the current `reply`.
struct StandInDaemon {
    reply: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl StandInDaemon {
    fn serve(home: &Path, reply: serde_json::Value) -> Self {
        use std::sync::atomic::Ordering;
        use tungstenite::Message;

        let socket = control_socket(home);
        std::fs::create_dir_all(socket.parent().expect("parent")).expect("mkdir");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        let daemon = Self {
            reply: std::sync::Arc::new(std::sync::Mutex::new(reply)),
            connections: std::sync::Arc::default(),
        };
        let (reply, connections) = (daemon.reply.clone(), daemon.connections.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                connections.fetch_add(1, Ordering::SeqCst);
                let Ok(mut peer) = tungstenite::accept(stream.expect("accept")) else {
                    continue;
                };
                while let Ok(Message::Text(frame)) = peer.read() {
                    let request: serde_json::Value =
                        serde_json::from_str(&frame).expect("a JSON-RPC frame");
                    let result = match request["method"].as_str() {
                        Some("initialize") => serde_json::json!({"userAgent": "codex/0.154.0"}),
                        Some("thread/loaded/list") => reply.lock().expect("reply").clone(),
                        _ => continue,
                    };
                    let response = serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
                    if peer
                        .send(Message::Text(response.to_string().into()))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        daemon
    }

    fn answer(&self, reply: serde_json::Value) {
        *self.reply.lock().expect("reply") = reply;
    }

    fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(unix)]
#[test]
fn a_live_daemon_answers_with_the_threads_its_own_socket_lists() {
    use serde_json::json;

    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path();
    let mut process = Command::new("sleep").arg("60").spawn().expect("spawn");
    let pid = process.id();
    let record = |start: &str| format!(r#"{{"pid":{pid},"processStartTime":"{start}"}}"#);
    let started = start_time(pid);
    write_app_record(home, &record(&started));
    let live = |count| DaemonSessions::Live(std::num::NonZeroUsize::new(count).expect("nonzero"));
    let env = |extra: &[(&str, &str)]| -> BTreeMap<String, String> {
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .chain([("CODEX_HOME".to_owned(), home.display().to_string())])
            .collect()
    };

    assert_eq!(sessions_under(home), DaemonSessions::Unknown, "no socket");

    let daemon = StandInDaemon::serve(home, json!({"data": []}));
    assert_eq!(sessions_under(home), DaemonSessions::Clear, "empty set");
    assert_eq!(daemon.connections(), 1);
    daemon.answer(json!({"data": ["thread-a"], "nextCursor": "next"}));
    assert_eq!(
        sessions_under(home),
        DaemonSessions::Unknown,
        "endless pages"
    );
    daemon.answer(json!({"data": ["thread-a", "thread-b"]}));
    assert_eq!(sessions_under(home), live(2));
    daemon.answer(json!({}));
    assert_eq!(sessions_under(home), DaemonSessions::Unknown, "wire drift");

    // Another account's daemon, reachable through the socket override, never
    // answers for this home.
    let other = tempfile::tempdir().expect("tempdir");
    let elsewhere = StandInDaemon::serve(other.path(), json!({"data": ["thread-c"]}));
    let override_path = control_socket(other.path()).display().to_string();
    daemon.answer(json!({"data": []}));
    for sock in [override_path.as_str(), ""] {
        let env = env(&[("RIMZ_CODEX_APP_SERVER_SOCK", sock)]);
        assert_eq!(writes_history(&env), DaemonSessions::Clear, "{sock:?}");
    }
    daemon.answer(json!({"data": ["thread-a"]}));
    assert_eq!(
        writes_history(&env(&[("RIMZ_CODEX_APP_SERVER_SOCK", "")])),
        live(1)
    );
    assert_eq!(elsewhere.connections(), 0);

    // Without a confirmed process no socket is opened, whatever it would say.
    let asked = daemon.connections();
    write_app_record(home, &record("Thu Jan  1 00:00:00 1970"));
    assert_eq!(sessions_under(home), DaemonSessions::Clear, "start time");
    std::fs::remove_file(home.join("app-server-daemon/app-server.pid")).expect("remove");
    assert_eq!(sessions_under(home), DaemonSessions::Clear, "no record");
    write_app_record(home, &record(&started));
    process.kill().expect("kill");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app_server_runs_under(home) {
        assert!(Instant::now() < deadline, "zombie");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(sessions_under(home), DaemonSessions::Clear, "zombie");
    process.wait().expect("wait");
    assert_eq!(sessions_under(home), DaemonSessions::Clear, "dead pid");
    assert_eq!(daemon.connections(), asked);
}
