//! Child process lifecycle helpers. Long-lived RimZ processes hand
//! fire-and-forget children to the global reaper and supervise foreground
//! children with event-driven exit and signal waits.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::RuntimePaths;

#[cfg(unix)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const WAIT_STEP: Duration = Duration::from_millis(25);

static REAPER: OnceLock<Sender<Child>> = OnceLock::new();
static REAPER_INIT: Mutex<()> = Mutex::new(());

#[cfg(test)]
static REAPER_STARTS: AtomicUsize = AtomicUsize::new(0);

/// A child whose exit can wake an event-driven supervisor.
pub struct SupervisedChild {
    #[cfg(unix)]
    pid: u32,
    #[cfg(unix)]
    status: Arc<Mutex<Option<io::Result<std::process::ExitStatus>>>>,
    #[cfg(not(unix))]
    child: Child,
    _wake: Sender<()>,
}

impl SupervisedChild {
    /// Move `child` into a waiter thread and notify `wake` when it exits.
    pub fn adopt(child: Child, wake: Sender<()>) -> Self {
        #[cfg(unix)]
        {
            let pid = child.id();
            let status = Arc::new(Mutex::new(None));
            let waiter = SupervisedWaiter {
                child: Some(child),
                status: status.clone(),
                wake: wake.clone(),
            };
            if let Err(err) = std::thread::Builder::new()
                .name("rimz-child-wait".to_owned())
                .spawn(move || waiter.wait())
            {
                tracing::warn!(
                    pid,
                    error = %err,
                    "supervised child waiter could not start; child waited inline"
                );
            }
            Self {
                pid,
                status,
                _wake: wake,
            }
        }

        #[cfg(not(unix))]
        Self { child, _wake: wake }
    }

    pub fn id(&self) -> u32 {
        #[cfg(unix)]
        {
            self.pid
        }
        #[cfg(not(unix))]
        {
            self.child.id()
        }
    }

    /// Check whether the child has exited without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        #[cfg(unix)]
        {
            let status = match self.status.lock() {
                Ok(status) => status,
                Err(poisoned) => poisoned.into_inner(),
            };
            match status.as_ref() {
                Some(Ok(status)) => Ok(Some(*status)),
                Some(Err(err)) => Err(io::Error::new(err.kind(), err.to_string())),
                None => Ok(None),
            }
        }
        #[cfg(not(unix))]
        loop {
            match self.child.try_wait() {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }

    /// Ask the child to terminate.
    pub fn signal_term(&mut self) {
        #[cfg(unix)]
        signal_child(self.pid, ChildSignal::Term);
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }

    /// Force the child to exit.
    pub fn signal_kill(&mut self) {
        #[cfg(unix)]
        signal_child(self.pid, ChildSignal::Kill);
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }
}

#[cfg(unix)]
struct SupervisedWaiter {
    child: Option<Child>,
    status: Arc<Mutex<Option<io::Result<std::process::ExitStatus>>>>,
    wake: Sender<()>,
}

#[cfg(unix)]
impl SupervisedWaiter {
    fn wait(mut self) {
        if let Some(mut child) = self.child.take() {
            self.finish(child.wait());
        }
    }

    fn finish(&self, result: io::Result<std::process::ExitStatus>) {
        let mut status = match self.status.lock() {
            Ok(status) => status,
            Err(poisoned) => poisoned.into_inner(),
        };
        *status = Some(result);
        drop(status);
        let _ = self.wake.send(());
    }
}

#[cfg(unix)]
impl Drop for SupervisedWaiter {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            self.finish(child.wait());
        }
    }
}

/// Forward process signals to an event-driven supervisor.
#[cfg(unix)]
pub fn register_signal_wake(signals: Vec<i32>, wake: Sender<()>) -> io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new(signals)?;
    std::thread::Builder::new()
        .name("rimz-signal-wake".to_owned())
        .spawn(move || {
            for _ in signals.forever() {
                let _ = wake.send(());
            }
        })?;
    Ok(())
}

/// Signal wakeups are unavailable off Unix; polling remains bounded there.
#[cfg(not(unix))]
pub fn register_signal_wake(_signals: Vec<i32>, _wake: Sender<()>) -> io::Result<()> {
    Ok(())
}

/// Block until an event arrives or the optional deadline expires.
pub fn wait_wake(rx: &Receiver<()>, deadline: Option<Instant>) {
    #[cfg(unix)]
    let timeout = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
    #[cfg(not(unix))]
    let timeout = Some(
        deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(WAIT_STEP)
            .min(WAIT_STEP),
    );

    let result = match timeout {
        Some(timeout) => rx.recv_timeout(timeout).map(|_| ()),
        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
    };
    if matches!(result, Err(RecvTimeoutError::Disconnected)) {
        std::thread::sleep(WAIT_STEP);
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum ChildSignal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_child(pid: u32, signal: ChildSignal) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let signal = match signal {
        ChildSignal::Term => Signal::SIGTERM,
        ChildSignal::Kill => Signal::SIGKILL,
    };
    let _ = kill(Pid::from_raw(pid as i32), signal);
}

/// Build a detached `rimz` helper command, anchored to RimZ-owned shared
/// disk_usage so a deleted launch CWD cannot ENOENT the spawn.
pub(crate) fn detached_rimz_command(exe: PathBuf, runtime: &RuntimePaths) -> Command {
    let mut cmd = Command::new(exe);
    cmd.current_dir(&runtime.shared_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// Spawn `cmd` detached and hand its `Child` to the global reaper thread so the
/// exited helper is `wait()`ed and never lingers as a zombie under a long-lived
/// parent. Fire-and-forget: callers null stdio and set their own argv/timeouts.
/// `label` is a static tag for tracing. Returns the spawned pid.
pub fn spawn_detached_reaped(cmd: &mut Command, label: &'static str) -> io::Result<u32> {
    let sender = reaper_sender()?;
    let child = cmd.spawn()?;
    let pid = child.id();

    match sender.send(child) {
        Ok(()) => {
            tracing::debug!(label, pid, "detached child handed to reaper");
        }
        Err(err) => {
            tracing::warn!(
                label,
                pid,
                "global child reaper unavailable; using fallback waiter"
            );
            spawn_fallback_waiter(err.0, label);
        }
    }

    Ok(pid)
}

fn reaper_sender() -> io::Result<&'static Sender<Child>> {
    if let Some(sender) = REAPER.get() {
        return Ok(sender);
    }

    let _guard = match REAPER_INIT.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(sender) = REAPER.get() {
        return Ok(sender);
    }

    let (tx, rx) = mpsc::channel::<Child>();
    std::thread::Builder::new()
        .name("rimz-child-reaper".to_owned())
        .spawn(move || reaper_loop(rx))?;
    #[cfg(test)]
    REAPER_STARTS.fetch_add(1, Ordering::SeqCst);
    let _ = REAPER.set(tx);

    REAPER
        .get()
        .ok_or_else(|| io::Error::other("child reaper sender unavailable after initialization"))
}

fn reaper_loop(rx: Receiver<Child>) {
    let mut pending = Vec::new();

    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(child) => pending.push(child),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if pending.is_empty() => return,
            Err(RecvTimeoutError::Disconnected) => {}
        }

        while let Ok(child) = rx.try_recv() {
            pending.push(child);
        }

        pending.retain_mut(|child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                tracing::debug!(pid = child.id(), ?status, "detached child reaped");
                false
            }
            Err(err) => {
                tracing::debug!(pid = child.id(), error = %err, "detached child reap failed");
                false
            }
        });
    }
}

fn spawn_fallback_waiter(child: Child, label: &'static str) {
    let pid = child.id();
    let waiter = WaitOnDrop {
        child: Some(child),
        label,
        pid,
    };
    if let Err(err) = std::thread::Builder::new()
        .name("rimz-child-fallback-reaper".to_owned())
        .spawn(move || waiter.wait())
    {
        tracing::warn!(
            label,
            pid,
            error = %err,
            "fallback child waiter could not start; child waited inline"
        );
    }
}

struct WaitOnDrop {
    child: Option<Child>,
    label: &'static str,
    pid: u32,
}

impl WaitOnDrop {
    fn wait(mut self) {
        if let Some(mut child) = self.child.take() {
            log_wait_result(child.wait(), self.label, self.pid);
        }
    }
}

impl Drop for WaitOnDrop {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            log_wait_result(child.wait(), self.label, self.pid);
        }
    }
}

fn log_wait_result(result: io::Result<std::process::ExitStatus>, label: &'static str, pid: u32) {
    match result {
        Ok(status) => tracing::debug!(label, pid, ?status, "detached child reaped"),
        Err(err) => tracing::debug!(label, pid, error = %err, "detached child reap failed"),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const WAIT_TIMEOUT: Duration = Duration::from_secs(3);
    const REAP_WAIT_STEP: Duration = Duration::from_millis(25);

    #[test]
    fn spawn_fast_child_returns_pid_and_reaps_it() {
        let pid = spawn_fast_child("child-process-fast").expect("spawn fast child");

        assert!(pid > 0);
        wait_until_reaped(pid);
    }

    #[test]
    fn detached_rimz_command_anchors_cwd_to_shared_root() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        let cmd = detached_rimz_command(std::path::PathBuf::from("/nonexistent/rimz"), &runtime);

        assert_eq!(cmd.get_current_dir(), Some(runtime.shared_root.as_path()));
    }

    #[test]
    fn many_spawns_share_one_reaper_thread() {
        let starts_before = REAPER_STARTS.load(Ordering::SeqCst);
        let pids = (0..16)
            .map(|_| spawn_fast_child("child-process-many").expect("spawn fast child"))
            .collect::<Vec<_>>();

        for pid in pids {
            wait_until_reaped(pid);
        }

        let starts_after = REAPER_STARTS.load(Ordering::SeqCst);
        assert!(
            starts_after <= starts_before + 1,
            "expected at most one global reaper start, got before={starts_before} after={starts_after}"
        );
    }

    #[test]
    fn supervised_child_exit_wakes_without_polling() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let child = Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .expect("spawn supervised child");
        let mut child = SupervisedChild::adopt(child, wake_tx.clone());
        wake_after(wake_tx, WAIT_TIMEOUT);

        wait_wake(&wake_rx, None);
        let status = child
            .try_wait()
            .expect("read supervised child status")
            .expect("child exit wake arrived before watchdog");

        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn supervised_child_signals_stop_running_children() {
        for signal in [
            SupervisedChild::signal_term as fn(&mut SupervisedChild),
            SupervisedChild::signal_kill,
        ] {
            let (wake_tx, wake_rx) = mpsc::channel();
            let child = Command::new("sleep")
                .arg("5")
                .spawn()
                .expect("spawn supervised sleep");
            let mut child = SupervisedChild::adopt(child, wake_tx.clone());
            wake_after(wake_tx, WAIT_TIMEOUT);

            signal(&mut child);
            wait_wake(&wake_rx, None);
            child
                .try_wait()
                .expect("read signaled child status")
                .expect("child signal wake arrived before watchdog");
        }
    }

    #[test]
    fn registered_signal_wakes_blocked_receiver() {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        use signal_hook::consts::signal::SIGUSR1;

        let (wake_tx, wake_rx) = mpsc::channel();
        register_signal_wake(vec![SIGUSR1], wake_tx.clone()).expect("register signal wake");
        wake_after(wake_tx, WAIT_TIMEOUT);

        let started = Instant::now();
        kill(Pid::this(), Signal::SIGUSR1).expect("raise SIGUSR1");
        wait_wake(&wake_rx, None);

        assert!(
            started.elapsed() < WAIT_TIMEOUT,
            "signal wake came from watchdog"
        );
    }

    fn spawn_fast_child(label: &'static str) -> io::Result<u32> {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        spawn_detached_reaped(&mut cmd, label)
    }

    fn wake_after(wake: Sender<()>, delay: Duration) {
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = wake.send(());
        });
    }

    fn wait_until_reaped(pid: u32) {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            match crate::proc::stat_metrics(pid).map(|stat| stat.state) {
                None => return,
                Some('Z') => {}
                Some(_) if Instant::now() < deadline => {}
                Some(state) => panic!("pid {pid} remained alive with state {state:?}"),
            }

            assert!(
                Instant::now() < deadline,
                "pid {pid} was not reaped; last state was {:?}",
                crate::proc::stat_metrics(pid).map(|stat| stat.state)
            );
            std::thread::sleep(REAP_WAIT_STEP);
        }
    }
}
