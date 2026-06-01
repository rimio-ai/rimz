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
//!   heartbeat directory and post a `ledger_delta` envelope to each fresh
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
use serde::Serialize;
use tracing::debug;

use crate::bridge::{WakeupFrame, feed_socket_path};
use crate::feed::FeedItem;
use crate::ids::{EventId, RequestId, WorkspaceId};
use crate::ledger::RuntimePaths;
use crate::schema::SIDEBAR_PROTOCOL_VERSION;
use crate::schema::event::EventEnvelope;
use crate::schema::heartbeat::SidebarHeartbeat;

/// Maximum age of a sidebar heartbeat before we treat it as dead and skip
/// it. Matches the `~5s` figure in `docs/internals/ledger.md`.
pub const SIDEBAR_HEARTBEAT_TTL: Duration = Duration::from_secs(5);

/// Sidebar wakeup envelope per `docs/internals/ledger.md`. Sent on every
/// ledger mutation to every fresh sidebar heartbeat's `wakeup_socket`.
#[derive(Clone, Debug, Serialize)]
pub struct SidebarWakeup {
    pub kind: &'static str,
    pub workspace_id: WorkspaceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_event_name: Option<String>,
    pub protocol_version: &'static str,
}

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

/// Walk the runtime heartbeat dir and post a `ledger_delta` envelope to
/// each fresh sidebar's `wakeup_socket`. Per-target failures are logged
/// and skipped — they never error the ledger write that triggered us.
pub fn wake_sidebars(
    rt: &RuntimePaths,
    workspace_id: &WorkspaceId,
    request_id: &RequestId,
) -> Result<()> {
    wake_sidebars_inner(rt, workspace_id, Some(request_id), None, None, None)
}

pub fn wake_sidebars_for_event(rt: &RuntimePaths, event: &EventEnvelope) -> Result<()> {
    wake_sidebars_inner(
        rt,
        &event.workspace_id,
        None,
        Some(&event.event_id),
        Some(&event.method),
        agent_event_name(event),
    )
}

/// Wake every fresh sidebar after a context-sidecar write (Claude's statusline
/// `$`/token/rate-limit update). The sidecar is not the ledger, so it fires no
/// request/event delta on its own; this posts the same `ledger_delta` envelope
/// the renderer folds into one refetch, so a cost change repaints within a
/// wakeup instead of waiting for the renderer's next poll tick.
pub fn wake_sidebars_for_context(rt: &RuntimePaths, workspace_id: &WorkspaceId) -> Result<()> {
    wake_sidebars_inner(rt, workspace_id, None, None, None, None)
}

fn wake_sidebars_inner(
    rt: &RuntimePaths,
    workspace_id: &WorkspaceId,
    request_id: Option<&RequestId>,
    event_id: Option<&EventId>,
    event_method: Option<&str>,
    agent_event_name: Option<&str>,
) -> Result<()> {
    let payload = serde_json::to_vec(&SidebarWakeup {
        kind: "ledger_delta",
        workspace_id: workspace_id.clone(),
        request_id: request_id.cloned(),
        event_id: event_id.cloned(),
        event_method: event_method.map(str::to_owned),
        agent_event_name: agent_event_name.map(str::to_owned),
        protocol_version: crate::schema::SIDEBAR_PROTOCOL_VERSION,
    })?;

    let sidebars = collect_fresh_sidebars(rt)?;
    send_datagrams(
        &payload,
        sidebars.iter().map(|hb| hb.wakeup_socket.as_path()),
    );
    Ok(())
}

fn agent_event_name(event: &EventEnvelope) -> Option<&str> {
    (event.method == "agent.lifecycle")
        .then(|| {
            event
                .params
                .get("event_name")
                .and_then(serde_json::Value::as_str)
        })
        .flatten()
}

/// Tell every fresh sidebar of this workspace to re-exec its own binary, so it
/// picks up a freshly-installed renderer in place — no session rebirth, no pane
/// churn. The per-tick `rimz` snapshot subprocess already reloads on its own;
/// this covers the long-lived renderer process. Returns how many sidebars were
/// signaled. A wedged or already-dead sidebar receives nothing; relaunch it via
/// `rimz start`/`rimz attach` instead.
pub fn reload_sidebars(rt: &RuntimePaths) -> Result<usize> {
    let sidebars = collect_fresh_sidebars(rt)?;
    let signaled = sidebars.len();
    send_datagrams(
        RELOAD_WAKEUP,
        sidebars.iter().map(|hb| hb.wakeup_socket.as_path()),
    );
    Ok(signaled)
}

/// Control word the renderer decodes into a re-exec. Shared so the wakeup
/// sender and the sidebar's decoder cannot drift.
pub const RELOAD_WAKEUP: &[u8] = b"reload";

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

fn send_datagrams<'a>(payload: &[u8], targets: impl IntoIterator<Item = &'a Path>) {
    let Some(sender) = sender_socket() else {
        return;
    };
    for target in targets {
        send_datagram_with(&sender, payload, target);
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
