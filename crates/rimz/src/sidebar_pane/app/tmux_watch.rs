//! tmux presence fast path: forward control-mode overlays to sidebars.
//!
//! The elected producer holds one read-only [`PresenceWatch`]
//! (`crate::mux::tmux`) on the session, subscribes to tmux's per-pane format
//! stream, and broadcasts the same typed overlays Zellij's presence plugin
//! emits. Identity-free topology lines stay as [`PanesChanged`] nudges. Latency
//! only, never truth: the poll remains the presence backstop
//! (docs/internals/mux/multiplexers.md), a dead watcher degrades to the
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

use crate::mux::tmux::{PresenceRoster, PresenceWatch, control_socket_from_env};
use crate::{RuntimePaths, SidebarInstanceId};
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
                crate::sidebar::cache::write_presence_stamp(runtime);
                let mut roster = PresenceRoster::default();
                let mut seed_deadline = None;
                while let Some(line) = watch.next_line() {
                    // Demotion check per nudge: a demoted instance stops
                    // forwarding and releases its control client.
                    if !is_producer(runtime, instance_id) {
                        break;
                    }
                    let now = Instant::now();
                    let deadline = seed_deadline.get_or_insert(now + SEED_WINDOW);
                    let seeding = now < *deadline;
                    for event in roster.apply(line, seeding) {
                        let _ = crate::store::wakeup::broadcast_sidebar_event(
                            runtime,
                            Some(session_name),
                            event,
                        );
                    }
                    crate::sidebar::cache::write_presence_stamp(runtime);
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
