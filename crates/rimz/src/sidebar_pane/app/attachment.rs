//! One sidebar pane's renderer: its wakeup socket, heartbeat, loop state, and frame loop over the pane's own output.
//!
//! The room host runs one attachment per pane it was handed. Nothing here reads the process terminal: geometry comes from the [`PaneBackend`], input and resize words from the supervisor through the wakeup socket.

use std::io::Write;
use std::os::unix::net::UnixDatagram;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType};
use tracing::warn;

use crate::diag::record::{DiagEvent, RendererExitCause};
use crate::sidebar_pane::pixel::probe::escalate_own_pane_passthrough;
use crate::sidebar_pane::pixel::{PixelLease, PixelRenderCaps};
use crate::{MuxName, RuntimePaths};

use super::backend::PaneBackend;
use super::fetch::{FetchDispatcher, FetchRequest, FetchRole};
use super::input::wait_for_wakeup;
use super::loop_state::{LoopFlow, LoopState};
use super::plane::DataPlane;
use super::socket::{bind_socket, write_heartbeat};
use super::{Result, RuntimeFileGuard, ServeConfig, fetch_deadline_timeout};

/// Why an attachment's frame loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) enum AttachmentExit {
    /// Closed from outside, through its [`CloseHandle`].
    Closed,
    /// The pane's tab looks empty; the supervisor confirms against the mux.
    SelfClose,
    /// A different build is recorded for the room.
    Reload,
    /// The fold stayed degraded past the renderer's patience.
    GaveUp,
}

/// Closes a running attachment from another thread.
#[derive(Clone)]
pub(in crate::sidebar_pane) struct CloseHandle {
    requested: Arc<AtomicBool>,
    socket_path: PathBuf,
}

impl CloseHandle {
    pub(in crate::sidebar_pane) fn close(&self) {
        self.requested.store(true, Ordering::SeqCst);
        // A bare wake: the loop reads the request on its next turn. Best
        // effort, since a loop that already ended has no socket, and never
        // waiting, since a full inbox already holds a wake.
        if let Ok(waker) = UnixDatagram::unbound()
            && waker.set_nonblocking(true).is_ok()
        {
            let _ = waker.send_to(b"", &self.socket_path);
        }
    }
}

pub(in crate::sidebar_pane) struct Attachment {
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    socket: UnixDatagram,
    socket_path: PathBuf,
    close_requested: Arc<AtomicBool>,
    // Dropped with the attachment, the heartbeat too: a lingering heartbeat
    // stays mtime-fresh for `SIDEBAR_HEARTBEAT_TTL`, during which `rimz`'s
    // freshness gate would skip relaunch and let a plain `attach` rebirth the
    // session with no sidebar.
    _socket_cleanup: RuntimeFileGuard,
    _heartbeat_cleanup: RuntimeFileGuard,
}

impl Attachment {
    /// Bind the pane's wakeup socket, so input and resize words queue from
    /// here on, before the caller wires whatever sends them.
    pub(in crate::sidebar_pane) fn open(
        config: ServeConfig,
        runtime: &RuntimePaths,
        diag: crate::diag::DiagSink,
    ) -> Result<Self> {
        let socket_path = runtime.sidebar_socket_path(&config.instance_id);
        let socket = bind_socket(&socket_path)?;
        Ok(Self {
            _socket_cleanup: RuntimeFileGuard {
                path: socket_path.clone(),
            },
            _heartbeat_cleanup: RuntimeFileGuard {
                path: runtime.sidebar_heartbeat_path(&config.instance_id),
            },
            config,
            runtime: runtime.clone(),
            diag,
            socket,
            socket_path,
            close_requested: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(in crate::sidebar_pane) fn close_handle(&self) -> CloseHandle {
        CloseHandle {
            requested: self.close_requested.clone(),
            socket_path: self.socket_path.clone(),
        }
    }

    /// Paint the pane until it closes. An error is the pane's output failing,
    /// which is how a closed pane first shows; either way the wakeup socket
    /// and heartbeat are gone when this returns.
    pub(in crate::sidebar_pane) fn run(
        self,
        plane: &DataPlane,
        backend: PaneBackend,
    ) -> Result<AttachmentExit> {
        let Self {
            config,
            runtime,
            diag,
            socket,
            socket_path,
            close_requested,
            _socket_cleanup,
            _heartbeat_cleanup,
        } = self;
        let pet_render_caps = plane
            .caps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .detect(
                config.mux,
                &config.session_name,
                PixelRenderCaps::default(),
                config.own_pane.as_ref(),
            );
        if config.mux == MuxName::Tmux
            && let Err(err) = escalate_own_pane_passthrough(config.own_pane.as_ref())
        {
            warn!(
                session = %config.session_name,
                error = %err,
                "sidebar pane passthrough escalation failed",
            );
        }
        let mut terminal = Terminal::new(backend)?;
        let mut titled = set_pane_title(&mut terminal)?;
        terminal.backend_mut().clear_region(ClearType::All)?;
        terminal.swap_buffers();

        let initial_width = terminal.size().map(|s| s.width).ok();
        let subscription = plane.subscribe(&config, socket_path.clone());
        let pixel_lease = match PixelLease::acquire(&runtime) {
            Ok(Some(lease)) => Some(lease),
            Ok(None) => {
                warn!("sidebar pixel slots exhausted; using cell rendering for this pane");
                None
            }
            Err(err) => {
                warn!(error = %err, "sidebar pixel lease failed; using cell rendering for this pane");
                None
            }
        };
        let mut state = LoopState::new(
            config.clone(),
            runtime.clone(),
            socket_path.clone(),
            diag.clone(),
            subscription.results,
            initial_width,
            subscription.observe,
            pet_render_caps,
            pixel_lease.as_ref().map(|lease| lease.slot),
        );
        state.room_caps = plane.caps.clone();
        state.share_width_geometry(plane.geometry.clone());
        state.observe_events = plane.observe_events.clone();
        state.set_probed_aspect(terminal.backend().cell_aspect());
        state.seed_published(if plane.is_producer() {
            FetchRole::Producer
        } else {
            FetchRole::Consumer
        });
        // Zellij's percentage template needs a startup trim on capped wide views.
        // tmux births through its live absolute-column hook; its resize wakeups
        // own later convergence, avoiding a startup resize that can reflow the
        // sidebar's scrollback before the self-close paint hold is armed.
        if initial_width.is_some() && config.mux == MuxName::Zellij {
            state.run_width_control(
                &mut terminal,
                crate::diag::record::SidebarWidthControlTrigger::Retarget,
            );
        }
        // The dispatcher coalesces requests so a store-delta storm or a slow
        // produce can never queue more than one extra run. The fetch worker
        // posts `SNAPSHOT_WAKEUP` when a result is ready; the frame/tick path
        // also drains the result channel so that wakeup stays a latency hint.
        let mut fetch = FetchDispatcher::for_plane(subscription.requests, subscription.covered);

        // Write the heartbeat immediately so the freshness gate never sees a gap.
        // Errors are non-fatal; the gate re-probes after the TTL.
        if let Err(err) = write_heartbeat(
            &config,
            &runtime,
            &socket_path,
            terminal.backend().pane_size(),
        ) {
            warn!(
                session = %config.session_name,
                error = %err,
                "initial heartbeat write failed",
            );
        }

        // Fire the first fetch on the background worker and start the main loop
        // immediately rather than blocking on a synchronous call: the first fetch
        // can take several seconds (Zellij just started, git cold-start), and a
        // blocked main thread delays the self-close watchdog, stalling cleanup.
        // The published seed renders while the correction is in flight; only
        // a room with no valid publication starts with the placeholder. Forced,
        // because a worker other panes already share may have nothing new to fold.
        fetch.request(FetchRequest::force_fold(), false);

        // One fixed-timestep event loop. Events fold into the in-process model and
        // mark the frame dirty; the loop paints at most once per configured base
        // frame boundary, coalescing every change that landed mid-frame into a
        // single paint. Data and animation ride this frame grid; input paints
        // synchronously for instant feedback (see `apply_input`). The grid stays
        // warm while there is something to show (`active`) and relaxes to the
        // `tick` backstop when idle, snapping back the instant an event or
        // animation arrives. Fetches run off-thread. Width geometry probes still
        // fork on target refresh, structural, classification, and commit events
        // (or tmux's first press without cached geometry), never on cached
        // presses or resize feedback; on Zellij the opening press of a burst
        // re-reads the topology memo and room share, a stat and one small
        // share-file read, no fork. Width commits and trailing capability
        // refreshes run once after their bursts settle; resize actuators run
        // off-thread.
        let loop_result: Result<()> = (|| {
            while !state.should_exit && !close_requested.load(Ordering::SeqCst) {
                let (active, mut timeout) = state.frame_timing();
                timeout = fetch_deadline_timeout(timeout, fetch.next_deadline(), Instant::now());
                socket.set_read_timeout(Some(timeout))?;
                match state.on_wakeup(&mut fetch, &mut terminal, wait_for_wakeup(&socket)?)? {
                    LoopFlow::Continue => {}
                    LoopFlow::Repoll => continue,
                    LoopFlow::Exit => break,
                }
                // A pane the mux had not laid out at attach gets its title
                // with its first size.
                if !titled {
                    titled = set_pane_title(&mut terminal)?;
                }

                state.run_maintenance(&mut fetch, &terminal);
                state.run_width_control_backstop(&mut terminal);
                state.maybe_remind(&mut terminal);
                state.paint_frame_if_due(&mut terminal, active)?;
            }
            Ok(())
        })();
        state.clear_pixel(&mut terminal);
        loop_result?;
        if !state.reload_requested
            && !state.tab_emptied
            && let Some(cause) = state.exit_cause
        {
            diag.emit_unlimited(DiagEvent::RendererExit { cause });
        }
        Ok(
            if state.exit_cause == Some(RendererExitCause::DegradedGaveUp) {
                AttachmentExit::GaveUp
            } else if state.reload_requested {
                AttachmentExit::Reload
            } else if state.tab_emptied {
                AttachmentExit::SelfClose
            } else {
                AttachmentExit::Closed
            },
        )
    }
}

/// Name the pane as sidebar chrome. `false` while the pane is unsized, when
/// the backend drops everything written to it.
fn set_pane_title(terminal: &mut Terminal<PaneBackend>) -> std::io::Result<bool> {
    let backend = terminal.backend_mut();
    if backend.is_unsized() {
        return Ok(false);
    }
    write!(backend, "\x1b]2;{}\x07", crate::pane::SIDEBAR_CHROME_TITLE)?;
    Write::flush(backend)?;
    Ok(true)
}
