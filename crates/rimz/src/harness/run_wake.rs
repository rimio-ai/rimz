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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::harness::run::{self, RunRecord, RunStatus};
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

#[derive(Debug, thiserror::Error)]
pub enum WaitUntilTerminalErr<E>
where
    E: std::fmt::Debug + std::fmt::Display + 'static,
{
    #[error("creating run wait runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("waiting on run wake socket: {0}")]
    Socket(#[source] RunWakeErr),
    #[error("reading or updating durable run state: {0}")]
    Store(#[source] crate::store::run_store::RunStoreErr),
    #[error("observing durable run state: {0}")]
    Observer(E),
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

/// Block on one supervised run until its durable record becomes terminal.
///
/// Wake frames only shorten the next durable reload. Terminal records win over
/// an interrupt or deadline observed in the same iteration.
pub fn wait_until_terminal<E>(
    sock: StdUnixDatagram,
    expected: ExpectedRunFrame,
    paths: &crate::store::StatePaths,
    timeout: Option<Duration>,
    interrupt: &AtomicBool,
    mut observer: impl FnMut(&RunRecord) -> std::result::Result<(), E>,
) -> std::result::Result<RunRecord, WaitUntilTerminalErr<E>>
where
    E: std::fmt::Debug + std::fmt::Display + 'static,
{
    const WAIT_TICK: Duration = Duration::from_millis(250);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(WaitUntilTerminalErr::Runtime)?;
    let deadline = timeout.map(|duration| Instant::now() + duration);
    runtime.block_on(async {
        let sock = adopt(sock).map_err(WaitUntilTerminalErr::Socket)?;
        loop {
            let record = run::load(paths, &expected.run_id).map_err(WaitUntilTerminalErr::Store)?;
            observer(&record).map_err(WaitUntilTerminalErr::Observer)?;
            if record.status.is_terminal() {
                return Ok(record);
            }

            if interrupt.load(Ordering::SeqCst) {
                let (record, _wrote) =
                    run::cancel(paths, &expected.run_id).map_err(WaitUntilTerminalErr::Store)?;
                observer(&record).map_err(WaitUntilTerminalErr::Observer)?;
                return Ok(record);
            }

            let wait = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        let record = run::timeout(paths, &expected.run_id)
                            .map_err(WaitUntilTerminalErr::Store)?;
                        observer(&record).map_err(WaitUntilTerminalErr::Observer)?;
                        return Ok(record);
                    }
                    (deadline - now).min(WAIT_TICK)
                }
                None => WAIT_TICK,
            };

            let _hint = wait_for_run_completion(&sock, &expected, Some(wait))
                .await
                .map_err(WaitUntilTerminalErr::Socket)?;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::run::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::store::StatePaths;
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

        sender
            .send_to(b"not-json", &sock_path)
            .await
            .expect("send malformed");
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

    struct RunFixture {
        _dir: tempfile::TempDir,
        workspace_id: WorkspaceId,
        paths: StatePaths,
        runtime: RuntimePaths,
        record: RunRecord,
    }

    impl RunFixture {
        fn new(status: RunStatus) -> Self {
            let dir = short_runtime_root();
            let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
            let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
            let runtime =
                RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
            paths.ensure_dirs().expect("state dirs");
            runtime.ensure_dirs().expect("runtime dirs");
            let mut record = RunRecord::new(
                workspace_id.clone(),
                AgentKind::new_unchecked("codex"),
                PermissionMode::Auto,
                "go".to_owned(),
                Path::new("/tmp/rimz-run").to_path_buf(),
            );
            record.status = status;
            run::create(&paths, &record).expect("create run");
            Self {
                _dir: dir,
                workspace_id,
                paths,
                runtime,
                record,
            }
        }

        fn expected(&self) -> ExpectedRunFrame {
            ExpectedRunFrame {
                workspace_id: self.workspace_id.clone(),
                run_id: self.record.run_id.clone(),
            }
        }

        fn bind(&self) -> (StdUnixDatagram, PathBuf) {
            bind_run(&self.runtime, &self.record.run_id).expect("bind run")
        }

        fn write_status(&self, status: RunStatus) {
            let mut record = run::load(&self.paths, &self.record.run_id).expect("load run");
            record.status = status;
            crate::store::run_store::write(&self.paths.runs_dir, &record).expect("write run");
        }
    }

    #[test]
    fn durable_terminal_wins_before_interrupt_and_zero_timeout() {
        let fixture = RunFixture::new(RunStatus::Completed);
        let (sock, path) = fixture.bind();
        let _guard = SocketGuard::new(path);
        let interrupt = AtomicBool::new(true);
        let mut observed = Vec::new();

        let record = wait_until_terminal(
            sock,
            fixture.expected(),
            &fixture.paths,
            Some(Duration::ZERO),
            &interrupt,
            |record| {
                observed.push(record.status);
                Ok::<(), std::io::Error>(())
            },
        )
        .expect("terminal wait");

        assert_eq!(record.status, RunStatus::Completed);
        assert_eq!(observed, vec![RunStatus::Completed]);
    }

    #[test]
    fn interrupt_cancels_and_observes_final_durable_record() {
        let fixture = RunFixture::new(RunStatus::Running);
        let (sock, path) = fixture.bind();
        let _guard = SocketGuard::new(path);
        let interrupt = AtomicBool::new(true);
        let mut observed = Vec::new();

        let record = wait_until_terminal(
            sock,
            fixture.expected(),
            &fixture.paths,
            None,
            &interrupt,
            |record| {
                observed.push(record.status);
                Ok::<(), std::io::Error>(())
            },
        )
        .expect("interrupted wait");

        assert_eq!(record.status, RunStatus::Canceled);
        assert_eq!(observed, vec![RunStatus::Running, RunStatus::Canceled]);
        assert_eq!(
            run::load(&fixture.paths, &fixture.record.run_id)
                .expect("load canceled")
                .status,
            RunStatus::Canceled
        );
    }

    #[test]
    fn timeout_marks_and_observes_final_durable_record() {
        let fixture = RunFixture::new(RunStatus::Running);
        let (sock, path) = fixture.bind();
        let _guard = SocketGuard::new(path);
        let mut observed = Vec::new();

        let record = wait_until_terminal(
            sock,
            fixture.expected(),
            &fixture.paths,
            Some(Duration::ZERO),
            &AtomicBool::new(false),
            |record| {
                observed.push(record.status);
                Ok::<(), std::io::Error>(())
            },
        )
        .expect("timed wait");

        assert_eq!(record.status, RunStatus::TimedOut);
        assert_eq!(observed, vec![RunStatus::Running, RunStatus::TimedOut]);
    }

    #[test]
    fn wake_status_is_only_a_reload_hint() {
        let fixture = RunFixture::new(RunStatus::Running);
        let (sock, path) = fixture.bind();
        let _guard = SocketGuard::new(path.clone());
        let frame = WakeupFrame::RunCompleted {
            workspace_id: fixture.workspace_id.clone(),
            run_id: fixture.record.run_id.clone(),
            status: RunStatus::Completed,
        };
        let sender = StdUnixDatagram::unbound().expect("sender");
        sender
            .send_to(&serde_json::to_vec(&frame).expect("frame"), &path)
            .expect("send frame");

        let record = wait_until_terminal(
            sock,
            fixture.expected(),
            &fixture.paths,
            Some(Duration::from_millis(10)),
            &AtomicBool::new(false),
            |_| Ok::<(), std::io::Error>(()),
        )
        .expect("wait after hint");

        assert_eq!(record.status, RunStatus::TimedOut);
    }

    #[test]
    fn lost_wake_still_observes_durable_terminal_record() {
        let fixture = RunFixture::new(RunStatus::Running);
        let (sock, path) = fixture.bind();
        let _guard = SocketGuard::new(path);

        let record = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                fixture.write_status(RunStatus::Failed);
            });
            wait_until_terminal(
                sock,
                fixture.expected(),
                &fixture.paths,
                Some(Duration::from_secs(1)),
                &AtomicBool::new(false),
                |_| Ok::<(), std::io::Error>(()),
            )
        })
        .expect("wait without wake");

        assert_eq!(record.status, RunStatus::Failed);
    }

    #[test]
    fn observer_failure_is_typed_and_leaves_run_unchanged() {
        let fixture = RunFixture::new(RunStatus::Running);
        let (sock, path) = fixture.bind();
        let guard = SocketGuard::new(path.clone());

        let err = wait_until_terminal(
            sock,
            fixture.expected(),
            &fixture.paths,
            None,
            &AtomicBool::new(false),
            |_| Err(std::io::Error::other("sink closed")),
        )
        .expect_err("observer failure");

        assert!(matches!(err, WaitUntilTerminalErr::Observer(_)));
        assert_eq!(
            run::load(&fixture.paths, &fixture.record.run_id)
                .expect("load unchanged")
                .status,
            RunStatus::Running
        );
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn caller_guard_cleans_socket_after_success_timeout_and_interrupt() {
        fn assert_cleanup(status: RunStatus, timeout: Option<Duration>, interrupted: bool) {
            let fixture = RunFixture::new(status);
            let (sock, path) = fixture.bind();
            {
                let _guard = SocketGuard::new(path.clone());
                wait_until_terminal(
                    sock,
                    fixture.expected(),
                    &fixture.paths,
                    timeout,
                    &AtomicBool::new(interrupted),
                    |_| Ok::<(), std::io::Error>(()),
                )
                .expect("terminal wait");
            }
            assert!(!path.exists());
        }

        assert_cleanup(RunStatus::Completed, None, false);
        assert_cleanup(RunStatus::Running, Some(Duration::ZERO), false);
        assert_cleanup(RunStatus::Running, None, true);
    }
}
