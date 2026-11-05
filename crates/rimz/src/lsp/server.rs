//! Server process ownership and bounded stderr capture.

use super::{Result, registry::CrashCause};
use std::io::{self, PipeReader, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::child_process::StderrTail;

// A default 64 KiB pipe's worth. A writer outside the killed group cannot
// hold the crash read past this bound.
const DRAIN_READS: usize = 8;

pub(crate) struct Server {
    pub(crate) child: Child,
    capture: Arc<StderrCapture>,
    group: Option<Pid>,
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
    let stderr = child.stderr.take().expect("piped stderr");
    let capture = Arc::new(StderrCapture {
        pipe: OwnedFd::from(stderr).into(),
        tail: Mutex::new(StderrTail::default()),
    });
    let group = Pid::from_raw(child.id() as i32);
    let server = Server {
        child,
        capture: capture.clone(),
        group: Some(group),
    };
    fcntl(&capture.pipe, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).map_err(io::Error::from)?;
    std::thread::spawn(move || capture.pump());
    Ok(server)
}

impl Server {
    /// Reap the server, retaining spontaneous status and draining stderr without an EOF wait.
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
        let reaped = self.stop();
        let status = crash_status(status, reaped);
        CrashCause {
            at_ms: crate::utils::time::unix_now_ms(),
            exit_code: status.and_then(|status| status.code()),
            signal: status.and_then(|status| status.signal()),
            stderr_tail: self.capture.drained_text(),
            error,
        }
    }

    fn stop(&mut self) -> Option<ExitStatus> {
        let group = self.group.take()?;
        let _ = killpg(group, Signal::SIGKILL);
        self.child.wait().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct StderrCapture {
    pipe: PipeReader,
    tail: Mutex<StderrTail>,
}

impl StderrCapture {
    fn pump(&self) {
        let mut fds = [PollFd::new(self.pipe.as_fd(), PollFlags::POLLIN)];
        loop {
            match poll(&mut fds, PollTimeout::NONE) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue,
                Err(_) => break,
            }
            let mut tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
            match self.read_pipe(&mut tail) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => break,
            }
        }
    }

    fn drained_text(&self) -> String {
        let mut tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..DRAIN_READS {
            match self.read_pipe(&mut tail) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        tail.text()
    }

    fn read_pipe(&self, tail: &mut MutexGuard<'_, StderrTail>) -> io::Result<usize> {
        let mut bytes = [0; 8192];
        let count = (&self.pipe).read(&mut bytes)?;
        tail.append(&bytes[..count]);
        Ok(count)
    }
}

fn crash_status(polled: Option<ExitStatus>, reaped: Option<ExitStatus>) -> Option<ExitStatus> {
    polled.or_else(|| reaped.filter(|status| status.signal() != Some(Signal::SIGKILL as i32)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const INCIDENT: &str = concat!(
        "error: Unknown binary 'rust-analyzer' in official toolchain ",
        "'1.98.1-x86_64-unknown-linux-gnu'.\n"
    );

    #[test]
    fn crash_drain_reads_stderr_without_the_pump_or_eof() {
        let (pipe, mut writer) = std::io::pipe().unwrap();
        fcntl(&pipe, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        let capture = StderrCapture {
            pipe,
            tail: Mutex::new(StderrTail::default()),
        };
        capture.tail.lock().unwrap().append(b"before\n");
        writer.write_all(INCIDENT.as_bytes()).unwrap();
        assert_eq!(capture.drained_text(), format!("before\n{INCIDENT}"));
        drop(writer);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn crash_preserves_stderr_and_exit_while_an_escaped_child_holds_the_pipe() {
        struct EscapedChild {
            pid: u32,
            token: String,
        }
        impl Drop for EscapedChild {
            fn drop(&mut self) {
                if crate::proc::process_is_live(self.pid, Some(&self.token)) {
                    let pid = Pid::from_raw(self.pid as i32);
                    let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
                }
            }
        }

        let root = tempfile::tempdir().unwrap();
        let script = "setsid sh -c 'echo $$ > \"$1\"; exec sleep 347' sh \"$1\" >/dev/null &\n\
             while [ ! -s \"$1\" ]; do sleep 0.01; done\n\
             printf '%s' \"$2\" >&2\n\
             exit 1";
        let config = serde_json::from_value(serde_json::json!({
            "command": ["sh", "-c", script, "sh", "stderr-holder", INCIDENT],
            "extensions": ["rs"], "root-markers": []
        }))
        .unwrap();
        let mut server = spawn(root.path(), &config).unwrap();
        let mut stdout = server.child.stdout.take().unwrap();
        stdout.read_to_end(&mut Vec::new()).unwrap();
        let pid = std::fs::read_to_string(root.path().join("stderr-holder"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let escaped = EscapedChild {
            pid,
            token: crate::proc::process_start_token(pid).unwrap(),
        };
        let cause = server.crash(None);
        let held_open = crate::proc::process_is_live(pid, Some(&escaped.token));
        drop(escaped);
        assert!(held_open, "crash waited for the escaped stderr holder");
        assert_eq!(cause.exit_code, Some(1));
        assert_eq!(cause.signal, None);
        assert_eq!(cause.stderr_tail, INCIDENT);
    }

    #[test]
    fn crash_status_prefers_the_poll_then_the_reap_except_sigkill() {
        let exit = ExitStatus::from_raw(1 << 8);
        let term = ExitStatus::from_raw(Signal::SIGTERM as i32);
        let kill = ExitStatus::from_raw(Signal::SIGKILL as i32);
        for (polled, reaped, expected) in [
            (Some(exit), None, Some(exit)),
            (Some(exit), Some(kill), Some(exit)),
            (Some(kill), Some(exit), Some(kill)),
            (None, Some(exit), Some(exit)),
            (None, Some(term), Some(term)),
            (None, Some(kill), None),
            (None, None, None),
        ] {
            assert_eq!(
                crash_status(polled, reaped),
                expected,
                "polled {polled:?}, reaped {reaped:?}"
            );
        }
    }
}
