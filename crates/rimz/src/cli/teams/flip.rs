//! Resolve a live team cohort and present the durable stage-flip receipt.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use rimz::harness::ancestry::{resolve_caller, resolve_launch_caller};
use rimz::harness::team_stage::{self, Compaction, Delivery, FlipRequest, Flipper};
use rimz::utils::path::normalize_path_lexical;

use super::super::{Ctx, GlobalFlags, render};
use super::FlipArgs;

pub(super) fn run(args: FlipArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let worktree = match std::env::var_os(rimz::workspace::ENV_WORKTREE_PATH) {
        Some(path) => normalize_path_lexical(Path::new(&path)),
        None => {
            let output = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(std::env::current_dir()?)
                .output()
                .context("resolve the current worktree with git")?;
            if !output.status.success() {
                bail!("cannot resolve the current worktree; run from a git worktree");
            }
            let path =
                String::from_utf8(output.stdout).context("git worktree path is not UTF-8")?;
            normalize_path_lexical(Path::new(path.trim()))
        }
    };
    let cohorts = rimz::address::team_cohorts(&snapshot.agents);
    let candidates = cohorts
        .iter()
        .filter(|cohort| args.team.as_deref().is_none_or(|team| cohort.team == team))
        .filter(|cohort| {
            cohort.members.iter().all(|member| {
                member
                    .worktree_path
                    .as_deref()
                    .is_some_and(|path| normalize_path_lexical(Path::new(path)) == worktree)
            })
        })
        .collect::<Vec<_>>();
    let cohort = match candidates.as_slice() {
        [cohort] => *cohort,
        [] => bail!(
            "no matching live team cohort in worktree {}",
            worktree.display()
        ),
        [first, rest @ ..] if rest.iter().all(|cohort| cohort.team == first.team) => bail!(
            "team `{}` has multiple live cohorts in worktree {} in channels {}; stop the extra cohorts with `rimz teams stop {} -w <channel>` before flipping",
            first.team,
            worktree.display(),
            candidates
                .iter()
                .map(|cohort| format!("#{}", cohort.channel))
                .collect::<Vec<_>>()
                .join(", "),
            first.team
        ),
        _ => bail!(
            "multiple live team cohorts in worktree {}; select a team with --team <name>: {}",
            worktree.display(),
            candidates
                .iter()
                .map(|cohort| format!("{}#{}", cohort.team, cohort.channel))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let team_name = cohort.team;
    let caller = resolve_caller(&snapshot.agents)
        .map(|caller| resolve_launch_caller(&snapshot.agents, &caller))
        .transpose()?;
    let member = caller.filter(|caller| {
        cohort
            .members
            .iter()
            .any(|member| member.kind == caller.kind && member.agent_id == caller.agent_id)
    });
    if member.is_none()
        && let Some(caller) = caller
        && cohorts.iter().any(|cohort| {
            cohort
                .members
                .iter()
                .any(|member| member.kind == caller.kind && member.agent_id == caller.agent_id)
        })
    {
        bail!(
            "calling agent belongs to a different team cohort; selected {team_name}#{} in worktree {}",
            cohort.channel,
            worktree.display()
        );
    }
    let machine = rimz::config::MachineConfig::load()?;
    let effective = rimz::config::effective::load(&machine, &ctx.workspace.project_root)?;
    effective.block_untrusted_reference(
        rimz::config::effective::ProfileScope::Agents,
        Some(team_name),
        &machine.agents.commands,
    )?;
    let team = effective
        .teams
        .0
        .get(team_name)
        .with_context(|| format!("team `{team_name}` is no longer configured"))?;
    let by = match member {
        Some(agent) => {
            let role = agent
                .role
                .as_deref()
                .context("calling team member has no role")?;
            let pane = snapshot
                .agent_panes
                .iter()
                .find(|pane| {
                    pane.kind == agent.kind && pane.agent_id.as_ref() == Some(&agent.agent_id)
                })
                .map(|pane| pane.pane_id.clone());
            Flipper::Member { role, agent, pane }
        }
        None => Flipper::User,
    };
    let (by_name, flip_compact) = match &by {
        Flipper::Member { role, .. } => {
            (*role, team.flip_compact(role, machine.harness.flip_compact))
        }
        Flipper::User => ("user", None),
    };
    let members = cohort
        .members
        .iter()
        .map(|agent| (*agent).clone())
        .collect::<Vec<_>>();
    let receipt = team_stage::flip(FlipRequest {
        workspace: &ctx.workspace,
        store: &ctx.store,
        team_name,
        team,
        channel: &cohort.channel,
        worktree: &worktree,
        members: &members,
        to: &args.stage,
        note: &args.note,
        flip_compact,
        by,
        mux: globals.mux,
        now: Timestamp::now(),
    })?;
    let mut out = render::out();
    match &receipt.from {
        Some(from) => write!(out, "Flipped {from} -> {}", receipt.to)?,
        None => write!(out, "Opened {}", receipt.to)?,
    }
    writeln!(
        out,
        " by @{by_name}  ({team_name}#{} · {})",
        cohort.channel,
        worktree
            .file_name()
            .unwrap_or(worktree.as_os_str())
            .to_string_lossy()
    )?;
    if !team.stages.is_empty() {
        writeln!(
            out,
            "  {}",
            super::stage_strip(&team.stages, Some(&receipt.to))
        )?;
    }
    writeln!(out, "  note     {}", args.note.replace(['\r', '\n'], " "))?;
    match receipt.delivery {
        Delivery::Sent { .. } | Delivery::Queued { .. } => {
            // The domain only delivers a stage after resolving its owner.
            let owner = receipt.owner.as_deref().expect("delivered stage owner");
            let timing = if matches!(receipt.delivery, Delivery::Sent { .. }) {
                "sent now"
            } else {
                "woken at its next turn boundary"
            };
            writeln!(out, "  owner    @{owner}, {timing}")?;
        }
        Delivery::OwnerNotLive { owner } => {
            writeln!(out, "  owner    @{owner}, not live; woken on resume")?
        }
        Delivery::SelfOwned => writeln!(out, "  owner    you, carry on")?,
        Delivery::Terminal => {}
    }
    match receipt.compaction {
        Compaction::Queued {
            occupied_tokens,
            threshold,
            ..
        } => writeln!(
            out,
            "  compact  queued for you: {}k tokens, over {}k",
            occupied_tokens / 1000,
            threshold / 1000
        )?,
        Compaction::Sent { .. } => writeln!(out, "  compact  sent")?,
        Compaction::Skipped { reason } => writeln!(out, "  compact  skipped: {reason}")?,
        Compaction::BelowThreshold | Compaction::NotConfigured | Compaction::NotHandedOff => {}
    }
    Ok(())
}
