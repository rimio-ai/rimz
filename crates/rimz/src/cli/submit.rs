//! `rimz submit`: hand in the answer an `agent` call's `schema` asked the calling agent for.

use std::io::Write;

use anyhow::{Result, anyhow};
use clap::Args;
use rimz::harness::call::SubmitErr;

use super::{Ctx, GlobalFlags, render};

#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// The answer: one JSON value that fits the schema your launch was given.
    #[arg(allow_hyphen_values = true)]
    answer: String,
}

/// Store the calling agent's answer on its own launch's call record. The caller is the agent's
/// own row, so a child of that agent cannot answer for it.
pub fn run(args: SubmitArgs, globals: &GlobalFlags) -> Result<()> {
    let outside = || {
        anyhow!(
            "caller is not an agent RimZ launched; rimz submit hands in an answer from the \
             agent's own pane"
        )
    };
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.store.snapshot()?;
    let agent =
        rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents).map_err(|_| outside())?;
    let launch_id = agent.launch_id.as_ref().ok_or_else(outside)?;
    match rimz::harness::call::submit(
        ctx.store.paths(),
        launch_id,
        &args.answer,
        jiff::Timestamp::now(),
    ) {
        Ok(_) => {}
        Err(error @ SubmitErr::NotJson(_)) => {
            return Err(super::usage::UsageError::new(error.to_string()).into());
        }
        Err(error) => return Err(error.into()),
    }
    writeln!(
        render::out(),
        "answer accepted; it is this task's result once your turn ends"
    )?;
    Ok(())
}
