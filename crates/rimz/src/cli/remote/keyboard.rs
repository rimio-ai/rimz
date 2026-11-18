//! Keyboard relay for a supervised attach: a pty pair in front of the attach
//! ssh, with rimz reading the real terminal and writing the pty master.
//!
//! ssh takes its raw mode, window size, and remote PTY modes from the pty
//! slave, which starts with the shell's saved termios and the terminal's size.
//! The real terminal's raw state follows the slave's: the relay makes the
//! terminal raw and forwards only while ssh holds the slave raw, and gives the
//! terminal back in the shell's modes whenever ssh leaves raw mode, so an
//! authentication prompt ssh writes through `/dev/tty` reads the terminal as
//! it does without the relay. Anything written to the slave reaches stdout.
//! The relay stops reading the terminal in the pass in which the master hangs
//! up, so input typed after the session ends stays queued for whoever reads
//! the terminal next. A failed read or write of the master is that hangup; any
//! other error stops the relay with the error, which the supervisor reads as
//! the attach's end while ssh still runs.

use std::fs::File;
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::sys::select::{FdSet, select};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{self, LocalFlags, SetArg, Termios};
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::Pid;
use rimz::remote::paste::{Input, InputScanner};

/// How often a cooked slave is checked for ssh's raw flip.
const COOKED_RECHECK: Duration = Duration::from_millis(10);
/// How often a raw slave is checked for ssh leaving raw mode.
const RAW_RECHECK: Duration = Duration::from_millis(100);

/// The pty pair one attach runs on, before its ssh child spawns.
pub(super) struct KeyboardPty {
    slave: OwnedFd,
    relay: KeyboardRelay,
    start: mpsc::Sender<Pid>,
}

impl KeyboardPty {
    /// Opens the pair with the shell's modes and the terminal's size, and
    /// starts the relay thread, which waits for the child's pid.
    pub(super) fn open(saved: &Termios) -> io::Result<Self> {
        let size = rustix::termios::tcgetwinsize(io::stdin())?;
        let size = nix::pty::Winsize {
            ws_row: size.ws_row,
            ws_col: size.ws_col,
            ws_xpixel: size.ws_xpixel,
            ws_ypixel: size.ws_ypixel,
        };
        let pty = nix::pty::openpty(&size, saved)?;
        // openpty cannot set close-on-exec atomically; a child another thread
        // spawns in this window holds the slave, and the session's end is then
        // learned from the supervisor's stop instead of the hangup.
        for fd in [&pty.master, &pty.slave] {
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        }
        let (start, started) = mpsc::channel();
        let relay = KeyboardRelay::start(File::from(pty.master), saved.clone(), started)?;
        Ok(Self {
            slave: pty.slave,
            relay,
            start,
        })
    }

    /// The slave for the child's stdin; the caller drops every copy it keeps
    /// once the child has spawned, since an open slave hides the hangup.
    pub(super) fn slave(&self) -> io::Result<OwnedFd> {
        self.slave.try_clone()
    }

    /// Hands the spawned child to the relay.
    pub(super) fn spawned(self, child: u32) -> KeyboardRelay {
        let pid = i32::try_from(child).map_or(Pid::from_raw(0), Pid::from_raw);
        let _ = self.start.send(pid);
        self.relay
    }

    /// Ends the relay of a child that never spawned.
    pub(super) fn abandon(self) {
        drop(self.start);
        let _ = self.relay.finish();
    }
}

/// The running relay thread of one attach.
pub(super) struct KeyboardRelay {
    stop: PipeWriter,
    worker: JoinHandle<io::Result<()>>,
}

impl KeyboardRelay {
    fn start(master: File, saved: Termios, started: mpsc::Receiver<Pid>) -> io::Result<Self> {
        let (stopped, stop) = io::pipe()?;
        let (winched, winch) = io::pipe()?;
        let worker = thread::Builder::new()
            .name("remote-keyboard".to_owned())
            .spawn(move || {
                let Ok(child) = started.recv() else {
                    return Ok(());
                };
                let id = signal_hook::low_level::pipe::register(
                    signal_hook::consts::signal::SIGWINCH,
                    winch,
                )?;
                let mut relay = Relay {
                    master,
                    child,
                    saved,
                    raw: false,
                    hung_up: false,
                    scanner: InputScanner::default(),
                    hold_until: None,
                    results: Vec::new(),
                };
                let result = relay.run(&stopped, &winched);
                signal_hook::low_level::unregister(id);
                relay.leave_raw();
                result
            })?;
        Ok(Self { stop, worker })
    }

    /// Whether the relay has stopped, on the hangup or an error.
    pub(super) fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    /// Stops the relay if the hangup has not already, and waits for it, so
    /// nothing reads the terminal once this returns.
    pub(super) fn finish(mut self) -> io::Result<()> {
        let _ = self.stop.write_all(b"x");
        self.worker
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("keyboard relay panicked")))
    }
}

struct Relay {
    master: File,
    child: Pid,
    saved: Termios,
    raw: bool,
    /// A write to the master failed: the session has ended.
    hung_up: bool,
    scanner: InputScanner,
    hold_until: Option<Instant>,
    /// Paste results waiting for the user's open bracketed paste to close.
    results: Vec<u8>,
}

/// What one scanned input becomes.
enum Verdict {
    /// Bytes for the session, unchanged.
    Forward(Vec<u8>),
    /// A trigger held back while the upload it starts runs.
    #[expect(dead_code, reason = "the Ctrl+V and drop triggers start uploads")]
    Upload(Input),
}

impl Relay {
    fn run(&mut self, stopped: &PipeReader, winched: &PipeReader) -> io::Result<()> {
        let stdin = io::stdin();
        let mut buffer = [0_u8; 4096];
        loop {
            if self.hung_up {
                return Ok(());
            }
            let raw = slave_is_raw(&self.master)?;
            if raw != self.raw {
                if raw {
                    self.enter_raw()?;
                } else {
                    self.leave_raw();
                }
            }
            let ready = self.wait(stdin.as_fd(), stopped.as_fd(), winched.as_fd())?;
            if ready.master {
                // What the session writes to its terminal rather than its
                // stdout (a local client such as `tmux attach` does) still
                // reaches the screen.
                match (&self.master).read(&mut buffer) {
                    Ok(0) | Err(_) => return Ok(()),
                    Ok(read) => {
                        let mut stdout = io::stdout().lock();
                        stdout.write_all(&buffer[..read])?;
                        stdout.flush()?;
                    }
                }
            }
            if ready.stopped {
                return Ok(());
            }
            if ready.winched {
                let _ = (&*winched).read(&mut buffer)?;
                if self.raw {
                    self.resize()?;
                }
            }
            // ssh can leave raw while `select` waits; a key typed then is
            // left for its `/dev/tty` prompt.
            if ready.stdin && !slave_is_raw(&self.master)? {
                self.leave_raw();
            } else if ready.stdin {
                // Read the descriptor itself: `Stdin`'s buffer would keep
                // bytes `select` can no longer see.
                let read = match nix::unistd::read(stdin.as_fd(), &mut buffer) {
                    Ok(0) => return Ok(()),
                    Ok(read) => read,
                    Err(Errno::EINTR) => continue,
                    Err(err) => return Err(err.into()),
                };
                self.feed(&buffer[..read]);
            } else if self.hold_until.is_some_and(|until| Instant::now() >= until) {
                let mut inputs = Vec::new();
                self.scanner.expire(&mut inputs);
                self.hold_until = None;
                self.dispatch(inputs);
            }
        }
    }

    fn wait(
        &self,
        stdin: BorrowedFd<'_>,
        stopped: BorrowedFd<'_>,
        winched: BorrowedFd<'_>,
    ) -> io::Result<Ready> {
        let recheck = if self.raw {
            RAW_RECHECK
        } else {
            COOKED_RECHECK
        };
        let timeout = self.hold_until.map_or(recheck, |until| {
            until.saturating_duration_since(Instant::now()).min(recheck)
        });
        let mut fds = FdSet::new();
        fds.insert(self.master.as_fd());
        fds.insert(stopped);
        fds.insert(winched);
        // The terminal is read only while ssh holds the slave raw; otherwise
        // its input is left for ssh's own prompts through `/dev/tty`.
        if self.raw {
            fds.insert(stdin);
        }
        let mut timeout = TimeVal::microseconds(timeout.as_micros().try_into().unwrap_or(i64::MAX));
        match select(None, &mut fds, None, None, &mut timeout) {
            Ok(_) => Ok(Ready {
                master: fds.contains(self.master.as_fd()),
                stopped: fds.contains(stopped),
                winched: fds.contains(winched),
                stdin: self.raw && fds.contains(stdin),
            }),
            Err(Errno::EINTR) => Ok(Ready::default()),
            Err(err) => Err(err.into()),
        }
    }

    /// Scans one read of typed input and writes it to the session.
    fn feed(&mut self, mut bytes: &[u8]) {
        // A result held for an open paste goes out right after the byte that
        // closes it, ahead of what the same read typed next.
        while !self.results.is_empty()
            && self.scanner.in_paste()
            && let Some((first, rest)) = bytes.split_first()
        {
            self.scan(std::slice::from_ref(first));
            bytes = rest;
        }
        self.scan(bytes);
        self.hold_until = self
            .scanner
            .hold_deadline()
            .map(|hold| Instant::now() + hold);
    }

    fn scan(&mut self, bytes: &[u8]) {
        let mut inputs = Vec::new();
        self.scanner.feed(bytes, &mut inputs);
        self.dispatch(inputs);
    }

    /// Writes typed input to the session, each input as its verdict says.
    fn dispatch(&mut self, inputs: Vec<Input>) {
        for input in inputs {
            match verdict(input) {
                Verdict::Forward(bytes) => self.forward(&bytes),
                Verdict::Upload(_) => {}
            }
        }
        self.flush_results();
    }

    /// Writes an upload's paste result where it cannot land inside a
    /// bracketed paste the user is still sending: at once outside one, else
    /// as soon as it closes.
    #[cfg_attr(not(test), expect(dead_code, reason = "uploads write their results"))]
    fn write_result(&mut self, bytes: &[u8]) {
        self.results.extend_from_slice(bytes);
        self.flush_results();
    }

    fn flush_results(&mut self) {
        if self.results.is_empty() || self.scanner.in_paste() {
            return;
        }
        let results = std::mem::take(&mut self.results);
        self.forward(&results);
    }

    fn forward(&mut self, bytes: &[u8]) {
        if !self.hung_up && self.master.write_all(bytes).is_err() {
            self.hung_up = true;
        }
    }

    fn enter_raw(&mut self) -> io::Result<()> {
        let mut raw = self.saved.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(io::stdin(), SetArg::TCSANOW, &raw)?;
        self.raw = true;
        self.resize()
    }

    fn leave_raw(&mut self) {
        if !std::mem::take(&mut self.raw) {
            return;
        }
        if let Err(err) = termios::tcsetattr(io::stdin(), SetArg::TCSANOW, &self.saved) {
            tracing::debug!(error = %err, "local tty restore after relay failed");
        }
    }

    /// Copies the terminal's size to the slave and tells ssh, which learns of
    /// a new size only from SIGWINCH.
    fn resize(&self) -> io::Result<()> {
        let size = rustix::termios::tcgetwinsize(io::stdin())?;
        rustix::termios::tcsetwinsize(&self.master, size)?;
        if self.child.as_raw() > 0 {
            let _ = kill(self.child, Signal::SIGWINCH);
        }
        Ok(())
    }
}

#[derive(Default)]
struct Ready {
    master: bool,
    stopped: bool,
    winched: bool,
    stdin: bool,
}

/// Every input forwards unchanged until the upload triggers act on Ctrl+V
/// and dropped paths.
fn verdict(input: Input) -> Verdict {
    Verdict::Forward(input.bytes().to_vec())
}

fn slave_is_raw(master: &File) -> io::Result<bool> {
    Ok(!termios::tcgetattr(master)?
        .local_flags
        .contains(LocalFlags::ICANON))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_result_waits_for_an_open_bracketed_paste_to_close() {
        let pty = nix::pty::openpty(None, None).expect("open pty");
        let saved = termios::tcgetattr(&pty.slave).expect("read pty modes");
        let (mut session, master) = io::pipe().expect("pipe");
        let mut relay = Relay {
            master: File::from(OwnedFd::from(master)),
            child: Pid::from_raw(0),
            saved,
            raw: false,
            hung_up: false,
            scanner: InputScanner::default(),
            hold_until: None,
            results: Vec::new(),
        };
        let mut inputs = Vec::new();
        relay.scanner.feed(b"\x1b[200~text", &mut inputs);
        relay.dispatch(inputs);
        relay.write_result(b"/r/a.png");
        relay.feed(b" more\x1b[201~\r");
        relay.write_result(b" /r/b.png");
        drop(relay);

        let mut written = Vec::new();
        session.read_to_end(&mut written).expect("read the session");
        assert_eq!(
            String::from_utf8_lossy(&written),
            "\x1b[200~text more\x1b[201~/r/a.png\r /r/b.png"
        );
    }
}
