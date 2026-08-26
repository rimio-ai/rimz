//! tmux presence fast path: forward control-mode overlays to sidebars.
//!
//! The elected producer holds one [`PresenceWatch`]
//! (`crate::mux::tmux`) on the session, subscribes to tmux's per-pane format
//! stream. tmux state and Zellij observations both flow through the host
//! projector that emits typed overlays. Identity-free topology lines stay as
//! [`PanesChanged`] nudges. Latency
//! only, never truth: the poll remains the presence backstop
//! (docs/internals/multiplexers.md), a dead watcher degrades to the
//! poll, and this thread respawns the client with backoff.
//!
//! One control client per workspace: only the eldest live instance (the same
//! election as the produce fork) attaches; the rest sleep on the election
//! poll. Demotion is rare (an elder appearing above a live producer), so it
//! is re-checked per nudge rather than mid-block.
//!
//! [`PanesChanged`]: crate::sidebar::events::SidebarEvent::PanesChanged

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::RuntimePaths;
use crate::diag::DiagSink;
use crate::ids::MuxName;
use crate::mux::tmux::{PresenceWatch, managed_server_socket_path};
use crate::sidebar::ProducerElectionTracker;
use crate::sidebar::presence::projector::project_presence;
use crate::sidebar::presence::tmux::TmuxPresenceState;
use tracing::debug;

/// Idle cadence for the producer-election re-check while not attached.
const ELECTION_POLL: Duration = Duration::from_secs(5);
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
    election: ProducerElectionTracker,
) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime, &session_name, &election))
}

fn watch_loop(runtime: &RuntimePaths, session_name: &str, election: &ProducerElectionTracker) {
    let control_socket = managed_server_socket_path();
    loop {
        if !is_producer(election) {
            std::thread::sleep(ELECTION_POLL);
            continue;
        }
        match PresenceWatch::attach(&control_socket, session_name) {
            Ok(mut watch) => {
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
                    // Demotion check per nudge: a demoted instance stops
                    // forwarding and releases its control client.
                    if !is_producer(election) {
                        break;
                    }
                    let now = Instant::now();
                    let deadline = seed_deadline.get_or_insert(now + SEED_WINDOW);
                    let seeding = now < *deadline;
                    let (transitions, boundary_move) = state.apply(line, seeding);
                    if let Some(event) = boundary_move {
                        diag.emit(event);
                    }
                    for event in project_presence(transitions) {
                        let _ =
                            crate::sidebar::wakeup::broadcast(runtime, Some(session_name), event);
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

fn is_producer(election: &ProducerElectionTracker) -> bool {
    election.elder_instance().is_none()
}
