//! Append one board entry and print its exact receipt.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rimz::harness::board::{self, RecordRequest};

use super::super::{Ctx, GlobalFlags};
use super::{RecordArgs, board_context};

pub(super) fn run(args: RecordArgs, globals: &GlobalFlags) -> Result<()> {
    let section = args.section.parse()?;
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
    let worktree = board_context::worktree()?;
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let caller = board_context::caller(&snapshot.agents)?;
    let cohorts = rimz::address::team_cohorts(&snapshot.agents);
    let member = caller.filter(|caller| {
        cohorts.iter().any(|cohort| {
            cohort
                .members
                .iter()
                .any(|member| member.kind == caller.kind && member.agent_id == caller.agent_id)
        })
    });
    let by = if let Some(member) = member {
        let local = cohorts.iter().any(|cohort| {
            board_context::in_worktree(cohort, &worktree)
                && cohort.members.iter().any(|candidate| {
                    candidate.kind == member.kind && candidate.agent_id == member.agent_id
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
    let receipt = board::record(RecordRequest {
        store: &ctx.store,
        worktree: &worktree,
        section,
        by,
        text: &text,
        now: Timestamp::now(),
    })?;
    writeln!(std::io::stdout().lock(), "{}", receipt.entry)?;
    Ok(())
}
