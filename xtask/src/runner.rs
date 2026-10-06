use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::deadline;

/// How often a waiting task checks its child and its budget.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long a terminated child gets to reap its own children before the kill.
/// `cargo` and `nextest` both tear down their spawned processes on `SIGTERM`.
const TERMINATE_GRACE: Duration = Duration::from_secs(5);
// A descendant may keep a pipe open after the child has been terminated.
const CAPTURE_GRACE: Duration = Duration::from_millis(250);

pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) output: String,
}

#[derive(Debug)]
pub(crate) struct CaptureTimeout {
    pub(crate) summary: String,
    pub(crate) next_step: String,
    pub(crate) output: String,
}

impl std::fmt::Display for CaptureTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n{}", self.summary, self.next_step)
    }
}

impl std::error::Error for CaptureTimeout {}

struct CaptureReader<R> {
    reader: R,
    output: Arc<Mutex<Vec<u8>>>,
}

impl<R: Read> Read for CaptureReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.reader.read(bytes)?;
        self.output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }
}

pub(crate) fn run<I, S>(root: &Path, program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with_env(root, program, args, &[])
}

pub(crate) fn run_with_env<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with_env_and_removed(root, program, args, envs, &[])
}

pub(crate) fn run_with_env_and_removed<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let mut child = build_command(root, program, &args, envs, removed_envs)
        .spawn()
        .with_context(|| format!("running `{program}`"))?;
    let status = wait_bounded(&mut child, program, &args, &mut || {})?;
    ensure_success(program, &args, status)
}

/// Run `program` on the operator's own stdio: nothing is captured, so the child
/// writes straight to the terminal. Returns the exit status rather than failing
/// on it, leaving the caller to classify what a non-zero code means.
pub(crate) fn run_inherited<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
) -> Result<ExitStatus>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let mut child = build_command(root, program, &args, envs, removed_envs)
        .spawn()
        .with_context(|| format!("running `{program}`"))?;
    wait_bounded(&mut child, program, &args, &mut || {})
}

pub(crate) fn run_streamed<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
    on_line: &mut dyn FnMut(&str),
) -> Result<Captured>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let mut child = build_command(root, program, &args, envs, removed_envs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running `{program}`"))?;
    let stdout = child.stdout.take().context("capturing command stdout")?;
    let stderr = child.stderr.take().context("capturing command stderr")?;
    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
    let stdout = CaptureReader {
        reader: stdout,
        output: Arc::clone(&stdout_bytes),
    };
    let stderr = CaptureReader {
        reader: stderr,
        output: Arc::clone(&stderr_bytes),
    };
    let stdout_worker = thread::spawn(move || {
        let mut output = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut output).map(|_| output)
    });
    // Stderr streams on its own thread and hands lines back over a channel, so
    // the waiting thread stays free to watch the child and its budget.
    let (lines_tx, lines_rx) = mpsc::channel();
    let stderr_worker = thread::spawn(move || {
        capture_lines_lossy(BufReader::new(stderr), &mut |line| {
            let _ = lines_tx.send(line.to_owned());
        })
    });

    let status = wait_bounded(&mut child, program, &args, &mut || {
        while let Ok(line) = lines_rx.try_recv() {
            on_line(&line);
        }
    });
    if let Err(mut error) = status {
        if let Some(timeout) = error.downcast_mut::<CaptureTimeout>() {
            let until = Instant::now() + CAPTURE_GRACE;
            while !(stdout_worker.is_finished() && stderr_worker.is_finished())
                && Instant::now() < until
            {
                thread::sleep(POLL_INTERVAL);
            }
            for bytes in [&stdout_bytes, &stderr_bytes] {
                let bytes = bytes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                timeout.output.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        return Err(error);
    }
    let status = status?;
    let stdout = stdout_worker
        .join()
        .map_err(|_| anyhow::anyhow!("command stdout reader panicked"))?
        .context("reading command stdout")?;
    let stderr_output = stderr_worker
        .join()
        .map_err(|_| anyhow::anyhow!("command stderr reader panicked"))?
        .context("reading command stderr")?;

    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let mut combined = stdout.clone();
    combined.push_str(&stderr_output);
    Ok(Captured {
        status,
        stdout,
        output: combined,
    })
}

/// Wait for `child`, terminating it once the run spends its wall-clock budget.
/// `on_tick` runs between polls so a streaming caller keeps draining output.
fn wait_bounded<S: AsRef<OsStr>>(
    child: &mut Child,
    program: &str,
    args: &[S],
    on_tick: &mut dyn FnMut(),
) -> Result<ExitStatus> {
    loop {
        on_tick();
        if let Some(status) = child.try_wait().context("waiting for command")? {
            return Ok(status);
        }
        if let Some(overrun) = deadline::overrun() {
            terminate(child);
            return Err(CaptureTimeout {
                summary: format!("{overrun}: terminated `{program} {}`", rendered_args(args)),
                next_step: overrun.next_step(),
                output: String::new(),
            }
            .into());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Ask the child to stop, then insist. `SIGTERM` first gives `cargo` and
/// `nextest` their own chance to tear down compiles and test processes; the
/// kill covers a child that ignores it.
fn terminate(child: &mut Child) {
    signal_child(child.id(), "-TERM");
    let deadline = Instant::now() + TERMINATE_GRACE;
    while Instant::now() < deadline {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn signal_child(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args([signal, "--", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn capture_lines_lossy(
    mut reader: impl BufRead,
    on_line: &mut dyn FnMut(&str),
) -> std::io::Result<String> {
    let mut output = String::new();
    let mut bytes = Vec::new();
    while reader.read_until(b'\n', &mut bytes)? != 0 {
        let line = String::from_utf8_lossy(&bytes);
        on_line(line.trim_end_matches(['\r', '\n']));
        output.push_str(&line);
        bytes.clear();
    }
    Ok(output)
}

fn build_command<S: AsRef<OsStr>>(
    root: &Path,
    program: &str,
    args: &[S],
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
) -> Command {
    let mut command = Command::new(program);
    command
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(root)
        .envs(envs.iter().map(|(key, value)| (*key, value)));
    if crate::sccache::should_wrap(program, args) {
        command.env("RUSTC_WRAPPER", "sccache");
    }
    if program == "cargo" {
        for key in env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| is_cargo_run_package_key(key))
        {
            command.env_remove(key);
        }
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    command
}

/// True for the keys `cargo run` exports to describe the package it launched:
/// here always xtask's own. A nested `cargo` must not inherit them. Cargo
/// compares a build script's `rerun-if-env-changed` keys against its own
/// environment, and `ring` tracks `CARGO_MANIFEST_DIR` and `CARGO_PKG_*`, so a
/// build unit shared by `cargo xtask` itself and the nested build would re-run
/// that script, and rebuild everything above it, on every alternation.
fn is_cargo_run_package_key(key: &OsStr) -> bool {
    key.to_str().is_some_and(|key| {
        key == "CARGO_MANIFEST_DIR" || key == "CARGO_MANIFEST_PATH" || key.starts_with("CARGO_PKG_")
    })
}

pub(crate) fn ensure_success<S: AsRef<OsStr>>(
    program: &str,
    args: &[S],
    status: ExitStatus,
) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    bail!("command failed: {program} {}", rendered_args(args));
}

fn rendered_args<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|arg| arg.as_ref().to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn workspace_root() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("reading current directory")?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() && manifest_declares_workspace(&manifest)? {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("could not find workspace root from current directory");
        }
    }
}

fn manifest_declares_workspace(manifest: &Path) -> Result<bool> {
    let raw =
        fs::read_to_string(manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let parsed = toml::from_str::<toml::Value>(&raw)
        .with_context(|| format!("parsing {}", manifest.display()))?;
    Ok(parsed.get("workspace").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_out_capture_keeps_both_streams() {
        assert_timeout_capture(
            "printf 'early stdout\\n'; printf 'early stderr' >&2; exec sleep 30",
        );
    }

    #[test]
    fn timed_out_capture_does_not_wait_for_descendant_pipes() {
        assert_timeout_capture(
            "printf 'early stdout\\n'; printf 'early stderr' >&2; sleep 30 & echo $!; wait",
        );
    }

    fn assert_timeout_capture(script: &str) {
        deadline::arm_with("test", Some(Duration::from_millis(300)));
        let started = Instant::now();
        let err = run_streamed(Path::new("."), "sh", ["-c", script], &[], &[], &mut |_| {})
            .err()
            .expect("must time out");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "reader wait was unbounded"
        );
        let timeout = err.downcast_ref::<CaptureTimeout>().expect("typed timeout");
        // Reap the pipe holder rather than leave it alive after the test.
        if let Some(pid) = timeout
            .output
            .lines()
            .find_map(|line| line.parse::<u32>().ok())
        {
            signal_child(pid, "-KILL");
        }
        assert!(
            timeout.output.contains("early stdout"),
            "{}",
            timeout.output
        );
        assert!(
            timeout.output.contains("early stderr"),
            "{}",
            timeout.output
        );
        assert!(
            timeout
                .summary
                .starts_with("xtask `test` exceeded its 300ms budget after")
        );
        assert!(
            timeout
                .summary
                .ends_with(&format!("terminated `sh -c {script}`"))
        );
        assert_eq!(
            timeout.next_step,
            "NEXT: rerun the slow step on its own, or widen the budget for one run with RIMZ_XTASK_TIMEOUT=900ms"
        );
    }

    // The budget arms once per process; nextest runs each test in its own, so
    // this test owns the armed budget for the whole process.
    #[test]
    fn a_spent_budget_terminates_the_child_and_names_the_next_step() {
        crate::deadline::arm_with("gate", Some(Duration::from_millis(200)));
        let started = Instant::now();

        let err = run(Path::new("."), "sleep", ["120"])
            .unwrap_err()
            .to_string();

        assert!(err.contains("exceeded its 200ms budget"), "{err}");
        assert!(err.contains("terminated `sleep 120`"), "{err}");
        assert!(err.contains("RIMZ_XTASK_TIMEOUT="), "{err}");
        assert!(
            started.elapsed() < TERMINATE_GRACE,
            "child outlived its budget by {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn nested_cargo_drops_the_package_keys_cargo_run_exported() {
        for key in [
            "CARGO_MANIFEST_DIR",
            "CARGO_MANIFEST_PATH",
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION_PRE",
        ] {
            assert!(is_cargo_run_package_key(OsStr::new(key)), "{key}");
        }
        // Build inputs and the toolchain pin survive.
        for key in [
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_PROFILE_DEV_DEBUG",
            "CARGO_INCREMENTAL",
        ] {
            assert!(!is_cargo_run_package_key(OsStr::new(key)), "{key}");
        }

        // nextest exports the same keys to each test, as `cargo run` does to xtask.
        assert!(env::var_os("CARGO_MANIFEST_DIR").is_some());
        let cargo = build_command(Path::new("."), "cargo", &["--version"], &[], &[]);
        let removed: Vec<_> = cargo
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_owned())
            .collect();
        assert!(removed.iter().any(|key| key == "CARGO_MANIFEST_DIR"));
        assert!(removed.iter().any(|key| key == "CARGO_PKG_NAME"));

        let other = build_command(Path::new("."), "git", &["status"], &[], &[]);
        assert_eq!(other.get_envs().count(), 0);
    }

    #[test]
    fn streamed_capture_preserves_lines_with_lossy_utf8() {
        let mut lines = Vec::new();
        let output = capture_lines_lossy(
            std::io::Cursor::new(b"first\xff line\r\nsecond line"),
            &mut |line| lines.push(line.to_owned()),
        )
        .unwrap();

        assert_eq!(lines, ["first� line", "second line"]);
        assert_eq!(output, "first� line\r\nsecond line");
    }
}
