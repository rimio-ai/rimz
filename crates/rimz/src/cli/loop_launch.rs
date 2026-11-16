//! `rimz loop add`'s check of the launch a task's fires make.
//!
//! The fire's argv is parsed here with the root parser, exactly as the fire's own `rimz` child
//! will parse it, and the parsed launch goes to the launching command's own check. This is the
//! one entry `loop_cmd` reaches another command's parse through (`cli/AGENTS.md`).

use std::path::Path;

use anyhow::{Result, bail};
use clap::Parser;
use rimz::config::LaunchKind;
use rimz::harness::schedule::TaskLaunch;

use super::{Cli, ColorWhen, Subcmd};

/// Where a task's fires launch, as the launching command's check needs it.
pub(super) struct LoopLaunchPlace<'a> {
    pub(super) project_root: &'a Path,
    pub(super) each_worktree: bool,
    pub(super) fresh_worktree: bool,
}

/// Check the launch a fire of `kind` makes with `target` and `args`. A shell task's argv is
/// refused only when it is itself a `rimz` launch a kind flag declares.
pub(super) fn check(
    kind: LaunchKind,
    target: Option<&str>,
    args: &[String],
    place: &LoopLaunchPlace<'_>,
) -> Result<()> {
    let Some(target) = target else {
        return check_shell(args);
    };
    let launch = TaskLaunch {
        kind,
        target: target.to_owned(),
        args: args.to_vec(),
    }
    .argv();
    let head = launch[..launch.len() - args.len() - 1].join(" ");
    let argv = std::iter::once("rimz".to_owned()).chain(launch);
    let cli = Cli::try_parse_from(argv).map_err(|err| {
        anyhow::anyhow!(
            "the tail does not parse as `rimz {head}`:\n{}",
            err.render().to_string().trim_end()
        )
    })?;
    refuse_global_flags(&cli)?;
    match (kind, cli.subcommand) {
        (LaunchKind::Agent, Some(Subcmd::Agents(parsed))) => {
            super::agents_cmd::check_loop_launch(&parsed, place)
        }
        (LaunchKind::Team, Some(Subcmd::Teams(parsed))) => {
            super::teams::check_loop_launch(&parsed, place)
        }
        (LaunchKind::Workflow, Some(Subcmd::Workflow(parsed))) => {
            super::workflow::check_loop_launch(&parsed, place)
        }
        _ => bail!("the tail does not parse as a `rimz {head}` launch"),
    }
}

/// A launch's tail sets none of the root's global flags: the fire runs in its own checkout,
/// with the room's own backend.
fn refuse_global_flags(cli: &Cli) -> Result<()> {
    let global = &cli.global;
    let set = [
        (global.mux.is_some(), "--mux"),
        (global.zellij, "--zellij"),
        (global.tmux, "--tmux"),
        (global.root.is_some(), "--root"),
        (global.color != ColorWhen::Auto, "--color"),
    ]
    .into_iter()
    .find_map(|(set, flag)| set.then_some(flag));
    if let Some(flag) = set {
        bail!("the tail sets the global flag {flag}; a fire launches in its own checkout's room");
    }
    Ok(())
}

/// A shell argv whose program is `rimz` and which reads as a launch a kind flag declares is
/// refused with that flag; `sh -c '…'` and a script stay shell tasks.
fn check_shell(argv: &[String]) -> Result<()> {
    let is_rimz = argv
        .first()
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == "rimz");
    if !is_rimz {
        return Ok(());
    }
    let Ok(cli) = Cli::try_parse_from(argv) else {
        return Ok(());
    };
    let flag = match cli.subcommand {
        Some(Subcmd::Agents(parsed)) if parsed.is_launch() => "--agent <SPEC>",
        Some(Subcmd::Teams(parsed)) if parsed.is_launch() => "--team <TEAM[#LANE]>",
        Some(Subcmd::Workflow(parsed)) if parsed.is_run() => "--workflow <NAME>",
        _ => return Ok(()),
    };
    bail!(
        "the command is a `rimz` launch; declare it with {flag} and put the rest of its arguments after `--`"
    )
}
