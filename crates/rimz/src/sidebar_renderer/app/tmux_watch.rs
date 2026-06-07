//! tmux presence fast path: forward control-mode topology nudges to sidebars.
//!
//! The elected producer holds one read-only [`PresenceWatch`]
//! (`crate::mux::tmux`) on the session and broadcasts a typed [`PanesChanged`]
//! nudge whenever a window or split opens or closes — control-mode lines say
//! *that* topology moved, not which pane, so the identity-free nudge is the
//! honest event. Every renderer receives it; only the elected producer pays
//! the fresh pane pull. Latency only, never truth: the poll remains the
//! presence backstop (docs/internals/multiplexers.md), a dead watcher degrades
//! to the poll, and this thread respawns the client with backoff.
//! Zellij has the same producer-publication contract through its presence plugin.
//!
//! One control client per workspace: only the eldest live instance (the same
//! election as the produce fork) attaches; the rest sleep on the election
//! poll. Demotion is rare (an elder appearing above a live producer), so it
//! is re-checked per nudge rather than mid-block.
//!
//! [`PanesChanged`]: SidebarEvent::PanesChanged

use std::thread::JoinHandle;
use std::time::Duration;

use crate::mux::tmux::{PresenceWatch, control_socket_from_env};
use crate::schema::sidebar_event::SidebarEvent;
use crate::{RuntimePaths, SidebarInstanceId};
use tracing::debug;

/// Idle cadence for the producer-election re-check while not attached.
const ELECTION_POLL: Duration = Duration::from_secs(5);
/// Backoff between control-client attach attempts, so a refusing tmux (too
/// old for `-f no-output`, server restarting) never spins the thread.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(5);

/// Spawn the watcher manager thread. It runs for the process lifetime; the
/// control client child needs no explicit teardown — it exits on stdin EOF,
/// which process exit guarantees by closing the pipe.
pub(super) fn spawn(
    runtime: RuntimePaths,
    instance_id: SidebarInstanceId,
    session_name: String,
) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime, &instance_id, &session_name))
}

fn watch_loop(runtime: &RuntimePaths, instance_id: &SidebarInstanceId, session_name: &str) {
    let control_socket = control_socket_from_env();
    loop {
        if !is_producer(runtime, instance_id) {
            std::thread::sleep(ELECTION_POLL);
            continue;
        }
        match PresenceWatch::attach(control_socket.as_deref(), session_name) {
            Ok(mut watch) => {
                while watch.next_presence().is_some() {
                    // Demotion check per nudge: a demoted instance stops
                    // forwarding and releases its control client.
                    if !is_producer(runtime, instance_id) {
                        break;
                    }
                    let _ = crate::ledger::wakeup::broadcast_sidebar_event(
                        runtime,
                        Some(session_name),
                        SidebarEvent::PanesChanged,
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

fn is_producer(runtime: &RuntimePaths, instance_id: &SidebarInstanceId) -> bool {
    !crate::sidebar::elder_sidebar_present(runtime, instance_id)
}
