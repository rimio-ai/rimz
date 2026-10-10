//! The room host: one detached process that paints every sidebar pane of a mux session.
//!
//! Each pane's supervisor keeps the pane's tty, holding its modes and reading its input, and hands the host only the pane's output fd over the attach wire (`attach.rs`). The host runs one attachment per pane on its own thread over one shared data plane, so a room's tabs cost one fold between them. It holds no terminal: geometry comes from each fd, and input arrives on each attachment's wakeup socket.
//!
//! The host lives as long as panes are attached. It leaves when none has been for a grace period, and when the room records a build other than its own, after telling every supervisor to attach again.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::RuntimePaths;
use crate::sidebar_pane::app::{
    Attachment, AttachmentExit, CloseHandle, DataPlane, PaneBackend, ServeConfig, SidebarAppErr,
};
use crate::sidebar_pane::attach::{
    self, Control, ControlLine, Hello, PROTOCOL, REJECT_BUILD, REJECT_CAPACITY, REJECT_PROTOCOL,
    Reply,
};
use crate::sidebar_pane::supervise::{RecordChange, RecordWatch};

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

/// Paint a session's sidebar panes until none is left. `template` is the
/// configuration every pane shares; each hello supplies the pane's own
/// identity and cadence.
pub fn run(template: ServeConfig) -> Result<(), SidebarAppErr> {
    crate::build_id::warm();
    // Leave the spawning pane's session, so no terminal is this process's
    // controlling one and closing that pane signals nothing here. Already
    // leading a session is the same outcome.
    let _ = nix::unistd::setsid();
    let runtime = RuntimePaths::for_workspace(template.workspace_id.clone())?;
    let state = crate::StatePaths::for_workspace(template.workspace_id.clone())?;
    if crate::Store::open_existing(state, runtime.clone()).is_none() {
        warn!(workspace = %template.workspace_id, "sidebar host: no room; run `rimz start` first");
        return Ok(());
    }
    runtime.ensure_dirs()?;
    let lock_path = runtime.sidebar_host_lock(template.mux, &template.session_name);
    let Ok(_lifetime) =
        crate::disk::lock::WorkspaceLock::acquire_with_timeout(&lock_path, SUCCESSION_WAIT)
    else {
        debug!(session = %template.session_name, "another sidebar host holds this session");
        return Ok(());
    };
    let socket_path = runtime.sidebar_host_socket_path(template.mux, &template.session_name);
    let listener = bind_listener(&socket_path)?;
    let diag = crate::diag::DiagSink::for_workspace(
        template.workspace_id.clone(),
        template.session_name.clone(),
        None,
    );
    crate::sidebar_pane::app::install_panic_diagnostic_hook(diag.clone());
    let plane = DataPlane::start(&template, &runtime, &diag);

    let mut watch = RecordWatch::new(&template.workspace_id);
    let mut recorded = match crate::reload::recorded_reexec_target(&template.workspace_id) {
        crate::reload::WorkspaceReexecTarget::Verified(target) => Some(target.build),
        crate::reload::WorkspaceReexecTarget::Absent
        | crate::reload::WorkspaceReexecTarget::Invalid => None,
    };
    let host = Host::new(
        template,
        runtime,
        diag,
        plane,
        socket_path,
        crate::build_id::current().map(str::to_owned),
        Timing::default(),
    );
    host.serve(listener, move || {
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
    template: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    plane: DataPlane,
    socket_path: PathBuf,
    build: Option<String>,
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
        template: ServeConfig,
        runtime: RuntimePaths,
        diag: crate::diag::DiagSink,
        plane: DataPlane,
        socket_path: PathBuf,
        build: Option<String>,
        timing: Timing,
    ) -> Arc<Self> {
        let (events, inbox) = std::sync::mpsc::channel();
        Arc::new(Self {
            template,
            runtime,
            diag,
            plane,
            socket_path,
            build,
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

    /// Accept panes until the host has reason to leave: no pane for the
    /// grace, or `recorded_build` naming a build other than this one.
    fn serve(
        self: &Arc<Self>,
        listener: UnixListener,
        mut recorded_build: impl FnMut() -> Option<String>,
    ) {
        let Some(inbox) = lock(&self.inbox).take() else {
            return;
        };
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
            match inbox.recv_timeout(self.timing.poll) {
                Ok(Event::Connection(stream)) => {
                    self.connections.fetch_add(1, Ordering::SeqCst);
                    let host = self.clone();
                    std::thread::spawn(move || {
                        host.serve_connection(stream);
                        host.connections.fetch_sub(1, Ordering::SeqCst);
                    });
                }
                Ok(Event::Wake) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
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
            pane = ?hello.pane_id,
            supervisor_build = ?hello.supervisor_build,
            "sidebar host attached a pane",
        );
        watch_for_detach(&stream, live.close.clone());

        // One pane's panic closes that pane: its runtime files go as its frame loop unwinds, and its supervisor shows a notice and reattaches.
        let exit = catch_unwind(AssertUnwindSafe(|| attachment.run(&self.plane, backend)));
        self.release(&hello);
        let control = match exit {
            Ok(Ok(AttachmentExit::SelfClose)) => Some(Control::SelfClose),
            Ok(Ok(AttachmentExit::Reload)) => {
                self.begin_drain();
                Some(Control::Reload)
            }
            Ok(Ok(AttachmentExit::Closed)) if live.leaving.load(Ordering::SeqCst) => {
                Some(Control::Reload)
            }
            Ok(Ok(AttachmentExit::Closed | AttachmentExit::GaveUp)) => None,
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
                    )
                    .map_err(|err| format!("pane attachment failed: {err}"))?;
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

    fn release(&self, hello: &Hello) {
        lock(&self.attachments).remove(hello.instance_id.as_str());
    }

    fn config_for(&self, hello: &Hello) -> ServeConfig {
        ServeConfig {
            instance_id: hello.instance_id.clone(),
            own_pane: hello.pane_id.clone(),
            tick_seconds: hello.tick_seconds.unwrap_or(self.template.tick_seconds),
            refresh_ms_override: hello.refresh_ms,
            ..self.template.clone()
        }
    }
}

/// The supervisor sends nothing after its hello, so the stream only ever
/// reads its end: the supervisor closed, or died. Either way the pane is no
/// longer this host's to paint.
fn watch_for_detach(stream: &UnixStream, close: CloseHandle) {
    let Ok(mut stream) = stream.try_clone() else {
        return;
    };
    std::thread::spawn(move || {
        let mut sink = [0u8; 64];
        while matches!(io::Read::read(&mut stream, &mut sink), Ok(read) if read > 0) {}
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
