//! Resolve a live or held team cohort and present the durable stage-flip receipt.

use std::io::Write;

use anyhow::{Result, bail};
use jiff::Timestamp;

use rimz::harness::resume::TeamHoldReason;
use rimz::harness::run::TeamReportOutcome;
use rimz::harness::team_stage::{self, CohortFlip, CohortFlipRequest, Compaction, Delivery};

use super::super::{Ctx, GlobalFlags, render};
use super::FlipArgs;

pub(super) fn run(args: FlipArgs, globals: &GlobalFlags) -> Result<()> {
    let (reference, args) = args.normalize()?;
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let (target, members) = super::board_context::resolve(
        &ctx,
        &snapshot.agents,
        reference.as_deref(),
        args.team.as_deref(),
    )?;
    let held_by_board = matches!(target.reason, TeamHoldReason::BoardStage(_));
    let worktree = target.checkout;
    let cohort = rimz::address::TeamCohort {
        instance: target.instance,
        members,
    };
    let cohorts = rimz::address::team_cohorts(snapshot.agents.iter());
    let caller = super::board_context::caller(&snapshot.agents)?;
    let member = caller.filter(|caller| cohort.contains(caller));
    if member.is_none()
        && let Some(caller) = caller
        && cohorts.iter().any(|cohort| cohort.contains(caller))
    {
        bail!(
            "calling agent belongs to a different team cohort; selected {} in worktree {}",
            cohort.instance,
            worktree.display()
        );
    }
    let CohortFlip {
        receipt,
        pipeline,
        report,
    } = team_stage::flip_cohort(CohortFlipRequest {
        workspace: &ctx.workspace,
        store: &ctx.store,
        snapshot: &snapshot,
        cohort: &cohort,
        worktree: &worktree,
        member,
        to: &args.stage,
        note: &args.note,
        mux: globals.mux,
        now: Timestamp::now(),
    })?;
    let by_label = match member {
        Some(agent) => rimz::address::handle_with_name(
            &format!("@{}", agent.role.as_deref().unwrap_or_default()),
            agent.name.as_deref(),
        ),
        None => "@user".to_owned(),
    };
    let members = &cohort.members;
    let mut out = render::out();
    match &receipt.from {
        Some(from) => write!(out, "Flipped {from} -> {}", receipt.to)?,
        None => write!(out, "Opened {}", receipt.to)?,
    }
    writeln!(
        out,
        " by {by_label}  ({} · {}{})",
        cohort.instance,
        worktree
            .file_name()
            .unwrap_or(worktree.as_os_str())
            .to_string_lossy(),
        if members.is_empty() {
            " · no live members"
        } else {
            ""
        }
    )?;
    if held_by_board && receipt.to == rimz::config::DONE_STAGE {
        writeln!(out, "  checkout released")?;
    }
    if !pipeline.is_empty() {
        writeln!(
            out,
            "  {}",
            team_stage::stage_strip(&pipeline, Some(&receipt.to))
        )?;
    }
    match receipt.delivery {
        Delivery::Sent { .. } | Delivery::Queued { .. } => {
            // The domain only delivers a stage after resolving its owner.
            let owner = receipt.owner.as_deref().expect("delivered stage owner");
            let timing = if matches!(receipt.delivery, Delivery::Sent { .. }) {
                "sent now"
            } else {
                "woken at its next turn boundary"
            };
            let owner_name = rimz::address::role_holders(members.iter().copied(), owner)
                .first()
                .and_then(|holder| holder.name.as_deref());
            writeln!(
                out,
                "  owner    {}, {timing}",
                rimz::address::handle_with_name(&format!("@{owner}"), owner_name)
            )?;
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
        Compaction::BelowThreshold | Compaction::NotConfigured | Compaction::Ineligible => {}
    }
    // The board already flipped; a report that cannot be queued warns rather than
    // failing a flip that has landed.
    let instance = &cohort.instance;
    match report {
        Ok(TeamReportOutcome::Queued {
            launcher,
            delivered,
        }) => {
            let timing = if delivered {
                "sent now"
            } else {
                "at its next turn boundary"
            };
            writeln!(out, "  report   TEAM_REPORT to @{launcher}, {timing}")?;
        }
        Ok(TeamReportOutcome::Reopened) => {
            writeln!(out, "  report   TEAM_REPORT again at the next Done")?;
        }
        Ok(TeamReportOutcome::Detached) => {
            writeln!(
                out,
                "  report   none: the team was launched detached; rimz teams wait {instance} blocks on Done"
            )?;
        }
        Ok(TeamReportOutcome::NotOwed) => {}
        Err(error) => {
            tracing::warn!(%error, %instance, "team report failed");
            writeln!(out, "  report   TEAM_REPORT failed: {error}")?;
        }
    }
    Ok(())
}
