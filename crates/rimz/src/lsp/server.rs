//! Server process ownership and bounded stderr capture.

use super::{Result, registry::CrashCause};
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::child_process::StderrTail;

pub(crate) struct Server {
    pub(crate) child: Child,
    tail: Arc<Mutex<StderrTail>>,
    drained: Option<mpsc::Receiver<()>>,
}

pub(crate) fn spawn(root: &Path, config: &crate::config::LspServerConfig) -> Result<Server> {
    let mut child = Command::new(&config.command[0])
        .args(&config.command[1..])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    // Stderr was piped above and has not yet been taken.
    let mut stderr = child.stderr.take().expect("piped stderr");
    let tail = Arc::new(Mutex::new(StderrTail::default()));
    let capture = tail.clone();
    let (done, drained) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        loop {
            match stderr.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => capture
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .append(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = done.send(());
    });
    Ok(Server {
        child,
        tail,
        drained: Some(drained),
    })
}

impl Server {
    /// Read a spontaneous exit before killing, including the EOF-before-exit race.
    pub(crate) fn crash(&mut self, error: Option<String>) -> CrashCause {
        let deadline = Instant::now() + Duration::from_millis(100);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break None,
            }
        };
        self.stop();
        CrashCause {
            at_ms: crate::utils::time::unix_now_ms(),
            exit_code: status.and_then(|status| status.code()),
            signal: status.and_then(|status| status.signal()),
            stderr_tail: self.tail.lock().unwrap_or_else(|e| e.into_inner()).text(),
            error,
        }
    }

    fn stop(&mut self) {
        let Some(drained) = self.drained.take() else {
            return;
        };
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = self.child.wait();
        let _ = drained.recv_timeout(Duration::from_millis(100));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}
