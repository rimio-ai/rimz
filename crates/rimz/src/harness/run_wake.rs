//! Supervised-run terminal waiting and wake sockets.
//!
//! A supervised run binds a Unix datagram socket while its durable waiter polls
//! terminal state, observes cancellation, and owns timeout transitions. Socket
//! paths are derived from run ids so binder and store writer share one source
//! of truth.
//!
//! Validation is by `(workspace_id, run_id)`, per
//! `docs/internals/store.md`. Frames that fail validation are logged
//! at `debug` and dropped; the waiter keeps recving until the cap.
//!
use std::os::unix::net::UnixDatagram as StdUnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::harness::run::{RunCancellation, RunRecord, RunStatus};
use crate::ids::{RunId, WorkspaceId};
use crate::sock;
use crate::store::{RuntimePaths, Store};

const RUN_WAIT_POLL: Duration = Duration::from_millis(250);

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

/// Persistent supervised-run waiter. Durable run records are truth; the
/// socket only shortens the polling interval.
pub struct RunWaiter {
    sock: StdUnixDatagram,
    expected: ExpectedRunFrame,
    cancellation: RunCancellation,
    socket_guard: SocketGuard,
}

pub type RunObserver<'a> = dyn FnMut(&RunRecord) -> anyhow::Result<()> + 'a;

impl RunWaiter {
    pub fn bind(
        rt: &RuntimePaths,
        expected: ExpectedRunFrame,
        cancellation: RunCancellation,
    ) -> Result<Self> {
        let (sock, path) = bind_run(rt, &expected.run_id)?;
        Ok(Self {
            sock,
            expected,
            cancellation,
            socket_guard: SocketGuard::new(path),
        })
    }

    pub fn cancellation(&self) -> &RunCancellation {
        &self.cancellation
    }

    pub fn socket_path(&self) -> &Path {
        self.socket_guard
            .path
            .as_deref()
            .expect("run waiter owns its socket until drop")
    }

    /// Wait until durable state reaches a terminal status. The observer may
    /// render newly durable output, but cannot alter wait policy.
    pub async fn wait_terminal(
        &self,
        store: &Store,
        timeout: Option<Duration>,
        mut observer: Option<&mut RunObserver<'_>>,
    ) -> anyhow::Result<RunRecord> {
        let sock = adopt(self.sock.try_clone().context("cloning run wait socket")?)
            .context("adopting run wait socket")?;
        let deadline = timeout.map(|duration| std::time::Instant::now() + duration);
        loop {
            let record = crate::harness::run::load(store.paths(), &self.expected.run_id)?;
            if let Some(observer) = observer.as_deref_mut() {
                observer(&record)?;
            }
            if record.status.is_terminal() {
                return Ok(record);
            }
            if self.cancellation.is_requested() {
                let record = crate::harness::run::cancel_and_wake(store, &self.expected.run_id)?;
                if let Some(observer) = observer.as_deref_mut() {
                    observer(&record)?;
                }
                return Ok(record);
            }
            let Some(wait) = next_wait(deadline) else {
                let record = crate::harness::run::timeout(store.paths(), &self.expected.run_id)?;
                if let Some(observer) = observer.as_deref_mut() {
                    observer(&record)?;
                }
                return Ok(record);
            };
            let _ = wait_for_run_completion(&sock, &self.expected, Some(wait)).await?;
        }
    }
}

fn next_wait(deadline: Option<std::time::Instant>) -> Option<Duration> {
    let Some(deadline) = deadline else {
        return Some(RUN_WAIT_POLL);
    };
    let now = std::time::Instant::now();
    (now < deadline).then(|| (deadline - now).min(RUN_WAIT_POLL))
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
    use crate::harness::run::{PermissionMode, RunCancellation, RunRecord};
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::store::{StatePaths, Store};
    use tokio::net::UnixDatagram;

    fn short_runtime_root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("r")
            .tempdir_in("/tmp")
            .expect("short runtime tempdir")
    }

    struct RunFixture {
        _dir: tempfile::TempDir,
        workspace_id: WorkspaceId,
        store: Store,
        record: RunRecord,
    }

    impl RunFixture {
        fn new(status: RunStatus) -> Self {
            let dir = short_runtime_root();
            let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run-waiter"));
            let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
            let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
            let store = Store::open(paths, runtime).expect("store");
            let mut record = RunRecord::new(
                workspace_id.clone(),
                AgentKind::new_unchecked("codex"),
                PermissionMode::Auto,
                "go".to_owned(),
                PathBuf::from("/tmp/rimz-run-waiter"),
            );
            record.status = status;
            crate::harness::run::create(store.paths(), &record).expect("create run");
            Self {
                _dir: dir,
                workspace_id,
                store,
                record,
            }
        }

        fn waiter(&self, cancellation: RunCancellation) -> RunWaiter {
            RunWaiter::bind(
                self.store.runtime_paths(),
                ExpectedRunFrame {
                    workspace_id: self.workspace_id.clone(),
                    run_id: self.record.run_id.clone(),
                },
                cancellation,
            )
            .expect("waiter")
        }

        fn complete(&self, message: &str) {
            let mut record = self.record.clone();
            record.status = RunStatus::Completed;
            record.last_message = Some(message.to_owned());
            crate::store::run_store::write(&self.store.paths().runs_dir, &record)
                .expect("complete run");
        }
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

    #[tokio::test(flavor = "current_thread")]
    async fn waiter_reloads_terminal_record_after_valid_wake() {
        let fixture = RunFixture::new(RunStatus::Running);
        let waiter = fixture.waiter(RunCancellation::new());
        fixture.complete("done");
        let sender = UnixDatagram::unbound().expect("sender");
        let frame = WakeupFrame::RunCompleted {
            workspace_id: fixture.workspace_id.clone(),
            run_id: fixture.record.run_id.clone(),
            status: RunStatus::Completed,
        };
        sender
            .send_to(
                &serde_json::to_vec(&frame).expect("frame"),
                waiter.socket_path(),
            )
            .await
            .expect("wake");

        let record = waiter
            .wait_terminal(&fixture.store, Some(Duration::from_secs(1)), None)
            .await
            .expect("terminal record");

        assert_eq!(record.status, RunStatus::Completed);
        assert_eq!(record.last_message.as_deref(), Some("done"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiter_treats_durable_terminal_state_as_truth_without_wake() {
        let fixture = RunFixture::new(RunStatus::Completed);
        let waiter = fixture.waiter(RunCancellation::new());

        let record = waiter
            .wait_terminal(&fixture.store, Some(Duration::ZERO), None)
            .await
            .expect("already terminal");

        assert_eq!(record.status, RunStatus::Completed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiter_recovers_from_wake_loss_by_polling_durable_state() {
        let fixture = RunFixture::new(RunStatus::Running);
        let waiter = fixture.waiter(RunCancellation::new());
        let paths = fixture.store.paths().clone();
        let mut completed = fixture.record.clone();
        completed.status = RunStatus::Completed;
        completed.last_message = Some("polled".to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            crate::store::run_store::write(&paths.runs_dir, &completed).expect("complete run");
        });

        let record = waiter
            .wait_terminal(&fixture.store, Some(Duration::from_secs(1)), None)
            .await
            .expect("poll recovery");

        assert_eq!(record.status, RunStatus::Completed);
        assert_eq!(record.last_message.as_deref(), Some("polled"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiter_owns_timeout_and_single_cancellation_transition() {
        let timed = RunFixture::new(RunStatus::Running);
        let timeout_waiter = timed.waiter(RunCancellation::new());
        let record = timeout_waiter
            .wait_terminal(&timed.store, Some(Duration::ZERO), None)
            .await
            .expect("timeout");
        assert_eq!(record.status, RunStatus::TimedOut);

        let canceled = RunFixture::new(RunStatus::Running);
        let cancellation = RunCancellation::new();
        cancellation.request();
        let cancel_waiter = canceled.waiter(cancellation);
        let first = cancel_waiter
            .wait_terminal(&canceled.store, Some(Duration::from_secs(1)), None)
            .await
            .expect("cancel");
        let second = crate::harness::run::cancel_and_wake(&canceled.store, &canceled.record.run_id)
            .expect("idempotent cancel");

        assert_eq!(first.status, RunStatus::Canceled);
        assert_eq!(second.updated_at, first.updated_at, "cancel writes once");
    }
}
