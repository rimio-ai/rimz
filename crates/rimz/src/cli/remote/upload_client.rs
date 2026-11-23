//! The laptop side of a remote paste: the keyboard relay's Ctrl+V and drop
//! triggers, and the upload client that sends their files to the host's
//! `rimz remote upload` receiver over the attach's control socket.
//!
//! A trigger reads the clipboard or the dropped paths, opens every file, and
//! waits for the receiver's ready line before it holds the key back: a local
//! problem, or a receiver that does not answer, forwards the key as typed. The
//! bytes then stream on a thread, which reports the remote paths back to the
//! relay through a channel and a wake pipe. A Ctrl+V while a transfer runs
//! cancels it, and so does the relay's end.

use std::fs::File;
use std::io::{self, BufRead, BufReader, PipeReader, PipeWriter, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rimz::remote::RemoteTarget;
use rimz::remote::paste::{drop_paste, parse_drop};
use rimz::remote::upload::{UploadFile, UploadHeader, UploadReply, upload_stream_spec};

use super::clipboard::{self, Clipboard};

/// A receiver that has not sent its ready line by now is not usable.
const READY_WAIT: Duration = Duration::from_secs(5);
/// A Ctrl+V this soon after its transfer became ready is the key repeating.
const REPEAT_WINDOW: Duration = Duration::from_secs(1);
/// An empty clipboard answers every Ctrl+V for this long without a new read.
const NOTHING_REUSE: Duration = Duration::from_millis(500);

/// Where one attach's uploads go.
#[derive(Clone)]
pub(super) struct UploadRoute {
    pub(super) target: RemoteTarget,
    pub(super) control_path: PathBuf,
    pub(super) limit: u64,
}

/// The upload triggers of one attach.
pub(super) struct Uploads {
    route: UploadRoute,
    running: Option<Transfer>,
    nothing_until: Option<Instant>,
    serial: u64,
    finished: mpsc::Sender<Finished>,
    results: mpsc::Receiver<Finished>,
    wake: PipeReader,
    woken: PipeWriter,
    /// A private directory for the clipboard reader's PNG, made on first use.
    scratch: Option<PathBuf>,
}

struct Transfer {
    serial: u64,
    child: Child,
    ready_at: Instant,
}

struct Finished {
    serial: u64,
    paths: Option<Vec<String>>,
}

struct Outgoing {
    name: String,
    file: File,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Ready,
    /// The receiver answered with an error: nothing is pasted.
    Refused,
    /// No usable receiver: the trigger forwards as typed.
    Unusable,
}

impl Uploads {
    pub(super) fn new(route: UploadRoute) -> io::Result<Self> {
        let (wake, woken) = io::pipe()?;
        let (finished, results) = mpsc::channel();
        Ok(Self {
            route,
            running: None,
            nothing_until: None,
            serial: 0,
            finished,
            results,
            wake,
            woken,
            scratch: None,
        })
    }

    /// Readable once a transfer has finished.
    pub(super) fn wake(&self) -> BorrowedFd<'_> {
        self.wake.as_fd()
    }

    /// Acts on a Ctrl+V; false leaves the key to forward.
    pub(super) fn ctrl_v(&mut self) -> bool {
        if let Some(running) = &self.running {
            if running.ready_at.elapsed() >= REPEAT_WINDOW {
                self.cancel();
            }
            return true;
        }
        if self
            .nothing_until
            .is_some_and(|until| Instant::now() < until)
        {
            return false;
        }
        let Ok(scratch) = self.scratch() else {
            return false;
        };
        let outgoing = match clipboard::read(&scratch) {
            Clipboard::Nothing => {
                self.nothing_until = Some(Instant::now() + NOTHING_REUSE);
                return false;
            }
            Clipboard::Files(paths) => open_all(&paths),
            Clipboard::Image(png) => {
                let name = jiff::ZonedDateTime::now()
                    .strftime("clipboard-%Y%m%d-%H%M%S.png")
                    .to_string();
                let opened = open(&png, name);
                let _ = std::fs::remove_file(&png);
                opened.map(|file| vec![file])
            }
        };
        outgoing.is_some_and(|outgoing| self.start(outgoing))
    }

    /// Acts on a held paste; false leaves it to forward.
    pub(super) fn paste(&mut self, payload: &[u8]) -> bool {
        if self.running.is_some() {
            return false;
        }
        parse_drop(payload, |path| usable(path).is_some())
            .and_then(|paths| open_all(&paths))
            .is_some_and(|outgoing| self.start(outgoing))
    }

    /// The paste for a transfer that finished with its remote paths.
    pub(super) fn finished(&mut self) -> Option<Vec<u8>> {
        let mut drained = [0_u8; 64];
        let _ = (&self.wake).read(&mut drained);
        let mut paste = None;
        for done in self.results.try_iter() {
            let Some(transfer) = self
                .running
                .take_if(|running| running.serial == done.serial)
            else {
                continue;
            };
            reap(transfer.child, false);
            paste = done.paths.map(|paths| drop_paste(&paths));
        }
        paste
    }

    fn start(&mut self, outgoing: Vec<Outgoing>) -> bool {
        let spawned = upload_stream_spec(&self.route.target, &self.route.control_path)
            .to_command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The fallback attach inherits the terminal's stderr; the upload
            // channel's diagnostics never reach it.
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            return false;
        };
        let (Some(stdin), Some(stdout), Ok(woken)) = (
            child.stdin.take(),
            child.stdout.take(),
            self.woken.try_clone(),
        ) else {
            reap(child, true);
            return false;
        };
        self.serial += 1;
        let serial = self.serial;
        let header = UploadHeader::new(
            self.route.limit,
            outgoing
                .iter()
                .map(|file| UploadFile {
                    name: file.name.clone(),
                    size: file.size,
                })
                .collect(),
        );
        let (ready, readied) = mpsc::channel();
        let finished = self.finished.clone();
        let streamed = thread::Builder::new()
            .name("remote-upload".to_owned())
            .spawn(move || {
                let paths = transfer(stdin, stdout, &header, outgoing, &ready);
                let _ = finished.send(Finished { serial, paths });
                let _ = (&woken).write_all(b"x");
            });
        if streamed.is_err() {
            reap(child, true);
            return false;
        }
        match readied.recv_timeout(READY_WAIT) {
            Ok(Answer::Ready) => {
                self.running = Some(Transfer {
                    serial,
                    child,
                    ready_at: Instant::now(),
                });
                true
            }
            Ok(Answer::Refused) => {
                // The receiver holds its failure on the sidebar for a while
                // before it exits.
                reap(child, false);
                true
            }
            Ok(Answer::Unusable) | Err(_) => {
                reap(child, true);
                false
            }
        }
    }

    /// Kills the running transfer, which the receiver reads as a cancel.
    fn cancel(&mut self) {
        if let Some(transfer) = self.running.take() {
            reap(transfer.child, true);
        }
    }

    fn scratch(&mut self) -> io::Result<PathBuf> {
        if let Some(dir) = &self.scratch {
            return Ok(dir.clone());
        }
        let dir = std::env::temp_dir().join(format!(
            "rimz-paste-{}-{:x}",
            std::process::id(),
            rimz::utils::time::unix_now_ms()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        self.scratch = Some(dir.clone());
        Ok(dir)
    }
}

impl Drop for Uploads {
    fn drop(&mut self) {
        self.cancel();
        if let Some(dir) = &self.scratch {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Sends one transfer and returns the remote paths it landed at.
fn transfer(
    mut stdin: ChildStdin,
    stdout: ChildStdout,
    header: &UploadHeader,
    outgoing: Vec<Outgoing>,
    ready: &mpsc::Sender<Answer>,
) -> Option<Vec<String>> {
    let mut replies = BufReader::new(stdout);
    let line = serde_json::to_string(header).ok()? + "\n";
    let verdict = match stdin
        .write_all(line.as_bytes())
        .ok()
        .and_then(|()| read_reply(&mut replies))
    {
        Some(reply) if reply.ok => Answer::Ready,
        Some(reply) if reply.error.is_some() => Answer::Refused,
        _ => Answer::Unusable,
    };
    let _ = ready.send(verdict);
    if verdict != Answer::Ready {
        return None;
    }
    for file in outgoing {
        // A file that shrank closes the stream short, which the receiver
        // reads as a cancel.
        if io::copy(&mut file.file.take(file.size), &mut stdin).ok()? != file.size {
            return None;
        }
    }
    drop(stdin);
    let result = read_reply(&mut replies)?;
    result.paths.filter(|_| result.ok)
}

fn read_reply(replies: &mut impl BufRead) -> Option<UploadReply> {
    let mut line = String::new();
    if replies.read_line(&mut line).ok()? == 0 {
        return None;
    }
    serde_json::from_str(&line).ok()
}

/// Every path opened as a readable regular file, or none at all.
fn open_all(paths: &[PathBuf]) -> Option<Vec<Outgoing>> {
    paths
        .iter()
        .map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            open(path, name)
        })
        .collect()
}

fn open(path: &Path, name: String) -> Option<Outgoing> {
    let (file, size) = usable(path)?;
    Some(Outgoing { name, file, size })
}

/// Opens without blocking or taking a controlling tty, so a FIFO or a device
/// the user named cannot stall the relay; only a regular file passes.
fn usable(path: &Path) -> Option<(File, u64)> {
    let flags = nix::fcntl::OFlag::O_NONBLOCK | nix::fcntl::OFlag::O_NOCTTY;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits())
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some((file, metadata.len()))
}

/// Waits for a finished or killed upload child off the relay thread.
fn reap(mut child: Child, kill: bool) {
    if kill {
        let _ = child.kill();
    }
    let _ = thread::Builder::new()
        .name("remote-upload-reap".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn within_deadline<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, answer) = mpsc::channel();
        thread::spawn(move || {
            let _ = done.send(work());
        });
        answer
            .recv_timeout(Duration::from_secs(5))
            .expect("blocked on a path the user supplied")
    }

    #[test]
    fn only_a_regular_file_is_usable_and_a_fifo_never_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"alpha").expect("write file");
        let (mut opened, size) = usable(&file).expect("regular file");
        let mut read = String::new();
        opened.read_to_string(&mut read).expect("read file");
        assert_eq!((size, read.as_str()), (5, "alpha"));

        let fifo = dir.path().join("pipe");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
        let probed = fifo.clone();
        assert!(within_deadline(move || usable(&probed).is_none()));
        let route = UploadRoute {
            target: RemoteTarget::parse("dev-box:/srv/app").expect("target"),
            control_path: dir.path().join("ctl"),
            limit: 1024,
        };
        let pasted = fifo.display().to_string().into_bytes();
        let taken = within_deadline(move || Uploads::new(route).expect("uploads").paste(&pasted));
        assert!(!taken, "a held paste of a FIFO forwards as typed");
    }
}
