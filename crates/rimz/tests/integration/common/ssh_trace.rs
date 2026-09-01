//! Shared `rimz remote connect` PTY harness and terminal-query responder.
//!
//! The Ghostty answers are the 1.3.1 bytes from the reconnect diagnosis. The
//! configured answer set responds in the same order a terminal receives its
//! queries, including RimZ's fence and a real mux client's attach probes.

use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtyPair, PtySize, native_pty_system};

use super::Env;

const STATUS_QUERY: &[u8] = b"\x1b[5n";
const STATUS_REPLY: &[u8] = b"\x1b[0n";
const DA1_QUERY: &[u8] = b"\x1b[c";
const DA2_QUERY: &[u8] = b"\x1b[>c";
const XTVERSION_QUERY: &[u8] = b"\x1b[>q";
const DA1_REPLY: &[u8] = b"\x1b[?62;22;52c";
const DA2_REPLY: &[u8] = b"\x1b[>1;10;0c";
const XTVERSION_REPLY: &[u8] = b"\x1bP>|ghostty 1.3.1\x1b\\";

pub(crate) fn ssh_shim() -> PathBuf {
    super::cargo_bin("ssh-trace", env!("CARGO_BIN_EXE_ssh-trace"))
}

/// One `Vec<argv>` per shim invocation, from the tab-joined trace log.
pub(crate) fn shim_invocations(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .expect("read ssh trace log")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(ToOwned::to_owned).collect())
        .collect()
}

pub(crate) fn is_probe_invocation(argv: &[String]) -> bool {
    argv.iter()
        .any(|arg| arg.contains("remote link-stats ingest"))
}

pub(crate) fn is_control_check_invocation(argv: &[String]) -> bool {
    argv.windows(2)
        .any(|args| args[0] == "-O" && args[1] == "check")
}

pub(crate) fn is_config_query_invocation(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "-G")
}

pub(crate) fn is_path_preflight_invocation(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg.starts_with("test -d "))
}

pub(crate) fn is_master_invocation(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "-M")
}

pub(crate) fn is_main_invocation(argv: &[String]) -> bool {
    !is_probe_invocation(argv)
        && !is_control_check_invocation(argv)
        && !is_config_query_invocation(argv)
        && !is_master_invocation(argv)
        && !is_path_preflight_invocation(argv)
}

pub(crate) fn main_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(ToOwned::to_owned).collect::<Vec<_>>())
        .filter(|argv| is_main_invocation(argv))
        .count()
}

pub(crate) fn master_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(ToOwned::to_owned).collect::<Vec<_>>())
        .filter(|argv| is_master_invocation(argv))
        .count()
}

pub(crate) fn remote_connect_pty_command(env: &Env, log: &Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.args(["remote", "connect", "dev-box:query-engine", "--attach"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("RIMZ_SSH_BIN", ssh_shim());
    cmd.env("RIMZ_TEST_SSH_LOG", log);
    cmd.env("RIMZ_REMOTE_DIAL_MS", "0");
    cmd.env("RIMZ_REMOTE_PROBE_MS", "0");
    cmd.env("RIMZ_REMOTE_INTERNET_PROBE", "0");
    cmd.env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "0");
    cmd.env("TERM", "xterm-256color");
    cmd
}

pub(crate) fn remote_connect_pty() -> PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open remote connect pty")
}

#[derive(Clone, Copy)]
pub(crate) enum Answers {
    StatusOnly,
    Ghostty,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) status: usize,
    pub(crate) da1: usize,
    pub(crate) da2: usize,
    pub(crate) xtversion: usize,
}

#[derive(Default)]
struct AtomicCounts {
    status: AtomicUsize,
    da1: AtomicUsize,
    da2: AtomicUsize,
    xtversion: AtomicUsize,
}

impl AtomicCounts {
    fn load(&self) -> Counts {
        Counts {
            status: self.status.load(Ordering::Acquire),
            da1: self.da1.load(Ordering::Acquire),
            da2: self.da2.load(Ordering::Acquire),
            xtversion: self.xtversion.load(Ordering::Acquire),
        }
    }
}

enum WriterMessage {
    Write(Vec<u8>),
    WriteAt(Instant, Vec<u8>),
    WriteAfterNextReply(Vec<u8>),
    ReplyAt(Instant, &'static [u8]),
    Stop,
}

pub(crate) struct FakeTerminal {
    master: Box<dyn MasterPty + Send>,
    messages: mpsc::Sender<WriterMessage>,
    counts: Arc<AtomicCounts>,
    output: Arc<Mutex<Vec<u8>>>,
    reader_thread: thread::JoinHandle<()>,
    writer_thread: thread::JoinHandle<()>,
}

impl FakeTerminal {
    pub(crate) fn new(
        master: Box<dyn MasterPty + Send>,
        latency: Duration,
        answers: Answers,
    ) -> Self {
        let mut reader = master.try_clone_reader().expect("clone terminal reader");
        let writer = master.take_writer().expect("take terminal writer");
        let (messages, receiver) = mpsc::channel();
        let writer_thread = thread::spawn(move || run_writer(writer, receiver));
        let output = Arc::new(Mutex::new(Vec::new()));
        let counts = Arc::new(AtomicCounts::default());
        let reader_output = Arc::clone(&output);
        let reader_counts = Arc::clone(&counts);
        let reader_messages = messages.clone();
        let reader_thread = thread::spawn(move || {
            let mut scanners = QueryScanners::default();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => read,
                };
                reader_output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&buffer[..read]);
                for &byte in &buffer[..read] {
                    scanners.feed(byte, latency, answers, &reader_counts, &reader_messages);
                }
            }
        });
        Self {
            master,
            messages,
            counts,
            output,
            reader_thread,
            writer_thread,
        }
    }

    /// Writes test input immediately through the terminal's sole PTY writer.
    pub(crate) fn write(&self, bytes: &[u8]) {
        self.messages
            .send(WriterMessage::Write(bytes.to_vec()))
            .expect("terminal writer alive");
    }

    pub(crate) fn write_after(&self, delay: Duration, bytes: &[u8]) {
        self.messages
            .send(WriterMessage::WriteAt(
                Instant::now() + delay,
                bytes.to_vec(),
            ))
            .expect("terminal writer alive");
    }

    /// Appends input to the next scheduled terminal reply in one PTY write.
    pub(crate) fn write_after_next_reply(&self, bytes: &[u8]) {
        self.messages
            .send(WriterMessage::WriteAfterNextReply(bytes.to_vec()))
            .expect("terminal writer alive");
    }

    pub(crate) fn queries_seen(&self) -> Counts {
        self.counts.load()
    }

    pub(crate) fn wait_for_status_query(&self, count: usize, deadline: Instant) {
        while self.queries_seen().status < count && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            self.queries_seen().status >= count,
            "status query {count} not seen; output: {:?}",
            String::from_utf8_lossy(&self.output())
        );
    }

    pub(crate) fn output(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        let Self {
            master,
            messages,
            output,
            reader_thread,
            writer_thread,
            ..
        } = self;
        let _ = messages.send(WriterMessage::Stop);
        drop(messages);
        writer_thread.join().expect("join terminal writer");
        drop(master);
        reader_thread.join().expect("join terminal reader");
        output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Default)]
struct QueryScanners {
    status: ExactScanner,
    da1: ExactScanner,
    da2: ExactScanner,
    xtversion: ExactScanner,
}

impl QueryScanners {
    fn feed(
        &mut self,
        byte: u8,
        latency: Duration,
        answers: Answers,
        counts: &AtomicCounts,
        messages: &mpsc::Sender<WriterMessage>,
    ) {
        let status = self.status.feed(STATUS_QUERY, byte);
        let da1 = self.da1.feed(DA1_QUERY, byte);
        let da2 = self.da2.feed(DA2_QUERY, byte);
        let xtversion = self.xtversion.feed(XTVERSION_QUERY, byte);
        let reply = if status {
            counts.status.fetch_add(1, Ordering::Release);
            Some(STATUS_REPLY)
        } else if da1 {
            counts.da1.fetch_add(1, Ordering::Release);
            matches!(answers, Answers::Ghostty).then_some(DA1_REPLY)
        } else if da2 {
            counts.da2.fetch_add(1, Ordering::Release);
            matches!(answers, Answers::Ghostty).then_some(DA2_REPLY)
        } else if xtversion {
            counts.xtversion.fetch_add(1, Ordering::Release);
            matches!(answers, Answers::Ghostty).then_some(XTVERSION_REPLY)
        } else {
            None
        };
        if let Some(reply) = reply {
            let _ = messages.send(WriterMessage::ReplyAt(Instant::now() + latency, reply));
        }
    }
}

#[derive(Default)]
struct ExactScanner {
    matched: usize,
}

impl ExactScanner {
    fn feed(&mut self, pattern: &[u8], byte: u8) -> bool {
        if byte == pattern[self.matched] {
            self.matched += 1;
            if self.matched == pattern.len() {
                self.matched = 0;
                return true;
            }
            return false;
        }
        self.matched = usize::from(byte == pattern[0]);
        false
    }
}

fn run_writer(mut writer: Box<dyn Write + Send>, receiver: mpsc::Receiver<WriterMessage>) {
    let mut scheduled: Vec<(Instant, Vec<u8>, bool)> = Vec::new();
    let mut after_next_reply = None;
    loop {
        scheduled.sort_by_key(|(due, _, _)| *due);
        let message = match scheduled.first() {
            Some((due, _, _)) => {
                receiver.recv_timeout(due.saturating_duration_since(Instant::now()))
            }
            None => receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(WriterMessage::Write(bytes)) => write_bytes(&mut writer, &bytes),
            Ok(WriterMessage::WriteAt(due, bytes)) => scheduled.push((due, bytes, false)),
            Ok(WriterMessage::WriteAfterNextReply(bytes)) => after_next_reply = Some(bytes),
            Ok(WriterMessage::ReplyAt(due, reply)) => {
                scheduled.push((due, reply.to_vec(), true));
            }
            Ok(WriterMessage::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let (_, mut bytes, is_reply) = scheduled.remove(0);
                if is_reply && let Some(after) = after_next_reply.take() {
                    bytes.extend(after);
                }
                write_bytes(&mut writer, &bytes);
            }
        }
    }
}

fn write_bytes(writer: &mut impl Write, bytes: &[u8]) {
    writer.write_all(bytes).expect("write terminal input");
    writer.flush().expect("flush terminal input");
}
