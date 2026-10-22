//! tmux presence fast path: record and fan out control-mode events through the plane.
//!
//! The room host holds one [`PresenceWatch`]
//! (`crate::mux::tmux`) on the session, subscribes to tmux's per-pane format
//! stream. tmux lines and Zellij plugin observations both flow through the
//! shared projector (`project_presence`), which emits typed overlays; Zellij's
//! runs in the `rimz sidebar wake` process. Identity-free topology lines stay as
//! [`PanesChanged`] nudges. Latency
//! only, never truth: the poll remains the presence backstop
//! (docs/internals/multiplexers.md), a dead watcher degrades to the
//! poll, and this thread respawns the client with backoff.
//!
//! [`PanesChanged`]: crate::wakeup::events::SidebarEvent::PanesChanged

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::RuntimePaths;
use crate::diag::DiagSink;
use crate::ids::MuxName;
use crate::mux::tmux::{PresenceWatch, managed_server_socket_path};
use crate::sidebar::presence::projector::project_presence;
use crate::sidebar::presence::tmux::TmuxPresenceState;
use crate::utils::time::unix_now_ms;
use crate::wakeup::events::SidebarEventEnvelope;
use tracing::debug;

use super::hidden_views::Sweep;
use super::plane::RoomEvents;

/// Backoff between control-client attach attempts, so a refusing tmux (too
/// old for `-f no-output`, server restarting) never spins the thread.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(5);
/// Initial subscription values describe panes that already exist. Treat the
/// first short burst as roster seed so attaching a watcher never paints fake
/// opens for the room it found.
const SEED_WINDOW: Duration = Duration::from_millis(300);

/// Spawn the watcher manager thread. It runs for the process lifetime; the
/// control client child needs no explicit teardown — it exits on stdin EOF,
/// which process exit guarantees by closing the pipe.
pub(super) fn spawn(
    runtime: RuntimePaths,
    session_name: String,
    sweep: Sweep,
    room_events: RoomEvents,
) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime, &session_name, &sweep, &room_events))
}

fn watch_loop(runtime: &RuntimePaths, session_name: &str, sweep: &Sweep, room_events: &RoomEvents) {
    let control_socket = managed_server_socket_path();
    loop {
        match PresenceWatch::attach(&control_socket, session_name) {
            Ok(mut watch) => {
                sweep.kick();
                let diag =
                    DiagSink::for_workspace(runtime.workspace_id.clone(), session_name, None);
                crate::sidebar::cache::write_presence_stamp(
                    runtime,
                    MuxName::Tmux,
                    Some(session_name),
                );
                let mut state = TmuxPresenceState::default();
                let mut seed_deadline = None;
                while let Some(line) = watch.next_line() {
                    let now = Instant::now();
                    let deadline = seed_deadline.get_or_insert(now + SEED_WINDOW);
                    let seeding = now < *deadline;
                    if state.should_fit_hidden(&line) {
                        sweep.kick();
                    }
                    let (transitions, boundary_move) = state.apply(line, seeding);
                    if let Some(event) = boundary_move {
                        diag.emit(event);
                    }
                    for event in project_presence(transitions) {
                        room_events.dispatch(SidebarEventEnvelope::new(
                            runtime.workspace_id.clone(),
                            Some(session_name.to_owned()),
                            unix_now_ms(),
                            event,
                        ));
                    }
                    crate::sidebar::cache::write_presence_stamp(
                        runtime,
                        MuxName::Tmux,
                        Some(session_name),
                    );
                }
            }
            Err(err) => {
                debug!(error = %err, "tmux presence watch attach failed; poll remains truth");
            }
        }
        std::thread::sleep(RESPAWN_BACKOFF);
    }
}
