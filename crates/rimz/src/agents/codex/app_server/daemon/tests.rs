use super::*;

#[test]
fn standalone_bin_resolves_only_when_install_exists() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(standalone_bin_under(home.path()).is_none());

    let bin = home
        .path()
        .join("packages")
        .join("standalone")
        .join("current")
        .join("codex");
    std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
    std::fs::write(&bin, b"#!/bin/sh\n").expect("write");
    assert_eq!(standalone_bin_under(home.path()), Some(bin));
}

#[test]
fn missing_standalone_guidance_uses_official_install_command() {
    let issue = Issue::StandaloneMissing.to_string();
    assert!(issue.contains(INSTALL_COMMAND));
    assert!(issue.contains("[remote_control] codex"));
}

#[test]
fn ensure_requires_toggle_and_standalone() {
    assert!(!should_ensure(false, false));
    assert!(!should_ensure(false, true));
    assert!(!should_ensure(true, false));
    assert!(should_ensure(true, true));
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
fn commands_anchor_descendants_to_codex_home() {
    let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
    let home = Path::new("/home/u/.codex");
    for argv in [command(bin, true), command(bin, false)] {
        let spec = command_spec(&argv, home).expect("non-empty Codex command");
        assert_eq!(spec.cwd.as_deref(), Some(home));
        assert_eq!(spec.program, argv[0]);
        assert_eq!(spec.args, argv[1..]);
    }
}

#[cfg(unix)]
fn pid_record(pid: u32) -> PidRecord {
    PidRecord {
        pid,
        process_start_time: "Mon Jul 13 02:03:45 2026".to_owned(),
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
