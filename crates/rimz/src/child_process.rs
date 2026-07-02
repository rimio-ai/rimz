//! Detached child process helpers. Long-lived Rimz processes hand
//! fire-and-forget children to the global reaper so exited helpers are
//! `wait()`ed and cannot linger as zombies under the Rimz parent.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::RuntimePaths;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

static REAPER: OnceLock<Sender<Child>> = OnceLock::new();
static REAPER_INIT: Mutex<()> = Mutex::new(());

#[cfg(test)]
static REAPER_STARTS: AtomicUsize = AtomicUsize::new(0);

/// Build a detached `rimz` helper command, anchored to Rimz-owned shared
/// storage so a deleted launch CWD cannot ENOENT the spawn.
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
    const WAIT_STEP: Duration = Duration::from_millis(25);

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

    fn spawn_fast_child(label: &'static str) -> io::Result<u32> {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        spawn_detached_reaped(&mut cmd, label)
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
            std::thread::sleep(WAIT_STEP);
        }
    }
}
