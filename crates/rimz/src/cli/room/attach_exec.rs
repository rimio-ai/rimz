//! Attach action selection, attach-command printing, and execution.

use std::ffi::OsStr;
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rimz::ids::{MuxName, WorkspaceId};

use super::{AttachAction, AttachMode};

// The local recovery panel parks its cursor on the Multiplexer symbol cell;
// this in-band marker lands there immediately before the mux paints.
const ATTACH_MARK: &[u8] = b"\x1b[32m\xe2\x9c\x93\x1b[39m";
// Terminals with alternate scroll enabled convert wheel ticks to arrow keys
// while the alternate screen has no mouse owner. tmux enters that state while
// repainting an attaching client, so preserve mode 1007 and bracket the client
// with it off. Ghostty 1.3.1 implements XTSAVE/XTRESTORE for this mode.
const ALTERNATE_SCROLL_SAVE: &[u8] = b"\x1b[?1007s";
const ALTERNATE_SCROLL_DISABLE: &[u8] = b"\x1b[?1007l";
const ALTERNATE_SCROLL_RESTORE: &[u8] = b"\x1b[?1007r";
const ATTACH_PROCESS_POLL: Duration = Duration::from_millis(150);
const REMOTE_ROOM_WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

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

#[derive(Debug, Default)]
struct RemoteRoomWatchdog {
    room_seen: bool,
    next_observation: Duration,
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
        self.next_observation = elapsed.saturating_add(REMOTE_ROOM_WATCHDOG_INTERVAL);
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
}

impl RemoteRoomMonitor {
    fn new(mux: MuxName, session_name: &str) -> Self {
        Self {
            backend: rimz::mux::backend_for(mux),
            session_name: session_name.to_owned(),
            watchdog: RemoteRoomWatchdog::default(),
        }
    }

    fn poll(&mut self, elapsed: Duration) -> RemoteRoomPoll {
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
            RemoteRoomPoll::Wait(REMOTE_ROOM_WATCHDOG_INTERVAL)
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
        AttachAction::Print => {
            print_attach_command(spec);
            Ok(())
        }
        AttachAction::Launch => {
            reap_remote_zellij_predecessors(mux, session_name, workspace_id);
            let supervised = std::env::var_os(rimz::remote::REMOTE_SUPERVISED_ENV)
                .is_some_and(|marker| !marker.is_empty());
            let mut monitor = supervised.then(|| RemoteRoomMonitor::new(mux, session_name));
            let outcome = launch_attach_command_with_bracket(
                spec,
                alternate_scroll_bracket_enabled(
                    std::env::var_os(rimz::remote::OUTER_SCROLL_BRACKET_ENV).as_deref(),
                ),
                monitor.as_mut(),
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
    let Some(lineage) = std::env::var(rimz::remote::REMOTE_LINEAGE_ENV)
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
        AttachMode::Print => AttachAction::Print,
        AttachMode::Auto if stdin_is_tty && stdout_is_tty && !inside_target_mux => {
            AttachAction::Launch
        }
        AttachMode::Auto => AttachAction::Print,
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
    workspace: &rimz::ResolvedWorkspace,
) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "You're already inside a {mux} session, which can't host a nested room.",
    )?;
    writeln!(
        stderr,
        "This directory's room is `{}`. Detach to (re)launch it, or run `rimz` from outside the session.",
        workspace.session_name,
    )?;
    Ok(())
}

pub(crate) fn launch_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    let outcome = launch_attach_command_with_bracket(spec, true, None)?;
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
    monitor: Option<&mut RemoteRoomMonitor>,
) -> Result<AttachOutcome> {
    let mut command = attach_command(spec);
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout().lock();
    run_attach_command(
        spec,
        &mut stdout,
        stdout_is_terminal,
        std::env::var_os(rimz::remote::ATTACH_MARK_ENV).as_deref(),
        bracket_alternate_scroll,
        |_| run_attach_process(&mut command, monitor),
    )
}

fn run_attach_command(
    spec: &rimz::mux::CommandSpec,
    stdout: &mut dyn Write,
    stdout_is_terminal: bool,
    attach_mark: Option<&OsStr>,
    bracket_alternate_scroll: bool,
    run: impl FnOnce(&mut dyn Write) -> std::io::Result<AttachOutcome>,
) -> Result<AttachOutcome> {
    if bracket_alternate_scroll {
        emit_terminal_bytes(stdout, stdout_is_terminal, ALTERNATE_SCROLL_SAVE);
        emit_terminal_bytes(stdout, stdout_is_terminal, ALTERNATE_SCROLL_DISABLE);
    }
    emit_attach_mark(stdout, attach_mark, stdout_is_terminal);
    let status = run(stdout);
    if bracket_alternate_scroll {
        emit_terminal_bytes(stdout, stdout_is_terminal, ALTERNATE_SCROLL_RESTORE);
    }
    status.with_context(|| format!("running `{}`", spec.display_line()))
}

#[cfg(not(unix))]
fn run_attach_process(
    command: &mut std::process::Command,
    monitor: Option<&mut RemoteRoomMonitor>,
) -> std::io::Result<AttachOutcome> {
    let Some(monitor) = monitor else {
        return command.status().map(AttachOutcome::Exited);
    };
    let started = Instant::now();
    let mut child = command.spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(AttachOutcome::Exited(status));
        }
        match monitor.poll(started.elapsed()) {
            RemoteRoomPoll::RoomEnded => {
                child.kill()?;
                child.wait()?;
                return Ok(AttachOutcome::RoomEnded);
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
) -> std::io::Result<AttachOutcome> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill, raise};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt;

    let started = monitor.as_ref().map(|_| Instant::now());
    let child = command.spawn()?;
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
                return Ok(AttachOutcome::Exited(std::process::ExitStatus::from_raw(
                    code << 8,
                )));
            }
            Ok(WaitStatus::Signaled(_, signal, dumped_core)) => {
                let raw = signal as i32 | if dumped_core { 0x80 } else { 0 };
                return Ok(AttachOutcome::Exited(std::process::ExitStatus::from_raw(
                    raw,
                )));
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
                let elapsed = started.as_ref().map_or(Duration::ZERO, Instant::elapsed);
                match monitor.poll(elapsed) {
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
                        return Ok(AttachOutcome::RoomEnded);
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

fn emit_attach_mark(stdout: &mut dyn Write, mark: Option<&OsStr>, stdout_is_terminal: bool) {
    if !attach_mark_enabled(mark, stdout_is_terminal) {
        return;
    }
    emit_terminal_bytes(stdout, true, ATTACH_MARK);
}

fn emit_terminal_bytes(stdout: &mut dyn Write, stdout_is_terminal: bool, bytes: &[u8]) {
    if stdout_is_terminal && stdout.write_all(bytes).is_ok() {
        let _ = stdout.flush();
    }
}

fn attach_mark_enabled(value: Option<&OsStr>, stdout_is_terminal: bool) -> bool {
    stdout_is_terminal && value.is_some_and(|value| !value.is_empty())
}

fn attach_command(spec: &rimz::mux::CommandSpec) -> std::process::Command {
    let mut command = spec.to_command();
    command.env_remove(rimz::remote::ATTACH_MARK_ENV);
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
mod tests {
    use super::*;

    #[test]
    fn attach_action_matrix_and_nested_report_policy() {
        assert_eq!(
            attach_action(AttachMode::Auto, true, true, false),
            AttachAction::Launch,
        );
        assert_eq!(
            attach_action(AttachMode::Auto, false, true, false),
            AttachAction::Print,
        );
        assert_eq!(
            attach_action(AttachMode::Auto, true, false, false),
            AttachAction::Print,
        );
        assert_eq!(
            attach_action(AttachMode::Auto, true, true, true),
            AttachAction::Print,
        );
        assert_eq!(
            attach_action(AttachMode::Attach, false, false, true),
            AttachAction::Launch,
        );
        assert_eq!(
            attach_action(AttachMode::Print, true, true, false),
            AttachAction::Print,
        );

        assert!(should_report_already_inside(AttachMode::Auto, true));
        assert!(!should_report_already_inside(AttachMode::Auto, false));
        assert!(!should_report_already_inside(AttachMode::Print, true));
        assert!(!should_report_already_inside(AttachMode::Attach, true));
    }

    #[test]
    fn attach_marker_requires_a_nonempty_request_and_a_tty() {
        assert!(attach_mark_enabled(Some(OsStr::new("1")), true));
        assert!(!attach_mark_enabled(Some(OsStr::new("")), true));
        assert!(!attach_mark_enabled(None, true));
        assert!(!attach_mark_enabled(Some(OsStr::new("1")), false));
    }

    #[test]
    fn attach_markers_do_not_reach_the_multiplexer_environment() {
        let command = attach_command(&rimz::mux::CommandSpec::new("mux"));

        assert!(command.get_envs().any(|(key, value)| {
            key == OsStr::new(rimz::remote::ATTACH_MARK_ENV) && value.is_none()
        }));
        assert!(command.get_envs().any(|(key, value)| {
            key == OsStr::new(rimz::remote::OUTER_SCROLL_BRACKET_ENV) && value.is_none()
        }));
    }

    #[test]
    fn outer_scroll_marker_suppresses_a_nested_bracket() {
        assert!(alternate_scroll_bracket_enabled(None));
        assert!(alternate_scroll_bracket_enabled(Some(OsStr::new(""))));
        assert!(!alternate_scroll_bracket_enabled(Some(OsStr::new("1"))));
    }

    #[test]
    fn alternate_scroll_sequences_are_exact() {
        assert_eq!(ALTERNATE_SCROLL_SAVE, b"\x1b[?1007s");
        assert_eq!(ALTERNATE_SCROLL_DISABLE, b"\x1b[?1007l");
        assert_eq!(ALTERNATE_SCROLL_RESTORE, b"\x1b[?1007r");
    }

    #[test]
    fn remote_session_loss_translates_only_missing_sessions() {
        assert!(remote_session_lost_exit(RemoteRoomObservation::Gone));
        assert!(!remote_session_lost_exit(RemoteRoomObservation::Live));
        assert!(!remote_session_lost_exit(RemoteRoomObservation::Unknown));
    }

    #[test]
    fn remote_room_watchdog_arms_on_live_and_fires_on_gone() {
        let mut watchdog = RemoteRoomWatchdog::default();

        assert_eq!(watchdog.next_observation_in(Duration::ZERO), Duration::ZERO);
        assert!(!watchdog.observe(Duration::ZERO, RemoteRoomObservation::Live));
        assert_eq!(
            watchdog.next_observation_in(Duration::ZERO),
            REMOTE_ROOM_WATCHDOG_INTERVAL
        );
        assert_eq!(
            watchdog.next_observation_in(REMOTE_ROOM_WATCHDOG_INTERVAL),
            Duration::ZERO
        );
        assert!(watchdog.observe(REMOTE_ROOM_WATCHDOG_INTERVAL, RemoteRoomObservation::Gone,));
    }

    #[test]
    fn remote_room_watchdog_fails_open_until_live_is_observed() {
        let mut watchdog = RemoteRoomWatchdog::default();

        assert!(!watchdog.observe(Duration::ZERO, RemoteRoomObservation::Unknown));
        assert!(!watchdog.observe(REMOTE_ROOM_WATCHDOG_INTERVAL, RemoteRoomObservation::Gone,));
    }

    #[cfg(unix)]
    #[test]
    fn attach_brackets_child_output_and_preserves_its_failure() {
        use std::os::unix::process::ExitStatusExt;

        let spec = rimz::mux::CommandSpec::new("sh").args(["-c", "printf child; exit 23"]);
        let mut output = Vec::new();
        let status = run_attach_command(&spec, &mut output, true, None, true, |stdout| {
            stdout.write_all(b"child")?;
            Ok(AttachOutcome::Exited(std::process::ExitStatus::from_raw(
                23 << 8,
            )))
        })
        .expect("child status");

        assert_eq!(
            output,
            [
                ALTERNATE_SCROLL_SAVE,
                ALTERNATE_SCROLL_DISABLE,
                b"child",
                ALTERNATE_SCROLL_RESTORE,
            ]
            .concat(),
        );
        let AttachOutcome::Exited(status) = status else {
            panic!("expected child exit")
        };
        assert_eq!(status.code(), Some(23));
    }

    #[test]
    fn attach_restores_terminal_bracket_when_the_room_ends() {
        let spec = rimz::mux::CommandSpec::new("mux");
        let mut output = Vec::new();
        let outcome = run_attach_command(&spec, &mut output, true, None, true, |_| {
            Ok(AttachOutcome::RoomEnded)
        })
        .expect("room-ended outcome");

        assert!(matches!(outcome, AttachOutcome::RoomEnded));
        assert_eq!(
            output,
            [
                ALTERNATE_SCROLL_SAVE,
                ALTERNATE_SCROLL_DISABLE,
                ALTERNATE_SCROLL_RESTORE,
            ]
            .concat(),
        );
    }

    #[test]
    fn attach_restores_after_launch_error_and_skips_non_terminal_sequences() {
        let spec = rimz::mux::CommandSpec::new("mux");
        let mut terminal_output = Vec::new();
        let result = run_attach_command(&spec, &mut terminal_output, true, None, true, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing mux",
            ))
        });
        assert_eq!(
            terminal_output,
            [
                ALTERNATE_SCROLL_SAVE,
                ALTERNATE_SCROLL_DISABLE,
                ALTERNATE_SCROLL_RESTORE,
            ]
            .concat(),
        );
        assert!(result.unwrap_err().to_string().contains("running `mux`"));

        let mut piped_output = Vec::new();
        let result = run_attach_command(&spec, &mut piped_output, false, None, true, |stdout| {
            stdout.write_all(b"child")?;
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing mux",
            ))
        });
        assert_eq!(piped_output, b"child");
        assert!(result.unwrap_err().to_string().contains("running `mux`"));

        let mut nested_output = Vec::new();
        let result = run_attach_command(&spec, &mut nested_output, true, None, false, |stdout| {
            stdout.write_all(b"child")?;
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing mux",
            ))
        });
        assert_eq!(nested_output, b"child");
        assert!(result.unwrap_err().to_string().contains("running `mux`"));
    }
}
