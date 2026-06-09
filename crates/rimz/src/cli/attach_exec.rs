use super::*;

pub(super) fn run_attach_action(
    spec: &rimz::mux::CommandSpec,
    mode: AttachMode,
    mux: MuxName,
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
        AttachAction::Exec => exec_attach_command(spec),
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

    let mut command = spec.to_command();
    let err = command.exec();
    Err::<(), _>(err).with_context(|| format!("execing `{}`", command_display(spec)))
}

#[cfg(not(unix))]
pub(crate) fn exec_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", command_display(spec)))?;
    if !status.success() {
        anyhow::bail!(
            "attach command `{}` exited with {status}",
            command_display(spec)
        );
    }
    Ok(())
}

fn print_attach_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", command_display(spec));
    }
}

fn command_display(spec: &rimz::mux::CommandSpec) -> String {
    if spec.args.is_empty() {
        spec.program.clone()
    } else {
        format!("{} {}", spec.program, spec.args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_action_matrix() {
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
    }

    #[test]
    fn report_already_inside_only_when_auto_and_nested() {
        assert!(should_report_already_inside(AttachMode::Auto, true));
        assert!(!should_report_already_inside(AttachMode::Auto, false));
        assert!(!should_report_already_inside(AttachMode::Print, true));
        assert!(!should_report_already_inside(AttachMode::Attach, true));
    }
}
