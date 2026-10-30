use super::*;

#[test]
fn hook_append_keeps_ancestors_private_under_restrictive_umask() {
    if std::env::var_os("RIMZ_TEST_HISTORY_ANCESTORS").is_none() {
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                "umask 077; exec \"$1\" --exact \"$2\" --nocapture",
                "sh",
            ])
            .arg(std::env::current_exe().unwrap())
            .arg(concat!(
                "agents::spending::history::tests::permissions::",
                "hook_append_keeps_ancestors_private_under_restrictive_umask"
            ))
            .env("RIMZ_TEST_HISTORY_ANCESTORS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_dir, runtime, _) = fixture();
    let paths = &runtime.spending_history;
    let accounts = paths.history_root.parent().unwrap();
    assert!(!accounts.exists());
    session_effort::record(&runtime, &effort("2026-06-01T12:00:00Z")).unwrap();
    assert_eq!(
        std::fs::metadata(accounts).unwrap().permissions().mode() & 0o7777,
        0o700,
        "a hook must not make the provider-home ancestor group-writable or setgid"
    );
    for directory in [&paths.history_root, &paths.effort_dir()] {
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o7777,
            DIRECTORY_MODE
        );
    }
}

#[test]
#[expect(clippy::print_stderr, reason = "report an unavailable test group")]
fn append_and_migration_repair_a_racing_directory_group() {
    let Some(group) = nix::unistd::getgroups()
        .unwrap()
        .into_iter()
        .find(|group| *group != nix::unistd::getgid())
    else {
        eprintln!("skipping group assertion: no supplementary group differs from primary");
        return;
    };
    let (_dir, runtime, now) = fixture();
    let paths = &runtime.spending_history;
    let accounts = paths.history_root.parent().unwrap();
    std::fs::create_dir_all(accounts).unwrap();
    if let Err(error) = std::os::unix::fs::chown(accounts, None, Some(group.as_raw())) {
        eprintln!("skipping group assertion: cannot assign supplementary group: {error}");
        return;
    }
    // Reproduce a directory created before the first writer chowns the history root.
    for directory in [paths.roles_dir(), paths.effort_dir()] {
        std::fs::create_dir_all(&directory).unwrap();
    }
    std::os::unix::fs::chown(&paths.history_root, None, Some(group.as_raw())).unwrap();
    session_effort::record(&runtime, &effort("2026-06-01T12:00:00Z")).unwrap();
    let legacy = runtime.legacy_session_roles().join("2026.jsonl");
    write_record(
        &legacy,
        &SessionRoleRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: "migrated".into(),
            at: "2026-06-01T12:00:00Z".parse().unwrap(),
            role: SessionRole::Subagent {
                profile: "shared".into(),
                owner: None,
            },
        },
    );
    migrate(&runtime, now).unwrap();
    for directory in [paths.roles_dir(), paths.effort_dir()] {
        assert_eq!(
            std::fs::metadata(&directory).unwrap().gid(),
            group.as_raw(),
            "a racing directory must take the history root's group: {directory:?}"
        );
    }
}
