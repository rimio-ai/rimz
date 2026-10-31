use super::*;

#[test]
fn test_sandbox_replaces_home_and_pins_both_muxes() {
    let workspace = TempDir::new().unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    for key in [
        "HOME",
        "TMUX_TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "ZELLIJ_CONFIG_DIR",
    ] {
        assert!(sandbox.env[key].starts_with(sandbox.root()), "{key}");
    }
    let trusted = Command::new("git")
        .arg("config")
        .arg("--file")
        .arg(sandbox.env["HOME"].join(".gitconfig"))
        .args(["--get-all", "safe.directory"])
        .output()
        .unwrap();
    assert!(trusted.status.success());
    assert_eq!(
        String::from_utf8(trusted.stdout).unwrap().trim(),
        workspace.path().to_string_lossy(),
    );
}

#[test]
fn manual_sandbox_replaces_home_and_every_persistent_xdg_root() {
    let sandbox = HostSandbox::for_manual_command().unwrap();
    for key in [
        "HOME",
        "RIMZ_HOME",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
    ] {
        assert!(sandbox.env[key].starts_with(sandbox.root()), "{key}");
    }
}

#[test]
fn unkept_manual_sandbox_is_removed_on_drop() {
    let sandbox = HostSandbox::for_manual_command().unwrap();
    let root = sandbox.root().to_path_buf();
    drop(sandbox);
    assert!(!root.exists(), "an unmarked sandbox must be removed");
}

#[test]
fn only_a_test_sandbox_names_a_skip_log_and_its_runs_keep_it() {
    let workspace = TempDir::new().unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    let exported = sandbox.command_env();
    let log = exported
        .iter()
        .find_map(|(key, value)| (*key == SKIP_LOG_ENV).then_some(value))
        .expect("test sandbox exports the skip log");
    assert!(log.starts_with(sandbox.root()), "{}", log.display());
    let ambient = [SKIP_LOG_ENV, "RIMZ_RUN_ID"].map(std::ffi::OsString::from);
    assert_eq!(
        test_removed_env(ambient.into_iter(), &sandbox.env, &[]),
        ["NO_COLOR", "RIMZ_RUN_ID"]
    );

    let manual = HostSandbox::for_manual_command().unwrap();
    assert!(
        manual
            .command_env()
            .iter()
            .all(|(key, _)| *key != SKIP_LOG_ENV)
    );
}

#[test]
fn only_a_test_sandbox_exports_a_missing_codex_and_its_runs_keep_it() {
    let workspace = TempDir::new().unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    let exported = sandbox.command_env();
    let codex = exported
        .iter()
        .find_map(|(key, value)| (*key == CODEX_BIN_ENV).then_some(value))
        .expect("test sandbox exports the codex override");
    assert!(!codex.exists(), "{}", codex.display());
    let ambient = [std::ffi::OsString::from(CODEX_BIN_ENV)];
    assert_eq!(
        test_removed_env(ambient.into_iter(), &sandbox.env, &[]),
        ["NO_COLOR"]
    );

    let manual = HostSandbox::for_manual_command().unwrap();
    assert!(
        manual
            .command_env()
            .iter()
            .all(|(key, _)| *key != CODEX_BIN_ENV)
    );
}

#[test]
fn only_a_test_sandbox_exports_offline_pricing_and_its_runs_keep_it() {
    let workspace = TempDir::new().unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    let exported = sandbox.command_env();
    assert_eq!(
        exported
            .iter()
            .find_map(|(key, value)| (*key == "RIMZ_PRICING_OFFLINE").then_some(value)),
        Some(&PathBuf::from("1"))
    );
    let ambient = [std::ffi::OsString::from("RIMZ_PRICING_OFFLINE")];
    assert_eq!(
        test_removed_env(ambient.into_iter(), &sandbox.env, &[]),
        ["NO_COLOR"]
    );

    let manual = HostSandbox::for_manual_command().unwrap();
    assert!(
        manual
            .command_env()
            .iter()
            .all(|(key, _)| *key != "RIMZ_PRICING_OFFLINE")
    );
}

#[test]
fn only_a_test_sandbox_exports_unit_test_roots_and_its_runs_keep_them() {
    let workspace = TempDir::new().unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    let exported = sandbox.command_env();
    let roots = [
        "RIMZ_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ];
    let actual = roots.map(|root| {
        let unit_key = format!("RIMZ_UNIT_TEST_{root}");
        exported
            .iter()
            .find_map(|(key, value)| (*key == unit_key).then_some(value))
    });
    assert_eq!(actual, roots.map(|key| Some(&sandbox.env[key])));
    let ambient = roots.map(|key| std::ffi::OsString::from(format!("RIMZ_UNIT_TEST_{key}")));
    assert_eq!(
        test_removed_env(ambient.into_iter(), &sandbox.env, &[]),
        ["NO_COLOR"]
    );

    let manual = HostSandbox::for_manual_command().unwrap();
    assert!(
        manual
            .command_env()
            .iter()
            .all(|(key, _)| !key.starts_with("RIMZ_UNIT_TEST_"))
    );
}

#[test]
fn skip_records_group_distinct_tests_by_reason() {
    let records = "\
journey::sandbox::b\tAF_UNIX bind is forbidden in this sandbox
backend::tmux::one\ttmux not on PATH
journey::sandbox::a\tAF_UNIX bind is forbidden in this sandbox
journey::sandbox::b\tAF_UNIX bind is forbidden in this sandbox
";
    assert_eq!(
        skip_report(records).as_deref(),
        Some(
            "self-skipped 3 tests:\n  \
             AF_UNIX bind is forbidden in this sandbox (2): journey::sandbox::a, journey::sandbox::b\n  \
             tmux not on PATH (1): backend::tmux::one"
        )
    );
    assert_eq!(skip_report(""), None);
}

#[test]
fn skip_report_bounds_the_names_per_reason() {
    let records = (0..8)
        .map(|index| format!("t{index}\ttmux not on PATH\n"))
        .collect::<String>();
    assert_eq!(
        skip_report(&records).as_deref(),
        Some("self-skipped 8 tests:\n  tmux not on PATH (8): t0, t1, t2, t3, t4, +3 more")
    );
}

#[test]
fn skip_report_reads_the_sandbox_log_and_tolerates_its_absence() {
    let workspace = TempDir::new().unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    assert_eq!(sandbox.skip_report(), None);
    std::fs::write(&sandbox.env[SKIP_LOG_ENV], "one\tgit not on PATH\n").unwrap();
    assert_eq!(
        sandbox.skip_report().as_deref(),
        Some("self-skipped 1 test:\n  git not on PATH (1): one")
    );
}

#[test]
fn denied_skips_name_the_unlisted_tests_and_end_with_the_fix() {
    let allowed = AllowedSkips::parse(
        "# root in the job container\n\nmode 000 files remain readable\nbubblewrap unusable:\n",
    );
    let listed = "\
sandbox::a\tbubblewrap unusable: bwrap not on PATH
sandbox::b\tbubblewrap unusable: probe failed
sandbox::c\tmode 000 files remain readable
";
    assert_eq!(denied_skips(listed, &allowed), None);
    assert_eq!(denied_skips("", &allowed), None);

    let unlisted = (0..6)
        .map(|index| format!("tmux::t{index}\ttmux not on PATH\n"))
        .chain(["web::one\t# root in the job container\n".to_owned()])
        .collect::<String>();
    assert_eq!(
        denied_skips(&format!("{listed}{unlisted}{unlisted}"), &allowed).as_deref(),
        Some(
            "--deny-skips: 7 tests self-skipped for a reason .config/allowed-test-skips.txt does not list:\n  \
             # root in the job container (1): web::one\n  \
             tmux not on PATH (6): tmux::t0, tmux::t1, tmux::t2, tmux::t3, tmux::t4, tmux::t5\n\
             install the missing capability in ci/Dockerfile, or add the reason to .config/allowed-test-skips.txt"
        )
    );
}

#[test]
fn every_recorded_unlisted_reason_is_denied_whatever_the_test_recorded_after_it() {
    let allowed = AllowedSkips::parse("mode 000 files remain readable\n");
    let records = "\
partial::a\ttmux not on PATH
partial::a\tmode 000 files remain readable
retried::b\ttmux not on PATH
retried::b\tgit not on PATH
retried::b\ttmux not on PATH
";
    assert_eq!(
        denied_skips(records, &allowed).as_deref(),
        Some(
            "--deny-skips: 2 tests self-skipped for a reason .config/allowed-test-skips.txt does not list:\n  \
             git not on PATH (1): retried::b\n  \
             tmux not on PATH (2): partial::a, retried::b\n\
             install the missing capability in ci/Dockerfile, or add the reason to .config/allowed-test-skips.txt"
        )
    );
    assert_eq!(
        skip_report(records).as_deref(),
        Some(
            "self-skipped 2 tests:\n  \
             mode 000 files remain readable (1): partial::a\n  \
             tmux not on PATH (1): retried::b"
        ),
        "the report keeps one reason per test"
    );
}

#[test]
fn allowed_skips_load_from_the_workspace_and_a_missing_file_names_its_path() {
    let workspace = TempDir::new().unwrap();
    let missing = AllowedSkips::load(workspace.path())
        .err()
        .expect("a missing allow-list is an error");
    assert!(
        format!("{missing:#}").contains(".config/allowed-test-skips.txt"),
        "{missing:#}"
    );

    std::fs::create_dir(workspace.path().join(".config")).unwrap();
    std::fs::write(
        workspace.path().join(ALLOWED_SKIPS_FILE),
        "git not on PATH\n",
    )
    .unwrap();
    let allowed = AllowedSkips::load(workspace.path()).unwrap();
    let sandbox = HostSandbox::for_tests(workspace.path()).unwrap();
    assert_eq!(sandbox.denied_skips(&allowed), None);
    std::fs::write(
        &sandbox.env[SKIP_LOG_ENV],
        "one\tgit not on PATH\ntwo\ttmux not on PATH\n",
    )
    .unwrap();
    let denied = sandbox.denied_skips(&allowed).expect("tmux is unlisted");
    assert!(denied.contains("tmux not on PATH (1): two"), "{denied}");
    assert!(!denied.contains("one"), "{denied}");
}

#[test]
fn session_key_covers_identity_and_mux_routing() {
    for key in ["RIMZ_WORKSPACE_ID", "TMUX", "TMUX_TMPDIR", "ZELLIJ_PANE_ID"] {
        assert!(session_key(OsStr::new(key)), "{key}");
    }
    assert!(!session_key(OsStr::new("PATH")));
}

#[test]
fn test_env_drops_room_session_keys_but_keeps_sandbox_and_build_inputs() {
    let sandbox = sandbox_env(Path::new("/tmp/rimz-sandbox-aB123z"));
    let ambient = [
        "RIMZ_RUN_ID",
        "RIMZ_WORKSPACE_ID",
        "TMUX_PANE",
        "ZELLIJ_SESSION_NAME",
        "TMUX_TMPDIR",
        "ZELLIJ_CONFIG_DIR",
        "RIMZ_PRICING_JSON_PATH",
        "RIMZ_BUILD_VERSION_OVERRIDE",
        "RIMZ_TEST_PARK_BUDGET_MIB",
        "RIMZ_TEST_BROWSER",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "PI_CODING_AGENT_SESSION_DIR",
        "PATH",
    ]
    .map(std::ffi::OsString::from);
    assert_eq!(
        test_removed_env(ambient.into_iter(), &sandbox, &[]),
        [
            "NO_COLOR",
            "RIMZ_RUN_ID",
            "RIMZ_WORKSPACE_ID",
            "TMUX_PANE",
            "ZELLIJ_SESSION_NAME",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_SESSION_DIR",
        ]
    );
}

#[test]
fn test_env_drops_git_local_variables_but_keeps_nonlocal_git_settings() {
    let local = ["GIT_DIR", "GIT_PREFIX", "GIT_CONFIG_PARAMETERS"].map(str::to_owned);
    let ambient = [
        "RIMZ_RUN_ID",
        "TMUX_PANE",
        "CODEX_HOME",
        "GIT_DIR",
        "GIT_PREFIX",
        "GIT_CONFIG_PARAMETERS",
        "GIT_EXEC_PATH",
        "GIT_EDITOR",
        "GIT_SEQUENCE_EDITOR",
        "GIT_TRACE",
        "GIT_AUTHOR_NAME",
        "GIT_CONFIG_GLOBAL",
    ]
    .map(std::ffi::OsString::from);
    assert_eq!(
        test_removed_env(ambient.into_iter(), &BTreeMap::new(), &local),
        [
            "NO_COLOR",
            "RIMZ_RUN_ID",
            "TMUX_PANE",
            "CODEX_HOME",
            "GIT_DIR",
            "GIT_PREFIX",
            "GIT_CONFIG_PARAMETERS",
        ]
    );
}

#[test]
fn git_local_removal_keeps_fixture_writes_out_of_the_callers_repository() {
    let root = TempDir::new().unwrap();
    let decoy = root.path().join("decoy");
    let fixture = root.path().join("fixture");
    std::fs::create_dir(&decoy).unwrap();
    std::fs::create_dir(&fixture).unwrap();
    let git = |root: &Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&decoy, &["init", "-b", "main"]);
    git(&decoy, &["config", "user.email", "decoy@example.invalid"]);
    git(&decoy, &["config", "user.name", "Decoy"]);
    git(&decoy, &["commit", "--allow-empty", "-m", "decoy"]);
    git(&decoy, &["remote", "add", "original", "../original.git"]);
    let head = git(&decoy, &["rev-parse", "HEAD"]);
    let email = git(&decoy, &["config", "user.email"]);
    let remotes = git(&decoy, &["remote", "-v"]);
    let local = git_local_env().unwrap();
    let injected: Vec<_> = local
        .iter()
        .map(|key| {
            let value = if key == "GIT_DIR" {
                decoy.join(".git")
            } else {
                PathBuf::from("must-be-removed")
            };
            (key.as_str(), value)
        })
        .collect();
    let removed = test_removed_env(
        local.iter().map(std::ffi::OsString::from),
        &BTreeMap::new(),
        &local,
    );
    let removed: Vec<_> = removed.iter().map(String::as_str).collect();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
        vec!["commit", "--allow-empty", "-m", "fixture"],
        vec!["remote", "add", "origin", "../remote.git"],
    ] {
        let output =
            crate::runner::run_streamed(&fixture, "git", &args, &injected, &removed, &mut |_| {})
                .unwrap();
        assert!(
            output.status.success(),
            "fixture git {args:?} must ignore ambient repository variables: {}",
            output.output
        );
    }
    assert!(fixture.join(".git").is_dir());
    assert_eq!(git(&fixture, &["log", "-1", "--format=%s"]), "fixture");
    assert_ne!(git(&fixture, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&decoy, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&decoy, &["config", "user.email"]), email);
    assert_eq!(git(&decoy, &["remote", "-v"]), remotes);
}

fn host_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> + use<> {
    let pairs: BTreeMap<String, std::ffi::OsString> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).into()))
        .collect();
    move |key| pairs.get(key).cloned()
}

#[test]
fn toolchain_homes_default_under_the_host_home_before_it_is_replaced() {
    assert_eq!(
        toolchain_env(host_env(&[("HOME", "/home/dev")])),
        [
            ("CARGO_HOME", PathBuf::from("/home/dev/.cargo")),
            ("RUSTUP_HOME", PathBuf::from("/home/dev/.rustup")),
        ]
    );
}

#[test]
fn toolchain_homes_already_set_are_inherited_not_repeated() {
    assert_eq!(
        toolchain_env(host_env(&[
            ("HOME", "/home/dev"),
            ("CARGO_HOME", "/opt/cargo"),
            ("RUSTUP_HOME", ""),
        ])),
        [("RUSTUP_HOME", PathBuf::from("/home/dev/.rustup"))]
    );
    assert!(toolchain_env(host_env(&[("CARGO_HOME", "/opt/cargo")])).is_empty());
}

#[test]
fn sandboxed_commands_carry_the_toolchain_pins_under_the_replaced_home() {
    let sandbox = HostSandbox::for_manual_command().unwrap();
    let env = sandbox.command_env();
    for (key, value) in &sandbox.toolchain {
        assert!(!value.starts_with(sandbox.root()), "{key}");
        assert!(env.contains(&(*key, value.clone())), "{key}");
    }
    assert!(
        env.iter()
            .any(|(key, value)| *key == "HOME" && value.starts_with(sandbox.root()))
    );
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn manual_command_puts_the_build_dir_first_on_path() {
    let build = TempDir::new().unwrap();
    std::fs::write(build.path().join("rimz"), "").unwrap();
    let command = manual_command(
        Path::new("/workspace"),
        build.path(),
        &args(&["rimz", "--version"]),
    )
    .unwrap();
    assert_eq!(command.get_program(), "rimz");
    assert_eq!(command.get_current_dir(), Some(Path::new("/workspace")));
    let path = command
        .get_envs()
        .find(|(key, _)| *key == "PATH")
        .and_then(|(_, value)| value)
        .unwrap();
    let paths: Vec<_> = std::env::split_paths(path).collect();
    assert_eq!(paths[0], build.path());
    assert_eq!(
        paths[1..],
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>()
    );
}

#[test]
fn bare_rimz_without_a_build_refuses_with_the_build_command() {
    let build = TempDir::new().unwrap();
    let error = manual_command(Path::new("/workspace"), build.path(), &args(&["rimz"]))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cargo build -p rimz --bin rimz --features testkit"),
        "{error}"
    );
    for (program, resolved) in [
        ("sh", "sh"),
        ("target/debug/rimz", "/workspace/target/debug/rimz"),
        ("/opt/rimz", "/opt/rimz"),
    ] {
        let command =
            manual_command(Path::new("/workspace"), build.path(), &args(&[program])).unwrap();
        assert_eq!(command.get_program(), resolved);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn sandbox_process_match_requires_a_path_boundary() {
    let environment = b"HOME=/tmp/rimz-sandbox-a/home\0PATH=/usr/bin\0";
    assert!(environment_mentions_root(
        environment,
        Path::new("/tmp/rimz-sandbox-a")
    ));
    assert!(!environment_mentions_root(
        environment,
        Path::new("/tmp/rimz-sandbox")
    ));
}

#[test]
fn reaper_accepts_only_generated_sandbox_roots() {
    assert!(validate_sandbox_root(Path::new("/tmp/rimz-sandbox-aB123z")).is_ok());
    for root in [
        "/tmp/rimz-sandbox-short",
        "/tmp/rimz-sandbox-aB_23z",
        "/var/tmp/rimz-sandbox-aB123z",
        "/tmp/other-aB123z",
        "/tmp/rimz-sandbox-aB123z/child",
    ] {
        assert!(validate_sandbox_root(Path::new(root)).is_err(), "{root}");
    }
}

#[cfg(target_os = "linux")]
mod reaper {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::sync::mpsc;

    use super::*;

    const ROLE_ENV: &str = "XTASK_SANDBOX_TEST_ROLE";
    const ROOT_ENV: &str = "XTASK_SANDBOX_TEST_ROOT";
    const KEEP_ENV: &str = "XTASK_SANDBOX_TEST_KEEP";
    const REPORT: &str = "sandbox startup failed\n\nCaused by:\n    test step\n";
    const CLEANUP_WITHIN: Duration = Duration::from_secs(15);

    #[test]
    fn kept_manual_sandbox_survives_drop_but_its_home_child_does_not() {
        let sandbox = HostSandbox::for_manual_command().unwrap();
        let root = sandbox.root().to_path_buf();
        let mut bystander = Command::new("sleep")
            .arg("600")
            .env("HOME", &sandbox.env["HOME"])
            .spawn()
            .unwrap();
        wait_for_home_child(&root, bystander.id());
        sandbox.keep(REPORT).unwrap();
        drop(sandbox);
        let root_exists = root.exists();
        let marker = std::fs::read_to_string(root.join("kept"));
        let outlived = alive_after_cleanup(&[bystander.id()]);
        let _ = bystander.kill();
        bystander.wait().unwrap();
        if root_exists {
            std::fs::remove_dir_all(&root).unwrap();
        }

        assert!(root_exists, "a marked sandbox must survive drop");
        assert_eq!(marker.unwrap(), REPORT);
        assert!(
            outlived.is_empty(),
            "drop must stop the sandbox's HOME child within {CLEANUP_WITHIN:?}"
        );
    }

    fn wait_for_home_child(root: &Path, pid: u32) {
        let deadline = Instant::now() + CLEANUP_WITHIN;
        while !sandbox_processes(root).contains(&pid) {
            assert!(
                Instant::now() < deadline,
                "sandbox never identified HOME child {pid} within {CLEANUP_WITHIN:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn role_command(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("sandbox::tests::reaper::{role}_role"),
                "--nocapture",
            ])
            .env(ROLE_ENV, role);
        command
    }

    fn in_role(role: &str) -> bool {
        std::env::var(ROLE_ENV).is_ok_and(|current| current == role)
    }

    #[test]
    #[expect(
        clippy::zombie_processes,
        reason = "the owner must die before waiting; only the reaper may stop the orphan"
    )]
    fn owner_role() {
        if !in_role("owner") {
            return;
        }
        let mut sandbox = HostSandbox::for_manual_command().unwrap();
        let mut stdout = std::io::stdout();
        writeln!(stdout, "root: {}", sandbox.root().display()).unwrap();
        stdout.flush().unwrap();
        sandbox.reaper = Some(
            SandboxReaper::start(role_command("reaper").env(ROOT_ENV, sandbox.root())).unwrap(),
        );
        writeln!(
            stdout,
            "reaper: {}",
            sandbox.reaper.as_ref().unwrap().child.id()
        )
        .unwrap();
        let bystander = Command::new("sleep")
            .arg("600")
            .env("HOME", &sandbox.env["HOME"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        if std::env::var_os(KEEP_ENV).is_some() {
            sandbox.keep(REPORT).unwrap();
        }
        writeln!(stdout, "bystander: {}", bystander.id()).unwrap();
        stdout.flush().unwrap();
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn reaper_role() {
        if in_role("reaper") {
            reap_on_eof(Path::new(&std::env::var_os(ROOT_ENV).unwrap())).unwrap();
        }
    }

    // Orphaned zombies wait for their adopter to reap them; they are no longer running.
    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
    }

    // A reap stops matching a process once its environment is unreadable, which is before the
    // process turns zombie, so a process the cleanup stopped can still read as alive for a while.
    fn alive_after_cleanup(pids: &[u32]) -> Vec<u32> {
        let deadline = Instant::now() + CLEANUP_WITHIN;
        loop {
            let living: Vec<u32> = pids.iter().copied().filter(|&pid| alive(pid)).collect();
            if living.is_empty() || Instant::now() >= deadline {
                return living;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    struct Owner {
        child: Child,
        root: Option<PathBuf>,
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(root) = &self.root
                && root.exists()
            {
                cleanup_sandbox(root, &sandbox_env(root));
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }

    #[test]
    fn group_signal_to_the_owner_leaves_the_reaper_to_clean_the_sandbox() {
        group_signal_to_owner(false);
    }

    #[test]
    fn group_signal_to_the_owner_leaves_a_marked_root_but_stops_its_child() {
        group_signal_to_owner(true);
    }

    fn group_signal_to_owner(keep: bool) {
        let mut command = role_command("owner");
        if keep {
            command.env(KEEP_ENV, "1");
        }
        let mut owner = Owner {
            child: command
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            root: None,
        };
        let stdout = owner.child.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    return;
                }
            }
        });
        let startup_deadline = Instant::now() + CLEANUP_WITHIN;
        let mut reaper = None;
        let bystander = loop {
            let line = receive
                .recv_timeout(startup_deadline.saturating_duration_since(Instant::now()))
                .expect("owner did not report its sandbox and bystander");
            if let Some(root) = line.strip_prefix("root: ") {
                owner.root = Some(PathBuf::from(root));
            }
            if let Some(pid) = line.strip_prefix("reaper: ") {
                reaper = Some(pid.parse::<u32>().unwrap());
            }
            if let Some(pid) = line.strip_prefix("bystander: ") {
                break pid.parse::<u32>().unwrap();
            }
        };
        let root = owner.root.as_ref().unwrap();
        assert!(root.exists());
        wait_for_home_child(root, bystander);
        assert!(alive(bystander));

        assert!(
            Command::new("kill")
                .args(["-TERM", "--", &format!("-{}", owner.child.id())])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(owner.child.wait().unwrap().signal(), Some(15));

        let reaper = reaper.unwrap();
        let outlived = alive_after_cleanup(&[reaper, bystander]);
        assert!(
            outlived.is_empty(),
            "sandbox cleanup after owner's process-group SIGTERM timed out: reaper_alive={}, bystander_alive={}",
            outlived.contains(&reaper),
            outlived.contains(&bystander),
        );
        assert_eq!(
            root.exists(),
            keep,
            "only a marked root must survive the reaper"
        );
        if keep {
            assert_eq!(std::fs::read_to_string(root.join("kept")).unwrap(), REPORT);
        }
    }
}
