//! Thin supervisor for the pane-resident sidebar renderer.
//!
//! The worker owns the TUI and its in-process panic diagnostics. The
//! supervisor exists only to observe deaths Rust hooks cannot catch: aborts and
//! fatal signals that otherwise leave the pane with no durable evidence.

use std::env;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::diag::record::DiagEvent;
use crate::ids::SidebarInstanceId;
use crate::sidebar_pane::app::ServeConfig;
use crate::tui::{MouseCapture, restore_terminal};

const WORKER_ENV: &str = "RIMZ_SIDEBAR_WORKER";
const INSTANCE_ENV: &str = "RIMZ_SIDEBAR_INSTANCE_ID";
#[cfg(feature = "testkit")]
const TEST_FAULT_ENV: &str = "RIMZ_TEST_SIDEBAR_WORKER_FAULT";
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const PANIC_EXIT_CODE: i32 = 101;

#[derive(Debug, thiserror::Error)]
pub enum SidebarSuperviseErr {
    #[error("spawning sidebar render worker `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("waiting for sidebar render worker: {0}")]
    Wait(#[source] io::Error),
    #[error(
        "sidebar render worker terminated abnormally (signal {signal:?}, exit code {exit_code:?})"
    )]
    WorkerTerminated {
        signal: Option<i32>,
        exit_code: Option<i32>,
    },
}

pub type Result<T> = std::result::Result<T, SidebarSuperviseErr>;

pub fn is_worker() -> bool {
    env::var_os(WORKER_ENV).is_some()
}

pub fn instance_id() -> SidebarInstanceId {
    env::var(INSTANCE_ENV)
        .ok()
        .and_then(|raw| SidebarInstanceId::parse(&raw).ok())
        .unwrap_or_default()
}

pub fn run_worker(config: ServeConfig) -> crate::sidebar_pane::app::Result<()> {
    inject_test_fault_if_requested();
    crate::sidebar_pane::app::serve(config)
}

pub fn run(config: ServeConfig) -> Result<()> {
    let exe = crate::proc::rimz_exe();
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let mut child = Command::new(&exe)
        .args(args)
        .env(WORKER_ENV, "1")
        .env(INSTANCE_ENV, config.instance_id.as_str())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| SidebarSuperviseErr::Spawn {
            program: render_program(&exe),
            source,
        })?;

    let stderr_tail = Arc::new(Mutex::new(StderrTail::new(STDERR_TAIL_BYTES)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| drain_stderr(stderr, stderr_tail.clone()));
    let status = child.wait().map_err(SidebarSuperviseErr::Wait)?;
    if let Some(handle) = stderr_handle {
        let _ = handle.join();
    }

    if status.success() {
        return Ok(());
    }
    if status.code() == Some(PANIC_EXIT_CODE) {
        std::process::exit(PANIC_EXIT_CODE);
    }

    let signal = termination_signal(&status);
    let exit_code = status.code();
    let stderr_excerpt = stderr_tail
        .lock()
        .map(|tail| tail.excerpt())
        .unwrap_or_default();
    restore_terminal(MouseCapture::Stdout);
    record_signal_death(&config, signal, exit_code, stderr_excerpt);
    Err(SidebarSuperviseErr::WorkerTerminated { signal, exit_code })
}

fn record_signal_death(
    config: &ServeConfig,
    signal: Option<i32>,
    exit_code: Option<i32>,
    stderr_excerpt: String,
) {
    let diag = crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    );
    diag.emit(DiagEvent::RendererSignalDeath {
        signal,
        exit_code,
        stderr_excerpt: stderr_excerpt.clone(),
    });
    report_sentry_signal_death(signal, exit_code, &stderr_excerpt);
}

#[cfg(feature = "sentry")]
fn report_sentry_signal_death(signal: Option<i32>, exit_code: Option<i32>, stderr_excerpt: &str) {
    tracing::error!(
        target: "rimz::sidebar::crash",
        {
            tags.operation = "sidebar.render_crash",
            signal = signal.unwrap_or(0),
            exit_code = exit_code.unwrap_or(0),
            stderr = %stderr_excerpt,
        },
        "sidebar render worker terminated abnormally",
    );
}

#[cfg(not(feature = "sentry"))]
fn report_sentry_signal_death(
    _signal: Option<i32>,
    _exit_code: Option<i32>,
    _stderr_excerpt: &str,
) {
}

fn drain_stderr<R>(mut stderr: R, tail: Arc<Mutex<StderrTail>>) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = [0_u8; 1024];
        loop {
            let n = match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &buf[..n];
            let _ = io::stderr().write_all(chunk);
            if let Ok(mut tail) = tail.lock() {
                tail.push(chunk);
            }
        }
    })
}

#[derive(Debug)]
struct StderrTail {
    bytes: Vec<u8>,
    cap: usize,
}

impl StderrTail {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= self.cap {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len().saturating_sub(self.cap)..]);
            return;
        }
        let overflow = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.cap);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn excerpt(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

#[cfg(unix)]
fn termination_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;

    status.signal()
}

#[cfg(not(unix))]
fn termination_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn render_program(exe: &std::path::Path) -> String {
    exe.to_string_lossy().into_owned()
}

#[cfg(feature = "testkit")]
fn inject_test_fault_if_requested() {
    let Some(fault) = env::var_os(TEST_FAULT_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    if fault.to_string_lossy() == "abort" {
        let _ = io::stderr().write_all(b"rimz test sidebar worker abort\n");
        std::process::abort();
    }
}

#[cfg(not(feature = "testkit"))]
fn inject_test_fault_if_requested() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_bounded_suffix() {
        let mut tail = StderrTail::new(5);

        tail.push(b"abc");
        tail.push(b"def");

        assert_eq!(tail.excerpt(), "bcdef");
    }

    #[test]
    fn stderr_tail_truncates_large_chunk_to_suffix() {
        let mut tail = StderrTail::new(4);

        tail.push(b"abcdef");

        assert_eq!(tail.excerpt(), "cdef");
    }
}
