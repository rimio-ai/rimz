use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read as _, Write};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::ids::WorkspaceId;
use rimz::remote::browser::{OpenReply, OpenRequest, OpenStatus};
use rimz::remote::link::{LinkAck, LinkOpen, LinkProbe, LinkStatsFile};
use rimz::room::session::workspace_record_for_session;

const LINK_SCHEMA_MISMATCH_EXIT: i32 = 2;
const PORTS_SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const PORTS_SWEEP_ENV: &str = "RIMZ_PORTS_SWEEP_MS";
const PROC_ROOT_ENV: &str = "RIMZ_PROC_ROOT";
/// How long a connected browser helper has to send its request, and the
/// ingest to answer it.
const OPEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const OPEN_REQUEST_LIMIT: u64 = 64 * 1024;
/// How often the open socket's thread checks that its socket file is still there.
const OPEN_REBIND_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(super) struct RoomArgs {
    /// Existing room session name.
    #[arg(long)]
    session: Option<String>,
    /// Room directory, resolved like `rimz start <dir>`.
    #[arg(long)]
    dir: Option<PathBuf>,
}

pub(super) fn ingest(args: RoomArgs) -> Result<()> {
    let (store, workspace_id) = room_store(args)?;
    let runtime = store.runtime_paths();
    let client = link_client_id();
    runtime
        .ensure_dirs()
        .context("preparing runtime directories for link stats")?;
    let path = rimz::remote::link::stats_path(runtime);
    let result = ingest_probes(&path, &client, workspace_id, runtime);
    remove_stats_if_owned(&path, &client);
    result
}

fn ingest_probes(
    path: &Path,
    client: &str,
    workspace_id: WorkspaceId,
    runtime: &rimz::RuntimePaths,
) -> Result<()> {
    let stdin = std::io::stdin();
    let mut ports = PortReporter::new(workspace_id);
    let mut relay: Option<(Arc<Mutex<LinkPresence>>, OpenListener)> = None;
    for line in stdin.lock().lines() {
        let line = line.context("reading link probe")?;
        if line.trim().is_empty() {
            continue;
        }
        let probe: LinkProbe = serde_json::from_str(&line).context("parsing link probe")?;
        if !probe.version_ok() {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "unsupported link probe schema `{}`", probe.v);
            remove_stats_if_owned(path, client);
            drop(relay);
            std::process::exit(LINK_SCHEMA_MISMATCH_EXIT);
        }
        let received_at_ms = rimz::utils::time::unix_now_ms();
        if probe.opens_urls {
            let now = LinkPresence {
                active_at_ms: rimz::remote::browser::active_at_ms(received_at_ms, probe.idle_ms),
                probed_at: Instant::now(),
                fresh_for: rimz::remote::browser::open_fresh_for(probe.interval_ms),
            };
            match &relay {
                Some((presence, _)) => {
                    *presence.lock().unwrap_or_else(PoisonError::into_inner) = now
                }
                None => {
                    let presence = Arc::new(Mutex::new(now));
                    relay = OpenListener::start(runtime, Arc::clone(&presence))
                        .map(|listener| (presence, listener));
                }
            }
        }
        let mut file = LinkStatsFile::new(received_at_ms, client.to_owned(), probe.stats.clone());
        file.forwards = probe.forwards;
        rimz::disk::atomic::write_temp_then_rename_cache(path, &file)
            .with_context(|| format!("writing {}", path.display()))?;
        let ack = match ports.report() {
            Some(ports) => LinkAck::with_ports(probe.seq, ports),
            None => LinkAck::new(probe.seq),
        };
        write_json_line(&mut std::io::stdout().lock(), &ack).context("writing link ack")?;
    }
    Ok(())
}

fn write_json_line(writer: &mut impl Write, value: &impl serde::Serialize) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(std::io::Error::other)?;
    writeln!(writer)?;
    writer.flush()
}

/// What this link tells a browser helper, as of the laptop's last capable probe.
struct LinkPresence {
    active_at_ms: Option<u64>,
    probed_at: Instant,
    fresh_for: Duration,
}

/// The status for a helper, or `None` once the laptop has gone quiet: one that
/// slept or lost its network leaves this stream open with nobody reading it.
fn fresh_status(presence: &Mutex<LinkPresence>) -> Option<OpenStatus> {
    let presence = presence.lock().unwrap_or_else(PoisonError::into_inner);
    (presence.probed_at.elapsed() <= presence.fresh_for)
        .then(|| OpenStatus::new(presence.active_at_ms))
}

/// The socket through which a room pane's browser helper hands this link a URL,
/// open while the laptop says it opens URLs. One thread serves helpers in turn,
/// each bounded by its timeouts, and binds the socket again when a room reset
/// removes it.
struct OpenListener {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl OpenListener {
    fn start(runtime: &rimz::RuntimePaths, presence: Arc<Mutex<LinkPresence>>) -> Option<Self> {
        let path = match rimz::remote::browser::socket_path(runtime, std::process::id()) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, "link opens no relayed URLs");
                return None;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_path, thread_stop) = (path.clone(), Arc::clone(&stop));
        let sock_dir = runtime.sock_dir.clone();
        std::thread::spawn(move || serve_opens(&thread_path, &sock_dir, &presence, &thread_stop));
        Some(Self { path, stop })
    }
}

impl Drop for OpenListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.path);
    }
}

fn serve_opens(path: &Path, sock_dir: &Path, presence: &Mutex<LinkPresence>, stop: &AtomicBool) {
    let mut bound: Option<(UnixListener, u64)> = None;
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        if bound
            .as_ref()
            .is_none_or(|(_, inode)| socket_inode(path) != Some(*inode))
        {
            bound = None;
            // A reset removed the directory: the reborn room makes it again.
            if sock_dir.is_dir() {
                match bind_open_socket(path, sock_dir) {
                    Ok(listener) => bound = Some(listener),
                    Err(error) if !warned => {
                        tracing::warn!(error = %format!("{error:#}"), "link opens no relayed URLs");
                        warned = true;
                    }
                    Err(_) => {}
                }
            }
        }
        let Some((listener, _)) = &bound else {
            std::thread::sleep(OPEN_REBIND_INTERVAL);
            continue;
        };
        if !accept_ready(listener) {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = serve_open(&stream, presence) {
                    tracing::debug!(%error, "browser helper connection ended");
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) if matches!(error.raw_os_error(), Some(code) if code == nix::libc::EMFILE || code == nix::libc::ENFILE) =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                tracing::debug!(%error, "browser helper accept failed; binding again");
                bound = None;
                std::thread::sleep(OPEN_REBIND_INTERVAL);
            }
        }
    }
}

/// Wait up to [`OPEN_REBIND_INTERVAL`] for a helper to connect, so the loop also
/// notices a removed socket and a stop.
fn accept_ready(listener: &UnixListener) -> bool {
    use std::os::fd::AsFd as _;

    let mut fds = [nix::poll::PollFd::new(
        listener.as_fd(),
        nix::poll::PollFlags::POLLIN,
    )];
    let timeout = nix::poll::PollTimeout::try_from(OPEN_REBIND_INTERVAL)
        .unwrap_or(nix::poll::PollTimeout::MAX);
    nix::poll::poll(&mut fds, timeout).is_ok_and(|ready| ready > 0)
}

fn socket_inode(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.ino())
}

fn bind_open_socket(path: &Path, sock_dir: &Path) -> Result<(UnixListener, u64)> {
    remove_dead_open_sockets(sock_dir);
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("preparing {}", path.display()))?;
    let inode = socket_inode(path).with_context(|| format!("reading {}", path.display()))?;
    Ok((listener, inode))
}

/// Remove the sockets of ingests that died without cleaning up (a killed link),
/// named by their process id.
fn remove_dead_open_sockets(sock_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(sock_dir) else {
        return;
    };
    for entry in entries.map_while(std::result::Result::ok) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(rimz::remote::browser::socket_pid)
        else {
            continue;
        };
        let Ok(raw) = i32::try_from(pid) else {
            continue;
        };
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None)
            == Err(nix::errno::Errno::ESRCH)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Answer one helper: say when this laptop was last active, then put the URL it
/// sends on the probe stream and say whether that write landed. A link gone
/// quiet answers nothing, so the helper moves on.
fn serve_open(stream: &UnixStream, presence: &Mutex<LinkPresence>) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(OPEN_REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(OPEN_REQUEST_TIMEOUT))?;
    let Some(status) = fresh_status(presence) else {
        return Ok(());
    };
    write_json_line(&mut &*stream, &status)?;
    let mut line = String::new();
    BufReader::new(stream.take(OPEN_REQUEST_LIMIT)).read_line(&mut line)?;
    if line.is_empty() {
        return Ok(());
    }
    let reply = match serde_json::from_str::<OpenRequest>(&line) {
        Ok(_) if fresh_status(presence).is_none() => {
            OpenReply::refused("the laptop has gone quiet")
        }
        Ok(request) if request.version_ok() => relay_open(&request.url),
        _ => OpenReply::refused("unreadable open request"),
    };
    write_json_line(&mut &*stream, &reply)
}

fn relay_open(url: &str) -> OpenReply {
    if let Err(refusal) = rimz::remote::browser::web_url(url) {
        return OpenReply::refused(refusal.to_string());
    }
    match write_json_line(&mut std::io::stdout().lock(), &LinkOpen::new(url)) {
        Ok(()) => OpenReply::sent(),
        Err(error) => OpenReply::refused(format!("the link stream is closed: {error}")),
    }
}

struct PortReporter {
    interval: Duration,
    last_sweep: Option<Instant>,
    ports: Option<Vec<u16>>,
    proc_root: PathBuf,
    workspace_id: WorkspaceId,
    // Positive and negative verdicts live only while their candidate inode stays listed.
    verdicts: BTreeMap<u64, bool>,
}

impl PortReporter {
    fn new(workspace_id: WorkspaceId) -> Self {
        let interval = std::env::var(PORTS_SWEEP_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(PORTS_SWEEP_INTERVAL);
        Self {
            interval,
            last_sweep: None,
            ports: None,
            proc_root: std::env::var_os(PROC_ROOT_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/proc")),
            workspace_id,
            verdicts: BTreeMap::new(),
        }
    }

    fn report(&mut self) -> Option<Vec<u16>> {
        if self
            .last_sweep
            .is_none_or(|last| last.elapsed() >= self.interval)
        {
            self.ports =
                read_candidate_ports(&self.proc_root, &self.workspace_id, &mut self.verdicts);
            self.last_sweep = Some(Instant::now());
        }
        self.ports.clone()
    }
}

#[cfg(target_os = "linux")]
fn read_candidate_ports(
    root: &Path,
    workspace_id: &WorkspaceId,
    verdicts: &mut BTreeMap<u64, bool>,
) -> Option<Vec<u16>> {
    let tcp = std::fs::read_to_string(root.join("net/tcp")).ok()?;
    let tcp6 = std::fs::read_to_string(root.join("net/tcp6")).ok()?;
    let listeners =
        rimz::remote::forward::candidate_listeners(&tcp, &tcp6, nix::unistd::getuid().as_raw());
    let inodes = listeners
        .iter()
        .map(|listener| listener.inode)
        .collect::<BTreeSet<_>>();
    verdicts.retain(|inode, _| inodes.contains(inode));
    let unknown = inodes
        .iter()
        .filter(|inode| !verdicts.contains_key(*inode))
        .copied()
        .collect();
    let room_inodes = rimz::proc::socket_inodes_for_env(
        root,
        nix::unistd::getuid().as_raw(),
        rimz::workspace::ENV_WORKSPACE_ID,
        workspace_id.as_str(),
        &unknown,
    );
    for inode in unknown {
        verdicts.insert(inode, room_inodes.contains(&inode));
    }
    let room_inodes = verdicts
        .iter()
        .filter_map(|(inode, in_room)| in_room.then_some(*inode))
        .collect();
    Some(rimz::remote::forward::candidate_ports(
        &listeners,
        &room_inodes,
    ))
}

#[cfg(not(target_os = "linux"))]
fn read_candidate_ports(
    _root: &Path,
    _workspace_id: &WorkspaceId,
    _verdicts: &mut BTreeMap<u64, bool>,
) -> Option<Vec<u16>> {
    None
}

fn remove_stats_if_owned(path: &Path, client: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(file) = serde_json::from_slice::<LinkStatsFile>(&bytes) else {
        return;
    };
    if file.client == client {
        let _ = std::fs::remove_file(path);
    }
}

/// The room a host-side helper serves, resolved from `--session` or `--dir`;
/// refuses when the room has no store.
pub(super) fn room_store(args: RoomArgs) -> Result<(rimz::Store, WorkspaceId)> {
    let (project_root, workspace_id) = match (args.session, args.dir) {
        (Some(session), None) => {
            let room = workspace_record_for_session(&session)?
                .with_context(|| format!("no RimZ workspace record for session `{session}`"))?;
            (room.project_root, room.workspace_id)
        }
        (None, Some(dir)) => {
            let room = rimz::WorkspaceResolver::resolve(&dir, None)
                .with_context(|| format!("resolving remote room dir {}", dir.display()))?;
            (room.project_root, room.workspace_id)
        }
        _ => bail!("give exactly one of --session or --dir"),
    };
    let state = rimz::StatePaths::for_project_root(&project_root)?;
    let store = crate::cli::open_existing_store_at(state)?
        .with_context(|| crate::cli::no_room(format_args!("at {}", project_root.display())))?;
    Ok((store, workspace_id))
}

fn link_client_id() -> String {
    std::env::var("SSH_CONNECTION").unwrap_or_else(|_| "ssh".to_owned())
}
