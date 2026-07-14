//! Loop runner domain: shell checks, run locks, and budget-window gates.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{
    WindowSurplus, longest_window_reset_at, longest_window_running, longest_window_surplus,
    shortest_window_running,
};
use crate::config::{CheckOn, TaskEntry};
use crate::harness::schedule::run_log::{CheckRecord, LoopRunResult};
use crate::ids::WorkspaceId;
use crate::store::paths::{RuntimePaths, runtime_home};
use crate::workspace::WorkspaceResolver;

pub const CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CHECK_OUTPUT_CAP: usize = 16 * 1024;
const TASK_TIMEOUT_UNITS: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];

pub struct RunLockGuard {
    file: File,
}

impl Drop for RunLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLockInfo {
    pub pid: u32,
    pub started_at: Timestamp,
}

pub enum RunLockAttempt {
    Acquired(RunLockGuard),
    Held(Option<RunLockInfo>),
}

pub fn acquire_run_lock(name: &str, entry: &TaskEntry) -> Result<RunLockAttempt> {
    let runtime =
        RuntimePaths::for_workspace(WorkspaceId::from_project_root(&entry.resolved_root()))
            .context("locating loop task runtime")?;
    std::fs::create_dir_all(&runtime.root)
        .with_context(|| format!("creating loop task runtime `{}`", runtime.root.display()))?;
    let path = runtime.root.join(format!("loop-run-{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening loop run lock `{}`", path.display()))?;
    acquire_run_lock_file(file, &path)
}

fn acquire_run_lock_file(mut file: File, path: &Path) -> Result<RunLockAttempt> {
    match file.try_lock() {
        Ok(()) => {
            let info = RunLockInfo {
                pid: std::process::id(),
                started_at: Timestamp::now(),
            };
            // Runtime scratch needs no fsync and stays on the locked fd:
            // renaming an atomic replacement would detach the advisory lock.
            file.set_len(0)
                .with_context(|| format!("truncating loop run lock `{}`", path.display()))?;
            file.rewind()
                .with_context(|| format!("rewinding loop run lock `{}`", path.display()))?;
            serde_json::to_writer(&mut file, &info)
                .with_context(|| format!("writing loop run lock `{}`", path.display()))?;
            file.flush()
                .with_context(|| format!("flushing loop run lock `{}`", path.display()))?;
            Ok(RunLockAttempt::Acquired(RunLockGuard { file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut payload = Vec::new();
            let info = file
                .read_to_end(&mut payload)
                .ok()
                .and_then(|_| serde_json::from_slice(&payload).ok());
            Ok(RunLockAttempt::Held(info))
        }
        Err(err) => Err(std::io::Error::from(err))
            .with_context(|| format!("locking loop run lock `{}`", path.display())),
    }
}

pub struct CheckOutcome {
    passed: bool,
    timed_out: bool,
    output: String,
    code: Option<i32>,
}

impl CheckOutcome {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

pub enum CheckEcho {
    Capture,
    Stream { prefix: String },
}

pub fn check_record(outcome: &CheckOutcome) -> CheckRecord {
    CheckRecord {
        code: outcome.code,
        timed_out: outcome.timed_out,
        output: outcome.output.clone(),
    }
}

pub fn deadline_expired(entry: &TaskEntry) -> bool {
    entry
        .deadline
        .is_some_and(|deadline| Timestamp::now() >= deadline)
}

pub fn check_timeout(entry: &TaskEntry) -> Result<Option<Duration>> {
    entry
        .timeout
        .as_deref()
        .map(|raw| super::parse_duration_units(raw, TASK_TIMEOUT_UNITS))
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

pub fn check_only_result(outcome: &CheckOutcome) -> LoopRunResult {
    if outcome.timed_out {
        LoopRunResult::TimedOut
    } else if outcome.passed {
        LoopRunResult::Completed
    } else {
        LoopRunResult::Failed
    }
}

pub fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed,
        CheckOn::Success => outcome.passed,
    }
}

pub fn augment_prompt(base: String, cmd: &str, outcome: &CheckOutcome) -> String {
    let status = if outcome.timed_out {
        "timeout".to_owned()
    } else {
        outcome
            .code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_owned())
    };
    format!(
        "{base}\n\n--- check `{cmd}` exited {status} ---\n{}",
        outcome.output
    )
}

pub fn run_check(
    dir: &Path,
    cmd: &str,
    timeout: Duration,
    echo: CheckEcho,
) -> Result<CheckOutcome> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running loop check `{cmd}` in {}", dir.display()))?;
    let prefix = match echo {
        CheckEcho::Capture => None,
        CheckEcho::Stream { prefix } => Some(prefix),
    };
    let stdout = drain_pipe(
        child.stdout.take(),
        prefix
            .clone()
            .map(|prefix| PipeForward::new(PipeDestination::Stdout, prefix)),
    );
    let stderr = drain_pipe(
        child.stderr.take(),
        prefix.map(|prefix| PipeForward::new(PipeDestination::Stderr, prefix)),
    );
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            break (status, false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping timed-out loop check `{cmd}`"))?;
            break (status, true);
        }
        std::thread::sleep(CHECK_POLL_INTERVAL);
    };
    let mut output = stdout.join().unwrap_or_default();
    output.extend(stderr.join().unwrap_or_default());
    let output = tail_output(&output, CHECK_OUTPUT_CAP);
    Ok(CheckOutcome {
        passed: status.success() && !timed_out,
        timed_out,
        output,
        code: status.code(),
    })
}

#[derive(Clone, Copy)]
enum PipeDestination {
    Stdout,
    Stderr,
}

struct PipeForward {
    destination: PipeDestination,
    prefix: Vec<u8>,
    pending: Vec<u8>,
}

impl PipeForward {
    fn new(destination: PipeDestination, prefix: String) -> Self {
        Self {
            destination,
            prefix: prefix.into_bytes(),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(line) = take_complete_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn finish(mut self) {
        if let Some(line) = take_trailing_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
        let mut painted = Vec::with_capacity(self.prefix.len() + line.len());
        painted.extend_from_slice(&self.prefix);
        painted.extend_from_slice(line);
        match self.destination {
            PipeDestination::Stdout => {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(&painted)?;
                out.flush()
            }
            PipeDestination::Stderr => {
                let mut err = anstream::AutoStream::auto(std::io::stderr().lock());
                err.write_all(&painted)?;
                err.flush()
            }
        }
    }
}

fn take_complete_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = pending.iter().position(|byte| *byte == b'\n')?;
    Some(pending.drain(..=end).collect())
}

fn take_trailing_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    if pending.is_empty() {
        return None;
    }
    let mut line = std::mem::take(pending);
    line.push(b'\n');
    Some(line)
}

fn drain_pipe(
    pipe: Option<impl Read + Send + 'static>,
    mut forward: Option<PipeForward>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8 * 1024];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                buf.extend_from_slice(bytes);
                if let Some(forward) = &mut forward {
                    forward.push(bytes);
                }
            }
        }
        if let Some(forward) = forward {
            forward.finish();
        }
        buf
    })
}

pub fn tail_output(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Whether `entry`'s provider already has a budget window counting down, read
/// from the shared account-scoped cache. The window state is account-scoped, so
/// the entry's workspace is resolved only to reach this user's runtime root.
pub fn window_already_running(entry: &TaskEntry, kind: &str) -> Result<bool> {
    let runtime = entry_runtime(entry)?;
    Ok(shortest_window_running(&runtime, kind, Timestamp::now()) == Some(true))
}

/// Whether `entry`'s provider already has its longest budget window counting
/// down, read from the shared account-scoped cache.
pub fn reset_window_already_running(entry: &TaskEntry, kind: &str) -> Result<bool> {
    let runtime = entry_runtime(entry)?;
    Ok(longest_window_running(&runtime, kind, Timestamp::now()) == Some(true))
}

/// Decide whether a task's provider-window surplus gate keeps this fire closed.
pub fn surplus_gate(entry: &TaskEntry, kind: &str, now: Timestamp) -> Result<Option<String>> {
    if entry.surplus.is_none() && entry.surplus_after.is_none() {
        return Ok(None);
    }
    let runtime = entry_runtime(entry)?;
    Ok(surplus_gate_in(
        entry,
        kind,
        longest_window_surplus(&runtime, kind, now),
    ))
}

fn surplus_gate_in(
    entry: &TaskEntry,
    kind: &str,
    reading: Option<WindowSurplus>,
) -> Option<String> {
    if entry.surplus.is_none() && entry.surplus_after.is_none() {
        return None;
    }
    let Some(reading) = reading else {
        return Some(format!(
            "no {kind} budget-window reading; surplus gate stays closed"
        ));
    };
    let after = match entry
        .surplus_after
        .as_deref()
        .map(super::parse_surplus_after)
    {
        Some(Ok(after)) => Some(after),
        Some(Err(_)) => {
            return Some("invalid surplus-after gate; surplus gate stays closed".to_owned());
        }
        None => None,
    };
    if let Some(after) = after
        && (reading.elapsed.as_secs().max(0) as u64) < after.as_secs()
    {
        return Some(format!(
            "{kind} {} window {} elapsed; fires after {}",
            window_label(reading.duration_mins),
            elapsed_label(reading.elapsed),
            entry.surplus_after.as_deref().unwrap_or_default().trim(),
        ));
    }
    let threshold = match entry.surplus.as_deref().map(super::parse_surplus) {
        Some(Ok(threshold)) => threshold,
        Some(Err(_)) => return Some("invalid surplus gate; surplus gate stays closed".to_owned()),
        None => 1.0,
    };
    (reading.headroom < threshold).then(|| {
        format!(
            "{kind} {} window surplus {:.1}x below {threshold:.1}x",
            window_label(reading.duration_mins),
            reading.headroom,
        )
    })
}

fn window_label(duration_mins: u32) -> String {
    if duration_mins.is_multiple_of(24 * 60) {
        format!("{}d", duration_mins / (24 * 60))
    } else if duration_mins.is_multiple_of(60) {
        format!("{}h", duration_mins / 60)
    } else {
        format!("{duration_mins}m")
    }
}

fn elapsed_label(elapsed: jiff::SignedDuration) -> String {
    let total_mins = elapsed.as_secs().max(0) / 60;
    let days = total_mins / (24 * 60);
    let hours = total_mins % (24 * 60) / 60;
    let mins = total_mins % 60;
    if days > 0 {
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours}h{mins}m")
        } else {
            format!("{hours}h")
        }
    } else {
        format!("{mins}m")
    }
}

/// Raw reset stamp for `entry`'s provider longest budget window.
pub fn window_reset_at(entry: &TaskEntry, kind: &str) -> Result<Option<Timestamp>> {
    let runtime = entry_runtime(entry)?;
    Ok(longest_window_reset_at(&runtime, kind))
}

fn entry_runtime(entry: &TaskEntry) -> Result<RuntimePaths> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    RuntimePaths::under(workspace.workspace_id, &runtime_home()).context("locating runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("task.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open lock");

        let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
            RunLockAttempt::Acquired(guard) => guard,
            RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
        };
        let written: RunLockInfo =
            serde_json::from_slice(&std::fs::read(&path).expect("read lock"))
                .expect("parse lock info");
        assert_eq!(written.pid, std::process::id());

        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open contender");
        match acquire_run_lock_file(contender, &path).expect("contend for lock") {
            RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
            RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
            RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
        }
        drop(guard);

        let empty_path = dir.path().join("legacy.lock");
        let empty = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&empty_path)
            .expect("open empty lock");
        empty.try_lock().expect("hold empty lock");
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&empty_path)
            .expect("open empty contender");
        assert!(matches!(
            acquire_run_lock_file(contender, &empty_path).expect("contend for empty lock"),
            RunLockAttempt::Held(None)
        ));
    }

    fn surplus_entry(surplus: Option<&str>, surplus_after: Option<&str>) -> TaskEntry {
        TaskEntry {
            surplus: surplus.map(ToOwned::to_owned),
            surplus_after: surplus_after.map(ToOwned::to_owned),
            ..TaskEntry::default()
        }
    }

    fn reading(elapsed_days: i64, headroom: f64) -> WindowSurplus {
        WindowSurplus {
            duration_mins: 7 * 24 * 60,
            elapsed: jiff::SignedDuration::from_secs(elapsed_days * 86_400),
            headroom,
        }
    }

    #[test]
    fn surplus_gate_covers_closed_elapsed_headroom_and_open_branches() {
        assert_eq!(surplus_gate_in(&TaskEntry::default(), "claude", None), None);
        assert_eq!(
            surplus_gate_in(&surplus_entry(Some("1.5x"), None), "claude", None).as_deref(),
            Some("no claude budget-window reading; surplus gate stays closed")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(2, 2.0)),
            )
            .as_deref(),
            Some("claude 7d window 2d elapsed; fires after 3d")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(4, 1.4)),
            )
            .as_deref(),
            Some("claude 7d window surplus 1.4x below 1.5x")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(4, 1.5)),
            ),
            None
        );
    }

    #[test]
    fn surplus_after_alone_implies_sustainable_headroom() {
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(None, Some("3d")),
                "codex",
                Some(reading(4, 0.9)),
            )
            .as_deref(),
            Some("codex 7d window surplus 0.9x below 1.0x")
        );
    }

    #[test]
    fn check_polarity_truth_table() {
        let passed = CheckOutcome {
            passed: true,
            timed_out: false,
            output: String::new(),
            code: Some(0),
        };
        let failed = CheckOutcome {
            passed: false,
            timed_out: false,
            output: String::new(),
            code: Some(1),
        };
        let timed_out = CheckOutcome {
            passed: false,
            timed_out: true,
            output: String::new(),
            code: None,
        };

        assert!(!polarity_fires(Some(CheckOn::Fail), &passed));
        assert!(polarity_fires(Some(CheckOn::Fail), &failed));
        assert!(polarity_fires(Some(CheckOn::Fail), &timed_out));
        assert!(polarity_fires(Some(CheckOn::Success), &passed));
        assert!(!polarity_fires(Some(CheckOn::Success), &failed));
        assert!(!polarity_fires(Some(CheckOn::Success), &timed_out));
    }

    #[test]
    fn run_check_captures_output_and_status() {
        let dir = tempfile::tempdir().expect("tempdir");

        let passed = run_check(
            dir.path(),
            "printf out; printf err >&2",
            Duration::from_secs(1),
            CheckEcho::Capture,
        )
        .expect("passed check");
        assert!(passed.passed);
        assert_eq!(passed.code, Some(0));
        assert!(passed.output.contains("out"));
        assert!(passed.output.contains("err"));

        let failed = run_check(
            dir.path(),
            "printf nope; exit 1",
            Duration::from_secs(1),
            CheckEcho::Capture,
        )
        .expect("failed check");
        assert!(!failed.passed);
        assert!(!failed.timed_out);
        assert_eq!(failed.code, Some(1));
        assert!(failed.output.contains("nope"));
    }

    #[test]
    fn run_check_honours_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");

        let outcome = run_check(
            dir.path(),
            "sleep 1",
            Duration::from_millis(50),
            CheckEcho::Capture,
        )
        .expect("timed-out check");

        assert!(!outcome.passed);
        assert!(outcome.timed_out);
    }

    #[test]
    fn pipe_forward_buffers_partial_lines_and_terminates_the_tail() {
        let mut pending = b"first".to_vec();
        assert_eq!(take_complete_line(&mut pending), None);

        pending.extend_from_slice(b" line\nsecond");
        assert_eq!(
            take_complete_line(&mut pending),
            Some(b"first line\n".to_vec())
        );
        assert_eq!(pending, b"second");
        assert_eq!(take_trailing_line(&mut pending), Some(b"second\n".to_vec()));
        assert!(pending.is_empty());
    }
}
