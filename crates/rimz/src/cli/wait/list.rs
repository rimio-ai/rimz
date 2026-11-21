use anyhow::Result;
use std::io::Write;

use rimz::agents::AgentState;

use super::*;
use crate::cli::{render as ui, wake_view};

pub(super) use crate::cli::wake_view::WakeRow;

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = wait::caller_agent(&snapshot, caller.as_ref(), None)?;
    let rows = wait::list(&room(&ctx, &snapshot), agent)?;
    if json {
        return ui::json(&rows);
    }
    write_rows(&mut ui::out(), &ctx, agent, &snapshot, rows)
}

pub(super) fn write_rows(
    out: &mut impl Write,
    ctx: &Ctx,
    agent: Option<&AgentState>,
    snapshot: &SidebarSnapshot,
    rows: Vec<WakeRow>,
) -> Result<()> {
    let now = jiff::Timestamp::now();
    if let Some(agent) = agent {
        return wake_view::write_self(
            out,
            agent,
            snapshot,
            &rows,
            &ctx.workspace.worktree_root,
            now,
        );
    }
    writeln!(
        out,
        "{} · whole room · {}",
        ui::home_relative(&ctx.workspace.project_root.to_string_lossy()),
        wake_view::counts(rows.iter().map(WakeRow::owner))
    )?;
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    wake_view::write_room(
        out,
        &rows,
        Some(snapshot),
        None,
        &ctx.workspace.worktree_root,
        now,
    )
}
