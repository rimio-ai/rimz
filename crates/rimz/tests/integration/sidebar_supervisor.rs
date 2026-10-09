//! `rimz sidebar serve` supervisor crash capture.

#![cfg(unix)]

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::process::{Child, ExitStatus};
#[cfg(target_os = "linux")]
use std::thread;
use std::time::Duration;
use std::time::Instant;

use rimz::diag::record::{DiagEnvelope, DiagEvent};

use crate::common::Env;

#[test]
#[cfg(target_os = "linux")]
fn transient_fallback_waits_for_a_stable_worker_to_exit_before_attaching() {
    transient_fallback(false, false);
}

#[test]
#[cfg(target_os = "linux")]
fn transient_fallback_rejection_backs_off_the_next_worker_probe() {
    transient_fallback(true, false);
}

#[test]
#[cfg(target_os = "linux")]
fn transient_fallback_reaps_a_stuck_worker_only_after_handoff_grace() {
    transient_fallback(false, true);
}

#[test]
#[cfg(target_os = "linux")]
fn all_young_attachments_start_one_successor_after_their_workers_are_stable() {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    use sha2::Digest;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let env = Env::new();
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let key = sha2::Sha256::digest(b"tmux\0rimz-test");
    let socket = runtime
        .sock_dir
        .join(format!("host.{}.sock", hex::encode(&key[..6])));
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut panes: Vec<_> = (0..2)
        .map(|index| {
            let pty = nix::pty::openpty(
                Some(&nix::pty::Winsize {
                    ws_col: 40,
                    ws_row: 12,
                    ws_xpixel: 320,
                    ws_ypixel: 192,
                }),
                None,
            )
            .unwrap();
            nix::fcntl::fcntl(
                &pty.master,
                nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
            )
            .unwrap();
            let tty = std::fs::File::from(pty.slave);
            let starts = env.home_root.join(format!("young-starts-{index}"));
            let mut command =
                supervisor_command(&env, &env.home_root.join("unused-exit"), &starts, "");
            command
                .env("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "2000")
                .env("RIMZ_TEST_SIDEBAR_HOST_PROBE_INTERVAL_MS", "100")
                .stdin(tty.try_clone().unwrap())
                .stdout(tty);
            (
                FallbackSupervisor {
                    child: command.spawn().unwrap(),
                    starts,
                },
                std::fs::File::from(pty.master),
            )
        })
        .collect();
    let mut accepted = Vec::new();
    let mut ids = Vec::new();
    for _ in &panes {
        let mut stream =
            fallback_connection(&listener, Duration::from_secs(10)).expect("young attachment");
        ids.push(
            rimz::SidebarInstanceId::parse(
                fallback_hello(&stream)["instance_id"].as_str().unwrap(),
            )
            .unwrap(),
        );
        stream
            .write_all(b"{\"accept\":{\"build\":null}}\n")
            .unwrap();
        accepted.push(stream);
    }
    drop(listener);
    std::fs::remove_file(&socket).unwrap();
    drop(accepted);
    let workers: Vec<_> = panes
        .iter()
        .map(|(supervisor, _)| {
            wait_for_start_after(
                &supervisor.starts,
                &env.rimz_bin(),
                0,
                Duration::from_secs(10),
            );
            std::fs::read_to_string(&supervisor.starts)
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap()
        })
        .collect();
    let host_cmdlines = || {
        rimz::proc::list_processes()
            .into_iter()
            .filter(|process| {
                process.cmdline.contains(" sidebar host ")
                    && process.cmdline.contains(env.workspace_id.as_str())
            })
            .collect::<Vec<_>>()
    };
    // A child the host has forked but not yet exec'd carries the host's cmdline;
    // a host is never spawned by a host, so parentage tells the two apart.
    let hosts = |matched: &[rimz::proc::ProcInfo]| {
        matched
            .iter()
            .filter(|process| !matched.iter().any(|parent| parent.pid == process.ppid))
            .count()
    };
    let listing = |matched: &[rimz::proc::ProcInfo]| {
        matched
            .iter()
            .map(|process| format!("{} {} {}\n", process.pid, process.ppid, process.cmdline))
            .collect::<String>()
    };
    let no_start_until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < no_start_until {
        let matched = host_cmdlines();
        assert!(
            hosts(&matched) == 0,
            "no successor start before worker stability; pid ppid cmdline of every match:\n{}",
            listing(&matched)
        );
        assert!(
            workers
                .iter()
                .all(|pid| kill(Pid::from_raw(*pid as i32), None).is_ok())
        );
        for (_, output) in &mut panes {
            let _ = output.read_to_end(&mut Vec::new());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for (_, output) in &mut panes {
            let _ = output.read_to_end(&mut Vec::new());
        }
        let reaped = workers
            .iter()
            .all(|pid| kill(Pid::from_raw(*pid as i32), None).is_err());
        let attached = ids.iter().all(|id| {
            rimz::wakeup::heartbeat::SidebarHeartbeat::read_from(
                &runtime.sidebar_heartbeat_path(id),
            )
            .is_ok_and(|beat| beat.size.is_some())
        });
        if reaped && attached {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "all young-loss workers must exit and attach to their successor without a new pane"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let matched = host_cmdlines();
    assert_eq!(
        hosts(&matched),
        1,
        "the stable probes coordinate one successor; pid ppid cmdline of every match:\n{}",
        listing(&matched)
    );
    for (supervisor, _) in &panes {
        assert_eq!(
            std::fs::read_to_string(&supervisor.starts)
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

#[cfg(target_os = "linux")]
struct FallbackSupervisor {
    child: Child,
    starts: PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for FallbackSupervisor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for line in std::fs::read_to_string(&self.starts)
            .unwrap_or_default()
            .lines()
        {
            let pid = line
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<i32>()
                .unwrap();
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn fallback_connection(
    listener: &std::os::unix::net::UnixListener,
    timeout: Duration,
) -> Option<std::os::unix::net::UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                return Some(stream);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => panic!("accept: {err}"),
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn fallback_hello(stream: &std::os::unix::net::UnixStream) -> serde_json::Value {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
#[cfg(target_os = "linux")]
fn fallback_worker_preserves_tty_and_removes_runtime_files_on_sigterm() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use std::io::Read;

    let env = Env::new();
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    let id = rimz::SidebarInstanceId::new();
    let pty = nix::pty::openpty(
        Some(&nix::pty::Winsize {
            ws_col: 40,
            ws_row: 12,
            ws_xpixel: 320,
            ws_ypixel: 192,
        }),
        None,
    )
    .unwrap();
    nix::fcntl::fcntl(
        &pty.master,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .unwrap();
    let tty = std::fs::File::from(pty.slave);
    let original = nix::sys::termios::tcgetattr(&tty).unwrap();
    let mut output = std::fs::File::from(pty.master);
    let mut command = supervisor_command(
        &env,
        &env.home_root.join("unused-exit"),
        &env.home_root.join("worker-start"),
        "",
    );
    command
        .env("RIMZ_SIDEBAR_WORKER", "1")
        .env("RIMZ_SIDEBAR_INSTANCE_ID", id.as_str())
        .stdin(tty.try_clone().unwrap())
        .stdout(tty.try_clone().unwrap())
        .stderr(Stdio::piped());
    let mut worker = command.spawn().unwrap();
    let mut bytes = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !runtime.sidebar_heartbeat_path(&id).exists() {
        let _ = output.read_to_end(&mut bytes);
        if Instant::now() >= deadline || worker.try_wait().unwrap().is_some() {
            let _ = worker.kill();
            let failed = worker.wait_with_output().unwrap();
            panic!(
                "worker did not publish its heartbeat: {} {}",
                failed.status,
                String::from_utf8_lossy(&failed.stderr)
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !nix::sys::termios::tcgetattr(&tty)
            .unwrap()
            .local_flags
            .contains(nix::sys::termios::LocalFlags::ICANON)
    );
    kill(Pid::from_raw(worker.id() as i32), Signal::SIGTERM).unwrap();
    let status = wait_child(&mut worker, Duration::from_secs(3));
    let _ = output.read_to_end(&mut bytes);

    assert!(
        status.success(),
        "SIGTERM must exit through the worker's cleanup, not default signal death: {status}"
    );
    assert_ne!(nix::sys::termios::tcgetattr(&tty).unwrap(), original);
    assert!(
        !nix::sys::termios::tcgetattr(&tty)
            .unwrap()
            .local_flags
            .contains(nix::sys::termios::LocalFlags::ICANON),
        "signal close keeps raw mode for the supervisor's next attachment"
    );
    assert!(!runtime.sidebar_heartbeat_path(&id).exists());
    assert!(
        !runtime
            .sock_dir
            .join(format!("sidebar.{}.sock", id.short()))
            .exists()
    );
    assert!(
        !bytes
            .windows(b"\x1b[?1006l\x1b[?1000l".len())
            .any(|part| part == b"\x1b[?1006l\x1b[?1000l")
    );
}

#[cfg(target_os = "linux")]
fn transient_fallback(reject_again: bool, stuck: bool) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use sha2::Digest;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixListener;

    let env = Env::new();
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let key = sha2::Sha256::digest(b"tmux\0rimz-test");
    let socket = runtime
        .sock_dir
        .join(format!("host.{}.sock", hex::encode(&key[..6])));
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let pty = nix::pty::openpty(
        Some(&nix::pty::Winsize {
            ws_col: 40,
            ws_row: 12,
            ws_xpixel: 320,
            ws_ypixel: 192,
        }),
        None,
    )
    .unwrap();
    nix::fcntl::fcntl(
        &pty.master,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .unwrap();
    let tty = std::fs::File::from(pty.slave);
    let mut output = std::fs::File::from(pty.master);
    let starts = env.home_root.join("fallback-starts");
    let mut command = supervisor_command(&env, &env.home_root.join("unused-exit"), &starts, "");
    command
        .env("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "300")
        .env("RIMZ_TEST_SIDEBAR_HOST_PROBE_INTERVAL_MS", "200")
        .env("RIMZ_TEST_SIDEBAR_HANDOFF_GRACE_MS", "500")
        .env("RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE", "empty")
        .stdin(tty.try_clone().unwrap())
        .stdout(tty.try_clone().unwrap());
    let mut running = FallbackSupervisor {
        child: command.spawn().unwrap(),
        starts,
    };
    let mut first =
        fallback_connection(&listener, Duration::from_secs(3)).expect("initial attachment");
    let id =
        rimz::SidebarInstanceId::parse(fallback_hello(&first)["instance_id"].as_str().unwrap())
            .unwrap();
    first
        .write_all(b"{\"reject\":{\"reason\":\"capacity\"}}\n")
        .unwrap();
    drop(first);

    for retry in 0..=usize::from(reject_again) {
        wait_for_start_after(
            &running.starts,
            &env.rimz_bin(),
            retry,
            Duration::from_secs(3),
        );
        let worker_pid: i32 = std::fs::read_to_string(&running.starts)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let began = Instant::now();
        let early = if retry == 0 {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(300)
        };
        assert!(
            fallback_connection(&listener, early).is_none(),
            "no probe before stability or rejection backoff"
        );
        if stuck {
            kill(Pid::from_raw(worker_pid), Signal::SIGSTOP).unwrap();
        }
        let mut bytes = Vec::new();
        let _ = output.read_to_end(&mut bytes);
        let probe = fallback_connection(&listener, Duration::from_secs(3))
            .expect("stable fallback must probe a reachable host");
        assert!(began.elapsed() >= early);
        let mut line = String::new();
        assert_eq!(
            BufReader::new(probe).read_line(&mut line).unwrap(),
            0,
            "probe is a bare connect, never a hello"
        );
        if stuck {
            assert!(
                fallback_connection(&listener, Duration::from_millis(100)).is_none(),
                "a stuck worker gets the handoff grace before forced termination"
            );
            assert!(kill(Pid::from_raw(worker_pid), None).is_ok());
        }
        let mut attached = fallback_connection(&listener, Duration::from_secs(3))
            .expect("normal attach after worker exit");
        assert_eq!(fallback_hello(&attached)["instance_id"], id.as_str());
        assert!(
            kill(Pid::from_raw(worker_pid), None).is_err(),
            "worker must be reaped before the next hello"
        );
        if stuck {
            attached
                .write_all(b"{\"accept\":{\"build\":null}}\n{\"control\":\"self-close\"}\n")
                .unwrap();
            drop(attached);
            break;
        }
        assert!(!runtime.sidebar_heartbeat_path(&id).exists());
        assert!(
            !runtime
                .sock_dir
                .join(format!("sidebar.{}.sock", id.short()))
                .exists()
        );
        let _ = output.read_to_end(&mut bytes);
        assert!(
            !bytes
                .windows(b"\x1b[?1006l\x1b[?1000l".len())
                .any(|part| part == b"\x1b[?1006l\x1b[?1000l"),
            "SIGTERM must not drop mouse reporting before reattachment"
        );
        assert!(
            !nix::sys::termios::tcgetattr(&tty)
                .unwrap()
                .local_flags
                .contains(nix::sys::termios::LocalFlags::ICANON),
            "raw mode remains enabled while the next hello awaits acceptance"
        );
        if reject_again && retry == 0 {
            attached
                .write_all(b"{\"reject\":{\"reason\":\"capacity\"}}\n")
                .unwrap();
            continue;
        }
        attached
            .write_all(b"{\"accept\":{\"build\":null}}\n{\"control\":\"self-close\"}\n")
            .unwrap();
        drop(attached);
    }
    assert!(wait_child(&mut running.child, Duration::from_secs(3)).success());
}

#[test]
fn sidebar_supervisor_records_worker_abort_and_respawns() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "abort")
    .stdout(Stdio::null())
    .stderr(Stdio::piped());

    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        None,
    )
    .log_path()
    .unwrap();
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = loop {
        let record = std::fs::read_to_string(&diag_path).ok().and_then(|text| {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str::<DiagEnvelope>(line).ok())
                .find(|record| matches!(record.event, DiagEvent::RendererSignalDeath { .. }))
        });
        if let Some(record) = record {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "renderer signal death diag timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "worker abort must leave the pane-resident supervisor running",
    );
    child.kill().expect("stop respawning supervisor");
    let output = child.wait_with_output().expect("collect supervisor output");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rimz test sidebar worker abort"),
        "supervisor should tee worker stderr",
    );

    match record.event {
        DiagEvent::RendererSignalDeath {
            signal,
            exit_code,
            stderr_excerpt,
        } => {
            assert_eq!(signal, Some(6));
            assert_eq!(exit_code, None);
            assert!(stderr_excerpt.contains("rimz test sidebar worker abort"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_reaps_stray_children_while_worker_runs() {
    let env = Env::new();
    env.record(&env.project_root);
    let stray_pid_path = env.home_root.join("stray.pid");
    let worker_exit_path = env.home_root.join("worker.exit");
    std::fs::write(&stray_pid_path, b"").expect("seed empty stray pid file");
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "exit_on_file")
    .env("RIMZ_TEST_SIDEBAR_WORKER_EXIT_FILE", &worker_exit_path)
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "10")
    .env(
        "RIMZ_TEST_SIDEBAR_SUPERVISOR_STRAY_PID_FILE",
        &stray_pid_path,
    )
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    let stray_pid = read_pid_file(&stray_pid_path, Duration::from_secs(2));

    wait_for_reap(stray_pid, Duration::from_secs(3));
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "worker should still be running when the supervisor reaps the stray child"
    );

    std::fs::write(&worker_exit_path, b"done").expect("release sidebar worker");
    thread::sleep(Duration::from_millis(50));
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "an unexpected clean worker exit must be respawned",
    );
    child.kill().expect("stop supervisor");
    child.wait().expect("reap supervisor");
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_pulls_a_record_update_without_external_wakeup() {
    let env = Env::new();
    env.record(&env.project_root);
    env.record(&env.project_root);
    let worker_exit_path = env.home_root.join("worker-never-exits");
    let starts = env.home_root.join("worker-starts.log");
    let proxy = proxy_rimz(&env, "next-rimz");
    let mut cmd = supervisor_command(&env, &worker_exit_path, &starts, "exit_on_file");
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    wait_for_start(&starts, &env.rimz_bin(), Duration::from_secs(2));

    record_target(&env, &proxy);
    wait_for_start(&starts, &proxy, Duration::from_secs(2));
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "record-driven worker replacement must preserve the supervisor",
    );

    child.kill().expect("stop supervisor");
    child.wait().expect("reap supervisor");
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_breaks_respawn_backoff_on_a_record_update() {
    let env = Env::new();
    env.record(&env.project_root);
    env.record(&env.project_root);
    let starts = env.home_root.join("backoff-worker-starts.log");
    let proxy = proxy_rimz(&env, "fixed-rimz");
    let worker_exit_path = env.home_root.join("unused-exit-file");
    let mut cmd = supervisor_command(&env, &worker_exit_path, &starts, "abort");
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    wait_for_start(&starts, &env.rimz_bin(), Duration::from_secs(2));
    wait_for_renderer_death(&env, Duration::from_secs(2));

    let updated = Instant::now();
    record_target(&env, &proxy);
    wait_for_start(&starts, &proxy, Duration::from_secs(1));
    assert!(
        updated.elapsed() < Duration::from_millis(500),
        "record polling should interrupt the one-second crash backoff",
    );

    child.kill().expect("stop supervisor");
    child.wait().expect("reap supervisor");
}

#[test]
#[cfg(target_os = "linux")]
fn crashing_recorded_build_keeps_old_supervisor_and_recovers_on_next_record() {
    let env = Env::new();
    env.record(&env.project_root);
    env.record(&env.project_root);
    let worker_exit_path = env.home_root.join("worker-never-exits");
    let starts = env.home_root.join("recovery-worker-starts.log");
    let bad = bad_rimz(&env);
    let mut cmd = supervisor_command(&env, &worker_exit_path, &starts, "exit_on_file");
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    wait_for_start(&starts, &env.rimz_bin(), Duration::from_secs(2));

    record_target(&env, &bad);
    wait_for_start(&starts, &bad, Duration::from_secs(2));
    thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "a crashing replacement worker must leave the old supervisor alive",
    );

    record_target(&env, &env.rimz_bin());
    wait_for_start_after(&starts, &env.rimz_bin(), 1, Duration::from_secs(2));
    assert!(child.try_wait().expect("poll supervisor").is_none());

    child.kill().expect("stop supervisor");
    child.wait().expect("reap supervisor");
}

#[test]
#[cfg(target_os = "linux")]
fn self_close_request_with_authoritative_siblings_respawns_worker() {
    let env = Env::new();
    env.record(&env.project_root);
    let starts = env.home_root.join("self-close-rejected-starts.log");
    let worker_exit_path = env.home_root.join("unused-exit-file");
    let mut cmd = supervisor_command(&env, &worker_exit_path, &starts, "self_close");
    cmd.env("TMUX_PANE", "%31")
        .env("RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE", "siblings")
        .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_RESPAWN_BACKOFF_MS", "10");
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");

    wait_for_start_after(&starts, &env.rimz_bin(), 1, Duration::from_secs(2));
    assert!(
        child.try_wait().expect("poll supervisor").is_none(),
        "a rejected self-close request must keep the pane supervisor alive",
    );

    child.kill().expect("stop supervisor");
    child.wait().expect("reap supervisor");
}

#[test]
#[cfg(target_os = "linux")]
fn self_close_request_with_authoritative_empty_view_exits_supervisor() {
    let env = Env::new();
    env.record(&env.project_root);
    let starts = env.home_root.join("self-close-confirmed-starts.log");
    let worker_exit_path = env.home_root.join("unused-exit-file");
    let mut cmd = supervisor_command(&env, &worker_exit_path, &starts, "self_close");
    cmd.env("TMUX_PANE", "%32")
        .env("RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE", "empty");
    let mut child = cmd.spawn().expect("spawn sidebar supervisor");

    let status = wait_child(&mut child, Duration::from_secs(2));
    assert!(
        status.success(),
        "confirmed self-close should end the pane owner"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_reaps_worker_when_its_pane_disappears() {
    let env = Env::new();
    env.record(&env.project_root);
    let instance =
        rimz::SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").expect("instance id");
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let heartbeat_path = runtime.sidebar_heartbeat_path(&instance);
    let socket_path = runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", instance.short()));
    std::fs::write(&heartbeat_path, b"seed heartbeat").expect("seed heartbeat");
    std::fs::write(&socket_path, b"seed socket").expect("seed socket");

    let worker_exit_path = env.home_root.join("worker-never-exits");
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_SIDEBAR_INSTANCE_ID", instance.as_str())
    .env("TMUX_PANE", "%11")
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "exit_on_file")
    .env("RIMZ_TEST_SIDEBAR_WORKER_EXIT_FILE", &worker_exit_path)
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_PANE_PROBE", "absent")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    let status = wait_child(&mut child, Duration::from_secs(3));

    assert!(status.success(), "supervisor exited with {status}");
    assert!(!heartbeat_path.exists(), "orphan heartbeat must be removed");
    assert!(!socket_path.exists(), "orphan socket must be removed");
    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        Some(instance),
    )
    .log_path()
    .expect("diag path");
    let record = std::fs::read_to_string(diag_path)
        .expect("orphan reap diagnostic")
        .lines()
        .filter_map(|line| serde_json::from_str::<DiagEnvelope>(line).ok())
        .find(|record| matches!(record.event, DiagEvent::RendererOrphanReaped { .. }))
        .expect("renderer orphan reap event");
    assert!(matches!(
        record.event,
        DiagEvent::RendererOrphanReaped {
            ref pane_id,
            worker_pid,
        } if pane_id == "tmux:%11" && worker_pid > 0
    ));
}

#[test]
#[cfg(target_os = "linux")]
fn sidebar_supervisor_keeps_pane_watchdog_across_worker_respawns() {
    let env = Env::new();
    env.record(&env.project_root);
    let instance =
        rimz::SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da05").expect("instance id");
    let starts = env.home_root.join("watchdog-worker-starts.log");
    let pane_absent = env.home_root.join("watchdog-pane-absent");
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_SIDEBAR_INSTANCE_ID", instance.as_str())
    .env("TMUX_PANE", "%12")
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", "abort_after_delay")
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "5")
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_RESPAWN_BACKOFF_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_WORKER_STARTED_FILE", &starts)
    .env("RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_PANE_PROBE_ABSENT_FILE", &pane_absent)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn sidebar supervisor");
    wait_for_start_after(&starts, &env.rimz_bin(), 1, Duration::from_secs(2));
    std::fs::write(&pane_absent, b"absent").expect("release pane watchdog");
    let status = wait_child(&mut child, Duration::from_secs(3));

    assert!(status.success(), "supervisor exited with {status}");
    let diag_path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        Some(instance),
    )
    .log_path()
    .expect("diag path");
    let records = std::fs::read_to_string(diag_path)
        .expect("supervisor diagnostics")
        .lines()
        .filter_map(|line| serde_json::from_str::<DiagEnvelope>(line).ok())
        .collect::<Vec<_>>();
    assert!(
        records
            .iter()
            .any(|record| matches!(record.event, DiagEvent::RendererSignalDeath { .. })),
        "the first worker must abort before the watchdog can fire",
    );
    assert!(records.iter().any(|record| matches!(
        record.event,
        DiagEvent::RendererOrphanReaped {
            ref pane_id,
            worker_pid,
        } if pane_id == "tmux:%12" && worker_pid > 0
    )));
}

#[cfg(target_os = "linux")]
fn read_pid_file(path: &Path, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    let mut last_raw = None;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(pid) = raw.trim().parse() {
                return pid;
            }
            last_raw = Some(raw);
        }
        assert!(
            Instant::now() < deadline,
            "stray pid file did not contain a pid: {:?}",
            last_raw.as_deref()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_reap(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let proc_path = Path::new("/proc").join(pid.to_string());
    loop {
        if !proc_path.exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "stray child {pid} was not reaped"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_child(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll supervisor") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sidebar supervisor did not finish within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn supervisor_command(
    env: &Env,
    worker_exit_path: &Path,
    starts: &Path,
    fault: &str,
) -> std::process::Command {
    let mut cmd = env.rimz();
    cmd.args([
        "sidebar",
        "serve",
        "--workspace-id",
        env.workspace_id.as_str(),
        "--mux",
        "tmux",
        "--session-name",
        "rimz-test",
    ])
    .env("RIMZ_TEST_SIDEBAR_WORKER_FAULT", fault)
    .env("RIMZ_TEST_SIDEBAR_WORKER_EXIT_FILE", worker_exit_path)
    .env("RIMZ_TEST_SIDEBAR_WORKER_STARTED_FILE", starts)
    .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "5")
    .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "10")
    .env("RIMZ_TEST_SIDEBAR_HANDOFF_GRACE_MS", "30")
    .env("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "10000")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd
}

#[cfg(target_os = "linux")]
fn proxy_rimz(env: &Env, name: &str) -> PathBuf {
    let path = env.home_root.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", env.rimz_bin().display()),
    )
    .expect("write proxy rimz");
    make_executable(&path);
    path
}

#[cfg(target_os = "linux")]
fn bad_rimz(env: &Env) -> PathBuf {
    let path = env.home_root.join("bad-rimz");
    std::fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nexit 1\n",
    )
    .expect("write bad rimz");
    make_executable(&path);
    path
}

#[cfg(target_os = "linux")]
fn make_executable(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(target_os = "linux")]
fn record_target(env: &Env, target: &Path) {
    let paths = env.state_path_for(&env.project_root);
    let mut record = rimz::workspace::record::read(&paths.workspace_record).unwrap();
    record.rimz_bin = Some(target.to_path_buf());
    record.rimz_build = Some(rimz::build_id::of_file(target).unwrap());
    record.updated_at = jiff::Timestamp::now();
    rimz::workspace::record::write(&paths, &record).unwrap();
}

#[cfg(target_os = "linux")]
fn wait_for_start(path: &Path, exe: &Path, timeout: Duration) {
    wait_for_start_after(path, exe, 0, timeout);
}

#[cfg(target_os = "linux")]
fn wait_for_start_after(path: &Path, exe: &Path, prior_matches: usize, timeout: Duration) {
    let needle = exe.display().to_string();
    let deadline = Instant::now() + timeout;
    loop {
        let matches = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.ends_with(&needle))
            .count();
        if matches > prior_matches {
            return;
        }
        assert!(Instant::now() < deadline, "worker {needle} did not start");
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_renderer_death(env: &Env, timeout: Duration) {
    let path = rimz::diag::DiagSink::under(
        env.state_path_for(&env.project_root).root,
        env.workspace_id.clone(),
        "rimz-test",
        None,
    )
    .log_path()
    .unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        let found = std::fs::read_to_string(&path).ok().is_some_and(|raw| {
            raw.lines()
                .filter_map(|line| serde_json::from_str::<DiagEnvelope>(line).ok())
                .any(|record| matches!(record.event, DiagEvent::RendererSignalDeath { .. }))
        });
        if found {
            return;
        }
        assert!(Instant::now() < deadline, "renderer death was not recorded");
        thread::sleep(Duration::from_millis(10));
    }
}
