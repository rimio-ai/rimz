//! Best-effort wakeup datagrams posted after every ledger mutation.
//!
//! Two channels:
//!
//! * **Per-request feed socket** — the waiting hook or script bound a socket
//!   via [`crate::bridge::bind`]; when a resolver writes the new feed state,
//!   the writer sends a small `feed_resolved` datagram so the waiter can
//!   exit before its polling tick fires.
//! * **Sidebar wakeup sockets** — each live sidebar instance writes a
//!   heartbeat JSON under `runtime/heartbeat/sidebar.*.json` carrying the
//!   path of a datagram socket it owns. After every mutation we walk the
//!   heartbeat directory and post a typed `LedgerDelta` event to each fresh
//!   socket.
//!
//! Per the docs: "Sidebar wakeups are latency, not truth." Per-target send
//! failures are absorbed (logged at `debug`); only directory-read failures
//! propagate. Stale heartbeats (older than [`SIDEBAR_HEARTBEAT_TTL`]) are
//! skipped.

use std::fs;
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use tracing::debug;

use crate::bridge::{WakeupFrame, feed_socket_path, run_socket_path};
use crate::feed::FeedItem;
use crate::ledger::RuntimePaths;
use crate::run::RunRecord;
use crate::schema::SIDEBAR_PROTOCOL_VERSION;
use crate::schema::event::{EventEnvelope, EventKind};
use crate::schema::heartbeat::SidebarHeartbeat;
use crate::schema::sidebar_event::{SidebarEvent, SidebarEventEnvelope};
pub use crate::sidebar::timing::SIDEBAR_HEARTBEAT_TTL;

#[derive(Debug, thiserror::Error)]
pub enum WakeupErr {
    #[error("reading sidebar heartbeat dir {path}: {source}")]
    ReadDir {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("serializing wakeup frame: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("creating sender socket: {0}")]
    Sender(#[source] io::Error),
}

pub type Result<T> = std::result::Result<T, WakeupErr>;

/// Send a `feed_resolved` datagram to the per-request socket bound by the
/// waiting hook or script. No-op for `native_ui` items (no waiter exists).
pub fn wake_per_request(rt: &RuntimePaths, item: &FeedItem) -> Result<()> {
    if !item.surface.hook_blocks() {
        return Ok(());
    }
    let target = feed_socket_path(rt, &item.request_id);
    if !target.exists() {
        // Common: a `feed ask` script exited before resolution (e.g. timed
        // out and removed its socket), or the waiter hasn't bound yet on a
        // very-fast race. The feed file on disk is still authoritative.
        return Ok(());
    }
    let frame = WakeupFrame::FeedResolved {
        workspace_id: item.workspace_id.clone(),
        request_id: item.request_id.clone(),
        nonce: item.nonce.clone(),
    };
    let payload = serde_json::to_vec(&frame)?;
    send_datagram(&payload, &target);
    Ok(())
}

/// Send a `run_completed` datagram to the `rimz run` waiter. The run record on
/// disk is authoritative; this socket only cuts latency for the blocking CLI.
pub fn wake_run(rt: &RuntimePaths, record: &RunRecord) -> Result<()> {
    let target = run_socket_path(rt, &record.run_id);
    if !target.exists() {
        return Ok(());
    }
    let frame = WakeupFrame::RunCompleted {
        workspace_id: record.workspace_id.clone(),
        run_id: record.run_id.clone(),
        status: record.status,
    };
    let payload = serde_json::to_vec(&frame)?;
    send_datagram(&payload, &target);
    Ok(())
}

/// Walk the runtime heartbeat dir and post a typed `LedgerDelta` event to
/// each fresh sidebar's `wakeup_socket`. Per-target failures are logged
/// and skipped — they never error the write that triggered us. Feed
/// mutations and context-sidecar writes (Claude's statusline `$`/token
/// update, the Codex rollout refresh) both land here, so a cost change
/// repaints within a wakeup instead of waiting for the renderer's next
/// poll tick.
pub fn wake_sidebars(rt: &RuntimePaths) -> Result<()> {
    broadcast_sidebar_event(
        rt,
        None,
        SidebarEvent::LedgerDelta {
            event_method: None,
            agent_event_name: None,
        },
    )?;
    Ok(())
}

pub fn wake_sidebars_for_event(rt: &RuntimePaths, event: &EventEnvelope) -> Result<()> {
    broadcast_sidebar_event(
        rt,
        None,
        SidebarEvent::LedgerDelta {
            event_method: Some(event.method.clone()),
            agent_event_name: agent_event_name(event),
        },
    )?;
    Ok(())
}

fn agent_event_name(event: &EventEnvelope) -> Option<String> {
    match event.kind() {
        EventKind::AgentLifecycle(payload) => {
            let payload = *payload;
            payload.event_name
        }
        EventKind::SessionRebirth | EventKind::Other { .. } => None,
    }
}

/// Tell every fresh sidebar of this workspace to re-exec its own binary, so it
/// picks up a freshly-installed renderer in place — no session rebirth, no pane
/// churn. One-shot `rimz sidebar snapshot` calls pick up the installed binary on
/// every run; this covers the long-lived renderer process that now owns the
/// in-process produce path. Returns how many sidebars were
/// signaled. A wedged or already-dead sidebar receives nothing; relaunch it via
/// `rimz start`/`rimz attach` instead.
pub fn reload_sidebars(rt: &RuntimePaths) -> Result<usize> {
    broadcast_sidebar_event(rt, None, SidebarEvent::Reload)
}

/// Post one typed event envelope to every fresh, protocol-current sidebar of
/// this workspace. `session_name: Some` scopes the event to the mux session
/// whose pane ids it names; `None` is workspace-scoped. Returns the number of
/// sidebars targeted; send failures are absorbed per target.
pub fn broadcast_sidebar_event(
    rt: &RuntimePaths,
    session_name: Option<&str>,
    event: SidebarEvent,
) -> Result<usize> {
    let sidebars = collect_fresh_sidebars(rt)?;
    let signaled = sidebars.len();
    let Some(sender) = sender_socket() else {
        return Ok(signaled);
    };
    let payload = serde_json::to_vec(&SidebarEventEnvelope::new(
        rt.workspace_id.clone(),
        session_name.map(str::to_owned),
        crate::sidebar::cache::unix_now_ms(),
        event,
    ))?;
    send_datagrams_with(
        &sender,
        &payload,
        sidebars.iter().map(|hb| hb.wakeup_socket.as_path()),
    );
    Ok(signaled)
}

/// Broadcast a typed `PaneFramePublished` event to every fresh, protocol-current
/// sidebar after the producer publishes a fresh shared pane frame.
pub fn wake_sidebars_pane_frame_published(rt: &RuntimePaths) -> Result<usize> {
    broadcast_sidebar_event(rt, None, SidebarEvent::PaneFramePublished)
}

/// Walk the runtime heartbeat dir and return every sidebar heartbeat that is
/// readable, on the current protocol, and fresh (including a TOCTOU re-stat
/// just before return). Both the ledger wakeup fanout and `reload` share this
/// so the freshness contract lives in one place.
fn collect_fresh_sidebars(rt: &RuntimePaths) -> Result<Vec<SidebarHeartbeat>> {
    let entries = match fs::read_dir(&rt.heartbeat_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(WakeupErr::ReadDir {
                path: rt.heartbeat_dir.clone(),
                source,
            });
        }
    };

    let now = Timestamp::now();
    let mut fresh = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !SidebarHeartbeat::is_heartbeat_file(&path) {
            continue;
        }
        let hb = match SidebarHeartbeat::read_from(&path) {
            Ok(hb) => hb,
            Err(e) => {
                debug!(?path, error = %e, "wakeup: skipping unreadable sidebar heartbeat");
                continue;
            }
        };
        if hb.protocol_version != SIDEBAR_PROTOCOL_VERSION {
            debug!(
                ?path,
                protocol = hb.protocol_version,
                expected = SIDEBAR_PROTOCOL_VERSION,
                "wakeup: skipping sidebar heartbeat with unsupported protocol version"
            );
            continue;
        }
        let age_seconds = now.duration_since(hb.last_seen).as_secs();
        if age_seconds.is_negative()
            || Duration::from_secs(age_seconds as u64) > SIDEBAR_HEARTBEAT_TTL
        {
            debug!(?path, "wakeup: skipping stale sidebar heartbeat");
            continue;
        }
        // TOCTOU re-stat: between deserialise and send, the sidebar may have
        // exited and unlinked both its heartbeat and the wakeup socket.
        // Re-check the heartbeat's mtime; skip if it disappeared or aged out.
        if !heartbeat_still_fresh(&path) {
            continue;
        }
        fresh.push(hb);
    }
    Ok(fresh)
}

/// Re-stat the heartbeat file to confirm it's still on disk and its mtime
/// is younger than [`SIDEBAR_HEARTBEAT_TTL`]. Returns `false` when the file
/// is gone, the mtime is stale, or the system clock disagrees with the
/// filesystem in the wrong direction.
fn heartbeat_still_fresh(path: &Path) -> bool {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!(?path, "wakeup: heartbeat unlinked between read and send");
            return false;
        }
        Err(e) => {
            debug!(?path, error = %e, "wakeup: re-stat failed");
            return false;
        }
    };
    let mtime = match meta.modified() {
        Ok(mtime) => mtime,
        Err(e) => {
            debug!(?path, error = %e, "wakeup: filesystem reports no mtime");
            return false;
        }
    };
    match mtime.elapsed() {
        Ok(age) if age <= SIDEBAR_HEARTBEAT_TTL => true,
        Ok(_) => {
            debug!(?path, "wakeup: heartbeat aged out between read and send");
            false
        }
        Err(_) => {
            // mtime is in the future — clock skew or test-set mtime newer
            // than `now`. Trust the file: a "future" heartbeat is fresh.
            true
        }
    }
}

fn send_datagram(payload: &[u8], target: &Path) {
    let Some(sender) = sender_socket() else {
        return;
    };
    send_datagram_with(&sender, payload, target);
}

fn send_datagrams_with<'a>(
    sender: &UnixDatagram,
    payload: &[u8],
    targets: impl IntoIterator<Item = &'a Path>,
) {
    for target in targets {
        send_datagram_with(sender, payload, target);
    }
}

fn sender_socket() -> Option<UnixDatagram> {
    match UnixDatagram::unbound() {
        Ok(sender) => Some(sender),
        Err(e) => {
            debug!(error = %e, "wakeup: creating sender socket failed");
            None
        }
    }
}

fn send_datagram_with(sender: &UnixDatagram, payload: &[u8], target: &Path) {
    match sender.send_to(payload, target) {
        Ok(_) => {}
        Err(e) => {
            debug!(
                ?target,
                error = %e,
                "wakeup: send_to failed (target may have exited)"
            );
        }
    }
}
