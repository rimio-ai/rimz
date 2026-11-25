use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, getpgrp, getpid, getsid, kill_process, kill_process_group};

use super::*;

const ROLE_ENV: &str = "XTASK_SESSION_TEST_ROLE";
const MODE_ENV: &str = "XTASK_SESSION_TEST_MODE";
const OUTER_PID_ENV: &str = "XTASK_SESSION_TEST_OUTER_PID";
const STARTED_FILE_ENV: &str = "XTASK_SESSION_TEST_STARTED_FILE";
const FINISHED_FILE_ENV: &str = "XTASK_SESSION_TEST_FINISHED_FILE";
const SKIP_LOG_ENV: &str = "RIMZ_TEST_SKIP_LOG";
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

fn role_command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("session::tests::{role}_role"),
            "--nocapture",
        ])
        .env(ROLE_ENV, role);
    command
}

fn in_role(role: &str) -> bool {
    std::env::var(ROLE_ENV).is_ok_and(|current| current == role)
}

fn has_python3() -> bool {
    match Command::new("python3")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

fn skip(reason: &str) {
    let _ = writeln!(std::io::stderr(), "skipping: {reason}");
    let Some(log) = std::env::var_os(SKIP_LOG_ENV) else {
        return;
    };
    let name = std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .or_else(|| thread::current().name().map(str::to_owned))
        .unwrap_or_else(|| "<unknown test>".to_owned());
    let line = format!("{name}\t{}\n", reason.replace(['\t', '\n'], " "));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(&log))
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .expect("append self-skip record");
}

fn parse_kv(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once(": ")?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn pid_from_u32(raw: u32) -> Pid {
    Pid::from_raw(i32::try_from(raw).unwrap()).unwrap()
}

fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        rustix::process::test_kill_process(pid_from_u32(pid)).is_ok()
    }
}

fn wait_all_dead(pids: &[u32], timeout: Duration) -> Vec<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        let living: Vec<u32> = pids.iter().copied().filter(|&pid| alive(pid)).collect();
        if living.is_empty() || Instant::now() >= deadline {
            return living;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_child_bounded(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return Some(status),
            None if Instant::now() >= deadline => return None,
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

struct GroupGuard {
    outer: Child,
    inner_pid: Option<u32>,
    grandchild_pid: Option<u32>,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        let _ = self.outer.kill();
        let _ = self.outer.wait();
        if let Some(inner) = self.inner_pid {
            let pid = pid_from_u32(inner);
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = kill_process(pid, Signal::KILL);
        }
        if let Some(grandchild) = self.grandchild_pid {
            let _ = kill_process(pid_from_u32(grandchild), Signal::KILL);
        }
    }
}

fn spawn_session_command(mut command: Command) -> (GroupGuard, u32, u32) {
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut guard = GroupGuard {
        outer: command.spawn().unwrap(),
        inner_pid: None,
        grandchild_pid: None,
    };
    let stdout = guard.outer.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if send.send(line).is_err() {
                return;
            }
        }
    });
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while guard.inner_pid.is_none() || guard.grandchild_pid.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = receive
            .recv_timeout(remaining)
            .expect("inner did not report its pid and sleeping grandchild");
        if let Some(pid) = line.strip_prefix("inner_pid: ") {
            guard.inner_pid = Some(pid.parse().unwrap());
        }
        if let Some(pid) = line.strip_prefix("grandchild_pid: ") {
            guard.grandchild_pid = Some(pid.parse().unwrap());
        }
    }
    let inner = guard.inner_pid.unwrap();
    let grandchild = guard.grandchild_pid.unwrap();
    assert!(alive(inner), "inner {inner} was not alive at start");
    assert!(
        alive(grandchild),
        "grandchild {grandchild} was not alive at start"
    );
    (guard, inner, grandchild)
}

fn spawn_sleeping_session() -> (GroupGuard, u32, u32) {
    let mut command = role_command("outer");
    command.env(MODE_ENV, "sleep-group");
    spawn_session_command(command)
}

#[test]
fn outer_role() {
    if !in_role("outer") {
        return;
    }
    let mode = std::env::var(MODE_ENV).unwrap_or_default();
    if mode == "probe" {
        let mut stdout = std::io::stdout();
        let sid = getsid(None).unwrap().as_raw_nonzero().get();
        let tty = if File::open("/dev/tty").is_ok() {
            "open"
        } else {
            "closed"
        };
        writeln!(stdout, "outer_sid: {sid}").unwrap();
        writeln!(stdout, "outer_tty: {tty}").unwrap();
        stdout.flush().unwrap();
    }
    let inner = match mode.as_str() {
        "exit-42" => {
            let mut stdout = std::io::stdout();
            writeln!(stdout, "---BEGIN-OUTER---").unwrap();
            stdout.flush().unwrap();
            let mut cmd = Command::new("sh");
            cmd.args([
                "-c",
                "printf 'inner stdout line\\n'; printf 'inner stderr line\\n' >&2; exit 42",
            ]);
            cmd
        }
        "exit-0" => {
            let mut stdout = std::io::stdout();
            writeln!(stdout, "---BEGIN-OUTER---").unwrap();
            stdout.flush().unwrap();
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "printf 'inner ok\\n'; exit 0"]);
            cmd
        }
        _ => {
            let mut cmd = role_command("inner");
            cmd.env(MODE_ENV, &mode)
                .env(OUTER_PID_ENV, std::process::id().to_string());
            cmd
        }
    };
    match run_outer_command(inner) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "{err:#}");
            std::process::exit(1);
        }
    }
}

#[test]
fn inner_role() {
    if !in_role("inner") {
        return;
    }
    let mode = std::env::var(MODE_ENV).unwrap_or_default();
    let outer_pid = std::env::var(OUTER_PID_ENV)
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .and_then(Pid::from_raw)
        .unwrap_or_else(|| rustix::process::getppid().expect("inner must have a parent pid"));
    if mode == "setsid-fail" {
        match enter(outer_pid) {
            Ok(()) => {
                let _ = writeln!(std::io::stdout(), "unexpected ok");
                std::process::exit(0);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "{err:#}");
                std::process::exit(99);
            }
        }
    }
    enter(outer_pid).unwrap();
    match mode.as_str() {
        "probe" => {
            let mut stdout = std::io::stdout();
            let pid = getpid().as_raw_nonzero().get();
            let pgid = getpgrp().as_raw_nonzero().get();
            let sid = getsid(None).unwrap().as_raw_nonzero().get();
            let tty = if File::open("/dev/tty").is_ok() {
                "open"
            } else {
                "closed"
            };
            writeln!(stdout, "inner_pid: {pid}").unwrap();
            writeln!(stdout, "inner_pgid: {pgid}").unwrap();
            writeln!(stdout, "inner_sid: {sid}").unwrap();
            writeln!(stdout, "inner_tty: {tty}").unwrap();
            stdout.flush().unwrap();
            std::process::exit(0);
        }
        "self-kill" => {
            let _ = kill_process(getpid(), Signal::KILL);
            std::process::exit(0);
        }
        "dead-outer" => {
            let mut stdout = std::io::stdout();
            writeln!(stdout, "reached-task").unwrap();
            stdout.flush().unwrap();
            thread::sleep(Duration::from_secs(600));
            std::process::exit(0);
        }
        "sleep-group" => {
            let mut grandchild = Command::new("sleep")
                .arg("600")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut stdout = std::io::stdout();
            writeln!(stdout, "inner_pid: {}", std::process::id()).unwrap();
            writeln!(stdout, "grandchild_pid: {}", grandchild.id()).unwrap();
            stdout.flush().unwrap();
            let _ = grandchild.wait();
            std::process::exit(0);
        }
        "slow-term-group" => {
            let started = std::env::var(STARTED_FILE_ENV).unwrap();
            let finished = std::env::var(FINISHED_FILE_ENV).unwrap();
            // The background child writes the started file once it has reset sh's inherited TERM
            // trap: a group SIGTERM sent before that reset reaches the trap's handler in the child,
            // and the surviving `sleep 600` then holds the outer's drain open past the test's bound.
            let mut grandchild = Command::new("sh")
                .args([
                    "-c",
                    "trap 'sleep 0.35; : > \"$2\"; exit 0' TERM; ( : > \"$1\"; exec sleep 600 ) & wait",
                    "_",
                    &started,
                    &finished,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut stdout = std::io::stdout();
            writeln!(stdout, "inner_pid: {}", std::process::id()).unwrap();
            writeln!(stdout, "grandchild_pid: {}", grandchild.id()).unwrap();
            stdout.flush().unwrap();
            let _ = grandchild.wait();
            std::process::exit(0);
        }
        other => panic!("unknown test mode: {other}"),
    }
}

#[test]
fn parse_in_session_accepts_outer_pid_and_rejects_malformed_values() {
    let plain = vec!["test".to_owned(), "auth".to_owned()];
    assert_eq!(parse_in_session(&plain).unwrap(), None);

    let pid_12345 = Pid::from_raw(12345).unwrap();
    let bare = vec![in_session_arg(pid_12345)];
    assert_eq!(parse_in_session(&bare).unwrap(), Some((pid_12345, &[][..])));

    let with_task = vec![
        in_session_arg(pid_12345),
        "gate".to_owned(),
        "--check".to_owned(),
    ];
    assert_eq!(
        parse_in_session(&with_task).unwrap(),
        Some((pid_12345, &with_task[1..]))
    );

    let doubled = vec![in_session_arg(pid_12345), in_session_arg(pid_12345)];
    assert_eq!(
        parse_in_session(&doubled).unwrap(),
        Some((pid_12345, &doubled[1..]))
    );

    for malformed in [
        IN_SESSION_ARG.to_owned(),
        format!("{IN_SESSION_ARG}="),
        format!("{IN_SESSION_ARG}=abc"),
        format!("{IN_SESSION_ARG}=0"),
        format!("{IN_SESSION_ARG}=-5"),
        format!("{IN_SESSION_ARG}=99999999999999999999"),
    ] {
        let err = parse_in_session(std::slice::from_ref(&malformed))
            .expect_err(&format!("malformed hidden arg `{malformed}` must fail"));
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains(&malformed),
            "error for `{malformed}` must name the rejected argument: {rendered}"
        );
    }
}

#[test]
fn inner_terminates_immediately_when_outer_is_already_dead() {
    let mut short_lived = Command::new("true").spawn().unwrap();
    let dead_pid = short_lived.id();
    let _ = short_lived.wait();

    let mut child = role_command("inner")
        .env(MODE_ENV, "dead-outer")
        .env(OUTER_PID_ENV, dead_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = wait_child_bounded(&mut child, WAIT_TIMEOUT);
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let status = status.expect("inner with already-dead outer pid must terminate promptly");
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        status.signal(),
        Some(SIGTERM),
        "inner with dead outer must terminate by SIGTERM; status={status:?}, stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("reached-task"),
        "inner must not reach the task when its outer is already gone; stdout={stdout:?}"
    );
}

#[test]
fn inner_runs_in_new_session_without_controlling_terminal() {
    if !has_python3() {
        skip("python3 not on PATH");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let driver = r#"
import os, pty, select, sys
cmd = sys.argv[1:]
pid, master = pty.fork()
if pid == 0:
    os.execv(cmd[0], cmd)
chunks = []
while True:
    ready, _, _ = select.select([master], [], [], 10.0)
    if not ready:
        break
    try:
        data = os.read(master, 4096)
    except OSError:
        break
    if not data:
        break
    chunks.append(data)
_, status = os.waitpid(pid, 0)
os.close(master)
sys.stdout.buffer.write(b"".join(chunks))
sys.exit(os.waitstatus_to_exitcode(status))
"#;
    let output = Command::new("python3")
        .args([
            "-c",
            driver,
            exe.to_str().unwrap(),
            "--exact",
            "session::tests::outer_role",
            "--nocapture",
        ])
        .env(ROLE_ENV, "outer")
        .env(MODE_ENV, "probe")
        .output()
        .expect("python3 pty driver failed to start");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "pty probe failed ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    let fields = parse_kv(&stdout);
    assert_eq!(
        fields.get("outer_tty").map(String::as_str),
        Some("open"),
        "outer must start with a controlling terminal; got:\n{stdout}"
    );
    assert_eq!(
        fields.get("inner_tty").map(String::as_str),
        Some("closed"),
        "inner must have no controlling terminal; got:\n{stdout}"
    );
    let outer_sid = fields.get("outer_sid").expect("missing outer_sid");
    let inner_sid = fields.get("inner_sid").expect("missing inner_sid");
    let inner_pid = fields.get("inner_pid").expect("missing inner_pid");
    let inner_pgid = fields.get("inner_pgid").expect("missing inner_pgid");
    assert_ne!(
        inner_sid, outer_sid,
        "inner must lead a new session; got:\n{stdout}"
    );
    assert_eq!(inner_sid, inner_pid, "inner must be its session leader");
    assert_eq!(inner_pgid, inner_pid, "inner must be its group leader");
}

#[test]
fn outer_preserves_inner_exit_code_and_output() {
    let failing = role_command("outer")
        .env(MODE_ENV, "exit-42")
        .output()
        .unwrap();
    let failing_stdout = String::from_utf8_lossy(&failing.stdout);
    let failing_stderr = String::from_utf8_lossy(&failing.stderr);
    assert_eq!(
        failing.status.code(),
        Some(42),
        "outer must mirror inner's non-zero exit code; stdout={failing_stdout:?}, stderr={failing_stderr:?}"
    );
    let (_, after_failing) = failing_stdout
        .split_once("---BEGIN-OUTER---\n")
        .unwrap_or_else(|| panic!("missing outer marker; stdout={failing_stdout:?}"));
    assert_eq!(after_failing, "inner stdout line\n");
    assert_eq!(failing_stderr, "inner stderr line\n");

    let passing = role_command("outer")
        .env(MODE_ENV, "exit-0")
        .output()
        .unwrap();
    let passing_stdout = String::from_utf8_lossy(&passing.stdout);
    let passing_stderr = String::from_utf8_lossy(&passing.stderr);
    assert_eq!(
        passing.status.code(),
        Some(0),
        "outer must mirror inner's zero exit code; stdout={passing_stdout:?}, stderr={passing_stderr:?}"
    );
    let (_, after_passing) = passing_stdout
        .split_once("---BEGIN-OUTER---\n")
        .unwrap_or_else(|| panic!("missing outer marker; stdout={passing_stdout:?}"));
    assert_eq!(after_passing, "inner ok\n");
    assert_eq!(passing_stderr, "");
}

#[test]
fn outer_exits_nonzero_when_inner_is_killed_by_signal() {
    let output = role_command("outer")
        .env(MODE_ENV, "self-kill")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(128 + 9),
        "outer must exit 128 + signal when inner dies by signal; status={:?}",
        output.status
    );
}

#[test]
fn failed_spawn_of_inner_returns_error_with_cause() {
    let err = run_outer_command(Command::new("/nonexistent/xtask-missing-binary"))
        .expect_err("missing inner binary must fail instead of succeeding");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("spawning in-session xtask")
            && (rendered.contains("No such file") || rendered.contains("not found")),
        "spawn failure must carry its cause; got: {rendered}"
    );
}

#[test]
fn failed_setsid_returns_error_with_cause() {
    let output = role_command("inner")
        .env(MODE_ENV, "setsid-fail")
        .process_group(0)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(99),
        "setsid in a group leader must return Err; status={:?}, stderr={stderr:?}",
        output.status
    );
    assert!(
        stderr.contains("creating a new session")
            && (stderr.contains("Operation not permitted") || stderr.contains("EPERM")),
        "setsid failure must name the cause; got: {stderr:?}"
    );
}

fn assert_signal_forwarded(signal: Signal, signal_number: i32) {
    let (mut guard, inner_pid, grandchild_pid) = spawn_sleeping_session();
    let outer_pid = pid_from_u32(guard.outer.id());
    kill_process(outer_pid, signal).expect("sending signal to outer failed");

    let status = wait_child_bounded(&mut guard.outer, WAIT_TIMEOUT)
        .expect("outer did not exit after receiving signal");
    assert_eq!(
        status.signal(),
        Some(signal_number),
        "outer must die by signal {signal_number}; got {status:?}"
    );
    assert_eq!(
        status.code(),
        None,
        "signal-killed outer must have no numeric exit code; got {status:?}"
    );
    let survivors = wait_all_dead(&[inner_pid, grandchild_pid], WAIT_TIMEOUT);
    assert!(
        survivors.is_empty(),
        "signal {signal_number} to outer left inner group alive: {survivors:?}"
    );
}

#[test]
fn sigint_to_outer_ends_inner_group_and_dies_by_sigint() {
    assert_signal_forwarded(Signal::INT, signal_hook::consts::SIGINT);
}

#[test]
fn sigterm_to_outer_ends_inner_group_and_dies_by_sigterm() {
    assert_signal_forwarded(Signal::TERM, signal_hook::consts::SIGTERM);
}

#[test]
fn sighup_to_outer_ends_inner_group_and_dies_by_sighup() {
    assert_signal_forwarded(Signal::HUP, signal_hook::consts::SIGHUP);
}

#[test]
fn outer_waits_for_live_group_member_after_forwarded_signal() {
    let dir = tempfile::tempdir().unwrap();
    let started = dir.path().join("started");
    let finished = dir.path().join("finished");
    let mut command = role_command("outer");
    command
        .env(MODE_ENV, "slow-term-group")
        .env(STARTED_FILE_ENV, &started)
        .env(FINISHED_FILE_ENV, &finished);
    let (mut guard, _inner_pid, grandchild_pid) = spawn_session_command(command);

    let ready_deadline = Instant::now() + WAIT_TIMEOUT;
    while !started.exists() {
        assert!(
            Instant::now() < ready_deadline,
            "slow-term grandchild never armed its SIGTERM trap"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let outer_pid = pid_from_u32(guard.outer.id());
    kill_process(outer_pid, Signal::TERM).expect("sending SIGTERM to outer failed");

    let status = wait_child_bounded(&mut guard.outer, WAIT_TIMEOUT)
        .expect("outer did not exit after live group member finished");
    assert_eq!(
        status.signal(),
        Some(SIGTERM),
        "outer must die by SIGTERM after draining live group; got {status:?}"
    );
    // Checked immediately with no post-exit poll: proves outer waited for the live member.
    assert!(
        finished.exists(),
        "outer returned before live group member finished its SIGTERM cleanup"
    );
    assert!(
        !alive(grandchild_pid),
        "grandchild {grandchild_pid} was still alive when outer returned"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sigterm_to_outer_returns_promptly_under_non_reaping_subreaper() {
    if !has_python3() {
        skip("python3 not on PATH");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let driver = r#"
import ctypes, os, signal, subprocess, sys, time

PR_SET_CHILD_SUBREAPER = 36
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
    raise OSError(ctypes.get_errno(), "prctl(PR_SET_CHILD_SUBREAPER) failed")

env = dict(os.environ)
env["XTASK_SESSION_TEST_ROLE"] = "outer"
env["XTASK_SESSION_TEST_MODE"] = "sleep-group"
proc = subprocess.Popen(
    sys.argv[1:],
    env=env,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
    start_new_session=False,
)
inner_pid = None
grandchild_pid = None
deadline = time.monotonic() + 5.0
while inner_pid is None or grandchild_pid is None:
    if time.monotonic() > deadline:
        proc.kill()
        sys.exit(2)
    line = proc.stdout.readline()
    if not line:
        break
    if line.startswith("inner_pid: "):
        inner_pid = int(line.split(": ", 1)[1].strip())
    elif line.startswith("grandchild_pid: "):
        grandchild_pid = int(line.split(": ", 1)[1].strip())

start = time.monotonic()
os.kill(proc.pid, signal.SIGTERM)
# Wait ONLY for outer; do not reap orphaned grandchild so it sits as a zombie.
_, status = os.waitpid(proc.pid, 0)
elapsed_ms = int((time.monotonic() - start) * 1000)
sig = os.WTERMSIG(status) if os.WIFSIGNALED(status) else 0
code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -1
print(f"outer_sig: {sig}")
print(f"outer_code: {code}")
print(f"elapsed_ms: {elapsed_ms}")
"#;
    let output = Command::new("python3")
        .args([
            "-c",
            driver,
            exe.to_str().unwrap(),
            "--exact",
            "session::tests::outer_role",
            "--nocapture",
        ])
        .output()
        .expect("python3 subreaper driver failed to start");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "subreaper driver failed ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    let fields = parse_kv(&stdout);
    let sig: i32 = fields
        .get("outer_sig")
        .expect("missing outer_sig")
        .parse()
        .unwrap();
    let elapsed_ms: u64 = fields
        .get("elapsed_ms")
        .expect("missing elapsed_ms")
        .parse()
        .unwrap();
    assert_eq!(
        sig, SIGTERM,
        "outer under subreaper must die by SIGTERM; got:\n{stdout}"
    );
    assert!(
        elapsed_ms < 5000,
        "zombie group member held outer for {elapsed_ms} ms under non-reaping subreaper; got:\n{stdout}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stat_is_live_group_member_ignores_zombies_and_handles_parens_in_comm() {
    let cases = [
        ("42 (sleep) S 1 42 42 0 -1", 42, true),
        ("43 (my (weird) proc) R 42 42 42 0 -1", 42, true),
        ("43 (sleep) Z 1 42 42 0 -1", 42, false),
        ("43 (sleep) X 1 42 42 0 -1", 42, false),
        ("44 (other) S 1 99 99 0 -1", 42, false),
    ];
    let results: Vec<_> = cases
        .into_iter()
        .map(|(stat, pgrp, expected)| (stat, stat_is_live_group_member(stat, pgrp), expected))
        .collect();
    let mismatches: Vec<_> = results
        .into_iter()
        .filter(|(_, actual, expected)| actual != expected)
        .collect();
    assert!(
        mismatches.is_empty(),
        "stat parser mismatches: {mismatches:?}"
    );
}

#[test]
fn parse_sig_ign_and_is_signal_ignored_match_proc_status_bits() {
    let status = "Name:\txtask\nSigIgn:\t0000000000000003\nSigCgt:\t0000000000000000\n";
    let mask = parse_sig_ign(status).expect("SigIgn line must parse");
    assert_eq!(mask, 0x3);
    assert!(is_signal_ignored(mask, SIGHUP), "bit 0 must match SIGHUP");
    assert!(is_signal_ignored(mask, SIGINT), "bit 1 must match SIGINT");
    assert!(
        !is_signal_ignored(mask, SIGTERM),
        "bit 14 must not be set in 0x3"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn ignored_signal_is_not_caught_or_forwarded_and_inner_inherits_ignore() {
    let exe = std::env::current_exe().unwrap();
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "trap '' HUP; exec \"$@\"",
            "_",
            exe.to_str().unwrap(),
            "--exact",
            "session::tests::outer_role",
            "--nocapture",
        ])
        .env(ROLE_ENV, "outer")
        .env(MODE_ENV, "sleep-group");
    let (mut guard, inner_pid, grandchild_pid) = spawn_session_command(command);
    let outer_pid = pid_from_u32(guard.outer.id());

    // Send the ignored signal to both outer and inner: neither may die or forward it.
    kill_process(outer_pid, Signal::HUP).expect("sending ignored SIGHUP to outer failed");
    kill_process(pid_from_u32(inner_pid), Signal::HUP)
        .expect("sending ignored SIGHUP to inner failed");
    thread::sleep(Duration::from_millis(300));
    assert!(
        guard.outer.try_wait().unwrap().is_none(),
        "outer must survive an inherited ignored SIGHUP"
    );
    assert!(
        alive(inner_pid),
        "inner must inherit ignored SIGHUP and stay alive"
    );
    assert!(
        alive(grandchild_pid),
        "grandchild must stay alive after ignored SIGHUP"
    );

    // A non-ignored signal still forwards and ends the group cleanly.
    kill_process(outer_pid, Signal::TERM).expect("sending SIGTERM to outer failed");
    let status = wait_child_bounded(&mut guard.outer, WAIT_TIMEOUT)
        .expect("outer did not exit after non-ignored SIGTERM");
    assert_eq!(
        status.signal(),
        Some(SIGTERM),
        "outer must die by non-ignored SIGTERM; got {status:?}"
    );
    let survivors = wait_all_dead(&[inner_pid, grandchild_pid], WAIT_TIMEOUT);
    assert!(
        survivors.is_empty(),
        "non-ignored SIGTERM left inner group alive: {survivors:?}"
    );
}

#[test]
fn forward_signal_retries_group_after_pid_fallback() {
    let trace = std::cell::RefCell::new(Vec::new());
    let reaped = AtomicBool::new(false);
    let mut group_calls = 0;
    forward_signal_with(
        &reaped,
        || {
            group_calls += 1;
            trace.borrow_mut().push("group");
            if group_calls == 1 {
                Err(rustix::io::Errno::SRCH)
            } else {
                Ok(())
            }
        },
        || trace.borrow_mut().push("pid"),
    );
    assert_eq!(
        *trace.borrow(),
        vec!["group", "pid", "group"],
        "ESRCH fallback must signal pid and retry the group once"
    );

    // Normal path (group already exists): exactly one group signal, no pid signal.
    let normal_trace = std::cell::RefCell::new(Vec::new());
    forward_signal_with(
        &reaped,
        || {
            normal_trace.borrow_mut().push("group");
            Ok(())
        },
        || normal_trace.borrow_mut().push("pid"),
    );
    assert_eq!(*normal_trace.borrow(), vec!["group"]);

    // Already reaped: never fall back to signalling a potentially recycled pid.
    let reaped_trace = std::cell::RefCell::new(Vec::new());
    reaped.store(true, Ordering::Release);
    forward_signal_with(
        &reaped,
        || {
            reaped_trace.borrow_mut().push("group");
            Err(rustix::io::Errno::SRCH)
        },
        || reaped_trace.borrow_mut().push("pid"),
    );
    assert_eq!(*reaped_trace.borrow(), vec!["group"]);
}

#[test]
fn sigkill_to_outer_ends_inner_group_within_seconds() {
    let (mut guard, inner_pid, grandchild_pid) = spawn_sleeping_session();
    guard.outer.kill().unwrap();
    let _ = guard.outer.wait();

    let survivors = wait_all_dead(&[inner_pid, grandchild_pid], WAIT_TIMEOUT);
    assert!(
        survivors.is_empty(),
        "SIGKILL to outer left inner group alive after {WAIT_TIMEOUT:?}: {survivors:?}"
    );
}
