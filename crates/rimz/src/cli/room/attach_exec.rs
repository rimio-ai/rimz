//! Attach action selection, attach-command printing, and exec.

use std::ffi::OsStr;
use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use rimz::ids::{MuxName, WorkspaceId};

use super::{AttachAction, AttachMode};

// The local recovery panel parks its cursor on the Multiplexer symbol cell;
// this in-band marker lands there immediately before the mux paints.
const ATTACH_MARK: &[u8] = b"\x1b[32m\xe2\x9c\x93\x1b[39m";

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
        AttachAction::Exec => {
            reap_remote_zellij_predecessors(mux, session_name, workspace_id);
            exec_attach_command(spec)
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
        AttachMode::Attach => AttachAction::Exec,
        AttachMode::Print => AttachAction::Print,
        AttachMode::Auto if stdin_is_tty && stdout_is_tty && !inside_target_mux => {
            AttachAction::Exec
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
/// hatches (scripting / forced exec), so they fall through to the normal path.
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

#[cfg(unix)]
pub(crate) fn exec_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    use std::os::unix::process::CommandExt;

    emit_attach_mark();
    let mut command = spec.to_command();
    let err = command.exec();
    Err::<(), _>(err).with_context(|| format!("execing `{}`", spec.display_line()))
}

#[cfg(not(unix))]
pub(crate) fn exec_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    emit_attach_mark();
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", spec.display_line()))?;
    if !status.success() {
        anyhow::bail!(
            "attach command `{}` exited with {status}",
            spec.display_line()
        );
    }
    Ok(())
}

fn emit_attach_mark() {
    let mark = std::env::var_os(rimz::remote::ATTACH_MARK_ENV);
    if !attach_mark_enabled(mark.as_deref(), std::io::stdout().is_terminal()) {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(ATTACH_MARK).is_ok() {
        let _ = stdout.flush();
    }
}

fn attach_mark_enabled(value: Option<&OsStr>, stdout_is_terminal: bool) -> bool {
    stdout_is_terminal && value.is_some_and(|value| !value.is_empty())
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
            AttachAction::Exec,
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
            AttachAction::Exec,
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
}
