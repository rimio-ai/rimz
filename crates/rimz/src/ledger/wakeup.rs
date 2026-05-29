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

use std::collections::HashSet;
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
use crate::ids::{EventId, MuxName, RequestId, WorkspaceId};
use crate::ledger::RuntimePaths;
use crate::mux::backend_for;
use crate::schema::SIDEBAR_PROTOCOL_VERSION;
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
    wake_sidebars_inner(rt, workspace_id, Some(request_id), None)
}

pub fn wake_sidebars_for_event(
    rt: &RuntimePaths,
    workspace_id: &WorkspaceId,
    event_id: &EventId,
) -> Result<()> {
    wake_sidebars_inner(rt, workspace_id, None, Some(event_id))
}

fn wake_sidebars_inner(
    rt: &RuntimePaths,
    workspace_id: &WorkspaceId,
    request_id: Option<&RequestId>,
    event_id: Option<&EventId>,
) -> Result<()> {
    let payload = serde_json::to_vec(&SidebarWakeup {
        kind: "ledger_delta",
        workspace_id: workspace_id.clone(),
        request_id: request_id.cloned(),
        event_id: event_id.cloned(),
        protocol_version: crate::schema::SIDEBAR_PROTOCOL_VERSION,
    })?;

    let mut piped_zellij_sessions: HashSet<String> = HashSet::new();
    for hb in collect_fresh_sidebars(rt)? {
        send_datagram(&payload, &hb.wakeup_socket);
        if hb.mux == MuxName::Zellij && piped_zellij_sessions.insert(hb.session_name.clone()) {
            dispatch_zellij_pipe(&hb.session_name, &payload);
        }
    }
    Ok(())
}

/// Tell every fresh sidebar of this workspace to re-exec its own binary, so it
/// picks up a freshly-installed renderer in place — no session rebirth, no pane
/// churn. The per-tick `rimz` snapshot subprocess already reloads on its own;
/// this covers the long-lived renderer process. Returns how many sidebars were
/// signaled. A wedged or already-dead sidebar receives nothing; relaunch it via
/// `rimz start`/`rimz attach` instead.
pub fn reload_sidebars(rt: &RuntimePaths) -> Result<usize> {
    let mut signaled = 0;
    for hb in collect_fresh_sidebars(rt)? {
        send_datagram(RELOAD_WAKEUP, &hb.wakeup_socket);
        signaled += 1;
    }
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
        if !is_sidebar_heartbeat(&path) {
            continue;
        }
        let hb = match read_sidebar_heartbeat(&path) {
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

/// Issue the broadcast `zellij pipe` fast path described in
/// `docs/internals/ledger.md:108–114`. The UDP datagram above is the
/// channel of record; this is purely a latency hint. Per-call failures
/// are swallowed at `debug` — never error the ledger write that
/// triggered us.
fn dispatch_zellij_pipe(session_name: &str, payload: &[u8]) {
    let backend = backend_for(MuxName::Zellij);
    if let Err(err) = backend.wake_sidebar(session_name, payload) {
        debug!(
            session = session_name,
            error = %err,
            "wakeup: zellij pipe broadcast failed (UDP wakeup already sent)"
        );
    }
}

fn is_sidebar_heartbeat(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("sidebar.") && n.ends_with(".json"))
        .unwrap_or(false)
}

fn read_sidebar_heartbeat(path: &Path) -> std::result::Result<SidebarHeartbeat, io::Error> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
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
    match UnixDatagram::unbound().and_then(|s| s.send_to(payload, target)) {
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
