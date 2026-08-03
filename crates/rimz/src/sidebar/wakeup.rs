//! Best-effort datagrams posted to live sidebar consumers.
//!
//! Each renderer publishes a heartbeat naming its datagram socket. Senders
//! walk current-protocol heartbeats, enforce content and mtime freshness, and
//! post nonblocking events. Per-target failures are absorbed; only heartbeat
//! directory reads and envelope serialization propagate. Wakeups cut latency
//! but never carry truth.

use std::fs;
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use tracing::debug;

use crate::sidebar::events::{
    RELOAD_CONTROL_WORD, SUPERVISOR_HANDOFF_CONTROL_WORD, SidebarEvent, SidebarEventEnvelope,
};
use crate::sidebar::heartbeat::{SidebarHeartbeat, read_current_heartbeats};
use crate::sidebar::timing::SIDEBAR_HEARTBEAT_TTL;
use crate::store::RuntimePaths;

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
}

/// Post a semantic store-delta event to each fresh sidebar.
pub fn wake_store_delta(
    rt: &RuntimePaths,
    event_method: Option<String>,
    agent_signal: Option<String>,
) -> std::result::Result<(), WakeupErr> {
    broadcast(
        rt,
        None,
        SidebarEvent::StoreDelta {
            event_method,
            agent_signal,
        },
    )?;
    Ok(())
}

/// Tell every fresh sidebar to re-exec its own binary. Reload uses the bare
/// control word so it reaches renderers whose typed-event schema is older.
pub fn reload_all(rt: &RuntimePaths) -> std::result::Result<usize, WakeupErr> {
    let sidebars = collect_fresh_sidebars(rt)?;
    let signaled = sidebars.len();
    let Some(sender) = sender_socket() else {
        return Ok(signaled);
    };
    send_datagrams_with(
        &sender,
        RELOAD_CONTROL_WORD.as_bytes(),
        sidebars
            .iter()
            .map(|heartbeat| heartbeat.wakeup_socket.as_path()),
    );
    Ok(signaled)
}

/// Ask one known sidebar worker to exit cleanly for a supervisor handoff.
pub fn reload_one(
    rt: &RuntimePaths,
    instance_id: &crate::ids::SidebarInstanceId,
) -> std::result::Result<(), WakeupErr> {
    let target = rt.sidebar_socket_path(instance_id);
    send_datagram(SUPERVISOR_HANDOFF_CONTROL_WORD.as_bytes(), &target);
    Ok(())
}

/// Post one typed event to every fresh, protocol-current sidebar. A session
/// name scopes pane-bearing events to one mux session; `None` is workspace-wide.
pub fn broadcast(
    rt: &RuntimePaths,
    session_name: Option<&str>,
    event: SidebarEvent,
) -> std::result::Result<usize, WakeupErr> {
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
        sidebars
            .iter()
            .map(|heartbeat| heartbeat.wakeup_socket.as_path()),
    );
    Ok(signaled)
}

fn collect_fresh_sidebars(
    rt: &RuntimePaths,
) -> std::result::Result<Vec<SidebarHeartbeat>, WakeupErr> {
    let heartbeats =
        read_current_heartbeats(&rt.heartbeat_dir).map_err(|source| WakeupErr::ReadDir {
            path: rt.heartbeat_dir.clone(),
            source,
        })?;

    let now = Timestamp::now();
    let mut fresh = Vec::new();
    for (path, heartbeat) in heartbeats {
        let age_seconds = now.duration_since(heartbeat.last_seen).as_secs();
        if age_seconds.is_negative()
            || Duration::from_secs(age_seconds as u64) > SIDEBAR_HEARTBEAT_TTL
        {
            debug!(?path, "wakeup: skipping stale sidebar heartbeat");
            continue;
        }
        if heartbeat_still_fresh(&path) {
            fresh.push(heartbeat);
        }
    }
    Ok(fresh)
}

/// Re-stat immediately before send to close the heartbeat-read/renderer-exit
/// window. Future mtimes remain fresh, matching the content timestamp policy.
fn heartbeat_still_fresh(path: &Path) -> bool {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            debug!(?path, "wakeup: heartbeat unlinked between read and send");
            return false;
        }
        Err(error) => {
            debug!(?path, %error, "wakeup: re-stat failed");
            return false;
        }
    };
    let mtime = match meta.modified() {
        Ok(mtime) => mtime,
        Err(error) => {
            debug!(?path, %error, "wakeup: filesystem reports no mtime");
            return false;
        }
    };
    match mtime.elapsed() {
        Ok(age) if age <= SIDEBAR_HEARTBEAT_TTL => true,
        Ok(_) => {
            debug!(?path, "wakeup: heartbeat aged out between read and send");
            false
        }
        Err(_) => true,
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
            Err(error) => {
                debug!(%error, "wakeup: making sender socket non-blocking failed");
                None
            }
        },
        Err(error) => {
            debug!(%error, "wakeup: creating sender socket failed");
            None
        }
    }
}

fn send_datagram_with(sender: &UnixDatagram, payload: &[u8], target: &Path) {
    match sender.send_to(payload, target) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            debug!(?target, "wakeup: receiver queue full; dropping wakeup");
        }
        Err(error) => {
            debug!(?target, %error, "wakeup: send_to failed (target may have exited)");
        }
    }
}
