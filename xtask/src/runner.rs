use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::deadline;

/// How often a waiting task checks its child and its budget.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long a terminated child gets to reap its own children before the kill.
/// `cargo` and `nextest` both tear down their spawned processes on `SIGTERM`.
const TERMINATE_GRACE: Duration = Duration::from_secs(5);

pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) output: String,
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
    })?;
    let stdout = stdout_worker
        .join()
        .map_err(|_| anyhow::anyhow!("command stdout reader panicked"))?
        .context("reading command stdout")?;
    let stderr_output = stderr_worker
        .join()
        .map_err(|_| anyhow::anyhow!("command stderr reader panicked"))?
        .context("reading command stderr")?;

    let mut combined = String::from_utf8_lossy(&stdout).into_owned();
    combined.push_str(&stderr_output);
    Ok(Captured {
        status,
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
            bail!(
                "{overrun}: terminated `{program} {}`\n{}",
                rendered_args(args),
                overrun.next_step(),
            );
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
    let mut command = if crate::rtk::wrap_cargo(program, args) {
        let mut command = Command::new("rtk");
        command.arg(program);
        command
    } else {
        Command::new(program)
    };
    command
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(root)
        .envs(envs.iter().map(|(key, value)| (*key, value)));
    if crate::sccache::should_wrap(program, args) {
        command.env("RUSTC_WRAPPER", "sccache");
        command.env("CARGO_INCREMENTAL", "0");
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    command
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
