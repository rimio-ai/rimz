//! Attach action selection, attach-command printing, and execution.

use std::ffi::OsStr;
use std::io::{IsTerminal, Write};

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
            launch_attach_command_with_bracket(
                spec,
                alternate_scroll_bracket_enabled(
                    std::env::var_os(rimz::remote::OUTER_SCROLL_BRACKET_ENV).as_deref(),
                ),
            )
        }
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
    launch_attach_command_with_bracket(spec, true)
}

fn launch_attach_command_with_bracket(
    spec: &rimz::mux::CommandSpec,
    bracket_alternate_scroll: bool,
) -> Result<()> {
    let mut command = attach_command(spec);
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout().lock();
    let status = run_attach_command(
        spec,
        &mut stdout,
        stdout_is_terminal,
        std::env::var_os(rimz::remote::ATTACH_MARK_ENV).as_deref(),
        bracket_alternate_scroll,
        |_| run_attach_process(&mut command),
    )?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn run_attach_command(
    spec: &rimz::mux::CommandSpec,
    stdout: &mut dyn Write,
    stdout_is_terminal: bool,
    attach_mark: Option<&OsStr>,
    bracket_alternate_scroll: bool,
    run: impl FnOnce(&mut dyn Write) -> std::io::Result<std::process::ExitStatus>,
) -> Result<std::process::ExitStatus> {
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
) -> std::io::Result<std::process::ExitStatus> {
    command.status()
}

#[cfg(unix)]
fn run_attach_process(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::ExitStatus> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill, raise};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt;

    let child = command.spawn()?;
    let pid = Pid::from_raw(i32::try_from(child.id()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "child pid does not fit i32",
        )
    })?);
    loop {
        match waitpid(pid, Some(WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Exited(_, code)) => {
                return Ok(std::process::ExitStatus::from_raw(code << 8));
            }
            Ok(WaitStatus::Signaled(_, signal, dumped_core)) => {
                let raw = signal as i32 | if dumped_core { 0x80 } else { 0 };
                return Ok(std::process::ExitStatus::from_raw(raw));
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

    #[cfg(unix)]
    #[test]
    fn attach_brackets_child_output_and_preserves_its_failure() {
        use std::os::unix::process::ExitStatusExt;

        let spec = rimz::mux::CommandSpec::new("sh").args(["-c", "printf child; exit 23"]);
        let mut output = Vec::new();
        let status = run_attach_command(&spec, &mut output, true, None, true, |stdout| {
            stdout.write_all(b"child")?;
            Ok(std::process::ExitStatus::from_raw(23 << 8))
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
        assert_eq!(status.code(), Some(23));
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
