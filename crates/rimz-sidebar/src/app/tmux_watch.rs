//! tmux presence fast path: forward control-mode topology nudges to the loop.
//!
//! The elected producer holds one read-only [`PresenceWatch`]
//! (`rimz::mux::tmux`) on the session and posts [`PANES_CHANGED_WAKEUP`] to
//! the serve loop's own socket whenever a window or split opens or closes. The
//! loop asks only the producer for fresh panes; after the producer publishes,
//! every consumer wakes from `pane_frame_published` and folds the cache. Latency
//! only, never truth: the poll remains the presence backstop
//! (docs/internals/multiplexers.md), a dead watcher degrades to exactly today's
//! behavior, and this thread respawns the client with backoff.
//! Zellij has the same producer-publication contract through its presence plugin.
//!
//! One control client per workspace: only the eldest live instance (the same
//! election as the produce fork) attaches; the rest sleep on the election
//! poll. Demotion is rare (an elder appearing above a live producer), so it
//! is re-checked per nudge rather than mid-block.

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;

use rimz::mux::tmux::{PresenceWatch, control_socket_from_env};
use rimz::{RuntimePaths, SidebarInstanceId};
use tracing::debug;

use super::input::PANES_CHANGED_WAKEUP;

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
    socket_path: PathBuf,
) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime, &instance_id, &session_name, &socket_path))
}

fn watch_loop(
    runtime: &RuntimePaths,
    instance_id: &SidebarInstanceId,
    session_name: &str,
    socket_path: &std::path::Path,
) {
    let Ok(waker) = UnixDatagram::unbound() else {
        return;
    };
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
                    let _ = waker.send_to(PANES_CHANGED_WAKEUP, socket_path);
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
    !rimz::sidebar::elder_sidebar_present(runtime, instance_id)
}
