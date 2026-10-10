//! Host-only sidebar supervision over a pane tty and the real attach wire.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::Digest;

use crate::common::Env;

const WAIT: Duration = Duration::from_secs(10);

fn write_line(mut stream: &UnixStream, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut stream, value)?;
    stream.write_all(b"\n")
}

struct Pane<'a> {
    env: &'a Env,
    listener: UnixListener,
    tty: File,
    output: File,
    bytes: Vec<u8>,
    instance: rimz::SidebarInstanceId,
    child: Option<Child>,
}

impl<'a> Pane<'a> {
    fn new(env: &'a Env) -> Self {
        env.record(&env.project_root);
        let runtime = env.runtime_paths();
        runtime.ensure_dirs().unwrap();
        let key = sha2::Sha256::digest(b"tmux\0rimz-test");
        let listener = UnixListener::bind(
            runtime
                .sock_dir
                .join(format!("host.{}.sock", hex::encode(&key[..6]))),
        )
        .unwrap();
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
        Self {
            env,
            listener,
            tty: File::from(pty.slave),
            output: File::from(pty.master),
            bytes: Vec::new(),
            instance: rimz::SidebarInstanceId::new(),
            child: None,
        }
    }

    fn start(&mut self, configure: impl FnOnce(&mut Command)) {
        let mut command = supervisor_command(self.env);
        command
            .env("RIMZ_SIDEBAR_INSTANCE_ID", self.instance.as_str())
            .stdin(self.tty.try_clone().unwrap())
            .stdout(self.tty.try_clone().unwrap());
        configure(&mut command);
        self.child = Some(command.spawn().unwrap());
    }

    fn connection(&mut self) -> (UnixStream, Value) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.drain();
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(WAIT)).unwrap();
                    let mut line = String::new();
                    std::io::BufReader::new(&stream)
                        .read_line(&mut line)
                        .unwrap();
                    let hello: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(hello["instance_id"], self.instance.as_str());
                    return (stream, hello);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => panic!("accept: {err}"),
            }
            assert!(
                Instant::now() < deadline,
                "supervisor did not send another hello"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn accept(&mut self, build: Option<String>) -> (UnixStream, Value) {
        let (stream, hello) = self.connection();
        write_line(&stream, &serde_json::json!({"accept": {"build": build}})).unwrap();
        (stream, hello)
    }

    fn drain(&mut self) {
        let _ = self.output.read_to_end(&mut self.bytes);
    }

    fn notice(&mut self, expected: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.drain();
            let text = String::from_utf8_lossy(&self.bytes);
            if text.contains(expected) {
                assert!(
                    text.contains("\x1b[2J\x1b[1;1Hsidebar: "),
                    "clear and home before notice: {text:?}"
                );
                assert!(
                    text.contains("\r\nretrying in 1s\r\n"),
                    "raw-mode lines: {text:?}"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "notice {expected:?} not painted: {text:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn events(&self) -> Vec<Value> {
        let path = rimz::diag::DiagSink::under(
            self.env.state_path_for(&self.env.project_root).root,
            self.env.workspace_id.clone(),
            "rimz-test",
            Some(self.instance.clone()),
        )
        .log_path()
        .unwrap();
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["event"].clone())
            .collect()
    }

    fn event(&self, kind: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = self.events();
            if let Some(event) = events.iter().find(|event| event["kind"] == kind) {
                return event.clone();
            }
            assert!(Instant::now() < deadline, "missing {kind}: {events:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn seed_runtime(&self) {
        let runtime = self.env.runtime_paths();
        std::fs::write(runtime.sidebar_heartbeat_path(&self.instance), b"heartbeat").unwrap();
        std::fs::write(
            runtime
                .sock_dir
                .join(format!("sidebar.{}.sock", self.instance.short())),
            b"socket",
        )
        .unwrap();
    }

    fn assert_runtime_removed(&self) {
        let runtime = self.env.runtime_paths();
        assert!(!runtime.sidebar_heartbeat_path(&self.instance).exists());
        assert!(
            !runtime
                .sock_dir
                .join(format!("sidebar.{}.sock", self.instance.short()))
                .exists()
        );
    }

    fn finish(&mut self) -> ExitStatus {
        wait_child(self.child.as_mut().unwrap(), WAIT)
    }
}

impl Drop for Pane<'_> {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            stop_child(child);
        }
    }
}

fn stop_child(child: &mut Child) {
    for process in rimz::proc::list_processes()
        .into_iter()
        .filter(|process| process.ppid == child.id())
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process.pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn non_terminal_stdout_refuses_without_spawning_or_binding() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut child = supervisor_command(&env)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = wait_child(&mut child, WAIT);
    let output = child.wait_with_output().unwrap();
    assert!(!status.success(), "non-terminal stdout must refuse");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "error: rimz sidebar serve: stdout is not a terminal; the sidebar pane command must run in a pane"
    );
    assert_eq!(
        std::fs::read_dir(&env.runtime_paths().sock_dir)
            .unwrap()
            .count(),
        0,
        "the fixture creates runtime directories, but refusal must bind no socket"
    );
    assert!(
        !rimz::proc::list_processes()
            .iter()
            .any(|process| process.cmdline.contains(" sidebar host ")
                && process.cmdline.contains(env.workspace_id.as_str()))
    );
}

#[test]
fn invalid_runtime_paths_refuse_before_terminal_modes_or_socket_work() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    let original = nix::sys::termios::tcgetattr(&pane.tty).unwrap();
    let long_runtime = env.runtime_root.join("x".repeat(120));
    pane.start(|command| {
        command
            .env("XDG_RUNTIME_DIR", &long_runtime)
            .stderr(Stdio::piped());
    });
    let status = pane.finish();
    let mut stderr = String::new();
    pane.child
        .as_mut()
        .unwrap()
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!status.success(), "an unusable runtime root must refuse");
    assert!(
        stderr.contains("socket path") && stderr.contains("shorter runtime directory"),
        "{stderr}"
    );
    assert_eq!(nix::sys::termios::tcgetattr(&pane.tty).unwrap(), original);
    assert!(!long_runtime.exists());
}

#[test]
fn rejected_host_paints_notice_records_reason_and_retries_after_backoff() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.start(|_| {});
    let (stream, _) = pane.connection();
    let rejected = Instant::now();
    write_line(
        &stream,
        &serde_json::json!({"reject": {"reason": "capacity"}}),
    )
    .unwrap();
    drop(stream);
    pane.notice("sidebar: host rejected this pane: capacity");
    let event = pane.event("sidebar_host_unavailable");
    assert_eq!(event["cause"], "rejected");
    assert_eq!(event["reason"], "capacity");
    assert_eq!(event["retry_ms"], 1000);
    assert!(event["attached_ms"].is_null());
    let (_stream, _) = pane.accept(None);
    assert!(rejected.elapsed() >= Duration::from_secs(1));
}

#[test]
fn lost_host_paints_notice_and_record_update_cuts_backoff_short() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.start(|_| {});
    let (stream, _) = pane.accept(None);
    thread::sleep(Duration::from_millis(30));
    drop(stream);
    pane.notice("sidebar: host went away");
    let event = pane.event("sidebar_host_unavailable");
    assert_eq!(event["cause"], "lost");
    assert!(event["attached_ms"].as_u64().is_some());
    assert_eq!(event["retry_ms"], 1000);
    let proxy = proxy_rimz(&env);
    let changed = Instant::now();
    record_target(&env, &proxy);
    let (_stream, _) = pane.accept(None);
    assert!(
        changed.elapsed() < Duration::from_millis(500),
        "record change must interrupt the one-second wait"
    );
}

#[test]
fn sidebar_supervisor_pulls_a_record_update_without_external_wakeup() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.start(|command| {
        command.env("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "30");
    });
    let (stream, _) = pane.accept(None);
    let proxy = proxy_rimz(&env);
    let build = record_target(&env, &proxy);
    write_line(&stream, &serde_json::json!({"control": "reload"})).unwrap();
    drop(stream);
    let (stream, _) = pane.accept(Some(build.clone()));
    assert_eq!(pane.event("supervisor_convergence")["target_build"], build);
    drop(stream);
    let (_stream, _) = pane.accept(None);
    assert!(
        env.home_root.join("proxy-started").exists(),
        "supervisor must self-exec the proven build"
    );
}

#[test]
fn crashing_recorded_build_keeps_old_supervisor_and_recovers_on_next_record() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.start(|command| {
        command.env("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "30");
    });
    let bad = env.home_root.join("bad-rimz");
    std::fs::write(&bad, "#!/bin/sh\nexit 1\n").unwrap();
    make_executable(&bad);
    let build = record_target(&env, &bad);
    let (stream, _) = pane.accept(Some(build.clone()));
    assert_eq!(
        pane.event("supervisor_preflight_rejected")["target_build"],
        build
    );
    let events = pane.events();
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "supervisor_convergence"),
        "preflight must be preceded by convergence: {events:?}"
    );
    let convergence = events
        .iter()
        .position(|event| event["kind"] == "supervisor_convergence")
        .unwrap();
    let rejection = events
        .iter()
        .position(|event| event["kind"] == "supervisor_preflight_rejected")
        .unwrap();
    assert!(
        convergence < rejection,
        "convergence precedes preflight: {events:?}"
    );
    assert!(pane.child.as_mut().unwrap().try_wait().unwrap().is_none());
    drop(stream);
    record_target(&env, &env.rimz_bin());
    let (stream, _) = pane.accept(rimz::build_id::of_file(&env.rimz_bin()).ok());
    write_line(&stream, &serde_json::json!({"control": "self-close"})).unwrap();
    assert!(pane.finish().success());
}

#[test]
fn self_close_with_authoritative_siblings_records_rejection_and_reattaches() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.start(|command| {
        command
            .env("RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE", "siblings")
            .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_RESPAWN_BACKOFF_MS", "10");
    });
    let (stream, _) = pane.accept(None);
    write_line(&stream, &serde_json::json!({"control": "self-close"})).unwrap();
    pane.event("self_close_rejected");
    let (_stream, _) = pane.accept(None);
    assert!(pane.child.as_mut().unwrap().try_wait().unwrap().is_none());
}

#[test]
fn self_close_with_authoritative_empty_view_restores_tty_and_removes_runtime() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    let original = nix::sys::termios::tcgetattr(&pane.tty).unwrap();
    pane.seed_runtime();
    pane.start(|_| {});
    let (stream, _) = pane.accept(None);
    write_line(&stream, &serde_json::json!({"control": "self-close"})).unwrap();
    assert!(pane.finish().success());
    assert_eq!(nix::sys::termios::tcgetattr(&pane.tty).unwrap(), original);
    pane.assert_runtime_removed();
}

#[test]
fn pane_disappearance_exits_and_records_supervisor_pane_gone() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.seed_runtime();
    pane.start(|command| {
        command
            .env("RIMZ_TEST_SIDEBAR_PANE_PROBE", "absent")
            .env("RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS", "10");
    });
    let (_stream, _) = pane.accept(None);
    assert!(pane.finish().success());
    assert_eq!(pane.event("supervisor_pane_gone")["pane_id"], "tmux:%11");
    pane.assert_runtime_removed();
}

#[test]
fn pane_watchdog_keeps_absence_strikes_across_host_rounds() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.start(|command| {
        command
            .env("RIMZ_TEST_SIDEBAR_PANE_PROBE", "absent")
            .env("RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS", "1000");
    });
    let (stream, _) = pane.accept(None);
    thread::sleep(Duration::from_millis(1250));
    write_line(&stream, &serde_json::json!({"control": "reload"})).unwrap();
    drop(stream);
    let (_stream, _) = pane.accept(None);
    assert!(
        wait_child(pane.child.as_mut().unwrap(), Duration::from_millis(2500)).success(),
        "retained absence strikes must close before three new probes can run"
    );
    assert_eq!(pane.event("supervisor_pane_gone")["pane_id"], "tmux:%11");
}

#[test]
fn sidebar_supervisor_reaps_stray_children_while_host_is_attached() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    let path = env.home_root.join("stray.pid");
    pane.start(|command| {
        command.env("RIMZ_TEST_SIDEBAR_SUPERVISOR_STRAY_PID_FILE", &path);
    });
    let (_stream, _) = pane.accept(None);
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(raw) = std::fs::read_to_string(&path)
            && let Ok(pid) = raw.parse::<u32>()
            && !Path::new("/proc").join(pid.to_string()).exists()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "attached supervisor must spawn and reap its stray child"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(pane.child.as_mut().unwrap().try_wait().unwrap().is_none());
}

#[test]
fn supervisor_sigterm_preserves_tty_modes_and_removes_runtime() {
    let env = Env::new();
    let mut pane = Pane::new(&env);
    pane.seed_runtime();
    pane.start(|_| {});
    let (_stream, _) = pane.accept(None);
    assert!(
        !nix::sys::termios::tcgetattr(&pane.tty)
            .unwrap()
            .local_flags
            .contains(nix::sys::termios::LocalFlags::ICANON)
    );
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pane.child.as_ref().unwrap().id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert!(
        pane.finish().success(),
        "SIGTERM must run supervisor cleanup"
    );
    pane.assert_runtime_removed();
    assert!(
        !nix::sys::termios::tcgetattr(&pane.tty)
            .unwrap()
            .local_flags
            .contains(nix::sys::termios::LocalFlags::ICANON)
    );
    pane.drain();
    assert!(
        !pane
            .bytes
            .windows(b"\x1b[?1006l\x1b[?1000l".len())
            .any(|part| part == b"\x1b[?1006l\x1b[?1000l")
    );
}

fn supervisor_command(env: &Env) -> Command {
    let mut command = env.rimz();
    command
        .args([
            "sidebar",
            "serve",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--mux",
            "tmux",
            "--session-name",
            "rimz-test",
        ])
        .env("TMUX_PANE", "%11")
        .env("RIMZ_TEST_SIDEBAR_SUPERVISOR_REAP_POLL_MS", "5")
        .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "10")
        .env("RIMZ_TEST_SIDEBAR_PANE_PROBE", "present")
        .env("RIMZ_TEST_SIDEBAR_SELF_CLOSE_PROBE", "empty")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_child(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            stop_child(child);
            assert!(
                Instant::now() < deadline,
                "supervisor must exit after refusal or confirmed pane closure within {timeout:?}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn proxy_rimz(env: &Env) -> PathBuf {
    let path = env.home_root.join("next-rimz");
    std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = sidebar ] && [ \"$2\" = serve ]; then touch '{}'; fi\nexec '{}' \"$@\"\n", env.home_root.join("proxy-started").display(), env.rimz_bin().display())).unwrap();
    make_executable(&path);
    path
}

fn make_executable(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn record_target(env: &Env, target: &Path) -> String {
    let paths = env.state_path_for(&env.project_root);
    let mut record = rimz::workspace::record::read(&paths.workspace_record).unwrap();
    let build = rimz::build_id::of_file(target).unwrap();
    record.rimz_bin = Some(target.to_path_buf());
    record.rimz_build = Some(build.clone());
    record.updated_at = jiff::Timestamp::now();
    rimz::workspace::record::write(&paths, &record).unwrap();
    build
}
