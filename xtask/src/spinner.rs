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
const ENV_NO_PROGRESS: &str = "RIMZ_NO_PROGRESS";
const ENV_AGENT_KIND: &str = "RIMZ_AGENT_KIND";

pub(crate) struct Spinner {
    inner: Option<SpinnerInner>,
}

struct SpinnerInner {
    label: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Spinner {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        if !animation_enabled() {
            return Self { inner: None };
        }

        let label = Arc::new(Mutex::new(label.into()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_label = Arc::clone(&label);
        let worker_stop = Arc::clone(&stop);
        let started = Instant::now();
        let worker = thread::spawn(move || {
            let mut frame = 0;
            while !worker_stop.load(Ordering::Relaxed) {
                if let Ok(label) = worker_label.lock() {
                    let elapsed = format_elapsed(started.elapsed());
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
                thread::sleep(SPINNER_TICK);
            }
        });

        Self {
            inner: Some(SpinnerInner {
                label,
                stop,
                worker: Some(worker),
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

fn animation_enabled() -> bool {
    std::io::stderr().is_terminal()
        && animation_allowed(
            std::env::var(ENV_NO_PROGRESS).ok().as_deref(),
            std::env::var(ENV_AGENT_KIND).ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
}

fn animation_allowed(
    no_progress: Option<&str>,
    agent_kind: Option<&str>,
    term: Option<&str>,
) -> bool {
    !matches!(no_progress, Some("1") | Some("true"))
        && agent_kind.is_none_or(str::is_empty)
        && term != Some("dumb")
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
            let _ = worker.join();
        }
        let _ = Self::clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_requires_an_interactive_human_environment() {
        assert!(animation_allowed(None, None, None));
        assert!(!animation_allowed(Some("1"), None, None));
        assert!(!animation_allowed(Some("true"), None, None));
        assert!(animation_allowed(Some("0"), None, None));
        assert!(!animation_allowed(None, Some("codex"), None));
        assert!(animation_allowed(None, Some(""), None));
        assert!(!animation_allowed(None, None, Some("dumb")));
        assert!(animation_allowed(None, None, Some("xterm-256color")));
    }

    #[test]
    fn elapsed_time_uses_compact_second_and_minute_labels() {
        assert_eq!(format_elapsed(Duration::ZERO), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m05s");
    }
}
