//! Elected hook ingress draining, recovery, and the local drain-through wire.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Store;
use crate::agents::{HookIngressOwner, HookReply};
use crate::disk::lock::WorkspaceLock;
use crate::ids::EventId;
use crate::store::event_log::LogExtent;
use crate::store::ingress::{self, HookIngress};

const DRAINER_IDLE: Duration = Duration::from_secs(60);
const SPAWN_RETRY: Duration = Duration::from_millis(200);
const RETRY: Duration = Duration::from_millis(5);
const LOCK_WAIT: Duration = Duration::from_secs(30);
const WIRE_LIMIT: u64 = 1024 * 1024;

pub const HOOK_ENV_NAMES: &[&str] = &[
    "RIMZ_AGENT_PID",
    "RIMZ_HOOK_OWNER_KIND",
    "RIMZ_RUN_ID",
    "RIMZ_WORKSPACE_ID",
    "RIMZ_PROJECT_ROOT",
    "RIMZ_WORKTREE_PATH",
    "RIMZ_CHANNEL",
    "RIMZ_AGENT_KIND",
    "RIMZ_AGENT_ID",
    "RIMZ_AGENT_NAME",
    "RIMZ_AGENT_PROFILE",
    "RIMZ_AGENT_ROLE",
    "RIMZ_AGENT_MODEL",
    "RIMZ_AGENT_EFFORT",
    "RIMZ_AGENT_BUDGET",
    "RIMZ_TEAM",
    "RIMZ_LAUNCH_GROUP",
    "RIMZ_LAUNCH_ORDINAL",
    "RIMZ_RUNTIME_ENV",
    "RIMZ_LOOP_TASK",
    "RIMZ_ZELLIJ_BIN",
    "RIMZ_TMUX_BIN",
    "RIMZ_MESSAGE_INTERVAL_MS",
    "RIMZ_MESSAGE_SETTLE_MS",
    #[cfg(feature = "testkit")]
    "RIMZ_TEST_ZELLIJ_LOG",
    "RIMZ_CODEX_SESSIONS",
    "RIMZ_CODEX_APP_SERVER_SOCK",
    "RIMZ_CODEX_CONFIG",
    "TMUX",
    "TMUX_TMPDIR",
    "TMUX_PANE",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_SOCKET_DIR",
    "ZELLIJ_PANE_ID",
    "CLAUDE_CODE_PROJECT_DIR_NAME",
];

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
            names.extend([
                "HOME",
                "PATH",
                "TMPDIR",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
                "XDG_STATE_HOME",
                "XDG_RUNTIME_DIR",
            ]);
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
        self.stream.set_write_timeout(Some(SPAWN_RETRY))?;
        write_line(
            &mut self.stream,
            &Request {
                through,
                reply_for: None,
                epoch: None,
                stop: false,
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
            },
        )?;
        read_line(&self.stream)
    }
}

pub struct DrainerLease {
    store: Store,
    connection: Connection,
}

impl DrainerLease {
    pub fn acquire(store: &Store) -> Result<Self, HookDrainErr> {
        let deadline = Instant::now() + SPAWN_RETRY;
        spawn(store)?;
        let connection = loop {
            match Connection::connect(store) {
                Ok(connection) => break connection,
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => std::thread::sleep(RETRY),
            }
        };
        connection.stream.set_nonblocking(true)?;
        Ok(Self {
            store: store.clone(),
            connection,
        })
    }

    pub fn refresh(&mut self) -> Result<(), HookDrainErr> {
        match self.connection.stream.read(&mut [0]) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            _ => {
                *self = Self::acquire(&self.store)?;
                Ok(())
            }
        }
    }
}

pub fn nudge(store: &Store, through: u64) -> Result<(), HookDrainErr> {
    if let Ok(mut stream) = UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()) {
        stream.set_write_timeout(Some(SPAWN_RETRY))?;
        return write_line(
            &mut stream,
            &Request {
                through,
                reply_for: None,
                epoch: None,
                stop: false,
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
    let log_path = store.paths().hook_drainer_log();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let mut command = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    command
        .args(["hooks", "drain", "--project-root"])
        .arg(&record.project_root)
        .stderr(log);
    for name in HOOK_ENV_NAMES {
        command.env_remove(name);
    }
    command.envs(crate::workspace::pin_env(
        &store.paths().workspace_id,
        &record.project_root,
    ));
    crate::child_process::spawn_detached_reaped(&mut command, "hook-drainer")?;
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
    };
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let mut stream = UnixStream::connect(&socket).ok();
    if stream.is_none() && spawn(store).is_ok() {
        let connect_until = deadline.min(Instant::now() + SPAWN_RETRY);
        while Instant::now() < connect_until {
            if let Ok(connected) = UnixStream::connect(&socket) {
                stream = Some(connected);
                break;
            }
            std::thread::sleep(RETRY);
        }
    }
    if let Some(mut stream) = stream {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            stream.set_read_timeout(Some(remaining))?;
            stream.set_write_timeout(Some(remaining))?;
            if write_line(&mut stream, &request).is_ok()
                && let Ok(receipt) = read_line::<DrainReceipt>(&stream)
            {
                return Ok(receipt);
            }
        }
    }
    request.through = inline_through;
    let _lifetime = WorkspaceLock::acquire_with_timeout(
        &store.runtime_paths().hook_drainer_lock(),
        deadline
            .saturating_duration_since(Instant::now())
            .min(LOCK_WAIT),
    )?;
    let mut drain = Drainer::default();
    drain.drain(store, &_lifetime, None, Some(deadline), |drain| {
        request.epoch = Some(drain.position(&request).0);
        let lock = WorkspaceLock::acquire_with_timeout(&store.paths().workspace_lock, LOCK_WAIT)?;
        drain.finish_if_drained(store, &lock)?;
        Ok(drain.position(&request).1 >= request.through)
    })?;
    Ok(drain.receipt(&request, None))
}

struct Client {
    stream: UnixStream,
    bytes: Vec<u8>,
    request: Option<Request>,
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
                    request: None,
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
                if client.request.is_some() {
                    false
                } else if let Some(end) = client.bytes.iter().position(|byte| *byte == b'\n') {
                    match serde_json::from_slice::<Request>(&client.bytes[..end]) {
                        Ok(mut request) => {
                            if request.through == 0 {
                                request.through = fs::metadata(&store.paths().hook_ingress_log)
                                    .map(|metadata| metadata.len())
                                    .unwrap_or(0);
                            }
                            client.request = Some(request);
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
            .any(|client| client.request.as_ref().is_some_and(|request| request.stop))
        {
            stopping = true;
            return Ok(true);
        }
        for client in clients.iter_mut() {
            if let Some(request) = client.request.as_mut() {
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
        let Some(request) = client.request.as_ref() else {
            index += 1;
            continue;
        };
        if !exhausted && drain.position(request).1 < request.through {
            index += 1;
            continue;
        }
        client.stream.set_nonblocking(false)?;
        client.stream.set_write_timeout(Some(SPAWN_RETRY))?;
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
    let mut drain = Drainer::default();
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
        if !socket_guard.is_current() {
            return Ok(());
        }
        if poll_clients(store, &listener, &mut clients)? {
            active = Instant::now();
        }
        if clients
            .iter()
            .any(|client| client.request.as_ref().is_some_and(|request| request.stop))
        {
            return Ok(());
        }
        if clients.iter().any(|client| client.request.is_some()) {
            for client in &mut clients {
                if let Some(request) = client.request.as_mut() {
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
    if !socket_guard.is_current() {
        return Ok(());
    }
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

struct CachedReply {
    epoch: EventId,
    value: Option<Value>,
    at: Instant,
}

#[derive(Default)]
struct Drainer {
    applied: u64,
    epoch: EventId,
    replies: HashMap<EventId, CachedReply>,
    completed: HashMap<EventId, (u64, Instant)>,
}

impl Drainer {
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
                let frames =
                    ingress::repair_and_read(store.paths(), cursor.applied, &lock, remaining())?;
                let identity = fs::metadata(&store.paths().hook_ingress_log)
                    .ok()
                    .map(|metadata| (metadata.dev(), metadata.ino()));
                (cursor, frames, identity)
            };
            self.applied = cursor.applied;
            if frames.is_empty() {
                return Ok(());
            }
            let recovering = cursor.claimed > cursor.applied;
            let mut expected = cursor.clone();
            for (frame, end) in frames {
                let Some(frame) = frame else {
                    let lock = workspace_lock()?;
                    validate_ownership(store, socket, &expected, identity)?;
                    let mut cursor = ingress::read_cursor(store.paths())?;
                    cursor.applied = end;
                    cursor.claimed = end;
                    cursor.applied_event_id = None;
                    cursor.claimed_event_id = None;
                    ingress::write_cursor(store.paths(), &cursor, &lock)?;
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
                {
                    let lock = workspace_lock()?;
                    validate_ownership(store, socket, &expected, identity)?;
                    let mut cursor = ingress::read_cursor(store.paths())?;
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
                    ingress::write_cursor(store.paths(), &cursor, &lock)?;
                    expected = cursor;
                }
                let processor = PROCESSOR.get().ok_or_else(|| {
                    io::Error::other("hook processor was not registered by the CLI")
                })?;
                let reply = match processor(
                    store,
                    &frame,
                    recovering.then_some(cursor.log_extent_at_claim),
                    lifetime,
                    Instant::now() + LOCK_WAIT,
                ) {
                    Ok(reply) => reply,
                    Err(error) => {
                        tracing::warn!(ingress = %frame.event_id, %error, "hook ingress apply failed");
                        crate::diag::hook_drain::append(
                            store.paths(),
                            crate::diag::hook_drain::HookDrainEvent::ApplyFailed {
                                ingress: frame.event_id.clone(),
                                error: format!("{error:#}"),
                            },
                        );
                        HookReply::Silent
                    }
                };
                #[cfg(feature = "testkit")]
                crate::testkit::rendezvous("RIMZ_TEST_HOOK_DRAIN_AFTER_APPLY");
                let lock =
                    WorkspaceLock::acquire_with_timeout(&store.paths().workspace_lock, LOCK_WAIT)?;
                validate_ownership(store, socket, &expected, identity)?;
                let mut cursor = ingress::read_cursor(store.paths())?;
                cursor.applied = end;
                cursor.applied_event_id = Some(frame.event_id.clone());
                ingress::write_cursor(store.paths(), &cursor, &lock)?;
                expected = cursor;
                self.applied = end;
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
        if !ingress::truncate_drained(store.paths(), lock)? {
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

    #[test]
    fn receipt_never_claims_unapplied_ingress() {
        let request = Request {
            through: 100,
            reply_for: None,
            epoch: None,
            stop: false,
        };
        assert_eq!(Drainer::default().receipt(&request, None).applied, 0);
    }
}
