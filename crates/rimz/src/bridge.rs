//! Blocking decision bridge.
//!
//! A waiting hook or script binds a per-request Unix datagram socket and
//! parks on it until either a wakeup frame arrives from the ledger writer
//! (when a resolver calls `feed resolve`) or the cap fires. The socket path
//! is derived from the request id; both binder and ledger writer call
//! [`feed_socket_path`] so there is one source of truth.
//!
//! Validation is by `(workspace_id, request_id, nonce)` — never by PID alone,
//! per `docs/internals/sidebar/ledger.md`. Frames that fail validation are logged at
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

use crate::feed::FeedStatus;
use crate::ids::{RequestId, RunId, WorkspaceId};
use crate::ledger::RuntimePaths;
use crate::run::RunStatus;
use crate::sock;

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

    #[error(transparent)]
    SocketPathTooLong(#[from] crate::sock::SocketPathTooLong),

    #[error("recv on bridge socket: {0}")]
    Recv(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BridgeErr>;

/// Outcome from a single bridge wait. The caller reloads the feed item from
/// disk on `Resolved`; `Terminal` means the ledger closed the request without a
/// decision and the frame itself is the durable exit signal.
#[derive(Debug, PartialEq, Eq)]
pub enum BridgeOutcome {
    Resolved,
    Terminal,
    Neutral,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunWakeOutcome {
    Completed(RunStatus),
    Neutral,
}

/// Validation triple a waiter expects on every wakeup frame.
#[derive(Clone, Debug)]
pub struct ExpectedFrame {
    pub workspace_id: WorkspaceId,
    pub request_id: RequestId,
    pub nonce: String,
}

#[derive(Clone, Debug)]
pub struct ExpectedRunFrame {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
}

/// Wakeup frame the ledger writer sends to a per-request socket when a
/// resolution lands, when a request is closed without a decision, or to a
/// per-run socket when `rimz run` reaches a terminal state. Intentionally small:
/// disk files carry decision payloads.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeupFrame {
    FeedResolved {
        workspace_id: WorkspaceId,
        request_id: RequestId,
        nonce: String,
    },
    FeedTerminal {
        workspace_id: WorkspaceId,
        request_id: RequestId,
        nonce: String,
        status: FeedStatus,
    },
    RunCompleted {
        workspace_id: WorkspaceId,
        run_id: RunId,
        status: RunStatus,
    },
}

/// Per-request socket path. Single source of truth shared by binder and
/// resolver. Short id from [`RequestId::short`] keeps the path well under
/// the platform `AF_UNIX` budget.
pub fn feed_socket_path(rt: &RuntimePaths, request_id: &RequestId) -> PathBuf {
    rt.sock_dir
        .join(format!("feed.{}.sock", request_id.short()))
}

pub fn run_socket_path(rt: &RuntimePaths, run_id: &RunId) -> PathBuf {
    rt.sock_dir.join(format!("run.{}.sock", run_id.short()))
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
    bind_path(feed_socket_path(rt, request_id))
}

pub fn bind_run(rt: &RuntimePaths, run_id: &RunId) -> Result<(StdUnixDatagram, PathBuf)> {
    bind_path(run_socket_path(rt, run_id))
}

fn bind_path(path: PathBuf) -> Result<(StdUnixDatagram, PathBuf)> {
    sock::validate_socket_path(&path)?;
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
                Ok(WakeupFrame::FeedTerminal {
                    workspace_id,
                    request_id,
                    nonce,
                    ..
                }) => {
                    if workspace_id != expected.workspace_id
                        || request_id != expected.request_id
                        || nonce != expected.nonce
                    {
                        debug!(
                            ?workspace_id,
                            ?request_id,
                            "bridge: dropping terminal wakeup frame failing (workspace_id, request_id, nonce) check"
                        );
                        continue;
                    }
                    return Ok::<BridgeOutcome, BridgeErr>(BridgeOutcome::Terminal);
                }
                Ok(WakeupFrame::RunCompleted { .. }) => {}
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

pub async fn wait_for_run_completion(
    sock: &tokio::net::UnixDatagram,
    expected: &ExpectedRunFrame,
    cap: Option<Duration>,
) -> Result<RunWakeOutcome> {
    let recv_valid = async {
        let mut buf = vec![0u8; 4096];
        loop {
            let n = sock.recv(&mut buf).await.map_err(BridgeErr::Recv)?;
            match serde_json::from_slice::<WakeupFrame>(&buf[..n]) {
                Ok(WakeupFrame::RunCompleted {
                    workspace_id,
                    run_id,
                    status,
                }) => {
                    if workspace_id != expected.workspace_id || run_id != expected.run_id {
                        debug!(
                            ?workspace_id,
                            ?run_id,
                            "bridge: dropping run wakeup frame failing (workspace_id, run_id) check"
                        );
                        continue;
                    }
                    return Ok::<RunWakeOutcome, BridgeErr>(RunWakeOutcome::Completed(status));
                }
                Ok(WakeupFrame::FeedResolved { .. } | WakeupFrame::FeedTerminal { .. }) => {}
                Err(e) => {
                    debug!(error = %e, "bridge: dropping unparseable wakeup frame");
                }
            }
        }
    };

    match cap {
        Some(cap) => match tokio::time::timeout(cap, recv_valid).await {
            Ok(result) => result,
            Err(_elapsed) => Ok(RunWakeOutcome::Neutral),
        },
        None => recv_valid.await,
    }
}

pub async fn wait_for_run_completion_owning(
    sock: StdUnixDatagram,
    expected: ExpectedRunFrame,
    cap: Option<Duration>,
) -> Result<RunWakeOutcome> {
    let sock = adopt(sock)?;
    wait_for_run_completion(&sock, &expected, cap).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use tokio::net::UnixDatagram;

    /// The precondition fires before `bind(2)` ever sees the path, so the
    /// user gets the named fix (shorten the runtime dir) instead of an
    /// opaque `EINVAL` — and the test needs no real socket or filesystem.
    #[test]
    fn bind_fails_fast_when_the_socket_path_overflows_af_unix() {
        let deep_root = Path::new("/tmp").join("d".repeat(sock::AF_UNIX_PATH_LIMIT));
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let rt = RuntimePaths::under(workspace_id, &deep_root).expect("paths");
        let request_id = RequestId::new();

        let err = bind(&rt, &request_id).expect_err("overlong path must fail fast");
        match err {
            BridgeErr::SocketPathTooLong(source) => {
                assert_eq!(source.path, feed_socket_path(&rt, &request_id));
                assert_eq!(source.used, sock::path_len(&source.path) + 1);
                assert!(source.used > sock::AF_UNIX_PATH_LIMIT);
            }
            other => panic!("expected SocketPathTooLong, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_completion_wakeup_round_trips_and_ignores_wrong_run() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let rt = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        rt.ensure_dirs().expect("runtime dirs");
        let run_id = RunId::new();
        let other_run_id = RunId::new();
        let (sock, sock_path) = bind_run(&rt, &run_id).expect("bind run");
        let sender = UnixDatagram::unbound().expect("sender");

        let wrong = WakeupFrame::RunCompleted {
            workspace_id: workspace_id.clone(),
            run_id: other_run_id,
            status: RunStatus::Completed,
        };
        let wrong_bytes = serde_json::to_vec(&wrong).expect("serialize wrong");
        sender
            .send_to(&wrong_bytes, &sock_path)
            .await
            .expect("send wrong");
        let right = WakeupFrame::RunCompleted {
            workspace_id: workspace_id.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Failed,
        };
        let right_bytes = serde_json::to_vec(&right).expect("serialize right");
        sender
            .send_to(&right_bytes, &sock_path)
            .await
            .expect("send right");

        let outcome = wait_for_run_completion_owning(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id,
            },
            Some(Duration::from_secs(1)),
        )
        .await
        .expect("run wait");
        assert_eq!(outcome, RunWakeOutcome::Completed(RunStatus::Failed));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_completion_wait_times_out_neutral() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let rt = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        rt.ensure_dirs().expect("runtime dirs");
        let run_id = RunId::new();
        let (sock, _sock_path) = bind_run(&rt, &run_id).expect("bind run");

        let outcome = wait_for_run_completion_owning(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id,
            },
            Some(Duration::from_millis(10)),
        )
        .await
        .expect("run wait");
        assert_eq!(outcome, RunWakeOutcome::Neutral);
    }
}
