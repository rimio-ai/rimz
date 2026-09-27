//! Server process ownership and bounded stderr capture.

use super::{Result, registry::CrashCause};
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const TAIL_BYTES: usize = 8 * 1024;

#[derive(Default)]
struct StderrTail {
    bytes: Vec<u8>,
    cut: bool,
}

impl StderrTail {
    fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > TAIL_BYTES {
            let excess = self.bytes.len() - TAIL_BYTES;
            self.cut = self.bytes[excess - 1] != b'\n';
            self.bytes.drain(..excess);
        }
    }

    fn text(&self) -> String {
        let start = if self.cut {
            self.bytes
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(self.bytes.len(), |index| index + 1)
        } else {
            0
        };
        let text = String::from_utf8_lossy(&self.bytes[start..]);
        if text.len() <= TAIL_BYTES {
            return text.into_owned();
        }
        // Lossy replacement can expand invalid bytes beyond the byte budget.
        let excess = text.len() - TAIL_BYTES;
        let start = text.as_bytes()[excess..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(text.len(), |index| excess + index + 1);
        text[start..].to_owned()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_complete_lines_within_eight_kib() {
        let mut tail = StderrTail::default();
        tail.append(b"first\n");
        assert_eq!(tail.text(), "first\n");
        for _ in 0..2000 {
            tail.append(b"discarded\n");
        }
        tail.append(b"last line\n");
        let text = tail.text();
        assert!(text.len() <= 8192);
        assert!(!text.contains("first"));
        assert!(text.starts_with("discarded\n"));
        assert!(text.ends_with("last line\n"));
        tail.append(&vec![b'x'; 9000]);
        tail.append(b"end of oversized line\nkept\n");
        assert_eq!(tail.text(), "kept\n");
        tail.append(b"invalid: \xff\n");
        assert_eq!(tail.text(), "kept\ninvalid: \u{fffd}\n");
    }
}
