//! TTY-gated stderr spinner adapted from `crates/rimz/src/cli/spinner.rs`.
//!
//! The small implementation is duplicated deliberately so xtask stays free of
//! a dependency on the runtime crate.

use std::io::{self, IsTerminal, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_TICK: Duration = Duration::from_millis(80);
const HEARTBEAT_TICK: Duration = Duration::from_secs(1);
const HEARTBEAT_FIRST: Duration = Duration::from_secs(15);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const ENV_NO_PROGRESS: &str = "RIMZ_NO_PROGRESS";
const ENV_AGENT_KIND: &str = "RIMZ_AGENT_KIND";

pub(crate) struct Spinner {
    inner: Option<SpinnerInner>,
}

struct SpinnerInner {
    label: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    mode: ProgressMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressMode {
    Animated,
    Heartbeat,
    Silent,
}

struct HeartbeatPolicy {
    next_due: Duration,
}

impl HeartbeatPolicy {
    fn new() -> Self {
        Self {
            next_due: HEARTBEAT_FIRST,
        }
    }

    fn due(&mut self, elapsed: Duration) -> bool {
        if elapsed < self.next_due {
            return false;
        }
        while self.next_due <= elapsed {
            self.next_due += HEARTBEAT_INTERVAL;
        }
        true
    }
}

impl Spinner {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        let mode = progress_mode(
            std::io::stderr().is_terminal(),
            std::env::var(ENV_NO_PROGRESS).ok().as_deref(),
            std::env::var(ENV_AGENT_KIND).ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        );
        if mode == ProgressMode::Silent {
            return Self { inner: None };
        }

        let label = Arc::new(Mutex::new(label.into()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_label = Arc::clone(&label);
        let worker_stop = Arc::clone(&stop);
        let started = Instant::now();
        let worker = thread::spawn(move || {
            let mut frame = 0;
            let mut heartbeat = HeartbeatPolicy::new();
            while !worker_stop.load(Ordering::Relaxed) {
                let elapsed = started.elapsed();
                match mode {
                    ProgressMode::Animated => {
                        if let Ok(label) = worker_label.lock() {
                            let elapsed = format_elapsed(elapsed);
                            let mut stderr = std::io::stderr().lock();
                            let _ = write!(
                                stderr,
                                "\r{} {} ({elapsed})\x1b[K",
                                SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
                                *label
                            );
                            let _ = stderr.flush();
                            frame += 1;
                        }
                    }
                    ProgressMode::Heartbeat if heartbeat.due(elapsed) => {
                        if let Ok(label) = worker_label.lock() {
                            let elapsed = format_elapsed(elapsed);
                            let mut stderr = std::io::stderr().lock();
                            let _ = writeln!(stderr, "xtask: {} still running ({elapsed})", *label);
                            let _ = stderr.flush();
                        }
                    }
                    ProgressMode::Heartbeat | ProgressMode::Silent => {}
                }
                thread::park_timeout(match mode {
                    ProgressMode::Animated => SPINNER_TICK,
                    ProgressMode::Heartbeat | ProgressMode::Silent => HEARTBEAT_TICK,
                });
            }
        });

        Self {
            inner: Some(SpinnerInner {
                label,
                stop,
                worker: Some(worker),
                mode,
            }),
        }
    }

    pub(crate) fn set(&self, label: impl Into<String>) {
        let Some(inner) = &self.inner else {
            return;
        };
        if let Ok(mut current) = inner.label.lock() {
            *current = label.into();
        }
    }

    fn clear() -> io::Result<()> {
        let mut stderr = std::io::stderr().lock();
        write!(stderr, "\r\x1b[K")?;
        stderr.flush()?;
        Ok(())
    }
}

fn progress_mode(
    stderr_is_terminal: bool,
    no_progress: Option<&str>,
    agent_kind: Option<&str>,
    term: Option<&str>,
) -> ProgressMode {
    if matches!(no_progress, Some("1") | Some("true")) {
        ProgressMode::Silent
    } else if stderr_is_terminal && animation_allowed(agent_kind, term) {
        ProgressMode::Animated
    } else {
        ProgressMode::Heartbeat
    }
}

fn animation_allowed(agent_kind: Option<&str>, term: Option<&str>) -> bool {
    agent_kind.is_none_or(str::is_empty) && term != Some("dumb")
}

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        let Some(inner) = &mut self.inner else {
            return;
        };
        inner.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = inner.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        if inner.mode == ProgressMode::Animated {
            let _ = Self::clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_mode_distinguishes_animation_heartbeat_and_silence() {
        assert_eq!(
            progress_mode(true, None, None, Some("xterm-256color")),
            ProgressMode::Animated
        );
        assert_eq!(
            progress_mode(false, None, None, Some("xterm-256color")),
            ProgressMode::Heartbeat
        );
        assert_eq!(
            progress_mode(true, None, Some("codex"), Some("xterm-256color")),
            ProgressMode::Heartbeat
        );
        assert_eq!(
            progress_mode(true, None, None, Some("dumb")),
            ProgressMode::Heartbeat
        );
        assert_eq!(
            progress_mode(true, Some("1"), None, None),
            ProgressMode::Silent
        );
        assert_eq!(
            progress_mode(false, Some("true"), Some("codex"), Some("dumb")),
            ProgressMode::Silent
        );
        assert_eq!(
            progress_mode(true, Some("0"), Some(""), None),
            ProgressMode::Animated
        );
    }

    #[test]
    fn heartbeat_starts_at_fifteen_seconds_then_repeats_every_thirty() {
        let mut heartbeat = HeartbeatPolicy::new();
        assert!(!heartbeat.due(Duration::from_secs(14)));
        assert!(heartbeat.due(Duration::from_secs(15)));
        assert!(!heartbeat.due(Duration::from_secs(44)));
        assert!(heartbeat.due(Duration::from_secs(45)));
        assert!(!heartbeat.due(Duration::from_secs(74)));
        assert!(heartbeat.due(Duration::from_secs(75)));
    }

    #[test]
    fn heartbeat_coalesces_missed_intervals() {
        let mut heartbeat = HeartbeatPolicy::new();
        assert!(heartbeat.due(Duration::from_secs(80)));
        assert!(!heartbeat.due(Duration::from_secs(81)));
        assert!(heartbeat.due(Duration::from_secs(105)));
    }

    #[test]
    fn elapsed_time_uses_compact_second_and_minute_labels() {
        assert_eq!(format_elapsed(Duration::ZERO), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m05s");
    }
}
