//! TTY-gated stderr spinner for long-running human commands.

use std::io::{self, IsTerminal, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(crate) const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
pub(crate) const SPINNER_TICK: Duration = Duration::from_millis(80);

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
        if !std::io::stderr().is_terminal() {
            return Self { inner: None };
        }

        let label = Arc::new(Mutex::new(label.into()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_label = Arc::clone(&label);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut frame = 0;
            while !worker_stop.load(Ordering::Relaxed) {
                let label = worker_label
                    .lock()
                    .map(|label| label.clone())
                    .unwrap_or_default();
                let mut stderr = std::io::stderr().lock();
                let _ = write!(
                    stderr,
                    "\r{} {}\x1b[K",
                    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
                    label
                );
                let _ = stderr.flush();
                frame += 1;
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
