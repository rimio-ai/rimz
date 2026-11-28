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
    /// The loop's own `-w`, which the fire hands the launch unless the tail names it already.
    pub(super) worktree: Option<&'a str>,
}

/// The launch a task's fires make, as its check parsed it: what `loop add` reads back in its
/// receipt and defaults a window's provider and account from.
#[derive(Debug, Default, PartialEq)]
pub(super) struct LoopLaunch {
    /// The worktree the fire's launch arguments name, the loop's `-w` included; `Some("")` is
    /// a bare `-w`, which generates a new name each fire.
    pub(super) worktree: Option<String>,
    /// The tail's own `-w` names the loop's, so the row stores none and the fire passes one.
    pub(super) tail_names_worktree: bool,
    pub(super) prompt: bool,
    /// A `-p` run.
    pub(super) print: bool,
    /// The tail's `--stay`; `false` is a run that ends itself, as `-p` is.
    pub(super) stay: rimz::harness::block::Stay,
    /// The tail's `--timeout`, for an agent or a team.
    pub(super) timeout: Option<std::time::Duration>,
    /// The tail's `--account`.
    pub(super) account: Option<rimz::ids::LoginName>,
    /// The agent kinds the target resolves to as the launch resolves it, each once, in layout
    /// order; empty where the check resolves none (a team, a workflow).
    pub(super) kinds: Vec<String>,
}

/// Check the launch a fire of `kind` makes with `target` and `args`. A shell task's argv is
/// refused only when it is itself a `rimz` launch a kind flag declares. The loop's own `-w` is
/// checked against the worktree the tail names, then handed to the launch as the fire will.
pub(super) fn check(
    kind: LaunchKind,
    target: Option<&str>,
    args: &[String],
    place: &LoopLaunchPlace<'_>,
) -> Result<LoopLaunch> {
    let Some(target) = target else {
        check_shell(args)?;
        return Ok(LoopLaunch::default());
    };
    let tail = TaskLaunch {
        kind,
        target: target.to_owned(),
        worktree: None,
        args: args.to_vec(),
    };
    let alone = check_parsed(&tail, place);
    let Some(loop_worktree) = place.worktree else {
        return alone;
    };
    // A tail flag that needs a worktree (`--from-trunk`) parses only beside the loop's `-w`,
    // so a tail that fails alone is checked as the fire will send it; a tail naming its own
    // `-w` cannot parse there, so its own error stands.
    let described = match alone {
        Ok(described) => described,
        Err(error) => {
            let placed = TaskLaunch {
                worktree: Some(loop_worktree.to_owned()),
                ..tail
            };
            return check_parsed(&placed, place).map_err(|_| error);
        }
    };
    match described.worktree.as_deref() {
        None => check_parsed(
            &TaskLaunch {
                worktree: Some(loop_worktree.to_owned()),
                ..tail
            },
            place,
        ),
        Some(named) if named == loop_worktree => Ok(LoopLaunch {
            tail_names_worktree: true,
            ..described
        }),
        Some("") => bail!(
            "-w `{loop_worktree}` and the launch's worktree (a generated name) differ; keep one"
        ),
        Some(named) => {
            bail!("-w `{loop_worktree}` and the launch's worktree `{named}` differ; keep one")
        }
    }
}

/// Parse `launch` as its fire's `rimz` child will and hand it to its command's check. A parse
/// failure names the launching command, without the target or the loop's `-w`.
fn check_parsed(launch: &TaskLaunch, place: &LoopLaunchPlace<'_>) -> Result<LoopLaunch> {
    let written = TaskLaunch {
        worktree: None,
        ..launch.clone()
    }
    .argv();
    let head = written[..written.len() - launch.args.len() - 1].join(" ");
    let argv = std::iter::once("rimz".to_owned()).chain(launch.argv());
    let cli = Cli::try_parse_from(argv).map_err(|err| {
        anyhow::anyhow!(
            "the tail does not parse as `rimz {head}`:\n{}",
            err.render().to_string().trim_end()
        )
    })?;
    refuse_global_flags(&cli)?;
    match (launch.kind, cli.subcommand) {
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
