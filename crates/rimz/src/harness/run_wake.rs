//! Supervised-run wake sockets.
//!
//! A supervised `rimz agents -p` run binds a Unix datagram socket and parks on
//! it until either a run-complete wakeup arrives from the store writer or the
//! cap fires. Socket paths are derived from run ids so binder and store writer
//! share one source of truth.
//!
//! Validation is by `(workspace_id, run_id)`, per
//! `docs/internals/store.md`. Frames that fail validation are logged
//! at `debug` and dropped; the waiter keeps recving until the cap.
//!
use std::os::unix::net::UnixDatagram as StdUnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::harness::run::RunStatus;
use crate::ids::{RunId, WorkspaceId};
use crate::sock;
use crate::store::RuntimePaths;

#[derive(Debug, thiserror::Error)]
pub enum RunWakeErr {
    #[error("binding run wake socket at {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    SocketPathTooLong(#[from] crate::sock::SocketPathTooLong),

    #[error("recv on run wake socket: {0}")]
    Recv(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, RunWakeErr>;

#[derive(Debug, PartialEq, Eq)]
pub enum RunWakeOutcome {
    Completed(RunStatus),
    Neutral,
}

#[derive(Clone, Debug)]
pub struct ExpectedRunFrame {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
}

/// Wakeup frame the store writer sends to a per-run socket when a supervised
/// `rimz agents -p` turn reaches a terminal state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeupFrame {
    RunCompleted {
        workspace_id: WorkspaceId,
        run_id: RunId,
        status: RunStatus,
    },
}

pub fn run_socket_path(rt: &RuntimePaths, run_id: &RunId) -> PathBuf {
    rt.sock_dir.join(format!("run.{}.sock", run_id.short()))
}

/// Bind a per-run datagram socket. Caller owns both the returned
/// [`StdUnixDatagram`] and the file at the returned path; the file must be
/// removed via [`cleanup_socket`] when the waiter exits.
///
/// Returns the standard-library socket so callers don't need an ambient
/// tokio runtime to bind. [`adopt`] moves it into the reactor when the wait
/// actually starts.
///
/// The runtime sock dir is expected to exist already (the `Store` ensures
/// it during `open`); we re-check defensively here only by removing a stale
/// file at the derived path before binding.
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
    let sock = StdUnixDatagram::bind(&path).map_err(|source| RunWakeErr::Bind {
        path: path.clone(),
        source,
    })?;
    Ok((sock, path))
}

/// Remove the socket file. Best-effort; missing file is fine.
pub fn cleanup_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// RAII guard that cleans up the socket on drop. The bind path is derived from
/// a UUIDv7 run id so a missing file is benign.
#[must_use = "drop the guard at the end of the run wake wait to clean up the socket"]
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

/// Adopt a freshly-bound std `UnixDatagram` into a tokio one.
pub fn adopt(sock: StdUnixDatagram) -> Result<tokio::net::UnixDatagram> {
    sock.set_nonblocking(true).map_err(RunWakeErr::Recv)?;
    tokio::net::UnixDatagram::from_std(sock).map_err(RunWakeErr::Recv)
}

pub async fn wait_for_run_completion(
    sock: &tokio::net::UnixDatagram,
    expected: &ExpectedRunFrame,
    cap: Option<Duration>,
) -> Result<RunWakeOutcome> {
    let recv_valid = async {
        let mut buf = vec![0u8; 4096];
        loop {
            let n = sock.recv(&mut buf).await.map_err(RunWakeErr::Recv)?;
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
                            "run wake: dropping frame failing (workspace_id, run_id) check"
                        );
                        continue;
                    }
                    return Ok::<RunWakeOutcome, RunWakeErr>(RunWakeOutcome::Completed(status));
                }
                Err(e) => {
                    debug!(error = %e, "run wake: dropping unparseable frame");
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

    fn short_runtime_root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("r")
            .tempdir_in("/tmp")
            .expect("short runtime tempdir")
    }

    /// The precondition fires before `bind(2)` ever sees the path, so the
    /// user gets the named fix (shorten the runtime dir) instead of an
    /// opaque `EINVAL` — and the test needs no real socket or filesystem.
    #[test]
    fn bind_fails_fast_when_the_socket_path_overflows_af_unix() {
        let deep_root = Path::new("/tmp").join("d".repeat(sock::AF_UNIX_PATH_LIMIT));
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let rt = RuntimePaths::under(workspace_id, &deep_root).expect("paths");
        let run_id = RunId::new();

        let err = bind_run(&rt, &run_id).expect_err("overlong path must fail fast");
        match err {
            RunWakeErr::SocketPathTooLong(source) => {
                assert_eq!(source.path, run_socket_path(&rt, &run_id));
                assert_eq!(source.used, sock::path_len(&source.path) + 1);
                assert!(source.used > sock::AF_UNIX_PATH_LIMIT);
            }
            other => panic!("expected SocketPathTooLong, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_completion_wakeup_round_trips_and_ignores_wrong_run() {
        let dir = short_runtime_root();
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
        let dir = short_runtime_root();
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
