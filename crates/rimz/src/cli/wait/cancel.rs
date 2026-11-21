use std::io::Write;

use anyhow::Result;

use rimz::harness::schedule::wait::CancelTarget;

use super::*;

pub(super) fn run(
    name: Option<TaskName>,
    all: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = wait::canceling_agent(&snapshot, caller.as_ref())?;
    // clap requires exactly one of a name and --all.
    let target = match (all, name) {
        (false, Some(name)) => CancelTarget::Name(name),
        _ => CancelTarget::AllWaits,
    };
    let canceled = wait::cancel(&room(&ctx, &snapshot), agent, target)?;
    if json {
        return super::super::render::json(&canceled);
    }
    let mut out = super::super::render::out();
    if !canceled.canceled.is_empty() {
        writeln!(out, "canceled {}", canceled.canceled.join(", "))?;
    }
    list::write_rows(&mut out, &ctx, Some(agent), &snapshot, canceled.pending)
}
