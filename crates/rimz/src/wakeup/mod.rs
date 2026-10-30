//! Best-effort datagram transport to live sidebar consumers.
//!
//! The wire is a leaf over `ids`, `pane`, `disk`, `utils`, and `build_id`,
//! reached downward by the store's write tail, the mux seam, remote control,
//! the sidebar graph and its renderer, and the CLI. No production line here
//! imports upward; the inline tests borrow `agents` and `store::event`
//! vocabulary.
//!
//! Each renderer publishes a heartbeat naming its host's room socket. Senders
//! walk current-protocol heartbeats, enforce mtime freshness, and post
//! nonblocking events once per distinct socket. Per-target failures are
//! absorbed; only heartbeat directory reads and envelope serialization
//! propagate. Wakeups cut latency but never carry truth.

pub mod events;
pub mod heartbeat;

use std::collections::HashSet;
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use tracing::debug;

use self::events::{RELOAD_CONTROL_WORD, SidebarEvent, SidebarEventEnvelope};
use self::heartbeat::{SidebarHeartbeat, mtime_within_ttl, read_current_heartbeats};
use crate::disk::paths::RuntimePaths;

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

/// Nudge every fresh sidebar to observe the durable build target. The host drains attachments on a changed build; supervisors own their later exec. Reload uses the bare control word to reach older typed-event schemas.
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
            .map(|heartbeat| heartbeat.wakeup_socket.as_path())
            .collect::<HashSet<_>>(),
    );
    Ok(signaled)
}

/// Post one typed event to every fresh, protocol-current sidebar. A session
/// name scopes pane-bearing events to one mux session; `None` is workspace-wide.
pub fn broadcast(
    rt: &RuntimePaths,
    session_name: Option<&str>,
    event: SidebarEvent,
) -> std::result::Result<usize, WakeupErr> {
    broadcast_with_observation(rt, session_name, event, None)
}

/// Broadcast an event with the observation stamp of its accepted topology.
/// Send time remains the sender's current wall clock, independent of the
/// optional structural-width proof floor.
pub fn broadcast_with_observation(
    rt: &RuntimePaths,
    session_name: Option<&str>,
    event: SidebarEvent,
    panes_observed_at_ms: Option<u64>,
) -> std::result::Result<usize, WakeupErr> {
    let sidebars = collect_fresh_sidebars(rt)?;
    let signaled = sidebars.len();
    let Some(sender) = sender_socket() else {
        return Ok(signaled);
    };
    let mut envelope = SidebarEventEnvelope::new(
        rt.workspace_id.clone(),
        session_name.map(str::to_owned),
        crate::utils::time::unix_now_ms(),
        event,
    );
    envelope.panes_observed_at_ms = panes_observed_at_ms;
    let payload = serde_json::to_vec(&envelope)?;
    send_datagrams_with(
        &sender,
        &payload,
        sidebars
            .iter()
            .map(|heartbeat| heartbeat.wakeup_socket.as_path())
            .collect::<HashSet<_>>(),
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

    Ok(heartbeats
        .into_iter()
        .filter(|(path, _)| mtime_within_ttl(path))
        .map(|(_, heartbeat)| heartbeat)
        .collect())
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
