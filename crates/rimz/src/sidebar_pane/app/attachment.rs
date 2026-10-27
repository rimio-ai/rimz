//! One sidebar pane's renderer: its wake channel, heartbeat, loop state, and frame loop over the pane's own output.
//!
//! The room host runs one attachment per pane it was handed. Nothing here reads the process terminal: geometry comes from the [`PaneBackend`], input and resize words from the supervisor through the host.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType};
use tracing::{debug, warn};

use crate::sidebar_pane::pixel::probe::escalate_own_pane_passthrough;
use crate::sidebar_pane::pixel::{PixelLease, PixelRenderCaps};
use crate::{MuxName, RuntimePaths};

use super::backend::PaneBackend;
use super::fetch::FetchRequest;
use super::inbox::WakeupInbox;
use super::input::Wakeup;
use super::loop_state::{BaselineReporter, LoopState};
use super::plane::DataPlane;
use super::socket::write_heartbeat;
use super::{Result, RuntimeFileGuard, ServeConfig};

/// Why an attachment's frame loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) enum AttachmentExit {
    /// Closed from outside, through its [`CloseHandle`].
    Closed,
    /// The pane's tab looks empty; the supervisor confirms against the mux.
    SelfClose,
}

/// Closes a running attachment from another thread.
#[derive(Clone)]
pub(in crate::sidebar_pane) struct CloseHandle {
    requested: Arc<AtomicBool>,
    pub(in crate::sidebar_pane) wake: SyncSender<Wakeup>,
    pub(in crate::sidebar_pane) input: SyncSender<Wakeup>,
}

impl CloseHandle {
    pub(in crate::sidebar_pane) fn close(&self) {
        self.requested.store(true, Ordering::SeqCst);
        // A full inbox already holds a wake; closing must never wait for a pane.
        let _ = self.wake.try_send(Wakeup::Tick);
    }
}

pub(in crate::sidebar_pane) struct Attachment {
    config: ServeConfig,
    runtime: RuntimePaths,
    diag: crate::diag::DiagSink,
    wake: SyncSender<Wakeup>,
    pub(in crate::sidebar_pane) inbox: Receiver<Wakeup>,
    input_wake: SyncSender<Wakeup>,
    pub(in crate::sidebar_pane) input: Receiver<Wakeup>,
    close_requested: Arc<AtomicBool>,
    // Dropped with the attachment, the heartbeat too: a lingering heartbeat
    // stays mtime-fresh for `SIDEBAR_HEARTBEAT_TTL`, during which `rimz`'s
    // freshness gate would skip relaunch and let a plain `attach` rebirth the
    // session with no sidebar.
    _heartbeat_cleanup: RuntimeFileGuard,
}

impl Attachment {
    /// Open the pane's bounded room and input inboxes before the host wires its senders.
    pub(in crate::sidebar_pane) fn open(
        config: ServeConfig,
        runtime: &RuntimePaths,
        diag: crate::diag::DiagSink,
    ) -> Self {
        let (wake, inbox) = sync_channel(64);
        let (input_wake, input) = sync_channel(64);
        Self {
            _heartbeat_cleanup: RuntimeFileGuard {
                path: runtime.sidebar_heartbeat_path(&config.instance_id),
            },
            config,
            runtime: runtime.clone(),
            diag,
            wake,
            inbox,
            input_wake,
            input,
            close_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(in crate::sidebar_pane) fn close_handle(&self) -> CloseHandle {
        CloseHandle {
            requested: self.close_requested.clone(),
            wake: self.wake.clone(),
            input: self.input_wake.clone(),
        }
    }

    /// Paint the pane until it closes. An error is the pane's output failing,
    /// which is how a closed pane first shows; either way its heartbeat is gone when this returns.
    pub(in crate::sidebar_pane) fn run(
        self,
        plane: &DataPlane,
        backend: PaneBackend,
        request_reload: Box<dyn Fn() + Send>,
        baseline_pane: Option<crate::ids::PaneId>,
        baseline_changed: BaselineReporter,
    ) -> Result<AttachmentExit> {
        let subscription = plane.subscribe(&self.config, self.wake.clone());
        let Self {
            config,
            runtime,
            diag,
            wake,
            inbox,
            input_wake: _input_wake,
            input,
            close_requested,
            _heartbeat_cleanup,
        } = self;
        let pet_render_caps = plane
            .caps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .detect(
                config.room.mux,
                &config.room.session_name,
                PixelRenderCaps::default(),
                &config.own_pane,
            );
        if config.room.mux == MuxName::Tmux
            && let Err(err) = escalate_own_pane_passthrough(&config.own_pane)
        {
            warn!(
                session = %config.room.session_name,
                error = %err,
                "sidebar pane passthrough escalation failed",
            );
        }
        let mut terminal = Terminal::new(backend)?;
        let titled = terminal.backend_mut().set_pane_title()?;
        terminal.backend_mut().clear_region(ClearType::All)?;
        terminal.swap_buffers();

        let initial_width = terminal.size().map(|s| s.width).ok();
        let host_wake_path =
            runtime.sidebar_host_wake_socket_path(config.room.mux, &config.room.session_name);
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
            crate::sidebar_pane::app::clock::LoopClock::wall(),
            config.clone(),
            baseline_pane,
            runtime.clone(),
            host_wake_path.clone(),
            diag.clone(),
            subscription.fetch,
            initial_width,
            subscription.observe,
            pet_render_caps,
            pixel_lease.as_ref().map(|lease| lease.slot),
        );
        state.pane.titled = titled;
        state.pane.room_caps = plane.caps.clone();
        state.share_width_geometry(plane.geometry.clone(), plane.tmux_geometry.clone());
        state.views.events = plane.events.clone();
        state.room_events = Some(plane.room_events(&runtime));
        state.share_views(plane.views.clone(), subscription.revision);
        state.pane.focus_peers = Some(plane.focus_peers(config.instance_id.clone(), diag.clone()));
        state.pane.wake = Some(wake);
        state.pane.request_reload = request_reload;
        state.pane.baseline_changed = baseline_changed;
        state.set_probed_aspect(terminal.backend().cell_aspect());
        if let Some(seed) = subscription.seed {
            state.seed(&seed);
        }
        // Zellij's percentage template needs a startup trim on capped wide views.
        // tmux births through its live absolute-column hook; its resize wakeups
        // own later convergence, avoiding a startup resize that can reflow the
        // sidebar's scrollback before the self-close paint hold is armed.
        if initial_width.is_some() && config.room.mux == MuxName::Zellij {
            state.run_width_control(
                &mut terminal,
                crate::diag::record::SidebarWidthControlTrigger::Retarget,
            );
        }
        // Write the heartbeat immediately so the freshness gate never sees a gap.
        // Errors are non-fatal; the gate re-probes after the TTL.
        if let Err(err) = write_heartbeat(
            &config,
            &runtime,
            &host_wake_path,
            terminal.backend().pane_size(),
        ) {
            warn!(
                session = %config.room.session_name,
                error = %err,
                "initial heartbeat write failed",
            );
        }

        // Fire the first fetch on the background worker and start the main loop
        // immediately rather than blocking on a synchronous call: the first fetch
        // can take several seconds (Zellij just started, git cold-start), and a
        // blocked main thread delays the self-close watchdog, stalling cleanup.
        // The plane's last fold renders while the correction is in flight; a
        // host with no fold yet starts with the placeholder. Every request
        // runs a cycle, even when the shared worker has nothing new to fold.
        state.pane.fetch.request(FetchRequest::default());

        // One fixed-timestep event loop. Events fold into the in-process model and
        // mark the frame dirty; the loop paints at most once per configured base
        // frame boundary, coalescing every change that landed mid-frame into a
        // single paint. Data and animation ride this frame grid; input paints
        // synchronously for instant feedback, with queued wheel ticks sharing
        // one paint (see `apply_input`), and a wheel glide then samples elapsed
        // time on a 16 ms grid until it settles. The grid stays warm while
        // there is something to show (`active`) and relaxes to the `tick`
        // backstop when idle, snapping back the instant an event or animation
        // arrives. Fetches run off-thread. tmux width backstops share one
        // plane-wide attempt; target refresh, structural, classification, and
        // commit events need fresh samples, as does a first press without stored
        // geometry. Cached presses and resize feedback never fork. On Zellij
        // the opening press of a burst re-reads the topology memo and room share,
        // a stat and one small share-file read, no fork. Width commits and
        // trailing capability refreshes run once after their bursts settle;
        // resize actuators run off-thread.
        let mut inbox = WakeupInbox::new(inbox).with_input(input);
        let loop_result: Result<()> = (|| {
            while !state.pane.should_exit && !close_requested.load(Ordering::SeqCst) {
                state.step(&mut inbox, &mut terminal)?;
            }
            Ok(())
        })();
        state.report_baseline();
        state.clear_pixel(&mut terminal);
        loop_result?;
        if state.pane.should_exit {
            debug!(
                session = %config.room.session_name,
                "sidebar tab emptied; exiting so the pane closes itself",
            );
            return Ok(AttachmentExit::SelfClose);
        }
        Ok(AttachmentExit::Closed)
    }
}
