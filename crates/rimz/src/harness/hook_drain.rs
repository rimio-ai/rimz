//! Elected hook ingress draining, recovery, and the local drain-through wire.
//!
//! Drain-through callers poll the socket and lifetime lock until their deadline, requesting the elected drainer's result or applying inline as soon as the lock is free.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Store;
use crate::agents::hook_types::HOOK_ENV_EXTRA_NAMES;
use crate::agents::{HookIngressOwner, HookReply};
use crate::disk::lock::WorkspaceLock;
use crate::ids::EventId;
use crate::store::event_log::LogExtent;
use crate::store::ingress::{self, HookIngress};

const DRAINER_IDLE: Duration = Duration::from_secs(60);
const SOCKET_WAIT: Duration = Duration::from_millis(200);
const RETRY: Duration = Duration::from_millis(5);
const LOCK_WAIT: Duration = Duration::from_secs(30);
const WIRE_LIMIT: u64 = 1024 * 1024;

pub use crate::agents::hook_types::HOOK_ENV_NAMES;

type Processor = fn(
    &Store,
    &HookIngress,
    Option<LogExtent>,
    &WorkspaceLock,
    Instant,
) -> anyhow::Result<HookReply>;
static PROCESSOR: OnceLock<Processor> = OnceLock::new();

pub fn register_processor(processor: Processor) {
    let _ = PROCESSOR.set(processor);
}

/// Capture an apply child's output until the deadline, killing and reaping its PID on timeout.
pub fn wait_for_apply(
    child: std::process::Child,
    deadline: Instant,
) -> io::Result<std::process::Output> {
    let pid = child.id();
    let output = crate::proc::pump_child(child, None, deadline, crate::proc::KillScope::Process)?;
    if output.timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("hook apply child {pid} timed out"),
        ));
    }
    Ok(std::process::Output {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// How often the drainer has started the frame being applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameAttempt {
    First,
    /// A crashed attempt left no lifecycle envelope, so the apply runs whole.
    Redo,
    /// A crashed attempt published its lifecycle envelope, which is replayed.
    Replay,
}

struct FrameContext {
    env: BTreeMap<String, String>,
    attempt: FrameAttempt,
    timestamp: jiff::Timestamp,
}

thread_local! {
    static FRAME: RefCell<Option<FrameContext>> = const { RefCell::new(None) };
}

struct FrameGuard(Option<FrameContext>);
impl Drop for FrameGuard {
    fn drop(&mut self) {
        FRAME.with(|frame| {
            frame.replace(self.0.take());
        });
    }
}

pub fn capture_env() -> BTreeMap<String, String> {
    FRAME
        .with(|frame| frame.borrow().as_ref().map(|frame| frame.env.clone()))
        .unwrap_or_else(|| {
            let mut names = HOOK_ENV_NAMES.to_vec();
            names.extend(HOOK_ENV_EXTRA_NAMES);
            for agent in crate::agents::all_definitions() {
                names.extend(agent.config_home_env_keys());
                names.extend(agent.temp_dir_env_keys());
                names.extend(agent.hook_env_keys());
            }
            std::env::vars_os()
                .filter_map(|(name, value)| {
                    let name = name.to_str()?;
                    if !name.starts_with("RIMZ_") && !names.contains(&name) {
                        return None;
                    }
                    match value.into_string() {
                        Ok(value) => Some((name.to_owned(), value)),
                        Err(_) => {
                            tracing::warn!(
                                variable = name,
                                "hook environment value is not UTF-8; omitted"
                            );
                            None
                        }
                    }
                })
                .collect()
        })
}

/// Spawn a refresh helper with fresh null stdio and the captured hook environment, handing it to the shared reaper without waiting for completion.
pub fn spawn_refresh_detached(
    spawn: &crate::agents::RefreshSpawn,
    label: &'static str,
) -> io::Result<u32> {
    crate::child_process::spawn_detached_reaped(
        crate::proc::rimz_exe(),
        |command| {
            for name in HOOK_ENV_NAMES {
                command.env_remove(name);
            }
            command.args(&spawn.args).envs(capture_env());
            Ok(())
        },
        label,
    )
}

pub fn with_frame_env<T>(
    frame: &HookIngress,
    attempt: FrameAttempt,
    apply: impl FnOnce() -> T,
) -> T {
    let previous = FRAME.with(|context| {
        context.replace(Some(FrameContext {
            env: frame.env.clone(),
            attempt,
            timestamp: frame.ts,
        }))
    });
    let _frame = FrameGuard(previous);
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_HOOK_APPLY");
    apply()
}

/// Capture the accepted hook identity without probing it again in the drainer.
pub fn capture_ingress_env(owner: HookIngressOwner) -> BTreeMap<String, String> {
    let mut env = capture_env();
    env.insert(
        "RIMZ_HOOK_OWNER_KIND".to_owned(),
        match owner.kind {
            crate::RuntimeOwnerKind::Agent => "agent",
            crate::RuntimeOwnerKind::Daemon => "daemon",
            crate::RuntimeOwnerKind::Script => "script",
        }
        .to_owned(),
    );
    if let Some(pid) = owner.pid {
        env.insert("RIMZ_AGENT_PID".to_owned(), pid.to_string());
    } else {
        env.remove("RIMZ_AGENT_PID");
    }
    env
}

pub fn env_value(name: &str) -> Option<String> {
    FRAME.with(|frame| match frame.borrow().as_ref() {
        Some(frame) => frame.env.get(name).cloned(),
        None => std::env::var(name).ok(),
    })
}

pub fn frame_attempt() -> FrameAttempt {
    FRAME
        .with(|frame| frame.borrow().as_ref().map(|frame| frame.attempt))
        .unwrap_or(FrameAttempt::First)
}

pub fn is_replay() -> bool {
    frame_attempt() == FrameAttempt::Replay
}

pub fn frame_timestamp() -> jiff::Timestamp {
    FRAME
        .with(|frame| frame.borrow().as_ref().map(|frame| frame.timestamp))
        .unwrap_or_else(jiff::Timestamp::now)
}

#[derive(Debug, thiserror::Error)]
pub enum HookDrainErr {
    #[error(
        "hook drainer did not answer within {waited:?}: its socket never accepted and its lifetime lock {path} stayed held (run `fuser {path}` to find the holder)"
    )]
    Unanswered {
        waited: Duration,
        path: std::path::PathBuf,
    },
    #[error("hook drainer accepted the request but sent no receipt within {waited:?}")]
    NoReceipt { waited: Duration },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    Log(#[from] crate::store::event_log::EventLogErr),
    #[error(transparent)]
    Protocol(#[from] serde_json::Error),
}

pub type DrainOutcome = Result<DrainReceipt, HookDrainErr>;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct DrainReceipt {
    pub applied: u64,
    #[serde(default)]
    pub epoch: Option<EventId>,
    pub reply: Option<Value>,
}

#[derive(Deserialize, Serialize)]
struct Request {
    through: u64,
    reply_for: Option<EventId>,
    #[serde(default)]
    epoch: Option<EventId>,
    #[serde(default)]
    stop: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    lease: bool,
}

pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    pub fn connect(store: &Store) -> Result<Self, HookDrainErr> {
        Ok(Self {
            stream: UnixStream::connect(store.runtime_paths().hook_drainer_socket_path())?,
        })
    }

    pub fn nudge(mut self, through: u64) -> Result<(), HookDrainErr> {
        self.stream.set_write_timeout(Some(SOCKET_WAIT))?;
        write_line(
            &mut self.stream,
            &Request {
                through,
                reply_for: None,
                epoch: None,
                stop: false,
                lease: false,
            },
        )
    }

    pub fn reply(mut self, through: u64, reply_for: EventId, wait: Duration) -> DrainOutcome {
        self.stream.set_write_timeout(Some(wait))?;
        self.stream.set_read_timeout(Some(wait))?;
        write_line(
            &mut self.stream,
            &Request {
                through,
                reply_for: Some(reply_for),
                epoch: None,
                stop: false,
                lease: false,
            },
        )?;
        read_line(&self.stream)
    }

    fn lease(mut self, deadline: Instant) -> Result<Self, HookDrainErr> {
        let remaining = || {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "hook drainer lease timed out",
                ));
            }
            Ok(remaining)
        };
        self.stream.set_write_timeout(Some(remaining()?))?;
        write_line(
            &mut self.stream,
            &Request {
                through: 0,
                reply_for: None,
                epoch: None,
                stop: false,
                lease: true,
            },
        )?;
        self.stream.set_read_timeout(Some(remaining()?))?;
        let _: DrainReceipt = read_line(&self.stream)?;
        self.stream.set_nonblocking(true)?;
        Ok(self)
    }
}

pub struct DrainerLease {
    store: Store,
    connection: Connection,
}

impl DrainerLease {
    /// Return only after a live drainer answers and retains this lease connection.
    pub fn acquire(store: &Store, wait: Duration) -> Result<Self, HookDrainErr> {
        let deadline = Instant::now() + wait;
        loop {
            spawn(store)?;
            let round = Instant::now() + SOCKET_WAIT;
            loop {
                let connection = match Connection::connect(store) {
                    Ok(connection) => connection,
                    Err(error) => {
                        let now = Instant::now();
                        if now >= deadline {
                            return Err(error);
                        }
                        if now >= round {
                            break;
                        }
                        std::thread::sleep(RETRY.min(deadline.saturating_duration_since(now)));
                        continue;
                    }
                };
                match connection.lease(deadline) {
                    Ok(connection) => {
                        return Ok(Self {
                            store: store.clone(),
                            connection,
                        });
                    }
                    Err(_) if Instant::now() >= deadline => {
                        return Err(HookDrainErr::NoReceipt { waited: wait });
                    }
                    Err(_) => break,
                }
            }
        }
    }

    /// Keep this answered lease open across the next exec and return its inherited fd.
    pub fn prepare_handoff(&self) -> Result<RawFd, HookDrainErr> {
        nix::fcntl::fcntl(
            &self.connection.stream,
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()),
        )
        .map_err(io::Error::from)?;
        Ok(self.connection.stream.as_raw_fd())
    }

    /// Acquire the parked image's answered lease before closing its inherited lease fd.
    pub fn handover<E>(
        inherited: Option<RawFd>,
        acquire: impl FnOnce() -> Result<Option<Self>, E>,
    ) -> Result<Option<Self>, E> {
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_DRAINER_LEASE_HANDOVER");
        let lease = acquire();
        if let Some(fd) = inherited {
            let _ = nix::unistd::close(fd);
        }
        lease
    }

    pub fn refresh(&mut self) -> Result<(), HookDrainErr> {
        match self.connection.stream.read(&mut [0]) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            _ => {
                *self = Self::acquire(&self.store, SOCKET_WAIT)?;
                Ok(())
            }
        }
    }
}

pub(crate) enum RestartOutcome {
    NotRunning,
    Restarted,
}

pub(crate) fn restart(
    store: &Store,
) -> Result<RestartOutcome, crate::store::event_log::EventLogErr> {
    if Connection::connect(store).is_err() {
        return Ok(RestartOutcome::NotRunning);
    }
    drop(ingress::stop_drainer(store.runtime_paths())?);
    // The next spawner uses the recorded build even if this latency hint fails.
    let _ = nudge(store, 0);
    Ok(RestartOutcome::Restarted)
}

pub fn nudge(store: &Store, through: u64) -> Result<(), HookDrainErr> {
    if let Ok(mut stream) = UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()) {
        stream.set_write_timeout(Some(SOCKET_WAIT))?;
        return write_line(
            &mut stream,
            &Request {
                through,
                reply_for: None,
                epoch: None,
                stop: false,
                lease: false,
            },
        );
    }
    spawn(store)
}

fn spawn(store: &Store) -> Result<(), HookDrainErr> {
    let runtime = store.runtime_paths();
    runtime.ensure_dirs()?;
    let Some(_spawning) = WorkspaceLock::try_acquire(&runtime.hook_drainer_spawn_lock())? else {
        return Ok(());
    };
    if UnixStream::connect(runtime.hook_drainer_socket_path()).is_ok() {
        return Ok(());
    }
    let record = crate::workspace::record::read(&store.paths().workspace_record)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let binary = crate::disk::paths::env_path("RIMZ_BIN").unwrap_or_else(|| {
        crate::workspace::resolve_recorded_rimz_bin(
            &store.paths().workspace_id,
            record.rimz_bin.as_deref(),
        )
    });
    let log_path = store.paths().hook_drainer_log();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    crate::child_process::spawn_hook_drainer(binary, |command| {
        crate::child_process::anchor_detached_command(command, runtime)
            .args(["hooks", "drain", "--project-root"])
            .arg(&record.project_root)
            .stderr(log.try_clone()?);
        for name in HOOK_ENV_NAMES {
            command.env_remove(name);
        }
        command.envs(crate::workspace::pin_env(
            &store.paths().workspace_id,
            &record.project_root,
        ));
        Ok(())
    })?;
    Ok(())
}

pub fn drain_through(
    store: &Store,
    through: u64,
    reply_for: Option<EventId>,
    wait: Duration,
) -> DrainOutcome {
    let mut inline_through = through;
    if reply_for.is_none() {
        match fs::metadata(&store.paths().hook_ingress_log) {
            Ok(metadata) if metadata.len() == 0 => {
                return Ok(DrainReceipt {
                    applied: 0,
                    epoch: None,
                    reply: None,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DrainReceipt {
                    applied: 0,
                    epoch: None,
                    reply: None,
                });
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if through == 0 => inline_through = metadata.len(),
            Ok(_) => {}
        }
    }
    let deadline = Instant::now() + wait;
    let mut request = Request {
        through,
        reply_for,
        epoch: None,
        stop: false,
        lease: false,
    };
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let lifetime_path = store.runtime_paths().hook_drainer_lock();
    let mut stream = UnixStream::connect(&socket).ok();
    if stream.is_none() {
        let _ = spawn(store);
    }
    let mut accepted = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() && accepted {
            return Err(HookDrainErr::NoReceipt { waited: wait });
        }
        if remaining.is_zero() {
            return Err(HookDrainErr::Unanswered {
                waited: wait,
                path: lifetime_path,
            });
        }
        if let Some(mut stream) = stream.take().or_else(|| UnixStream::connect(&socket).ok()) {
            accepted = true;
            stream.set_read_timeout(Some(remaining))?;
            stream.set_write_timeout(Some(remaining))?;
            if write_line(&mut stream, &request).is_ok()
                && let Ok(receipt) = read_line::<DrainReceipt>(&stream)
            {
                return Ok(receipt);
            }
        }
        if let Some(lifetime) = WorkspaceLock::try_acquire(&lifetime_path)? {
            request.through = inline_through;
            let mut drain = Drainer::new(store);
            drain.drain(store, &lifetime, None, Some(deadline), |drain| {
                request.epoch = Some(drain.position(&request).0);
                let lock =
                    WorkspaceLock::acquire_with_timeout(&store.paths().workspace_lock, LOCK_WAIT)?;
                drain.finish_if_drained(store, &lock)?;
                Ok(drain.position(&request).1 >= request.through)
            })?;
            return Ok(drain.receipt(&request, None));
        }
        std::thread::sleep(RETRY.min(remaining));
    }
}

struct Client {
    stream: UnixStream,
    bytes: Vec<u8>,
    state: ClientState,
}

enum ClientState {
    Reading,
    Leased,
    Requested(Request),
}

fn poll_clients(
    store: &Store,
    listener: &UnixListener,
    clients: &mut Vec<Client>,
) -> io::Result<bool> {
    let mut changed = false;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(true)?;
                clients.push(Client {
                    stream,
                    bytes: Vec::new(),
                    state: ClientState::Reading,
                });
                changed = true;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error),
        }
    }
    clients.retain_mut(|client| {
        let mut buffer = [0; 4096];
        let keep = match client.stream.read(&mut buffer) {
            Ok(0) => false,
            Ok(count) => {
                client.bytes.extend_from_slice(&buffer[..count]);
                if !matches!(client.state, ClientState::Reading) {
                    false
                } else if let Some(end) = client.bytes.iter().position(|byte| *byte == b'\n') {
                    match serde_json::from_slice::<Request>(&client.bytes[..end]) {
                        Ok(request) if request.lease => {
                            if write_line(&mut client.stream, &DrainReceipt::default()).is_ok() {
                                client.state = ClientState::Leased;
                                true
                            } else {
                                false
                            }
                        }
                        Ok(mut request) => {
                            if request.through == 0 {
                                request.through = fs::metadata(&store.paths().hook_ingress_log)
                                    .map(|metadata| metadata.len())
                                    .unwrap_or(0);
                            }
                            client.state = ClientState::Requested(request);
                            true
                        }
                        Err(error) => {
                            tracing::debug!(%error, "hook drainer ignored an invalid request");
                            false
                        }
                    }
                } else {
                    client.bytes.len() < WIRE_LIMIT as usize
                }
            }
            Err(error) => error.kind() == io::ErrorKind::WouldBlock,
        };
        changed |= !keep;
        keep
    });
    Ok(changed)
}

fn drain_clients(
    drain: &mut Drainer,
    store: &Store,
    lifetime: &WorkspaceLock,
    listener: &UnixListener,
    socket: &OwnedSocket,
    clients: &mut Vec<Client>,
) -> Result<bool, HookDrainErr> {
    let mut stopping = false;
    drain.drain(store, lifetime, Some(socket), None, |drain| {
        poll_clients(store, listener, clients)?;
        if clients
            .iter()
            .any(|client| matches!(&client.state, ClientState::Requested(request) if request.stop))
        {
            stopping = true;
            answer_clients(drain, clients, false)?;
            return Ok(true);
        }
        for client in clients.iter_mut() {
            if let ClientState::Requested(request) = &mut client.state {
                request.epoch = Some(drain.position(request).0);
            }
        }
        let lock = WorkspaceLock::acquire(&store.paths().workspace_lock)?;
        drain.finish_if_drained(store, &lock)?;
        drop(lock);
        answer_clients(drain, clients, false)?;
        Ok(false)
    })?;
    if !stopping {
        answer_clients(drain, clients, true)?;
    }
    Ok(stopping)
}

fn answer_clients(
    drain: &mut Drainer,
    clients: &mut Vec<Client>,
    exhausted: bool,
) -> Result<(), HookDrainErr> {
    drain.expire_replies();
    let mut index = 0;
    while index < clients.len() {
        let client = &mut clients[index];
        let ClientState::Requested(request) = &client.state else {
            index += 1;
            continue;
        };
        if !exhausted && drain.position(request).1 < request.through {
            index += 1;
            continue;
        }
        client.stream.set_nonblocking(false)?;
        client.stream.set_write_timeout(Some(SOCKET_WAIT))?;
        let _ = write_line(&mut client.stream, &drain.receipt(request, None));
        clients.swap_remove(index);
    }
    Ok(())
}

pub fn run(store: &Store) -> Result<(), HookDrainErr> {
    store.runtime_paths().ensure_dirs()?;
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let until = Instant::now() + LOCK_WAIT;
    let _lifetime = loop {
        if let Some(lock) = WorkspaceLock::try_acquire(&store.runtime_paths().hook_drainer_lock())?
        {
            break lock;
        }
        if UnixStream::connect(&socket).is_ok() {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "hook drainer succession timed out",
            )
            .into());
        }
        std::thread::sleep(RETRY);
    };
    crate::sock::validate_socket_path(&socket).map_err(io::Error::other)?;
    let _ = nix::unistd::setsid();
    remove_socket(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let socket_guard = OwnedSocket::new(socket.clone())?;
    let mut drain = Drainer::new(store);
    let mut clients = Vec::new();
    if drain_clients(
        &mut drain,
        store,
        &_lifetime,
        &listener,
        &socket_guard,
        &mut clients,
    )? {
        return Ok(());
    }
    let mut active = Instant::now();
    #[cfg(feature = "testkit")]
    let mut ticks = std::env::var_os("RIMZ_TEST_HOOK_DRAIN_TICKS")
        .map(UnixStream::connect)
        .transpose()?;
    loop {
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_HOOK_DRAIN_BEFORE_POLL_CLIENTS");
        if poll_clients(store, &listener, &mut clients)? {
            active = Instant::now();
        }
        if clients
            .iter()
            .any(|client| matches!(&client.state, ClientState::Requested(request) if request.stop))
        {
            answer_clients(&mut drain, &mut clients, false)?;
            return Ok(());
        }
        if clients
            .iter()
            .any(|client| matches!(client.state, ClientState::Requested(_)))
        {
            for client in &mut clients {
                if let ClientState::Requested(request) = &mut client.state {
                    request.epoch = Some(drain.position(request).0);
                }
            }
            if drain_clients(
                &mut drain,
                store,
                &_lifetime,
                &listener,
                &socket_guard,
                &mut clients,
            )? {
                return Ok(());
            }
        }
        if !socket_guard.is_current() {
            return Ok(());
        }
        if clients.is_empty() && active.elapsed() >= idle() {
            break;
        }
        let timeout = if clients.is_empty() {
            PollTimeout::try_from(idle().saturating_sub(active.elapsed()))
                .unwrap_or(PollTimeout::MAX)
        } else {
            PollTimeout::NONE
        };
        let mut fds = Vec::with_capacity(clients.len() + 1);
        fds.push(PollFd::new(listener.as_fd(), PollFlags::POLLIN));
        fds.extend(
            clients
                .iter()
                .map(|client| PollFd::new(client.stream.as_fd(), PollFlags::POLLIN)),
        );
        #[cfg(feature = "testkit")]
        if let Some(ticks) = ticks.as_mut() {
            ticks.write_all(&[1])?;
        }
        match poll(&mut fds, timeout) {
            Ok(_) | Err(Errno::EINTR) => {}
            Err(error) => return Err(io::Error::from(error).into()),
        }
    }
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_HOOK_DRAIN_BEFORE_UNLINK");
    drop(socket_guard);
    drop(listener);
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_HOOK_DRAIN_BEFORE_FINAL_DRAIN");
    drain.drain(store, &_lifetime, None, None, |_| Ok(false))?;
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_HOOK_DRAIN_AFTER_FINAL_DRAIN");
    Ok(())
}

struct OwnedSocket {
    path: std::path::PathBuf,
    inode: (u64, u64),
}

impl OwnedSocket {
    fn new(path: std::path::PathBuf) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            inode: (metadata.dev(), metadata.ino()),
        })
    }

    fn is_current(&self) -> bool {
        fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.inode)
    }
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if self.is_current() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn validate_ownership(
    store: &Store,
    socket: Option<&OwnedSocket>,
    expected: &ingress::HookDrainCursor,
    identity: Option<(u64, u64)>,
) -> Result<(), HookDrainErr> {
    if socket.is_some_and(|socket| !socket.is_current())
        || ingress::read_cursor(store.paths())? != *expected
        || fs::metadata(&store.paths().hook_ingress_log)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
            != identity
    {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "hook drain ownership or ingress cursor changed",
        )
        .into());
    }
    Ok(())
}

fn idle() -> Duration {
    #[cfg(feature = "testkit")]
    if let Some(ms) = std::env::var("RIMZ_TEST_HOOK_DRAIN_IDLE_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    DRAINER_IDLE
}

#[cfg(feature = "testkit")]
struct DrainTiming(Value);

#[cfg(feature = "testkit")]
impl Drop for DrainTiming {
    fn drop(&mut self) {
        let _ = writeln!(io::stderr().lock(), "{}", self.0);
    }
}

struct CachedReply {
    epoch: EventId,
    value: Option<Value>,
    at: Instant,
}

struct Drainer {
    boot_id: Option<String>,
    applied: u64,
    epoch: EventId,
    replies: HashMap<EventId, CachedReply>,
    completed: HashMap<EventId, (u64, Instant)>,
    diag: crate::diag::DiagSink,
}

impl Drainer {
    fn new(store: &Store) -> Self {
        let diag = crate::workspace::record::read(&store.paths().workspace_record)
            .map(|record| {
                crate::diag::DiagSink::under(
                    store.paths().root.clone(),
                    store.paths().workspace_id.clone(),
                    record.session_name,
                    None,
                )
            })
            .unwrap_or_else(|_| crate::diag::DiagSink::disabled());
        Self {
            boot_id: crate::proc::boot_id(),
            applied: 0,
            epoch: EventId::default(),
            replies: HashMap::new(),
            completed: HashMap::new(),
            diag,
        }
    }

    fn drain(
        &mut self,
        store: &Store,
        lifetime: &WorkspaceLock,
        socket: Option<&OwnedSocket>,
        deadline: Option<Instant>,
        mut respond: impl FnMut(&mut Self) -> Result<bool, HookDrainErr>,
    ) -> Result<(), HookDrainErr> {
        let remaining = || {
            deadline.map_or(LOCK_WAIT, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            })
        };
        let workspace_lock =
            || WorkspaceLock::acquire_with_timeout(&store.paths().workspace_lock, remaining());
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(
                    io::Error::new(io::ErrorKind::TimedOut, "hook drain deadline reached").into(),
                );
            }
            let (cursor, frames, identity) = {
                let lock = workspace_lock()?;
                let cursor = ingress::read_cursor(store.paths())?;
                let frames = ingress::repair_and_read(
                    store.paths(),
                    cursor.applied,
                    &lock,
                    remaining(),
                    &self.diag,
                )?;
                let identity = fs::metadata(&store.paths().hook_ingress_log)
                    .ok()
                    .map(|metadata| (metadata.dev(), metadata.ino()));
                (cursor, frames, identity)
            };
            self.applied = cursor.applied;
            if frames.is_empty() {
                return Ok(());
            }
            let mut expected = cursor.clone();
            let mut frames = frames.into_iter().peekable();
            while let Some((frame, mut end)) = frames.next() {
                let recovering = cursor.claimed > cursor.applied && end == cursor.claimed;
                let Some(frame) = frame else {
                    let lock = workspace_lock()?;
                    validate_ownership(store, socket, &expected, identity)?;
                    let mut cursor = expected.clone();
                    cursor.applied = end;
                    cursor.claimed = end;
                    cursor.applied_event_id = None;
                    cursor.claimed_event_id = None;
                    ingress::write_cursor_cache(store.paths(), &cursor, &lock)?;
                    expected = cursor;
                    self.applied = end;
                    drop(lock);
                    if respond(self)? {
                        return Ok(());
                    }
                    continue;
                };
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "hook drain deadline reached",
                    )
                    .into());
                }
                let other_boot = |frame: &HookIngress| {
                    frame
                        .boot_id
                        .as_ref()
                        .zip(self.boot_id.as_ref())
                        .is_some_and(|(frame_boot, own_boot)| frame_boot != own_boot)
                };
                if other_boot(&frame) {
                    let first_ingress = frame.event_id;
                    let mut last_ingress = first_ingress.clone();
                    let mut dropped = 1;
                    while let Some((Some(frame), next_end)) =
                        frames.next_if(|(frame, _)| frame.as_ref().is_some_and(other_boot))
                    {
                        last_ingress = frame.event_id;
                        end = next_end;
                        dropped += 1;
                    }
                    let lock = workspace_lock()?;
                    validate_ownership(store, socket, &expected, identity)?;
                    self.diag
                        .emit(crate::diag::record::DiagEvent::HookIngressDropped {
                            dropped,
                            first_ingress,
                            last_ingress: last_ingress.clone(),
                        });
                    let mut cursor = expected.clone();
                    cursor.applied = end;
                    cursor.claimed = end;
                    cursor.applied_event_id = Some(last_ingress.clone());
                    cursor.claimed_event_id = Some(last_ingress);
                    ingress::write_cursor_cache(store.paths(), &cursor, &lock)?;
                    expected = cursor;
                    self.applied = end;
                    drop(lock);
                    if respond(self)? {
                        return Ok(());
                    }
                    continue;
                }
                let write_cursor = if self
                    .boot_id
                    .as_ref()
                    .is_some_and(|boot| frame.boot_id.as_ref() == Some(boot))
                {
                    ingress::write_cursor_cache
                } else {
                    ingress::write_cursor
                };
                #[cfg(feature = "testkit")]
                let mut processor_succeeded = true;
                #[cfg(feature = "testkit")]
                let timing_start = {
                    static ENABLED: OnceLock<bool> = OnceLock::new();
                    ENABLED
                        .get_or_init(|| {
                            std::env::var("RIMZ_TEST_HOOK_DRAIN_TIMING").as_deref() == Ok("1")
                        })
                        .then(Instant::now)
                };
                {
                    let lock = workspace_lock()?;
                    validate_ownership(store, socket, &expected, identity)?;
                    let mut cursor = expected.clone();
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "hook drain deadline reached",
                        )
                        .into());
                    }
                    if !recovering {
                        cursor.log_extent_at_claim = LogExtent {
                            generation: crate::store::snapshot::lifecycle_log_generation(
                                store.paths(),
                            ),
                            offset: fs::metadata(&store.paths().events_log)
                                .map(|metadata| metadata.len())
                                .unwrap_or(0),
                        };
                    }
                    cursor.claimed = end;
                    cursor.claimed_event_id = Some(frame.event_id.clone());
                    write_cursor(store.paths(), &cursor, &lock)?;
                    expected = cursor;
                }
                let processor = PROCESSOR.get().ok_or_else(|| {
                    io::Error::other("hook processor was not registered by the CLI")
                })?;
                let lag = jiff::Timestamp::now().duration_since(frame.ts);
                if lag > jiff::SignedDuration::from_secs(10)
                    && let Ok(metadata) = fs::metadata(&store.paths().hook_ingress_log)
                {
                    self.diag
                        .emit(crate::diag::record::DiagEvent::HookDrainLag {
                            ingress: frame.event_id.clone(),
                            hook_event: frame.event.clone(),
                            lag_ms: u64::try_from(lag.as_millis()).unwrap_or(u64::MAX),
                            pending_bytes: metadata.len().saturating_sub(expected.applied),
                        });
                }
                let reply = match processor(
                    store,
                    &frame,
                    recovering.then_some(cursor.log_extent_at_claim),
                    lifetime,
                    Instant::now() + LOCK_WAIT,
                ) {
                    Ok(reply) => reply,
                    Err(error) => {
                        #[cfg(feature = "testkit")]
                        {
                            processor_succeeded = false;
                        }
                        tracing::warn!(ingress = %frame.event_id, %error, "hook ingress apply failed");
                        self.diag
                            .emit(crate::diag::record::DiagEvent::HookApplyFailed {
                                ingress: frame.event_id.clone(),
                                error: format!("{error:#}"),
                            });
                        HookReply::Silent
                    }
                };
                #[cfg(feature = "testkit")]
                crate::testkit::rendezvous("RIMZ_TEST_HOOK_DRAIN_AFTER_APPLY");
                let lock =
                    WorkspaceLock::acquire_with_timeout(&store.paths().workspace_lock, LOCK_WAIT)?;
                validate_ownership(store, socket, &expected, identity)?;
                let mut cursor = expected.clone();
                cursor.applied = end;
                cursor.applied_event_id = Some(frame.event_id.clone());
                write_cursor(store.paths(), &cursor, &lock)?;
                expected = cursor;
                self.applied = end;
                #[cfg(feature = "testkit")]
                let elapsed = timing_start.map(|start| start.elapsed());
                #[cfg(feature = "testkit")]
                let _timing = elapsed.map(|elapsed| {
                    DrainTiming(serde_json::json!({"hook_drain_timing": {
                        "ingress": frame.event_id,
                        "hook_event": frame.event,
                        "claim_to_applied_us": u64::try_from(elapsed.as_micros())
                            .unwrap_or(u64::MAX),
                        "applied": processor_succeeded,
                    }}))
                });
                self.replies.insert(
                    frame.event_id,
                    CachedReply {
                        epoch: self.epoch.clone(),
                        value: match reply {
                            HookReply::Json(value) => Some(value),
                            HookReply::Silent => None,
                        },
                        at: Instant::now(),
                    },
                );
                drop(lock);
                if respond(self)? {
                    return Ok(());
                }
            }
            let lock =
                WorkspaceLock::acquire_with_timeout(&store.paths().workspace_lock, LOCK_WAIT)?;
            if self.finish_if_drained(store, &lock)? {
                return Ok(());
            }
        }
    }

    fn finish_if_drained(
        &mut self,
        store: &Store,
        lock: &WorkspaceLock,
    ) -> Result<bool, HookDrainErr> {
        let len = match fs::metadata(&store.paths().hook_ingress_log) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if self.applied != len || !ingress::truncate_drained(store.paths(), lock)? {
            return Ok(false);
        }
        self.completed
            .insert(self.epoch.clone(), (self.applied, Instant::now()));
        self.epoch = EventId::new();
        self.applied = 0;
        Ok(true)
    }

    fn position(&self, request: &Request) -> (EventId, u64) {
        let epoch = request
            .epoch
            .as_ref()
            .or_else(|| {
                request
                    .reply_for
                    .as_ref()
                    .and_then(|id| self.replies.get(id))
                    .map(|reply| &reply.epoch)
            })
            .unwrap_or(&self.epoch);
        let applied = if epoch == &self.epoch {
            self.applied
        } else {
            self.completed.get(epoch).map_or(0, |(applied, _)| *applied)
        };
        (epoch.clone(), applied)
    }

    fn receipt(&mut self, request: &Request, reply: Option<Value>) -> DrainReceipt {
        let (epoch, applied) = self.position(request);
        let cached = request
            .reply_for
            .as_ref()
            .and_then(|id| self.replies.get_mut(id))
            .and_then(|reply| reply.value.take());
        DrainReceipt {
            applied,
            epoch: Some(epoch),
            reply: reply.or(cached),
        }
    }

    fn expire_replies(&mut self) {
        let retention = reply_retention();
        self.replies
            .retain(|_, reply| reply.at.elapsed() < retention);
        self.completed.retain(|_, (_, at)| at.elapsed() < retention);
    }
}

fn reply_retention() -> Duration {
    #[cfg(feature = "testkit")]
    if let Some(ms) = std::env::var("RIMZ_TEST_HOOK_REPLY_RETENTION_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    DRAINER_IDLE
}

fn read_line<T: serde::de::DeserializeOwned>(stream: &UnixStream) -> Result<T, HookDrainErr> {
    let mut reader = BufReader::new(stream).take(WIRE_LIMIT);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "incomplete hook drain frame").into(),
        );
    }
    Ok(serde_json::from_str(&line)?)
}

fn write_line(stream: &mut UnixStream, value: &impl Serialize) -> Result<(), HookDrainErr> {
    serde_json::to_writer(&mut *stream, value)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn remove_socket(path: &std::path::Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(prefix_frames: usize) -> (tempfile::TempDir, Store, Vec<(HookIngress, u64)>) {
        use crate::disk::paths::{RuntimePaths, StatePaths};
        use crate::ids::{AgentKind, WorkspaceId};
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        let mut frames = Vec::new();
        let mut bytes = Vec::new();
        for _ in 0..prefix_frames + 4 {
            let frame = HookIngress {
                boot_id: None,
                schema_version: "1".into(),
                event_id: EventId::new(),
                ts: "2026-01-01T00:00:00Z".parse().unwrap(),
                source: AgentKind::new_unchecked("codex"),
                event: None,
                payload: "x".repeat(1024),
                cwd: "/tmp/project".into(),
                hook_pid: 1234,
                env: BTreeMap::new(),
            };
            let payload = serde_json::to_string(&frame).unwrap();
            writeln!(
                bytes,
                "{} {:08x} {}",
                payload.len(),
                crc32fast::hash(payload.as_bytes()),
                payload
            )
            .unwrap();
            frames.push((frame, bytes.len() as u64));
        }
        fs::create_dir_all(store.paths().hook_ingress_log.parent().unwrap()).unwrap();
        fs::write(&store.paths().hook_ingress_log, bytes).unwrap();
        let (frame, end) = &frames[prefix_frames - 1];
        let lock = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
        ingress::write_cursor(
            store.paths(),
            &ingress::HookDrainCursor {
                applied: *end,
                claimed: *end,
                applied_event_id: Some(frame.event_id.clone()),
                claimed_event_id: Some(frame.event_id.clone()),
                ..Default::default()
            },
            &lock,
        )
        .unwrap();
        (dir, store, frames.split_off(prefix_frames))
    }

    #[test]
    fn ingress_reads_are_independent_of_applied_prefix() {
        register_processor(|_, _, _, _, _| Ok(HookReply::Silent));
        let mut reads = Vec::new();
        for prefix in [1, 4096] {
            let (_dir, store, pending) = fixture(prefix);
            let log_dir = store.paths().hook_ingress_log.parent().unwrap();
            let before = crate::store::event_log::testkit::bytes_read_under(log_dir);
            let lifetime =
                WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
            let mut drain = Drainer::new(&store);
            drain
                .drain(&store, &lifetime, None, None, |_| Ok(false))
                .unwrap();
            let read = crate::store::event_log::testkit::bytes_read_under(log_dir) - before;
            let frame_bytes = pending[1].1 - pending[0].1;
            assert!(
                read <= frame_bytes * 32,
                "prefix {prefix}: read {read} bytes"
            );
            assert_eq!(
                drain.completed.values().next().unwrap().0,
                pending.last().unwrap().1
            );
            reads.push(read);
        }
        assert!(reads[0].abs_diff(reads[1]) <= 1024, "{reads:?}");
    }

    #[test]
    fn crash_redo_is_only_the_in_flight_frame() {
        static CALLS: std::sync::Mutex<Vec<(Option<LogExtent>, LogExtent)>> =
            std::sync::Mutex::new(Vec::new());
        register_processor(|store, _, recover, _, _| {
            let cursor = ingress::read_cursor(store.paths())?;
            CALLS
                .lock()
                .unwrap()
                .push((recover, cursor.log_extent_at_claim));
            Ok(HookReply::Silent)
        });
        let (_dir, store, pending) = fixture(1);
        let old = LogExtent {
            generation: 9,
            offset: 42,
        };
        {
            let lock = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
            let mut cursor = ingress::read_cursor(store.paths()).unwrap();
            cursor.claimed = pending[0].1;
            cursor.claimed_event_id = Some(pending[0].0.event_id.clone());
            cursor.log_extent_at_claim = old;
            ingress::write_cursor(store.paths(), &cursor, &lock).unwrap();
        }
        let lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
        Drainer::new(&store)
            .drain(&store, &lifetime, None, None, |_| Ok(false))
            .unwrap();
        let calls = CALLS.lock().unwrap();
        assert_eq!(calls.len(), pending.len());
        assert_eq!(calls[0], (Some(old), old));
        for (recover, extent) in &calls[1..] {
            assert_eq!(*recover, None);
            assert_eq!(
                *extent,
                LogExtent {
                    generation: 0,
                    offset: 0
                }
            );
        }
    }

    #[test]
    fn other_boot_frames_drop_before_pending_or_claimed_application() {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        register_processor(|_, _, _, _, _| {
            CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(HookReply::Silent)
        });
        for claimed in [false, true] {
            let (_dir, store, pending) = pending_boot_fixture(Some("other-boot"), 4);
            if claimed {
                let lock = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
                let cursor = ingress::HookDrainCursor {
                    claimed: pending[0].1,
                    claimed_event_id: Some(pending[0].0.event_id.clone()),
                    ..Default::default()
                };
                ingress::write_cursor(store.paths(), &cursor, &lock).unwrap();
            }
            let lifetime =
                WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
            let mut drain = Drainer::new(&store);
            drain.boot_id = Some("this-boot".into());
            drain.diag = crate::diag::DiagSink::under(
                store.paths().root.clone(),
                store.paths().workspace_id.clone(),
                "session",
                None,
            );
            let log = drain.diag.log_path().unwrap();
            let mut responses = 0;
            drain
                .drain(&store, &lifetime, None, None, |_| {
                    let cursor = ingress::read_cursor(store.paths()).unwrap();
                    let (frame, end) = pending.last().unwrap();
                    assert_eq!((cursor.applied, cursor.claimed), (*end, *end));
                    assert_eq!(cursor.applied_event_id.as_ref(), Some(&frame.event_id));
                    assert_eq!(cursor.claimed_event_id.as_ref(), Some(&frame.event_id));
                    responses += 1;
                    Ok(false)
                })
                .unwrap();
            assert_eq!(
                CALLS.load(std::sync::atomic::Ordering::Relaxed),
                0,
                "foreign frames must never invoke the processor, including recovery"
            );
            assert_eq!(responses, 1);
            let records = fs::read_to_string(log)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 1);
            assert_eq!(
                records[0]["event"],
                serde_json::json!({
                    "kind": "hook_ingress_dropped",
                    "dropped": pending.len(),
                    "first_ingress": pending[0].0.event_id,
                    "last_ingress": pending.last().unwrap().0.event_id,
                })
            );
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn other_boot_runs_publish_one_cursor_each_and_apply_intervening_frames() {
        use rustix::fs::inotify;

        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        register_processor(|_, frame, recover, _, _| {
            assert!(matches!(frame.boot_id.as_deref(), Some("this-boot") | None));
            assert!(recover.is_none());
            CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(HookReply::Silent)
        });
        for boot in [Some("this-boot"), None] {
            let (_dir, store, mut pending) = pending_boot_fixture(Some("other-boot"), 3);
            {
                let lock =
                    crate::disk::lock::SharedAppendLock::acquire(&store.paths().hook_ingress_lock)
                        .unwrap();
                for boot in [boot, Some("other-boot"), Some("another-boot")] {
                    let mut frame = pending[0].0.clone();
                    frame.event_id = EventId::new();
                    frame.boot_id = boot.map(str::to_owned);
                    let end = ingress::append(store.paths(), &frame, &lock).unwrap();
                    pending.push((frame, end));
                }
            }
            let watch = inotify::init(inotify::CreateFlags::NONBLOCK).unwrap();
            // Interleaved source events prevent identical target events from coalescing.
            inotify::add_watch(
                &watch,
                store.paths().hook_drain_cursor.parent().unwrap(),
                inotify::WatchFlags::MOVED_FROM | inotify::WatchFlags::MOVED_TO,
            )
            .unwrap();
            let lifetime =
                WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
            let mut drain = Drainer::new(&store);
            drain.boot_id = Some("this-boot".into());
            drain.diag = crate::diag::DiagSink::under(
                store.paths().root.clone(),
                store.paths().workspace_id.clone(),
                "session",
                None,
            );
            let log = drain.diag.log_path().unwrap();
            let mut responses = Vec::new();
            drain
                .drain(&store, &lifetime, None, None, |drain| {
                    let cursor = ingress::read_cursor(store.paths()).unwrap();
                    assert_eq!(cursor.applied, cursor.claimed);
                    assert_eq!(cursor.applied_event_id, cursor.claimed_event_id);
                    assert_eq!(cursor.applied, drain.applied);
                    assert!(
                        fs::read_to_string(&log)
                            .unwrap()
                            .contains("hook_ingress_dropped")
                    );
                    responses.push(cursor.applied);
                    Ok(cursor.applied == pending.last().unwrap().1)
                })
                .unwrap();
            assert_eq!(responses, vec![pending[2].1, pending[3].1, pending[5].1]);
            let mut buffer = [std::mem::MaybeUninit::uninit(); 4096];
            let mut reader = inotify::Reader::new(&watch, &mut buffer);
            let mut publications = 0;
            loop {
                match reader.next() {
                    Ok(event) => {
                        if event.file_name().is_some_and(|name| {
                            name.to_bytes()
                                == store
                                    .paths()
                                    .hook_drain_cursor
                                    .file_name()
                                    .unwrap()
                                    .as_encoded_bytes()
                        }) {
                            publications += 1;
                        }
                    }
                    Err(rustix::io::Errno::AGAIN) => break,
                    Err(error) => panic!("cursor watch failed: {error}"),
                }
            }
            assert_eq!(
                publications, 4,
                "one write per run, two for the applied frame"
            );
            let records = fs::read_to_string(log)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 2);
            for (record, (first, last)) in records.iter().zip([(0, 2), (4, 5)]) {
                assert_eq!(
                    record["event"],
                    serde_json::json!({
                        "kind": "hook_ingress_dropped",
                        "dropped": last - first + 1,
                        "first_ingress": pending[first].0.event_id,
                        "last_ingress": pending[last].0.event_id,
                    })
                );
            }
        }
        assert_eq!(CALLS.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    fn pending_boot_fixture(
        boot: Option<&str>,
        count: usize,
    ) -> (tempfile::TempDir, Store, Vec<(HookIngress, u64)>) {
        let (dir, store, pending) = fixture(1);
        let mut bytes = Vec::new();
        let mut frames = Vec::new();
        for (frame, _) in pending.into_iter().take(count) {
            let mut wire = serde_json::to_value(frame).unwrap();
            wire["boot_id"] = serde_json::json!(boot);
            wire["ts"] = serde_json::json!(jiff::Timestamp::now());
            let payload = serde_json::to_string(&wire).unwrap();
            writeln!(
                bytes,
                "{} {:08x} {}",
                payload.len(),
                crc32fast::hash(payload.as_bytes()),
                payload
            )
            .unwrap();
            frames.push((serde_json::from_value(wire).unwrap(), bytes.len() as u64));
        }
        fs::write(&store.paths().hook_ingress_log, bytes).unwrap();
        fs::remove_file(&store.paths().hook_drain_cursor).unwrap();
        (dir, store, frames)
    }

    fn cursor_fsyncs(boot: Option<&str>, own_boot: Option<&str>, finish: bool) -> u64 {
        register_processor(|_, _, _, _, _| Ok(HookReply::Silent));
        let (_dir, store, _) = pending_boot_fixture(boot, if finish { 1 } else { 2 });
        let lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
        let mut drain = Drainer::new(&store);
        drain.boot_id = own_boot.map(str::to_owned);
        let before = crate::disk::atomic::testkit::fsync_count();
        drain
            .drain(&store, &lifetime, None, None, |_| Ok(!finish))
            .unwrap();
        crate::disk::atomic::testkit::fsync_count() - before
    }

    #[test]
    fn same_boot_claim_and_applied_are_cache_class() {
        assert_eq!(
            cursor_fsyncs(Some("this-boot"), Some("this-boot"), false),
            0
        );
    }

    #[test]
    fn unstamped_claim_and_applied_stay_durable() {
        assert_eq!(cursor_fsyncs(None, Some("this-boot"), false), 4);
    }

    #[test]
    fn unknown_drainer_boot_keeps_stamped_claims_durable() {
        assert_eq!(cursor_fsyncs(Some("this-boot"), None, false), 4);
    }

    #[test]
    fn singleton_same_boot_drain_only_syncs_the_truncation() {
        assert_eq!(cursor_fsyncs(Some("this-boot"), Some("this-boot"), true), 1);
    }

    #[test]
    fn receipt_never_claims_unapplied_ingress() {
        let request = Request {
            through: 100,
            reply_for: None,
            epoch: None,
            stop: false,
            lease: false,
        };
        let (_dir, store, _) = fixture(1);
        assert_eq!(Drainer::new(&store).receipt(&request, None).applied, 0);
    }
}
