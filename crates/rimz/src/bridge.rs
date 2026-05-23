//! Blocking decision bridge.
//!
//! A waiting hook or script binds a per-request Unix datagram socket and
//! parks on it until either a wakeup frame arrives from the ledger writer
//! (when a resolver calls `feed resolve`) or the cap fires. The socket path
//! is derived from the request id; both binder and ledger writer call
//! [`feed_socket_path`] so there is one source of truth.
//!
//! Validation is by `(workspace_id, request_id, nonce)` — never by PID alone,
//! per `docs/internals/ledger.md`. Frames that fail validation are logged at
//! `debug` and dropped; the waiter keeps recving until the cap.
//!
//! The TOCTOU resolver-heartbeat re-stat described by the bridge path lives
//! in [`crate::resolver::freshness::restat`]; the hook calls it between
//! [`bind`] and pushing the feed item with `Surface::Bridge`.

use std::os::unix::net::UnixDatagram as StdUnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::ids::{RequestId, WorkspaceId};
use crate::ledger::RuntimePaths;

#[derive(Debug, thiserror::Error)]
pub enum BridgeErr {
    /// Returned by [`crate::resolver::freshness::restat`] when the resolver
    /// picked by the freshness walk is no longer serving: heartbeat missing,
    /// stale beyond the TTL, off the allowlist, or pinned-binary mismatch.
    /// The hook downgrades to `native_ui` and exits.
    #[error("resolver heartbeat went stale for {0}; downgrading to native_ui")]
    HeartbeatStale(RequestId),

    #[error("binding bridge socket at {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("recv on bridge socket: {0}")]
    Recv(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BridgeErr>;

/// Outcome from a single bridge wait. The caller reloads the feed item from
/// disk on `Resolved` — the ledger is the source of truth, this datagram is
/// the latency hint.
#[derive(Debug, PartialEq, Eq)]
pub enum BridgeOutcome {
    Resolved,
    Neutral,
}

/// Validation triple a waiter expects on every wakeup frame.
#[derive(Clone, Debug)]
pub struct ExpectedFrame {
    pub workspace_id: WorkspaceId,
    pub request_id: RequestId,
    pub nonce: String,
}

/// Wakeup frame the ledger writer sends to a per-request socket when a
/// resolution lands. Intentionally small: the feed file on disk carries the
/// decision payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeupFrame {
    FeedResolved {
        workspace_id: WorkspaceId,
        request_id: RequestId,
        nonce: String,
    },
}

/// Per-request socket path. Single source of truth shared by binder and
/// resolver. Short id from [`RequestId::short`] keeps the path well under
/// the 108-byte `AF_UNIX` budget.
pub fn feed_socket_path(rt: &RuntimePaths, request_id: &RequestId) -> PathBuf {
    rt.sock_dir
        .join(format!("feed.{}.sock", request_id.short()))
}

/// Bind a per-request datagram socket. Caller owns both the returned
/// [`StdUnixDatagram`] and the file at the returned path; the file must be
/// removed via [`cleanup_socket`] when the waiter exits.
///
/// Returns the standard-library socket so callers don't need an ambient
/// tokio runtime to bind. [`adopt`] moves it into the reactor when the wait
/// actually starts.
///
/// The runtime sock dir is expected to exist already (the `Ledger` ensures
/// it during `open`); we re-check defensively here only by removing a stale
/// file at the derived path before binding.
pub fn bind(rt: &RuntimePaths, request_id: &RequestId) -> Result<(StdUnixDatagram, PathBuf)> {
    let path = feed_socket_path(rt, request_id);
    if path.exists() {
        // Derived from a UUIDv7 — a leftover here means a previous waiter
        // crashed without cleanup. Safe to clear.
        let _ = std::fs::remove_file(&path);
    }
    let sock = StdUnixDatagram::bind(&path).map_err(|source| BridgeErr::Bind {
        path: path.clone(),
        source,
    })?;
    Ok((sock, path))
}

/// Remove the per-request socket file. Best-effort; missing file is fine.
pub fn cleanup_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// RAII guard that cleans up the per-request socket on drop. The bind path
/// is derived from a UUIDv7 request id so a missing file is benign.
#[must_use = "drop the guard at the end of the bridge wait to clean up the socket"]
pub struct SocketGuard {
    path: Option<PathBuf>,
}

impl SocketGuard {
    pub fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            cleanup_socket(&path);
        }
    }
}

/// Adopt a freshly-bound std `UnixDatagram` into a tokio one. The hook
/// bridge poll loop calls this once and keeps the tokio socket alive across
/// many [`wait_for_resolution`] calls; the one-shot `feed ask` path uses
/// [`wait_for_resolution_owning`] which adopts internally.
pub fn adopt(sock: StdUnixDatagram) -> Result<tokio::net::UnixDatagram> {
    sock.set_nonblocking(true).map_err(BridgeErr::Recv)?;
    tokio::net::UnixDatagram::from_std(sock).map_err(BridgeErr::Recv)
}

/// Wait for a valid wakeup frame on a tokio socket the caller already owns.
/// `None` cap waits forever. Invalid frames are dropped silently (logged at
/// `debug`); the waiter keeps recving until either a valid frame lands or the
/// cap expires.
///
/// Takes the socket by borrow so the hook bridge can keep the per-request
/// socket bound across chain-advance iterations without rebinding.
pub async fn wait_for_resolution(
    sock: &tokio::net::UnixDatagram,
    expected: &ExpectedFrame,
    cap: Option<Duration>,
) -> Result<BridgeOutcome> {
    let recv_valid = async {
        let mut buf = vec![0u8; 4096];
        loop {
            let n = sock.recv(&mut buf).await.map_err(BridgeErr::Recv)?;
            match serde_json::from_slice::<WakeupFrame>(&buf[..n]) {
                Ok(WakeupFrame::FeedResolved {
                    workspace_id,
                    request_id,
                    nonce,
                }) => {
                    if workspace_id != expected.workspace_id
                        || request_id != expected.request_id
                        || nonce != expected.nonce
                    {
                        debug!(
                            ?workspace_id,
                            ?request_id,
                            "bridge: dropping wakeup frame failing (workspace_id, request_id, nonce) check"
                        );
                        continue;
                    }
                    return Ok::<BridgeOutcome, BridgeErr>(BridgeOutcome::Resolved);
                }
                Err(e) => {
                    debug!(error = %e, "bridge: dropping unparseable wakeup frame");
                }
            }
        }
    };

    match cap {
        Some(cap) => match tokio::time::timeout(cap, recv_valid).await {
            Ok(result) => result,
            Err(_elapsed) => Ok(BridgeOutcome::Neutral),
        },
        None => recv_valid.await,
    }
}

/// One-shot variant for callers that do not loop (currently `feed ask`).
/// Takes the std socket by value, adopts it into the reactor, and delegates
/// to [`wait_for_resolution`].
pub async fn wait_for_resolution_owning(
    sock: StdUnixDatagram,
    expected: ExpectedFrame,
    cap: Option<Duration>,
) -> Result<BridgeOutcome> {
    let sock = adopt(sock)?;
    wait_for_resolution(&sock, &expected, cap).await
}
