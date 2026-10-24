//! Child process lifecycle helpers. Long-lived RimZ processes hand
//! fire-and-forget children to the global reaper and supervise foreground
//! children with event-driven exit and signal waits. A child that outlives an
//! agent's call gets the user's temp environment back, not the agent's unit.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::RuntimePaths;

#[cfg(unix)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const WAIT_STEP: Duration = Duration::from_millis(25);

const TAIL_BYTES: usize = 8 * 1024;

/// Clear inherited environment while preserving this process's LLVM profile path.
pub fn clear_env_preserving_coverage(command: &mut Command) {
    command.env_clear();
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
}

/// A line-boundary stderr tail with an 8 KiB text budget.
#[derive(Default)]
pub struct StderrTail {
    bytes: Vec<u8>,
    cut: bool,
}

impl StderrTail {
    pub fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > TAIL_BYTES {
            let excess = self.bytes.len() - TAIL_BYTES;
            self.cut = self.bytes[excess - 1] != b'\n';
            self.bytes.drain(..excess);
        }
    }

    pub fn text(&self) -> String {
        let start = if self.cut {
            self.bytes
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(self.bytes.len(), |index| index + 1)
        } else {
            0
        };
        let text = String::from_utf8_lossy(&self.bytes[start..]);
        if text.len() <= TAIL_BYTES {
            return text.into_owned();
        }
        // Lossy replacement can expand invalid bytes beyond the byte budget.
        let excess = text.len() - TAIL_BYTES;
        let start = text.as_bytes()[excess..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(text.len(), |index| excess + index + 1);
        text[start..].to_owned()
    }
}

static REAPER: OnceLock<Sender<Child>> = OnceLock::new();
static REAPER_INIT: Mutex<()> = Mutex::new(());

#[cfg(test)]
static REAPER_STARTS: AtomicUsize = AtomicUsize::new(0);

/// Keep cleanup signals pending across an image handoff and out of worker threads.
#[cfg(unix)]
pub struct CleanupSignalMask {
    previous: nix::sys::signal::SigSet,
}

#[cfg(unix)]
impl CleanupSignalMask {
    pub fn block() -> io::Result<Self> {
        use nix::sys::signal::{SigSet, SigmaskHow, pthread_sigmask};
        let mut previous = SigSet::empty();
        pthread_sigmask(
            SigmaskHow::SIG_BLOCK,
            Some(&Self::signals()),
            Some(&mut previous),
        )?;
        Ok(Self { previous })
    }

    pub fn unblock() -> io::Result<()> {
        use nix::sys::signal::{SigmaskHow, pthread_sigmask};
        pthread_sigmask(SigmaskHow::SIG_UNBLOCK, Some(&Self::signals()), None)?;
        Ok(())
    }

    fn signals() -> nix::sys::signal::SigSet {
        use nix::sys::signal::{SigSet, Signal};
        let mut signals = SigSet::empty();
        for signal in [Signal::SIGHUP, Signal::SIGTERM, Signal::SIGINT] {
            signals.add(signal);
        }
        signals
    }
}

#[cfg(unix)]
impl Drop for CleanupSignalMask {
    fn drop(&mut self) {
        use nix::sys::signal::{SigmaskHow, pthread_sigmask};
        if let Err(error) = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&self.previous), None) {
            tracing::error!(%error, "could not restore cleanup signal mask");
        }
    }
}

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
    /// Adopt the child retained across a same-process exec.
    #[cfg(unix)]
    pub fn adopt_pid(pid: u32, wake: Sender<()>) -> Self {
        Self::adopt_target(pid, WaitTarget::Pid(pid), wake)
    }

    #[cfg(unix)]
    fn adopt_target(pid: u32, target: WaitTarget, wake: Sender<()>) -> Self {
        let _worker_mask = CleanupSignalMask::block()
            .inspect_err(|error| tracing::warn!(%error, "could not mask child waiter signals"))
            .ok();
        let status = Arc::new(Mutex::new(None));
        let waiter = SupervisedWaiter {
            target: Some(target),
            status: status.clone(),
            wake: wake.clone(),
        };
        if let Err(err) = std::thread::Builder::new()
            .name("rimz-child-wait".to_owned())
            .spawn(move || waiter.wait())
        {
            tracing::warn!(pid, error = %err, "supervised child waiter could not start; child waited inline");
        }
        Self {
            pid,
            status,
            _wake: wake,
        }
    }

    /// Move `child` into a waiter thread and notify `wake` when it exits.
    pub fn adopt(child: Child, wake: Sender<()>) -> Self {
        #[cfg(unix)]
        {
            Self::adopt_target(child.id(), WaitTarget::Child(child), wake)
        }

        #[cfg(not(unix))]
        Self { child, _wake: wake }
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
        signal_term_with_scope(self.pid, crate::proc::cgroup::scope_of(self.pid), |pid| {
            signal_child(pid, ChildSignal::Term)
        });
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
fn signal_term_with_scope(
    pid: u32,
    scope: Option<crate::proc::cgroup::ScopePath>,
    signal: impl FnOnce(u32),
) {
    if let Some(scope) = scope
        && let Err(error) = crate::proc::cgroup::thaw(&scope)
    {
        tracing::warn!(pid, %error, "could not thaw supervised child before SIGTERM");
    }
    signal(pid);
}

#[cfg(unix)]
struct SupervisedWaiter {
    target: Option<WaitTarget>,
    status: Arc<Mutex<Option<io::Result<std::process::ExitStatus>>>>,
    wake: Sender<()>,
}

#[cfg(unix)]
impl SupervisedWaiter {
    fn wait(mut self) {
        if let Some(target) = self.target.take() {
            self.finish(target.wait());
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
        if let Some(target) = self.target.take() {
            self.finish(target.wait());
        }
    }
}

#[cfg(unix)]
enum WaitTarget {
    Child(Child),
    Pid(u32),
}

#[cfg(unix)]
impl WaitTarget {
    fn wait(self) -> io::Result<std::process::ExitStatus> {
        match self {
            Self::Child(mut child) => child.wait(),
            Self::Pid(pid) => wait_pid(pid),
        }
    }
}

#[cfg(unix)]
fn wait_pid(pid: u32) -> io::Result<std::process::ExitStatus> {
    use nix::sys::wait::{WaitStatus, waitpid};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt;

    loop {
        match waitpid(Pid::from_raw(pid as i32), None) {
            Ok(WaitStatus::Exited(_, code)) => {
                return Ok(std::process::ExitStatus::from_raw(code << 8));
            }
            Ok(WaitStatus::Signaled(_, signal, core_dumped)) => {
                return Ok(std::process::ExitStatus::from_raw(
                    signal as i32 | if core_dumped { 0x80 } else { 0 },
                ));
            }
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

/// Forward process signals to an event-driven supervisor.
#[cfg(unix)]
pub fn register_signal_wake(signals: Vec<i32>, wake: Sender<()>) -> io::Result<()> {
    let _worker_mask = CleanupSignalMask::block()?;
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

/// SIGTERM a process only while its durable start token still identifies it.
pub fn signal_process_term(pid: u32, expected_start: Option<&str>) -> bool {
    if !crate::proc::process_is_live(pid, expected_start) {
        return false;
    }
    #[cfg(unix)]
    {
        signal_child(pid, ChildSignal::Term);
        true
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The `TMPDIR` a launch replaced with the agent's temp unit, kept so a child
/// that outlives the agent's call gets the user's value instead. Present and
/// empty means the user had none; absent means the process is not in an
/// agent's tree.
pub(crate) const USER_TMPDIR_ENV: &str = "RIMZ_USER_TMPDIR";

/// The provider temp-root keys a launch pointed at the agent's temp unit,
/// space-separated, so the restore can drop them beside `TMPDIR` without
/// naming any provider.
pub(crate) const TEMP_ROOT_KEYS_ENV: &str = "RIMZ_TEMP_ROOT_KEYS";

/// The user's temp environment for a child spawned from an agent's tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UserTempEnv<'a> {
    /// The `TMPDIR` to set; `None` removes it.
    pub(crate) tmpdir: Option<&'a str>,
    /// The listed temp-root keys and both save variables, none of which may
    /// reach the child.
    pub(crate) removed: Vec<&'a str>,
}

/// Decide the user's temp environment from the caller's saved `TMPDIR`
/// ([`USER_TMPDIR_ENV`]) and temp-root key list ([`TEMP_ROOT_KEYS_ENV`]).
/// `None` means the caller is not in an agent's tree: leave the child alone.
pub(crate) fn user_temp_env<'a>(
    saved: Option<&'a str>,
    temp_root_keys: Option<&'a str>,
) -> Option<UserTempEnv<'a>> {
    let saved = saved?;
    let removed = [USER_TMPDIR_ENV, TEMP_ROOT_KEYS_ENV]
        .into_iter()
        .chain(temp_root_keys.unwrap_or_default().split_whitespace())
        .collect();
    Some(UserTempEnv {
        tmpdir: (!saved.is_empty()).then_some(saved),
        removed,
    })
}

/// Give `cmd` the user's temp environment, as `user_temp_env` decides from
/// this process's save variables, so a child that outlives the agent's call
/// never carries the agent's temp unit, which GC removes after its owner ends.
pub fn restore_user_temp_env(cmd: &mut Command) {
    apply_user_temp_env(
        cmd,
        std::env::var(USER_TMPDIR_ENV).ok().as_deref(),
        std::env::var(TEMP_ROOT_KEYS_ENV).ok().as_deref(),
    );
}

fn apply_user_temp_env(cmd: &mut Command, saved: Option<&str>, temp_root_keys: Option<&str>) {
    let Some(restore) = user_temp_env(saved, temp_root_keys) else {
        return;
    };
    for key in restore.removed {
        cmd.env_remove(key);
    }
    match restore.tmpdir {
        Some(tmpdir) => cmd.env("TMPDIR", tmpdir),
        None => cmd.env_remove("TMPDIR"),
    };
}

/// Build a detached `rimz` command with fresh null stdio, anchored to RimZ-owned
/// shared disk usage so a deleted launch CWD cannot ENOENT the spawn.
pub(crate) fn detached_rimz_command(exe: PathBuf, runtime: &RuntimePaths) -> Command {
    let mut cmd = Command::new(exe);
    anchor_detached_command(&mut cmd, runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

pub(crate) fn anchor_detached_command<'a>(
    cmd: &'a mut Command,
    runtime: &RuntimePaths,
) -> &'a mut Command {
    cmd.current_dir(&runtime.shared_root)
}

/// Render one typed hidden-agent-helper request as a single JSON argv value.
pub fn agent_helper_argv<T: Serialize>(verb: &str, request: &T) -> Vec<String> {
    // These internal request types contain only scalar values and cannot fail
    // JSON serialization.
    let request = serde_json::to_string(request)
        .expect("scalar hidden-agent-helper request serializes as JSON");
    vec![
        "agents".to_owned(),
        verb.to_owned(),
        "--request".to_owned(),
        request,
    ]
}

/// Spawn one detached RimZ helper. Unit tests compile the exact command but
/// suppress the subprocess centrally so policy tests cannot leak helpers.
pub(crate) fn spawn_detached_rimz<I, S>(
    runtime: &RuntimePaths,
    args: I,
    label: &'static str,
) -> io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    #[cfg(not(test))]
    {
        spawn_detached_reaped(
            crate::proc::rimz_exe(),
            |cmd| {
                anchor_detached_command(cmd, runtime).args(&args);
                Ok(())
            },
            label,
        )
        .map(drop)
    }
    #[cfg(test)]
    {
        let _ = (runtime, args, label);
        Ok(())
    }
}

/// Spawn a detached program in its own process group. Unit tests suppress the
/// subprocess and return no pid.
pub(crate) fn spawn_detached_program(
    program: &OsStr,
    args: &[std::ffi::OsString],
    runtime: &RuntimePaths,
    label: &'static str,
) -> io::Result<Option<u32>> {
    #[cfg(not(test))]
    {
        spawn_detached_reaped(
            program,
            |cmd| {
                anchor_detached_command(cmd, runtime).args(args);
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    cmd.process_group(0);
                }
                Ok(())
            },
            label,
        )
        .map(Some)
    }
    #[cfg(test)]
    {
        let _ = (program, args, runtime, label);
        Ok(None)
    }
}

/// Spawn a configured command and hand its `Child` to the global reaper so the
/// exited helper is `wait()`ed and never lingers as a zombie under a long-lived
/// parent. Stdio starts null; `configure` sets arguments, environment and logs.
/// It may run twice when a scope spawn fails, preserving configuration on retry.
/// `label` is a static tag for tracing. Returns the spawned pid. The child
/// gets the user's temp environment ([`restore_user_temp_env`]), since it
/// outlives the call that starts it. The child stays in the caller's process
/// group. A helper that must outlive the pane its caller runs in sets
/// `process_group(0)` in `configure`, as `workflow::engine::spawn`, the
/// signal wake of workflows and the exec wrapper's worktree cleanup do.
pub fn spawn_detached_reaped(
    program: impl AsRef<OsStr>,
    configure: impl FnMut(&mut Command) -> io::Result<()>,
    label: &'static str,
) -> io::Result<u32> {
    let prefix = cfg!(target_os = "linux")
        .then(|| std::fs::read_to_string("/proc/self/cgroup").ok())
        .flatten()
        .and_then(|text| helper_scope_argv(&text));
    spawn_detached_with(program.as_ref(), configure, label, prefix.as_deref())
}

/// Ten times systemd's default weight: the drainer outranks busy siblings in
/// `app.slice` without starving interactive agents. Helpers its apply children
/// spawn, a signal-fired loop run among them, inherit the scope and its weight.
const DRAINER_SCOPE_PROPERTIES: [&str; 2] = ["-p", "CPUWeight=1000"];

/// [`spawn_detached_reaped`] for the hook drainer, which on Linux requests its
/// own weighted helper scope from every spawner, with the same plain fallback.
pub(crate) fn spawn_hook_drainer(
    program: impl AsRef<OsStr>,
    configure: impl FnMut(&mut Command) -> io::Result<()>,
) -> io::Result<u32> {
    spawn_detached_with(
        program.as_ref(),
        configure,
        "hook-drainer",
        drainer_scope_prefix().as_deref(),
    )
}

fn drainer_scope_prefix() -> Option<Vec<String>> {
    cfg!(target_os = "linux").then(|| scope_argv(&DRAINER_SCOPE_PROPERTIES))
}

fn spawn_detached_with(
    program: &OsStr,
    mut configure: impl FnMut(&mut Command) -> io::Result<()>,
    label: &'static str,
    prefix: Option<&[String]>,
) -> io::Result<u32> {
    let mut command = match prefix.and_then(|argv| argv.split_first()) {
        Some((wrapper, args)) => {
            let mut command = Command::new(wrapper);
            command.args(args).arg(program);
            command
        }
        None => Command::new(program),
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure(&mut command)?;
    restore_user_temp_env(&mut command);
    let sender = reaper_sender()?;
    crate::proc::testkit::count_spawn();
    match command.spawn() {
        Ok(mut child) => {
            let unit = prefix.and_then(|argv| {
                argv.windows(2)
                    .find(|pair| pair[0] == "--unit")
                    .map(|pair| pair[1].as_str())
            });
            if prefix.is_some()
                && !wait_for_helper_scope(&mut child, unit).unwrap_or_else(|error| {
                    tracing::debug!(label, %error, "could not confirm helper scope handoff");
                    false
                })
            {
                let _ = child.kill();
                child.wait()?;
                tracing::debug!(
                    label,
                    "helper scope handoff failed; spawning without scope escape"
                );
                return spawn_detached_with(program, configure, label, None);
            }
            Ok(reap_child(child, sender, label))
        }
        Err(error) if prefix.is_some() => {
            tracing::debug!(label, %error, "helper scope spawn failed; spawning without scope escape");
            spawn_detached_with(program, configure, label, None)
        }
        Err(error) => Err(error),
    }
}

fn helper_scope_matches(cgroup: &str, unit: Option<&str>) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .any(|group| {
            std::path::Path::new(group)
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| Some(name) == unit)
        })
}

fn wait_for_helper_scope(child: &mut Child, unit: Option<&str>) -> io::Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let membership = format!("/proc/{}/cgroup", child.id());
    loop {
        if std::fs::read_to_string(&membership).is_ok_and(|text| helper_scope_matches(&text, unit))
        {
            return Ok(true);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(remaining.min(WAIT_STEP));
    }
}

fn helper_scope_argv(cgroup: &str) -> Option<Vec<String>> {
    let group = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
    let name = std::path::Path::new(group).file_name()?.to_str()?;
    if !name.starts_with("rimz-agent-") || !name.ends_with(".scope") {
        return None;
    }
    Some(scope_argv(&[]))
}

fn scope_argv(properties: &[&str]) -> Vec<String> {
    let mut argv: Vec<_> = [
        "systemd-run",
        "--user",
        "--scope",
        "--quiet",
        "--collect",
        "--unit",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    argv.push(format!(
        "rimz-helper-{}.scope",
        uuid::Uuid::now_v7().simple()
    ));
    argv.extend(properties.iter().map(|property| (*property).to_owned()));
    argv.push("--".into());
    argv
}

/// [`spawn_detached_reaped`] without the temp restore, for a child that runs
/// the agent's own command and so keeps the agent's temp unit.
pub(crate) fn spawn_detached_reaped_as_agent(
    cmd: &mut Command,
    label: &'static str,
) -> io::Result<u32> {
    let sender = reaper_sender()?;
    let child = cmd.spawn()?;
    Ok(reap_child(child, sender, label))
}

fn reap_child(child: Child, sender: &Sender<Child>, label: &'static str) -> u32 {
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

    pid
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

    #[cfg(unix)]
    let _worker_mask = CleanupSignalMask::block()?;
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
    #[cfg(unix)]
    let _worker_mask = CleanupSignalMask::block()
        .inspect_err(|error| tracing::warn!(%error, "could not mask fallback waiter signals"))
        .ok();
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
mod tests;
