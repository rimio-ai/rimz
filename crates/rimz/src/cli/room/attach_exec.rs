//! Attach action selection, attach-command printing, and execution.

use std::ffi::OsStr;
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rimz::ids::{MuxName, WorkspaceId};

use super::{AttachAction, AttachMode, PrintCause, RoomSituation};

// Terminals with alternate scroll enabled convert wheel ticks to arrow keys
// while the alternate screen has no mouse owner. tmux enters that state while
// repainting an attaching client, so preserve mode 1007 and bracket the client
// with it off. Ghostty 1.3.1 implements XTSAVE/XTRESTORE for this mode.
const ALTERNATE_SCROLL_SAVE: &[u8] = b"\x1b[?1007s";
const ALTERNATE_SCROLL_DISABLE: &[u8] = b"\x1b[?1007l";
const ALTERNATE_SCROLL_RESTORE: &[u8] = b"\x1b[?1007r";
const ATTACH_PROCESS_POLL: Duration = Duration::from_millis(150);
const REMOTE_ROOM_WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);
const EARLY_ATTACH_EXIT_WINDOW: Duration = Duration::from_secs(2);
const MAX_ATTACH_REATTACHES: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteRoomObservation {
    Live,
    Gone,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteRoomPoll {
    Wait(Duration),
    RoomEnded,
}

#[derive(Debug)]
struct RemoteRoomWatchdog {
    room_seen: bool,
    next_observation: Duration,
    interval: Duration,
}

fn remote_room_watchdog_interval(raw: Option<&str>) -> Duration {
    raw.and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(REMOTE_ROOM_WATCHDOG_INTERVAL, Duration::from_millis)
}

impl Default for RemoteRoomWatchdog {
    fn default() -> Self {
        Self {
            room_seen: false,
            next_observation: Duration::ZERO,
            interval: REMOTE_ROOM_WATCHDOG_INTERVAL,
        }
    }
}

impl RemoteRoomWatchdog {
    fn next_observation_in(&self, elapsed: Duration) -> Duration {
        self.next_observation.saturating_sub(elapsed)
    }

    fn observe(&mut self, elapsed: Duration, observation: RemoteRoomObservation) -> bool {
        match observation {
            RemoteRoomObservation::Live => self.room_seen = true,
            RemoteRoomObservation::Gone if self.room_seen => return true,
            RemoteRoomObservation::Gone | RemoteRoomObservation::Unknown => {}
        }
        self.next_observation = elapsed.saturating_add(self.interval);
        false
    }
}

#[derive(Debug)]
enum AttachOutcome {
    Exited(std::process::ExitStatus),
    RoomEnded,
}

struct RemoteRoomMonitor {
    backend: Box<dyn rimz::mux::MuxBackend>,
    session_name: String,
    watchdog: RemoteRoomWatchdog,
    started: Instant,
}

impl RemoteRoomMonitor {
    fn new(mux: MuxName, session_name: &str) -> Self {
        Self {
            backend: rimz::mux::backend_for(mux),
            session_name: session_name.to_owned(),
            watchdog: RemoteRoomWatchdog {
                interval: remote_room_watchdog_interval(
                    std::env::var("RIMZ_REMOTE_WATCHDOG_MS").ok().as_deref(),
                ),
                ..RemoteRoomWatchdog::default()
            },
            started: Instant::now(),
        }
    }

    fn poll(&mut self) -> RemoteRoomPoll {
        let elapsed = self.started.elapsed();
        let delay = self.watchdog.next_observation_in(elapsed);
        if !delay.is_zero() {
            return RemoteRoomPoll::Wait(delay);
        }
        if self.watchdog.observe(
            elapsed,
            observe_remote_room(self.backend.as_ref(), &self.session_name),
        ) {
            RemoteRoomPoll::RoomEnded
        } else {
            RemoteRoomPoll::Wait(self.watchdog.interval)
        }
    }

    fn is_armed(&self) -> bool {
        self.watchdog.room_seen
    }
}

pub(super) fn run_attach_action(
    spec: &rimz::mux::CommandSpec,
    mode: AttachMode,
    mux: MuxName,
    session_name: &str,
    workspace_id: Option<&WorkspaceId>,
) -> Result<()> {
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        inside_selected_mux(mux),
    ) {
        AttachAction::Print(cause) => {
            match cause {
                PrintCause::Explicit => {}
                PrintCause::NoTerminal => writeln!(
                    crate::cli::render::err(),
                    "rimz: no terminal here: the room is open but not entered; printed the attach command instead (same as --print). Pass --attach to enter it anyway."
                )?,
                PrintCause::InsideMux => writeln!(
                    crate::cli::render::err(),
                    "rimz: already inside {mux}: the room is open but not entered; printed the attach command instead (same as --print). Detach from {mux}, then run it again."
                )?,
            }
            print_attach_command(spec);
            Ok(())
        }
        AttachAction::Launch => {
            reap_remote_zellij_predecessors(mux, session_name, workspace_id);
            let supervised = std::env::var_os(rimz::remote::REMOTE_SUPERVISED_ENV)
                .is_some_and(|marker| !marker.is_empty());
            let mut monitor = supervised.then(|| RemoteRoomMonitor::new(mux, session_name));
            let backend = rimz::mux::backend_for(mux);
            let version = backend
                .attach_may_drop_early()
                .then(|| backend.version().ok())
                .flatten();
            let outcome = launch_attach_command_with_bracket(
                spec,
                alternate_scroll_bracket_enabled(
                    std::env::var_os(rimz::remote::OUTER_SCROLL_BRACKET_ENV).as_deref(),
                ),
                |command| {
                    run_attach_attempts(
                        version.as_deref(),
                        &mut std::io::stderr().lock(),
                        || run_attach_process(command, monitor.as_mut()),
                        || observe_remote_room(backend.as_ref(), session_name),
                        |event| {
                            if let Some(workspace_id) = workspace_id {
                                rimz::diag::DiagSink::for_workspace(
                                    workspace_id.clone(),
                                    session_name,
                                    None,
                                )
                                .emit_unlimited(event);
                            }
                        },
                    )
                },
            )?;
            let status = match outcome {
                AttachOutcome::RoomEnded => {
                    std::process::exit(rimz::remote::REMOTE_SESSION_LOST_EXIT)
                }
                AttachOutcome::Exited(status) => status,
            };
            if monitor.as_ref().is_some_and(|monitor| {
                monitor.is_armed()
                    && remote_session_lost_exit(observe_remote_room(
                        monitor.backend.as_ref(),
                        session_name,
                    ))
            }) {
                std::process::exit(rimz::remote::REMOTE_SESSION_LOST_EXIT);
            }
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(())
        }
    }
}

fn remote_session_lost_exit(observation: RemoteRoomObservation) -> bool {
    matches!(observation, RemoteRoomObservation::Gone)
}

fn observe_remote_room(
    backend: &dyn rimz::mux::MuxBackend,
    session_name: &str,
) -> RemoteRoomObservation {
    match backend.session_liveness(session_name) {
        Ok(rimz::mux::SessionLiveness::Live) => RemoteRoomObservation::Live,
        Ok(rimz::mux::SessionLiveness::Exited | rimz::mux::SessionLiveness::Absent) => {
            RemoteRoomObservation::Gone
        }
        Err(_) => RemoteRoomObservation::Unknown,
    }
}

fn reap_remote_zellij_predecessors(
    mux: MuxName,
    session_name: &str,
    workspace_id: Option<&WorkspaceId>,
) {
    if mux != MuxName::Zellij || inside_selected_mux(mux) {
        return;
    }
    let Some(lineage) = std::env::var(rimz::proc::REMOTE_LINEAGE_ENV)
        .ok()
        .filter(|lineage| !lineage.is_empty())
    else {
        return;
    };

    let outcome = rimz::mux::zellij::reap_lineage_clients(
        rimz::mux::backend_for(MuxName::Zellij).as_ref(),
        session_name,
        &lineage,
    )
    .unwrap_or_else(|err| rimz::mux::zellij::ReapOutcome {
        errors: vec![format!("reading the pre-reap client count: {err}")],
        ..rimz::mux::zellij::ReapOutcome::default()
    });
    let degraded = !outcome.settled;
    if let Some(workspace_id) = workspace_id {
        rimz::diag::DiagSink::for_workspace(workspace_id.clone(), session_name, None)
            .emit_unlimited(rimz::diag::record::DiagEvent::ClientReaped {
                killed_pids: outcome.killed_pids,
                pre_clients: outcome.pre_clients,
                post_clients: outcome.post_clients,
                settled: outcome.settled,
                timed_out: outcome.timed_out,
                errors: outcome.errors,
            });
    }
    if degraded {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "rimz: Zellij predecessor cleanup did not settle; attaching anyway"
        );
    }
}

pub(crate) fn attach_action(
    mode: AttachMode,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    inside_target_mux: bool,
) -> AttachAction {
    match mode {
        AttachMode::Attach => AttachAction::Launch,
        AttachMode::Print => AttachAction::Print(PrintCause::Explicit),
        AttachMode::Auto if stdin_is_tty && stdout_is_tty && !inside_target_mux => {
            AttachAction::Launch
        }
        AttachMode::Auto if inside_target_mux => AttachAction::Print(PrintCause::InsideMux),
        AttachMode::Auto => AttachAction::Print(PrintCause::NoTerminal),
    }
}

pub(super) fn inside_selected_mux(mux: MuxName) -> bool {
    match mux {
        MuxName::Zellij => {
            std::env::var_os("ZELLIJ").is_some() || std::env::var_os("ZELLIJ_PANE_ID").is_some()
        }
        MuxName::Tmux => {
            std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some()
        }
    }
}

/// Report the existing room instead of launching only when the attach mode is
/// opportunistic (`Auto`). Explicit `--print` / `--attach` stay literal escape
/// hatches (scripting / forced launch), so they fall through to the normal path.
pub(super) fn should_report_already_inside(mode: AttachMode, inside_mux: bool) -> bool {
    matches!(mode, AttachMode::Auto) && inside_mux
}

pub(super) fn report_already_inside(
    mux: MuxName,
    session: &str,
    situation: RoomSituation,
) -> Result<()> {
    let mut stderr = crate::cli::render::err();
    match situation {
        RoomSituation::CurrentRoom => writeln!(
            stderr,
            "You're already in this directory's room (`{session}`, {mux})."
        )?,
        RoomSituation::LiveElsewhere => {
            writeln!(
                stderr,
                "This directory's room `{session}` is running in another {mux} session."
            )?;
            writeln!(
                stderr,
                "Detach from this session, then run `rimz` here to enter it."
            )?;
        }
        RoomSituation::NotRunning => {
            writeln!(
                stderr,
                "This directory's room isn't running, and {mux} can't start one inside this session."
            )?;
            writeln!(
                stderr,
                "Detach and run `rimz` here, or run it from another terminal."
            )?;
        }
    }
    Ok(())
}

pub(crate) fn launch_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    let outcome = launch_attach_command_with_bracket(spec, true, |command| {
        run_attach_process(command, None).map(|(outcome, _)| outcome)
    })?;
    if let AttachOutcome::Exited(status) = outcome
        && !status.success()
    {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn launch_attach_command_with_bracket(
    spec: &rimz::mux::CommandSpec,
    bracket_alternate_scroll: bool,
    run: impl FnOnce(&mut std::process::Command) -> std::io::Result<AttachOutcome>,
) -> Result<AttachOutcome> {
    let mut command = attach_command(spec);
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout().lock();
    run_attach_command(
        spec,
        &mut stdout,
        stdout_is_terminal,
        std::env::var_os(rimz::remote::REMOTE_SUPERVISED_ENV).as_deref(),
        bracket_alternate_scroll,
        |_| run(&mut command),
    )
}

fn run_attach_command(
    spec: &rimz::mux::CommandSpec,
    stdout: &mut dyn Write,
    stdout_is_terminal: bool,
    supervised: Option<&OsStr>,
    bracket_alternate_scroll: bool,
    run: impl FnOnce(&mut dyn Write) -> std::io::Result<AttachOutcome>,
) -> Result<AttachOutcome> {
    if bracket_alternate_scroll {
        emit_terminal_bytes(stdout, stdout_is_terminal, ALTERNATE_SCROLL_SAVE);
        emit_terminal_bytes(stdout, stdout_is_terminal, ALTERNATE_SCROLL_DISABLE);
    }
    if supervised.is_some_and(|value| !value.is_empty()) {
        stdout.write_all(rimz::remote::tty::ROOM_OPEN_SENTINEL.as_bytes())?;
        stdout.flush()?;
    }
    let status = run(stdout);
    if bracket_alternate_scroll {
        emit_terminal_bytes(stdout, stdout_is_terminal, ALTERNATE_SCROLL_RESTORE);
    }
    status.with_context(|| format!("running `{}`", spec.display_line()))
}

fn run_attach_attempts(
    version: Option<&str>,
    stderr: &mut dyn Write,
    mut run: impl FnMut() -> std::io::Result<(AttachOutcome, Duration)>,
    mut observe: impl FnMut() -> RemoteRoomObservation,
    mut emit: impl FnMut(rimz::diag::record::DiagEvent),
) -> std::io::Result<AttachOutcome> {
    let mut reattaches = 0;
    loop {
        let (outcome, elapsed) = run()?;
        let AttachOutcome::Exited(status) = &outcome else {
            return Ok(outcome);
        };
        let Some(exit_code) = status.code().filter(|code| *code != 0) else {
            return Ok(outcome);
        };
        let Some(version) = version else {
            return Ok(outcome);
        };
        if elapsed >= EARLY_ATTACH_EXIT_WINDOW || observe() != RemoteRoomObservation::Live {
            return Ok(outcome);
        }
        if reattaches == MAX_ATTACH_REATTACHES {
            let _ = writeln!(
                stderr,
                "rimz: the room is still running; run `rimz attach` again"
            );
            return Ok(outcome);
        }
        reattaches += 1;
        emit(rimz::diag::record::DiagEvent::ClientReattached {
            attempt: reattaches,
            exit_code,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            zellij_version: version.to_owned(),
        });
        // Diagnostic I/O can outlive the room; do not spawn on the earlier verdict.
        if observe() != RemoteRoomObservation::Live {
            return Ok(outcome);
        }
        let _ = writeln!(
            stderr,
            concat!(
                "rimz: Zellij refused the client right after it started; ",
                "this is a known race in Zellij before 0.46; attaching again ({}/{})"
            ),
            reattaches, MAX_ATTACH_REATTACHES,
        );
    }
}

#[cfg(not(unix))]
fn run_attach_process(
    command: &mut std::process::Command,
    monitor: Option<&mut RemoteRoomMonitor>,
) -> std::io::Result<(AttachOutcome, Duration)> {
    let mut child = command.spawn()?;
    let started = Instant::now();
    let Some(monitor) = monitor else {
        return child
            .wait()
            .map(|status| (AttachOutcome::Exited(status), started.elapsed()));
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((AttachOutcome::Exited(status), started.elapsed()));
        }
        match monitor.poll() {
            RemoteRoomPoll::RoomEnded => {
                child.kill()?;
                child.wait()?;
                return Ok((AttachOutcome::RoomEnded, started.elapsed()));
            }
            RemoteRoomPoll::Wait(delay) => {
                std::thread::sleep(delay.min(ATTACH_PROCESS_POLL));
            }
        }
    }
}

#[cfg(unix)]
fn run_attach_process(
    command: &mut std::process::Command,
    mut monitor: Option<&mut RemoteRoomMonitor>,
) -> std::io::Result<(AttachOutcome, Duration)> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill, raise};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt;

    let child = command.spawn()?;
    let started = Instant::now();
    let pid = Pid::from_raw(i32::try_from(child.id()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "child pid does not fit i32",
        )
    })?);
    let wait_flags = monitor.as_ref().map_or(WaitPidFlag::WUNTRACED, |_| {
        WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED
    });
    loop {
        match waitpid(pid, Some(wait_flags)) {
            Ok(WaitStatus::Exited(_, code)) => {
                return Ok((
                    AttachOutcome::Exited(std::process::ExitStatus::from_raw(code << 8)),
                    started.elapsed(),
                ));
            }
            Ok(WaitStatus::Signaled(_, signal, dumped_core)) => {
                let raw = signal as i32 | if dumped_core { 0x80 } else { 0 };
                return Ok((
                    AttachOutcome::Exited(std::process::ExitStatus::from_raw(raw)),
                    started.elapsed(),
                ));
            }
            Ok(WaitStatus::Stopped(_, _)) => {
                // tmux's suspend-client stops only itself. Mirror that stop so
                // the launching shell regains its prompt, then resume the child
                // when this foreground job is continued.
                raise(Signal::SIGTSTP).map_err(errno_io)?;
                match kill(pid, Signal::SIGCONT) {
                    Ok(()) | Err(Errno::ESRCH) => {}
                    Err(err) => return Err(errno_io(err)),
                }
            }
            Ok(WaitStatus::StillAlive) => {
                let Some(monitor) = monitor.as_deref_mut() else {
                    continue;
                };
                match monitor.poll() {
                    RemoteRoomPoll::RoomEnded => {
                        match kill(pid, Signal::SIGKILL) {
                            Ok(()) | Err(Errno::ESRCH) => {}
                            Err(err) => return Err(errno_io(err)),
                        }
                        loop {
                            match waitpid(pid, None) {
                                Ok(_) | Err(Errno::ECHILD) => break,
                                Err(Errno::EINTR) => {}
                                Err(err) => return Err(errno_io(err)),
                            }
                        }
                        return Ok((AttachOutcome::RoomEnded, started.elapsed()));
                    }
                    RemoteRoomPoll::Wait(delay) => {
                        std::thread::sleep(delay.min(ATTACH_PROCESS_POLL));
                    }
                }
            }
            Ok(_) => {}
            Err(Errno::EINTR) => {}
            Err(err) => return Err(errno_io(err)),
        }
    }
}

#[cfg(unix)]
fn errno_io(err: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(err as i32)
}

fn emit_terminal_bytes(stdout: &mut dyn Write, stdout_is_terminal: bool, bytes: &[u8]) {
    if stdout_is_terminal && stdout.write_all(bytes).is_ok() {
        let _ = stdout.flush();
    }
}

fn attach_command(spec: &rimz::mux::CommandSpec) -> std::process::Command {
    let mut command = spec.to_command();
    command.env_remove(rimz::remote::OUTER_SCROLL_BRACKET_ENV);
    command
}

fn alternate_scroll_bracket_enabled(outer_bracket: Option<&OsStr>) -> bool {
    outer_bracket.is_none_or(OsStr::is_empty)
}

fn print_attach_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", spec.display_line());
    }
}

#[cfg(test)]
mod tests;
