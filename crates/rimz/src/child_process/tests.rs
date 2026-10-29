use super::*;
use std::time::{Duration, Instant};

const WAIT_TIMEOUT: Duration = Duration::from_secs(3);
const REAP_WAIT_STEP: Duration = Duration::from_millis(25);

#[test]
fn stderr_tail_keeps_complete_lines_within_eight_kib() {
    let mut tail = StderrTail::default();
    tail.append(b"first\n");
    assert_eq!(tail.text(), "first\n");
    for _ in 0..12000 {
        tail.append(b"discarded\n");
    }
    tail.append(b"last line\n");
    let text = tail.text();
    assert!(text.len() <= 8192);
    assert!(!text.contains("first"));
    assert!(text.starts_with("discarded\n"));
    assert!(text.ends_with("last line\n"));
    tail.append(&vec![b'x'; 9000]);
    tail.append(b"end of oversized line\nkept\n");
    assert_eq!(tail.text(), "kept\n");
    tail.append(b"invalid: \xff\n");
    assert_eq!(tail.text(), "kept\ninvalid: \u{fffd}\n");
}

#[test]
fn user_temp_env_follows_the_saved_tmpdir() {
    assert_eq!(user_temp_env(None, Some("CLAUDE_CODE_TMPDIR")), None);
    assert_eq!(
        user_temp_env(
            Some("/var/folders/t"),
            Some("CLAUDE_CODE_TMPDIR OTHER_TMPDIR")
        ),
        Some(UserTempEnv {
            tmpdir: Some("/var/folders/t"),
            removed: vec![
                USER_TMPDIR_ENV,
                TEMP_ROOT_KEYS_ENV,
                "CLAUDE_CODE_TMPDIR",
                "OTHER_TMPDIR"
            ],
        })
    );
    assert_eq!(
        user_temp_env(Some(""), None),
        Some(UserTempEnv {
            tmpdir: None,
            removed: vec![USER_TMPDIR_ENV, TEMP_ROOT_KEYS_ENV],
        })
    );
}

#[test]
fn restoring_a_command_applies_the_user_temp_env() {
    let envs = |saved, keys| {
        let mut cmd = Command::new("true");
        apply_user_temp_env(&mut cmd, saved, keys);
        cmd.get_envs()
            .map(|(key, value)| {
                (
                    key.to_str().unwrap().to_owned(),
                    value.map(|value| value.to_str().unwrap().to_owned()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let expect = |pairs: &[(&str, Option<&str>)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(envs(None, Some("CLAUDE_CODE_TMPDIR")), expect(&[]));
    assert_eq!(
        envs(Some("/var/folders/t"), Some("CLAUDE_CODE_TMPDIR")),
        expect(&[
            ("TMPDIR", Some("/var/folders/t")),
            (USER_TMPDIR_ENV, None),
            (TEMP_ROOT_KEYS_ENV, None),
            ("CLAUDE_CODE_TMPDIR", None),
        ])
    );
    assert_eq!(
        envs(Some(""), None),
        expect(&[
            ("TMPDIR", None),
            (USER_TMPDIR_ENV, None),
            (TEMP_ROOT_KEYS_ENV, None),
        ])
    );
}

#[test]
fn spawn_fast_child_returns_pid_and_reaps_it() {
    let before = crate::proc::testkit::spawn_count();
    let pid = spawn_fast_child("child-process-fast").expect("spawn fast child");

    assert!(
        (1..=2).contains(&(crate::proc::testkit::spawn_count() - before)),
        "count the helper attempt and any scope-escape retry"
    );
    assert!(pid > 0);
    wait_until_reaped(pid);
}

#[test]
fn detached_rimz_command_anchors_cwd_to_shared_root() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    let mut cmd = Command::new(crate::proc::rimz_exe());
    anchor_detached_command(&mut cmd, &runtime).args(["agents", "auto-continue"]);

    assert_eq!(cmd.get_program(), crate::proc::rimz_exe());
    assert_eq!(cmd.get_current_dir(), Some(runtime.shared_root.as_path()));
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        [OsStr::new("agents"), OsStr::new("auto-continue")]
    );
}

#[test]
fn detached_rimz_helper_does_not_spawn_in_unit_tests() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
    let mut runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.shared_root = dir.path().join("missing");

    spawn_detached_rimz(&runtime, ["agents", "auto-continue"], "test-helper")
        .expect("unit-test spawn suppression");
    assert_eq!(
        spawn_detached_program(
            OsStr::new("missing-loop-test-program"),
            &["loop".into(), "run".into(), "task".into()],
            &runtime,
            "test-loop-run",
        )
        .expect("unit-test program spawn suppression"),
        None,
    );
}

#[test]
fn agent_helper_request_stays_one_typed_argv_value() {
    let request = crate::harness::AutoContinueRequest {
        workspace_id: crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
            "/workspace with spaces",
        )),
        kind: crate::ids::AgentKind::new_unchecked("codex"),
        agent_id: crate::ids::AgentSessionId::from("session -- 1"),
        pane_id: crate::ids::PaneId::parse("tmux:%3").unwrap(),
        message_id: Some(crate::ids::MessageId::parse("msg_0123456789abcdef").unwrap()),
        parked_since: "2026-01-02T03:04:05Z".parse().unwrap(),
        text: "--resume with spaces".to_owned(),
        reason: "overloaded_backoff_retry".to_owned(),
        label: None,
    };

    let argv = agent_helper_argv("auto-continue", &request);
    assert_eq!(&argv[..3], ["agents", "auto-continue", "--request"]);
    assert_eq!(
        serde_json::from_str::<crate::harness::AutoContinueRequest>(&argv[3]).unwrap(),
        request
    );
}

#[test]
fn many_spawns_share_one_reaper_thread() {
    let starts_before = REAPER_STARTS.load(Ordering::SeqCst);
    let pids = (0..16)
        .map(|_| spawn_fast_child("child-process-many").expect("spawn fast child"))
        .collect::<Vec<_>>();

    for pid in pids {
        wait_until_reaped(pid);
    }

    let starts_after = REAPER_STARTS.load(Ordering::SeqCst);
    assert!(
        starts_after <= starts_before + 1,
        "expected at most one global reaper start, got before={starts_before} after={starts_after}"
    );
}

#[test]
fn supervised_child_exit_wakes_without_polling() {
    let (wake_tx, wake_rx) = mpsc::channel();
    let child = Command::new("sh")
        .args(["-c", "exit 3"])
        .spawn()
        .expect("spawn supervised child");
    let mut child = SupervisedChild::adopt(child, wake_tx.clone());
    wake_after(wake_tx, WAIT_TIMEOUT);

    wait_wake(&wake_rx, None);
    let status = child
        .try_wait()
        .expect("read supervised child status")
        .expect("child exit wake arrived before watchdog");

    assert_eq!(status.code(), Some(3));
}

#[cfg(unix)]
#[test]
fn adopted_pid_wakes_with_exit_status_and_supports_signals() {
    for signal in [
        None,
        Some(SupervisedChild::signal_term as fn(&mut SupervisedChild)),
        Some(SupervisedChild::signal_kill),
    ] {
        let (wake, receiver) = mpsc::channel();
        let child = Command::new("sh")
            .args([
                "-c",
                if signal.is_some() {
                    "exec sleep 5"
                } else {
                    "exit 7"
                },
            ])
            .spawn()
            .unwrap();
        let pid = child.id();
        drop(child);
        let mut adopted = SupervisedChild::adopt_pid(pid, wake);
        if let Some(signal) = signal {
            signal(&mut adopted);
        }
        assert!(
            receiver.recv_timeout(WAIT_TIMEOUT).is_ok(),
            "adopted pid must wake its supervisor"
        );
        let status = adopted
            .try_wait()
            .unwrap()
            .expect("exit published before wake");
        if signal.is_none() {
            assert_eq!(status.code(), Some(7));
        } else {
            use std::os::unix::process::ExitStatusExt;
            assert!(status.signal().is_some());
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn supervised_term_thaws_before_delivering_the_signal() {
    let dir = tempfile::tempdir().unwrap();
    let uid = nix::unistd::Uid::current().as_raw();
    let scope = crate::proc::cgroup::ScopePath::from_cgroup(
        dir.path(),
        &format!(
            "0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/rimz-agent-test.scope"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(scope.as_path()).unwrap();
    let freeze = scope.as_path().join("cgroup.freeze");
    std::fs::write(&freeze, "1").unwrap();
    signal_term_with_scope(10, Some(scope), |pid| {
        assert_eq!(pid, 10);
        assert_eq!(
            std::fs::read_to_string(&freeze).unwrap(),
            "0",
            "SIGTERM must observe an unfrozen scope"
        );
    });
}

#[test]
fn supervised_child_signals_stop_running_children() {
    for signal in [
        SupervisedChild::signal_term as fn(&mut SupervisedChild),
        SupervisedChild::signal_kill,
    ] {
        let (wake_tx, wake_rx) = mpsc::channel();
        let child = Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn supervised sleep");
        let mut child = SupervisedChild::adopt(child, wake_tx.clone());
        wake_after(wake_tx, WAIT_TIMEOUT);

        signal(&mut child);
        wait_wake(&wake_rx, None);
        child
            .try_wait()
            .expect("read signaled child status")
            .expect("child signal wake arrived before watchdog");
    }
}

#[test]
fn registered_signal_wakes_blocked_receiver() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use signal_hook::consts::signal::SIGUSR1;

    let (wake_tx, wake_rx) = mpsc::channel();
    let signal_wake = wake_tx.clone();
    register_signal_wake(vec![SIGUSR1], move |_| {
        let _ = signal_wake.send(());
    })
    .expect("register signal wake");
    wake_after(wake_tx, WAIT_TIMEOUT);

    let started = Instant::now();
    kill(Pid::this(), Signal::SIGUSR1).expect("raise SIGUSR1");
    wait_wake(&wake_rx, None);

    assert!(
        started.elapsed() < WAIT_TIMEOUT,
        "signal wake came from watchdog"
    );
}

fn spawn_fast_child(label: &'static str) -> io::Result<u32> {
    spawn_detached_reaped(
        "sh",
        |cmd| {
            cmd.args(["-c", "exit 0"]);
            Ok(())
        },
        label,
    )
}

#[test]
fn helper_scope_nonzero_exit_retries_plain() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("ran");
    let prefix = vec!["sh".into(), "-c".into(), "exit 1".into(), "--".into()];
    let before = crate::proc::testkit::spawn_count();
    let pid = spawn_detached_with(
        OsStr::new("sh"),
        |cmd| {
            cmd.args(["-c", "printf ran > \"$1\"", "--"]).arg(&output);
            Ok(())
        },
        "helper-exit-fallback-test",
        Some(&prefix),
    )
    .unwrap();
    wait_until_reaped(pid);
    assert_eq!(crate::proc::testkit::spawn_count() - before, 2);
    assert_eq!(std::fs::read_to_string(output).ok().as_deref(), Some("ran"));
}

#[test]
fn helper_scope_spawn_failure_preserves_configuration_on_plain_retry() {
    let root = tempfile::tempdir().unwrap();
    let log = std::fs::File::create(root.path().join("helper.log")).unwrap();
    let prefix = vec![
        root.path()
            .join("missing-systemd-run")
            .display()
            .to_string(),
        "--".into(),
    ];
    let pid = spawn_detached_with(
        OsStr::new("sh"),
        |cmd| {
            cmd.current_dir(root.path())
                .env("RIMZ_TEST_HELPER", "preserved")
                .args(["-c", "printf '%s' \"$RIMZ_TEST_HELPER\" >&2; pwd > cwd"])
                .stderr(log.try_clone()?);
            Ok(())
        },
        "helper-fallback-test",
        Some(&prefix),
    );
    assert!(pid.is_ok(), "{pid:?}");
    wait_until_reaped(pid.unwrap());
    assert_eq!(
        std::fs::read_to_string(root.path().join("helper.log")).unwrap(),
        "preserved"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("cwd"))
            .unwrap()
            .trim(),
        root.path().to_str().unwrap()
    );
}

#[test]
fn detached_helpers_escape_only_agent_scopes() {
    let prefix = helper_scope_argv(
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/rimz-agent-x-y.scope\n",
    );
    assert!(prefix.is_some());
    let prefix = prefix.unwrap();
    assert_eq!(
        &prefix[..6],
        [
            "systemd-run",
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--unit"
        ]
    );
    assert!(prefix[6].starts_with("rimz-helper-"));
    assert!(prefix[6].ends_with(".scope"));
    assert_eq!(prefix[7], "--");
    for text in [
        "0::/app.slice/rimz-helper-z.scope",
        "0::/session-3.scope",
        "0::/app.slice/rimz-agent-x.scope/child",
        "1:memory:/app.slice/rimz-agent-x.scope",
    ] {
        assert!(helper_scope_argv(text).is_none());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn drainer_scope_is_weighted_in_a_fresh_unit() {
    let mut units = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let prefix = drainer_scope_prefix().expect("a Linux drainer requests a scope");
        assert!(
            prefix
                .windows(2)
                .any(|pair| pair == ["-p", "CPUWeight=1000"]),
            "the drainer must request its CPU weight: {prefix:?}"
        );
        let unit = &prefix.windows(2).find(|pair| pair[0] == "--unit").unwrap()[1];
        assert!(unit.starts_with("rimz-helper-") && unit.ends_with(".scope"));
        assert!(units.insert(unit.clone()), "each spawn gets its own unit");
        assert!(helper_scope_matches(
            &format!("0::/app.slice/{unit}"),
            Some(unit)
        ));
        assert_eq!(prefix.last().unwrap(), "--");
    }
}

#[test]
fn inherited_helper_scope_does_not_confirm_the_requested_move() {
    assert!(!helper_scope_matches(
        "0::/app.slice/rimz-helper-parent.scope",
        Some("rimz-helper-requested.scope"),
    ));
}

#[test]
fn drainer_scope_failures_preserve_plain_configuration() {
    for failure in ["spawn", "exit", "unconfirmed"] {
        let root = tempfile::tempdir().unwrap();
        let log = std::fs::File::create(root.path().join("helper.log")).unwrap();
        let mut prefix = scope_argv(&DRAINER_SCOPE_PROPERTIES);
        match failure {
            "spawn" => {
                prefix[0] = root
                    .path()
                    .join("missing-systemd-run")
                    .display()
                    .to_string()
            }
            "exit" => prefix[0] = "sh".into(),
            _ => {
                let unit = prefix.windows(2).find(|pair| pair[0] == "--unit").unwrap()[1].clone();
                prefix = vec![
                    "sh".into(),
                    "-c".into(),
                    "exec sleep 10".into(),
                    "--".into(),
                    "--unit".into(),
                    unit,
                ];
            }
        }
        let before = crate::proc::testkit::spawn_count();
        let pid = spawn_detached_with(
            OsStr::new("sh"),
            |cmd| {
                cmd.current_dir(root.path())
                    .env("RIMZ_TEST_HELPER", "preserved")
                    .args(["-c", "printf '%s' \"$RIMZ_TEST_HELPER\" >&2; pwd > cwd"])
                    .stderr(log.try_clone()?);
                Ok(())
            },
            "drainer-fallback-test",
            Some(&prefix),
        )
        .unwrap();
        wait_until_reaped(pid);
        assert_eq!(crate::proc::testkit::spawn_count() - before, 2, "{failure}");
        assert!(
            std::fs::read_to_string(root.path().join("helper.log"))
                .unwrap()
                .ends_with("preserved")
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("cwd"))
                .unwrap()
                .trim(),
            root.path().to_str().unwrap(),
        );
    }
}

fn wake_after(wake: Sender<()>, delay: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let _ = wake.send(());
    });
}

fn wait_until_reaped(pid: u32) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        match crate::proc::stat_metrics(pid).map(|stat| stat.state) {
            None => return,
            Some('Z') => {}
            Some(_) if Instant::now() < deadline => {}
            Some(state) => panic!("pid {pid} remained alive with state {state:?}"),
        }

        assert!(
            Instant::now() < deadline,
            "pid {pid} was not reaped; last state was {:?}",
            crate::proc::stat_metrics(pid).map(|stat| stat.state)
        );
        std::thread::sleep(REAP_WAIT_STEP);
    }
}
#[cfg(unix)]
#[test]
fn cleanup_mask_is_inherited_by_workers_and_restored_on_the_caller() {
    use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
    let current = || {
        let mut signals = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, None, Some(&mut signals)).unwrap();
        signals
    };
    let before = current();
    let mask = CleanupSignalMask::block().unwrap();
    for signal in [Signal::SIGHUP, Signal::SIGTERM, Signal::SIGINT] {
        assert!(
            current().contains(signal),
            "handoff must retain {signal:?} as pending"
        );
    }
    let worker = std::thread::spawn(current).join().unwrap();
    for signal in [Signal::SIGHUP, Signal::SIGTERM, Signal::SIGINT] {
        assert!(
            worker.contains(signal),
            "workers must not consume {signal:?} during exec"
        );
    }
    drop(mask);
    for signal in [
        Signal::SIGHUP,
        Signal::SIGTERM,
        Signal::SIGINT,
        Signal::SIGUSR1,
    ] {
        assert_eq!(
            current().contains(signal),
            before.contains(signal),
            "provider spawn keeps its original {signal:?} mask"
        );
    }
}
