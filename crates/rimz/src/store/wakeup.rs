//! Best-effort wakeup datagrams posted after every store mutation.
//!
//! Two channels:
//!
//! * **Run socket** — a supervised `rimz agents -p` caller bound a socket via
//!   [`crate::harness::run_wake::bind_run`]; when the run completes, the writer sends a
//!   small datagram so the waiter can exit before its polling tick fires.
//! * **Sidebar wakeup sockets** — each live sidebar instance writes a
//!   heartbeat JSON under `runtime/heartbeat/sidebar.*.json` carrying the
//!   path of a datagram socket it owns. After every mutation we walk the
//!   heartbeat directory and post a typed `StoreDelta` event to each fresh
//!   socket.
//!
//! Per the docs: "Sidebar wakeups are latency, not truth." Sends are
//! non-blocking, so a full receiver queue drops the wakeup. Per-target send
//! failures are absorbed (logged at `debug`); only directory-read failures
//! propagate. Stale heartbeats (older than [`SIDEBAR_HEARTBEAT_TTL`]) are skipped.

use std::fs;
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use tracing::debug;

use crate::harness::run::RunRecord;
use crate::harness::run_wake::{WakeupFrame, run_socket_path};
use crate::sidebar::events::{
    RELOAD_CONTROL_WORD, SUPERVISOR_HANDOFF_CONTROL_WORD, SidebarEvent, SidebarEventEnvelope,
};
use crate::sidebar::heartbeat::{SidebarHeartbeat, read_current_heartbeats};
pub use crate::sidebar::timing::SIDEBAR_HEARTBEAT_TTL;
use crate::store::RuntimePaths;
use crate::store::event::{EventEnvelope, EventKind};

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

/// Send a `run_completed` datagram to the supervised agents waiter. The run record on
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

/// Walk the runtime heartbeat dir and post a typed `StoreDelta` event to
/// each fresh sidebar's `wakeup_socket`. Per-target failures are logged
/// and skipped — they never error the write that triggered us. Event-log
/// mutations and context-sidecar writes (Claude's statusline `$`/token update,
/// the Codex rollout refresh) both land here, so a cost change repaints within
/// a wakeup instead of waiting for the renderer's next poll tick.
pub fn wake_sidebars(rt: &RuntimePaths) -> Result<()> {
    broadcast_sidebar_event(
        rt,
        None,
        SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
    )?;
    Ok(())
}

pub fn wake_sidebars_for_event(rt: &RuntimePaths, event: &EventEnvelope) -> Result<()> {
    broadcast_sidebar_event(
        rt,
        None,
        SidebarEvent::StoreDelta {
            event_method: Some(event.method.clone()),
            agent_signal: agent_signal(event),
        },
    )?;
    Ok(())
}

fn agent_signal(event: &EventEnvelope) -> Option<String> {
    match event.kind() {
        EventKind::AgentLifecycle(payload) => {
            let payload = *payload;
            Some(payload.observation.signal.tag().to_owned())
        }
        EventKind::AgentAttach(_)
        | EventKind::AgentLaunch(_)
        | EventKind::Message { .. }
        | EventKind::SessionRebirth
        | EventKind::SessionDeath(_)
        | EventKind::Other { .. } => None,
    }
}

/// Tell every fresh sidebar of this workspace to re-exec its own binary. Reload
/// uses the bare control word, not a typed event envelope, so it still reaches a
/// renderer whose sidebar-event schema predates the current one.
pub fn reload_sidebars(rt: &RuntimePaths) -> Result<usize> {
    let sidebars = collect_fresh_sidebars(rt)?;
    let signaled = sidebars.len();
    let Some(sender) = sender_socket() else {
        return Ok(signaled);
    };
    send_datagrams_with(
        &sender,
        RELOAD_CONTROL_WORD.as_bytes(),
        sidebars.iter().map(|hb| hb.wakeup_socket.as_path()),
    );
    Ok(signaled)
}

/// Ask one known sidebar worker to exit cleanly for a supervisor handoff.
/// The durable workspace record remains the target truth; this datagram only
/// bounds how long the old worker keeps serving after the supervisor notices.
pub fn reload_sidebar(
    rt: &RuntimePaths,
    instance_id: &crate::ids::SidebarInstanceId,
) -> Result<()> {
    let target = rt.sidebar_socket_path(instance_id);
    send_datagram(SUPERVISOR_HANDOFF_CONTROL_WORD.as_bytes(), &target);
    Ok(())
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
        crate::sidebar::timing::unix_now_ms(),
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
pub fn wake_sidebars_pane_frame_published(
    rt: &RuntimePaths,
    publication: crate::sidebar::events::PaneFramePublicationKind,
) -> Result<usize> {
    broadcast_sidebar_event(rt, None, SidebarEvent::PaneFramePublished { publication })
}

/// Apply the wakeup fanout freshness filter over the shared sidebar heartbeat
/// discovery walk, including a TOCTOU re-stat just before return.
fn collect_fresh_sidebars(rt: &RuntimePaths) -> Result<Vec<SidebarHeartbeat>> {
    let heartbeats =
        read_current_heartbeats(&rt.heartbeat_dir).map_err(|source| WakeupErr::ReadDir {
            path: rt.heartbeat_dir.clone(),
            source,
        })?;

    let now = Timestamp::now();
    let mut fresh = Vec::new();
    for (path, hb) in heartbeats {
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
        Ok(sender) => match sender.set_nonblocking(true) {
            Ok(()) => Some(sender),
            Err(e) => {
                debug!(error = %e, "wakeup: making sender socket non-blocking failed");
                None
            }
        },
        Err(e) => {
            debug!(error = %e, "wakeup: creating sender socket failed");
            None
        }
    }
}

fn send_datagram_with(sender: &UnixDatagram, payload: &[u8], target: &Path) {
    match sender.send_to(payload, target) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            debug!(?target, "wakeup: receiver queue full; dropping wakeup");
        }
        Err(e) => {
            debug!(
                ?target,
                error = %e,
                "wakeup: send_to failed (target may have exited)"
            );
        }
    }
}
