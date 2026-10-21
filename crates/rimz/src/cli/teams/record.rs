//! Append one board entry and print a one-line receipt naming its section and author.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rimz::harness::board::{self, RecordRequest};

use super::super::{Ctx, GlobalFlags};
use super::{RecordArgs, board_context};

pub(super) fn run(args: RecordArgs, globals: &GlobalFlags) -> Result<()> {
    let (reference, args) = args.normalize()?;
    let section: board::BoardSection = args.section.parse()?;
    let text = if let Some(text) = args.text {
        text
    } else if let Some(path) = args.file {
        std::fs::read_to_string(&path)
            .with_context(|| format!("read board entry from {}", path.display()))?
    } else {
        let mut text = String::new();
        std::io::stdin()
            .lock()
            .read_to_string(&mut text)
            .context("reading stdin")?;
        text
    };
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let selected = reference
        .as_deref()
        .map(|reference| board_context::resolve(&ctx, &snapshot.agents, Some(reference), None))
        .transpose()?;
    let worktree = match &selected {
        Some((target, _)) => target.checkout.clone(),
        None => board_context::worktree()?,
    };
    let caller = board_context::caller(&snapshot.agents)?;
    let cohorts = rimz::address::team_cohorts(snapshot.agents.iter());
    let member = caller.filter(|caller| cohorts.iter().any(|cohort| cohort.contains(caller)));
    let by = if let Some(member) = member {
        let local = cohorts.iter().any(|cohort| {
            board_context::in_worktree(cohort, &worktree)
                && cohort.contains(member)
                && selected.as_ref().is_none_or(|(target, _)| {
                    cohort.team == target.team && cohort.channel == target.channel
                })
        });
        if !local {
            bail!("calling agent belongs to a team cohort in another worktree");
        }
        member
            .role
            .as_deref()
            .context("calling team member has no role")?
    } else {
        "user"
    };
    board::record(RecordRequest {
        store: &ctx.store,
        worktree: &worktree,
        team: member
            .and_then(|member| member.team.as_deref())
            .or_else(|| selected.as_ref().map(|(target, _)| target.team.as_str())),
        section,
        by,
        text: &text,
        now: Timestamp::now(),
    })?;
    writeln!(std::io::stdout().lock(), "recorded {section} @{by}")?;
    Ok(())
}
