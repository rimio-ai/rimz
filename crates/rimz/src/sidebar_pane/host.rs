//! The room host: one detached process that paints every sidebar pane of a mux session.
//!
//! Each pane's supervisor keeps the pane's tty, holding its modes and reading its input, and hands the host the pane's output fd over the attach wire (`attach.rs`), then forwards input words on that stream. The host runs one attachment per pane on its own thread over one shared data plane, so a room's tabs cost one fold between them. It holds no terminal: geometry comes from each fd. One room socket receives external events; the host filters their scope and routes them to bounded attachment channels.
//!
//! The host lives as long as panes are attached. It leaves when none has been for a grace period, and when the room records a build other than its own, after telling every supervisor to attach again.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, ErrorKind};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

mod reload;

use crate::RuntimePaths;
use crate::diag::focus_trace::{FocusTraceEvent, PushDelivery};
use crate::reload::{RecordChange, RecordWatch};
use crate::sidebar_pane::app::{
    Attachment, AttachmentExit, CloseHandle, DataPlane, PaneBackend, RoomConfig, RoomEvents,
    ServeConfig, SidebarAppErr, Wakeup, decode_wakeup,
};
use crate::sidebar_pane::attach::{
    self, Control, ControlLine, Hello, PROTOCOL, REJECT_BUILD, REJECT_CAPACITY, REJECT_PROTOCOL,
    Reply,
};
use crate::wakeup::events::{RELOAD_CONTROL_WORD, SidebarEvent};

/// How long a host with no pane attached stays up for the next one.
const UNATTACHED_GRACE: Duration = Duration::from_secs(10);
/// How long a leaving host waits for its panes to let go.
const DRAIN_GRACE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// A supervisor says hello in its first message; a silent peer is not one.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a new hello waits for the pane's previous attachment to close.
const EVICT_WAIT: Duration = Duration::from_secs(2);
/// A starting host outwaits the one it replaces, which holds the session's
/// lock until its panes have let go.
const SUCCESSION_WAIT: Duration = Duration::from_secs(12);
const MAX_ATTACHMENTS: usize = 512;
/// The descriptor soft limit the host asks for. Each attachment holds about
/// six descriptors for its lifetime and its mux commands add pipes briefly,
/// so a room of a few hundred panes outgrows the common 1,024 default.
const OPEN_FILE_TARGET: nix::libc::rlim_t = 65_536;
/// macOS refuses a soft limit above `OPEN_MAX` even when the hard limit is
/// unlimited, so a refused raise retries at this floor.
const OPEN_FILE_FALLBACK: nix::libc::rlim_t = 10_240;

/// Raise this process's descriptor soft limit toward its hard limit. Headroom
/// is enrichment: a refused raise warns and the host serves under the
/// inherited limit.
fn raise_open_file_limit() {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    let (soft, hard) = match getrlimit(Resource::RLIMIT_NOFILE) {
        Ok(limits) => limits,
        Err(err) => {
            warn!(%err, "sidebar host: cannot read the open-file limit");
            return;
        }
    };
    let mut last_err = None;
    for target in [OPEN_FILE_TARGET, OPEN_FILE_FALLBACK] {
        let wanted = open_file_soft_target(soft, hard, target);
        if wanted <= soft {
            return;
        }
        match setrlimit(Resource::RLIMIT_NOFILE, wanted, hard) {
            Ok(()) => {
                debug!(
                    from = soft,
                    to = wanted,
                    "sidebar host: raised open-file limit"
                );
                return;
            }
            Err(err) => last_err = Some(err),
        }
    }
    if let Some(err) = last_err {
        warn!(%err, soft, hard, "sidebar host: cannot raise the open-file limit; a large room may exhaust descriptors");
    }
}

/// The soft limit to request: `target` capped by `hard`, never below `soft`.
fn open_file_soft_target(
    soft: nix::libc::rlim_t,
    hard: nix::libc::rlim_t,
    target: nix::libc::rlim_t,
) -> nix::libc::rlim_t {
    target.min(hard).max(soft)
}

/// Paint a session's sidebar panes until none is left. Each hello supplies
/// its pane's identity and cadence.
pub fn run(room: RoomConfig) -> Result<(), SidebarAppErr> {
    crate::build_id::warm();
    // Leave the spawning pane's session, so no terminal is this process's
    // controlling one and closing that pane signals nothing here. Already
    // leading a session is the same outcome.
    let _ = nix::unistd::setsid();
    raise_open_file_limit();
    let runtime = RuntimePaths::for_workspace(room.workspace_id.clone())?;
    let state = crate::StatePaths::for_workspace(room.workspace_id.clone())?;
    if crate::Store::open_existing(state, runtime.clone()).is_none() {
        warn!(workspace = %room.workspace_id, "sidebar host: no room; run `rimz start` first");
        return Ok(());
    }
    runtime.ensure_dirs()?;
    let lock_path = runtime.sidebar_host_lock(room.mux, &room.session_name);
    let Ok(_lifetime) =
        crate::disk::lock::WorkspaceLock::acquire_with_timeout(&lock_path, SUCCESSION_WAIT)
    else {
        debug!(session = %room.session_name, "another sidebar host holds this session");
        return Ok(());
    };
    let socket_path = runtime.sidebar_host_socket_path(room.mux, &room.session_name);
    let listener = bind_listener(&socket_path)?;
    let wake =
        bind_wake_socket(&runtime.sidebar_host_wake_socket_path(room.mux, &room.session_name))?;
    let diag = crate::diag::DiagSink::for_workspace(
        room.workspace_id.clone(),
        room.session_name.clone(),
        None,
    );
    crate::sidebar_pane::app::install_panic_diagnostic_hook(diag.clone());
    let plane = DataPlane::start(&room, &runtime, &diag);

    let mut watch = RecordWatch::new(&room.workspace_id);
    let mut recorded = match crate::reload::recorded_reexec_target(&room.workspace_id) {
        crate::reload::WorkspaceReexecTarget::Verified(target) => Some(target.build),
        crate::reload::WorkspaceReexecTarget::Absent
        | crate::reload::WorkspaceReexecTarget::Invalid => None,
    };
    let host = Host::new(
        room,
        runtime,
        diag,
        plane,
        socket_path,
        crate::build_id::current().map(str::to_owned),
        Timing::default(),
    );
    host.serve(listener, wake, move || {
        match watch.poll_if_due(Instant::now()) {
            Some(RecordChange::Verified(target)) => recorded = Some(target.build),
            Some(RecordChange::Unavailable) => recorded = None,
            None => {}
        }
        recorded.clone()
    });
    Ok(())
}

/// Only the holder of the session's host lock binds: a socket file found here
/// is a dead host's.
fn bind_listener(path: &Path) -> io::Result<UnixListener> {
    crate::sock::validate_socket_path(path).map_err(io::Error::other)?;
    remove_socket(path)?;
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub(super) fn bind_wake_socket(path: &Path) -> io::Result<UnixDatagram> {
    crate::sock::validate_socket_path(path).map_err(io::Error::other)?;
    remove_socket(path)?;
    let socket = UnixDatagram::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    socket.set_read_timeout(Some(POLL_INTERVAL))?;
    Ok(socket)
}

fn remove_socket(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[derive(Clone, Copy)]
struct Timing {
    unattached_grace: Duration,
    drain_grace: Duration,
    poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            unattached_grace: UNATTACHED_GRACE,
            drain_grace: DRAIN_GRACE,
            poll: POLL_INTERVAL,
        }
    }
}

enum Event {
    Connection(UnixStream),
    Reload,
    /// A pane asked the host to leave; look now rather than at the next poll.
    Wake,
}

/// One attached pane, as the rest of the host reaches it.
struct Live {
    close: CloseHandle,
    /// Set when the host closes the pane to leave, so its supervisor is told
    /// to attach again rather than left to read the close as a failure.
    leaving: Arc<AtomicBool>,
}

struct Host {
    room: RoomConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    plane: DataPlane,
    room_events: RoomEvents,
    socket_path: PathBuf,
    build: Option<String>,
    reload_action: fn(&crate::WorkspaceId) -> reload::ReloadAction,
    timing: Timing,
    attachments: Mutex<BTreeMap<String, Live>>,
    /// Connections being served, from accept to the end of their attachment.
    connections: AtomicUsize,
    /// Leaving: the socket is gone, no pane is accepted, the attached close.
    draining: AtomicBool,
    events: Sender<Event>,
    inbox: Mutex<Option<Receiver<Event>>>,
    #[cfg(test)]
    before_admit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Host {
    fn new(
        room: RoomConfig,
        runtime: RuntimePaths,
        diag: crate::diag::DiagSink,
        plane: DataPlane,
        socket_path: PathBuf,
        build: Option<String>,
        timing: Timing,
    ) -> Arc<Self> {
        let (events, inbox) = std::sync::mpsc::channel();
        let room_events = plane.room_events(&runtime);
        Arc::new(Self {
            room,
            runtime,
            diag,
            plane,
            room_events,
            socket_path,
            build,
            reload_action: reload::reload_action,
            timing,
            attachments: Mutex::default(),
            connections: AtomicUsize::new(0),
            draining: AtomicBool::new(false),
            events,
            inbox: Mutex::new(Some(inbox)),
            #[cfg(test)]
            before_admit: Mutex::default(),
        })
    }

    fn receive_room(self: &Arc<Self>, wake: UnixDatagram) {
        let receiver = self.clone();
        std::thread::spawn(move || {
            let mut bytes = [0; 16 * 1024];
            while !receiver.draining.load(Ordering::SeqCst) {
                match wake.recv(&mut bytes) {
                    Ok(len) => receiver.room_wakeup(&bytes[..len]),
                    Err(err)
                        if matches!(
                            err.kind(),
                            ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                        ) => {}
                    Err(err) => {
                        debug!(error = %err, "sidebar host room receive failed");
                        break;
                    }
                }
            }
        });
    }

    /// Accept panes until the host has reason to leave: no pane for the
    /// grace, or `recorded_build` naming a build other than this one.
    fn serve(
        self: &Arc<Self>,
        listener: UnixListener,
        wake: UnixDatagram,
        mut recorded_build: impl FnMut() -> Option<String>,
    ) {
        let Some(inbox) = lock(&self.inbox).take() else {
            return;
        };
        self.receive_room(wake);
        let acceptor = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if acceptor.draining.load(Ordering::SeqCst) {
                    return;
                }
                match stream {
                    Ok(stream) => {
                        if acceptor.events.send(Event::Connection(stream)).is_err() {
                            return;
                        }
                    }
                    Err(err) => debug!(error = %err, "sidebar host accept failed"),
                }
            }
        });

        let mut unattached_since = Some(Instant::now());
        while !self.draining.load(Ordering::SeqCst) {
            let first = match inbox.recv_timeout(self.timing.poll) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut requested_reload = false;
            for event in first.into_iter().chain(inbox.try_iter()) {
                match event {
                    Event::Connection(stream) => {
                        self.connections.fetch_add(1, Ordering::SeqCst);
                        let host = self.clone();
                        if let Err(err) = std::thread::Builder::new()
                            .name("rimz-attach".into())
                            .spawn(move || {
                                host.serve_connection(stream);
                                host.connections.fetch_sub(1, Ordering::SeqCst);
                            })
                        {
                            self.connections.fetch_sub(1, Ordering::SeqCst);
                            debug!(error = %err, "sidebar host attachment spawn failed");
                        }
                    }
                    Event::Reload => requested_reload = true,
                    Event::Wake => {}
                }
            }
            if requested_reload {
                match (self.reload_action)(&self.room.workspace_id) {
                    reload::ReloadAction::Reexec(target) => {
                        debug!(session = %self.room.session_name, target = %target.display(),
                            "reload: on-disk binary differs; asking supervisor to re-exec");
                        self.begin_drain();
                    }
                    reload::ReloadAction::AlreadyCurrent => {
                        debug!(session = %self.room.session_name,
                            "reload: binary unchanged; refetching in place without re-exec");
                        self.plane.hard_refresh();
                    }
                    reload::ReloadAction::Missing => {
                        warn!(session = %self.room.session_name,
                            "reload requested but no renderer binary is on disk; refetching in place");
                        self.plane.hard_refresh();
                    }
                }
            }
            if let (Some(recorded), Some(own)) = (recorded_build(), self.build.as_ref())
                && recorded != *own
            {
                debug!(%recorded, %own, "sidebar host leaving for the recorded build");
                self.begin_drain();
            }
            if self.connections.load(Ordering::SeqCst) > 0 {
                unattached_since = None;
            } else if unattached_since.get_or_insert_with(Instant::now).elapsed()
                >= self.timing.unattached_grace
            {
                break;
            }
        }
        self.leave();
    }

    /// Stop taking panes. The socket goes first: a supervisor told to attach
    /// again must find no host here and start the next one, not be turned
    /// away by this one.
    fn begin_drain(&self) {
        let attachments = lock(&self.attachments);
        if self.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(err) = remove_socket(&self.socket_path) {
            warn!(error = %err, "sidebar host socket removal failed");
        }
        if let Err(err) = remove_socket(
            &self
                .runtime
                .sidebar_host_wake_socket_path(self.room.mux, &self.room.session_name),
        ) {
            warn!(error = %err, "sidebar host room socket removal failed");
        }
        for live in attachments.values() {
            live.leaving.store(true, Ordering::SeqCst);
            live.close.close();
        }
        let _ = self.events.send(Event::Wake);
    }

    /// Close every pane, telling each supervisor to attach again, and wait
    /// for them to let go.
    fn leave(&self) {
        self.begin_drain();
        // The acceptor is blocked in `accept` on a socket no path names any
        // more; it ends with the process.
        let deadline = Instant::now() + self.timing.drain_grace;
        while self.connections.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn serve_connection(&self, stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
        let (hello, output) = match attach::recv_hello(&stream) {
            Ok(Some(hello)) => hello,
            // A socket liveness probe: connected, said nothing, left.
            Ok(None) => return,
            Err(err) => {
                debug!(error = %err, "sidebar host dropped a malformed hello");
                return;
            }
        };
        let _ = stream.set_read_timeout(None);
        let (attachment, live) = match self.admit(&hello) {
            Ok(admitted) => admitted,
            Err(reason) => {
                debug!(instance = %hello.instance_id, %reason, "sidebar host rejected a pane");
                let _ = attach::write_line(&stream, &Reply::Reject { reason });
                return;
            }
        };
        let backend = match PaneBackend::for_fd(output) {
            Ok(backend) => backend,
            Err(err) => {
                self.release(&hello);
                let _ = attach::write_line(
                    &stream,
                    &Reply::Reject {
                        reason: format!("pane output unusable: {err}"),
                    },
                );
                return;
            }
        };
        let accept = Reply::Accept {
            build: self.build.clone(),
        };
        if attach::write_line(&stream, &accept).is_err() {
            self.release(&hello);
            return;
        }
        debug!(
            instance = %hello.instance_id,
            pane = %hello.pane_id,
            supervisor_build = ?hello.supervisor_build,
            "sidebar host attached a pane",
        );
        let report_stream = match stream.try_clone() {
            Ok(stream) => stream,
            Err(err) => {
                debug!(error = %err, "sidebar attach stream could not be cloned");
                self.release(&hello);
                return;
            }
        };
        read_input(&stream, live.close.clone());

        // One pane's panic closes that pane: its runtime files go as its frame loop unwinds, and its supervisor shows a notice and reattaches.
        let events = self.events.clone();
        let reload = Box::new(move || {
            let _ = events.send(Event::Reload);
        });
        let baseline_changed = Box::new(move |pane: Option<&crate::ids::PaneId>| {
            let _ = attach::write_line(
                &report_stream,
                &ControlLine {
                    control: Control::Baseline(pane.cloned()),
                },
            );
        });
        let exit = catch_unwind(AssertUnwindSafe(|| {
            attachment.run(
                &self.plane,
                backend,
                reload,
                hello.baseline_pane.clone(),
                baseline_changed,
            )
        }));
        self.release(&hello);
        let control = match exit {
            Ok(Ok(AttachmentExit::SelfClose)) => Some(Control::SelfClose),
            Ok(Ok(AttachmentExit::Closed)) if live.leaving.load(Ordering::SeqCst) => {
                Some(Control::Reload)
            }
            Ok(Ok(AttachmentExit::Closed)) => None,
            Ok(Err(err)) => {
                debug!(instance = %hello.instance_id, error = %err, "sidebar pane output closed");
                None
            }
            Err(_) => {
                warn!(instance = %hello.instance_id, "sidebar pane attachment panicked");
                None
            }
        };
        if let Some(control) = control {
            let _ = attach::write_line(&stream, &ControlLine { control });
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    /// Decide a hello and, on yes, open the pane's attachment and enter it in
    /// the host's set. The error is the reject reason.
    fn admit(&self, hello: &Hello) -> Result<(Attachment, Live), String> {
        if hello.protocol != PROTOCOL {
            return Err(REJECT_PROTOCOL.to_owned());
        }
        if self.draining.load(Ordering::SeqCst) {
            return Err(REJECT_BUILD.to_owned());
        }
        #[cfg(test)]
        if let Some(before_admit) = lock(&self.before_admit).take() {
            before_admit();
        }
        // A supervisor that re-exec'd says hello again under the same id
        // before the host has read its old stream's end.
        let key = hello.instance_id.as_str();
        let deadline = Instant::now() + EVICT_WAIT;
        loop {
            let mut attachments = lock(&self.attachments);
            if self.draining.load(Ordering::SeqCst) {
                return Err(REJECT_BUILD.to_owned());
            }
            match attachments.get(key) {
                Some(previous) if Instant::now() < deadline => previous.close.close(),
                Some(_) => return Err("pane is still attached".to_owned()),
                None if attachments.len() >= MAX_ATTACHMENTS => {
                    return Err(REJECT_CAPACITY.to_owned());
                }
                None => {
                    let attachment = Attachment::open(
                        self.config_for(hello),
                        &self.runtime,
                        self.diag.for_renderer(hello.instance_id.clone()),
                    );
                    let live = Live {
                        close: attachment.close_handle(),
                        leaving: Arc::new(AtomicBool::new(false)),
                    };
                    attachments.insert(
                        key.to_owned(),
                        Live {
                            close: live.close.clone(),
                            leaving: live.leaving.clone(),
                        },
                    );
                    return Ok((attachment, live));
                }
            }
            drop(attachments);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn room_wakeup(&self, bytes: &[u8]) {
        if bytes == RELOAD_CONTROL_WORD.as_bytes() {
            let _ = self.events.send(Event::Reload);
            return;
        }
        if bytes.first() == Some(&b'{') {
            let Wakeup::Event(envelope) = decode_wakeup(bytes) else {
                return;
            };
            if envelope.workspace_id != self.room.workspace_id
                || envelope
                    .session_name
                    .as_deref()
                    .is_some_and(|session| session != self.room.session_name)
            {
                return;
            }
            if envelope.event == SidebarEvent::Reload {
                let _ = self.events.send(Event::Reload);
                return;
            }
            self.room_events.dispatch(envelope);
            return;
        }
        let Some((instance, position)) = std::str::from_utf8(bytes)
            .ok()
            .and_then(|word| word.strip_prefix("click:"))
            .and_then(|word| word.split_once(':'))
        else {
            return;
        };
        let Some((column, row)) = position
            .split_once(':')
            .and_then(|(column, row)| Some((column.parse().ok()?, row.parse().ok()?)))
        else {
            return;
        };
        let Ok(instance_id) = crate::SidebarInstanceId::parse(instance) else {
            return;
        };
        let delivery = {
            let attachments = lock(&self.attachments);
            let Some(live) = attachments.get(instance) else {
                return;
            };
            match live
                .close
                .input
                .try_send(Wakeup::MouseClick { column, row })
            {
                Ok(()) => {
                    let _ = live.close.wake.try_send(Wakeup::Tick);
                    PushDelivery::Sent
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => PushDelivery::InboxFull,
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => PushDelivery::InboxClosed,
            }
        };
        self.diag.for_renderer(instance_id).trace_focus(
            FocusTraceEvent::ClickDelivery {
                column,
                row,
                delivery,
            },
            std::collections::HashSet::new(),
        );
    }

    fn release(&self, hello: &Hello) {
        lock(&self.attachments).remove(hello.instance_id.as_str());
    }

    fn config_for(&self, hello: &Hello) -> ServeConfig {
        ServeConfig {
            room: self.room.clone(),
            instance_id: hello.instance_id.clone(),
            own_pane: hello.pane_id.clone(),
            tick_seconds: hello.tick_seconds,
            refresh_ms_override: hello.refresh_ms,
        }
    }
}

/// Input words share the attach stream; EOF still releases the pane.
fn read_input(stream: &UnixStream, close: CloseHandle) {
    let Ok(stream) = stream.try_clone() else {
        return;
    };
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let wake = decode_wakeup(line.as_bytes());
            if matches!(wake, Wakeup::Tick | Wakeup::Event(_)) {
                continue;
            }
            if close.input.send(wake).is_err() {
                break;
            }
            let _ = close.wake.try_send(Wakeup::Tick);
        }
        close.close();
    });
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every guarded value here is whole between statements, so a panic
    // elsewhere while it was held leaves nothing half-written.
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests;
