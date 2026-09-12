//! Resolve a live team cohort and present the durable stage-flip receipt.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use rimz::harness::ancestry::{resolve_caller, resolve_launch_caller};
use rimz::harness::team_stage::{self, Compaction, Delivery, FlipRequest, Flipper};
use rimz::utils::path::normalize_path_lexical;

use super::super::{Ctx, GlobalFlags, render};
use super::{FlipArgs, cohort};

pub(super) fn run(args: FlipArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let caller = resolve_caller(&snapshot.agents)
        .map(|caller| resolve_launch_caller(&snapshot.agents, &caller))
        .transpose()?;
    let team_name = match args
        .team
        .or_else(|| caller.and_then(|caller| caller.team.clone()))
    {
        Some(team) => team,
        None => {
            let teams = rimz::address::team_cohorts(&snapshot.agents)
                .into_iter()
                .filter(|cohort| Some(cohort.channel.as_str()) == ctx.channel())
                .map(|cohort| cohort.team)
                .collect::<BTreeSet<_>>();
            if teams.len() != 1 {
                bail!(
                    "select a team with --team <name>; live teams in this channel: {}",
                    teams.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
            teams
                .into_iter()
                .next()
                .context("no live team in this channel")?
                .to_owned()
        }
    };
    let (team_name, worktree) = super::team_lane(team_name, args.worktree)?;
    let cohort = cohort::select(
        &team_name,
        worktree.as_deref(),
        ctx.channel(),
        &snapshot.agents,
    )?;
    let machine = rimz::config::MachineConfig::load()?;
    let effective = rimz::config::effective::load(&machine, &ctx.workspace.project_root)?;
    effective.block_untrusted_reference(
        rimz::config::effective::ProfileScope::Agents,
        Some(&team_name),
        &machine.agents.commands,
    )?;
    let team = effective
        .teams
        .0
        .get(&team_name)
        .with_context(|| format!("team `{team_name}` is no longer configured"))?;
    let mut worktrees = cohort.members.iter().map(|agent| {
        agent
            .worktree_path
            .as_deref()
            .map(|path| normalize_path_lexical(Path::new(path)))
    });
    let worktree = worktrees
        .next()
        .flatten()
        .context("team cohort has no recorded worktree")?;
    if !worktrees.all(|path| path.as_ref() == Some(&worktree)) {
        bail!(
            "team `{team_name}#{}` members disagree on their worktree",
            cohort.channel
        );
    }
    let by = match caller.filter(|caller| {
        cohort
            .members
            .iter()
            .any(|member| member.kind == caller.kind && member.agent_id == caller.agent_id)
    }) {
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
                .context("calling team member has no live pane")?;
            Flipper::Member {
                role,
                agent,
                pane: pane.pane_id.clone(),
            }
        }
        None => Flipper::User,
    };
    let members = cohort
        .members
        .iter()
        .map(|agent| (*agent).clone())
        .collect::<Vec<_>>();
    let receipt = team_stage::flip(FlipRequest {
        workspace: &ctx.workspace,
        store: &ctx.store,
        team_name: &team_name,
        team,
        channel: &cohort.channel,
        worktree: &worktree,
        members: &members,
        to: &args.stage,
        note: args.note.as_deref(),
        steer: args.steer,
        by,
        mux: globals.mux,
        now: Timestamp::now(),
    })?;
    let mut out = render::out();
    write!(
        out,
        "flipped {} -> {}",
        receipt.from.as_deref().unwrap_or("(none)"),
        receipt.to
    )?;
    if let Some(owner) = &receipt.owner {
        write!(out, " (@{owner})")?;
    }
    writeln!(out, " in {team_name}#{}", cohort.channel)?;
    writeln!(out, "  board    {}", receipt.board.display())?;
    match receipt.delivery {
        Delivery::Sent { label } => writeln!(out, "  message  sent to {label}")?,
        Delivery::Queued { label } => writeln!(
            out,
            "  message  queued for {label}, delivers at its next turn boundary"
        )?,
        Delivery::Steered { label } => writeln!(out, "  message  steered {label}")?,
        Delivery::OwnerNotLive { owner } => writeln!(
            out,
            "  message  owner @{owner} is not live; it is woken on resume"
        )?,
        Delivery::SelfOwned => writeln!(out, "  message  stage is yours; carry on")?,
        Delivery::Terminal => {}
    }
    match receipt.compaction {
        Compaction::Queued { .. } => {
            writeln!(out, "  compact  queued for your next turn boundary")?
        }
        Compaction::Sent { .. } => writeln!(out, "  compact  sent")?,
        Compaction::Skipped { reason } => writeln!(out, "  compact  skipped: {reason}")?,
        Compaction::NotConfigured | Compaction::NotHandedOff => {}
    }
    Ok(())
}
