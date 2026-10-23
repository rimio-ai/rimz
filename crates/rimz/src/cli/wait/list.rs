use anyhow::Result;
use std::io::Write;

use rimz::agents::AgentState;
use rimz::harness::schedule::arming::{self, ArmState};
use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::harness::schedule::pending::session_deliveries;
use rimz::harness::schedule::runner::parse_task_timeout;
use rimz::harness::schedule::signal::watcher_info;
use rimz::harness::schedule::{TaskKind, Trigger};

use super::*;
use crate::cli::{render as ui, wake_view};

pub(super) use crate::cli::wake_view::WakeRow;

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = caller_agent(&snapshot, caller.as_ref())?;
    let rows = if agent.is_some() {
        pending_rows(&ctx, agent, &snapshot.agents)?
    } else {
        room_rows(&ctx, &snapshot.agents)?
    };
    if json {
        return ui::json(&rows);
    }
    write_rows(&mut ui::out(), &ctx, agent, &snapshot.agents, rows)
}

pub(super) fn pending_rows(
    ctx: &Ctx,
    agent: Option<&AgentState>,
    agents: &[AgentState],
) -> Result<Vec<WakeRow>> {
    let session = agent.map(|agent| (agent.kind.clone(), agent.agent_id.clone()));
    let root = &ctx.workspace.project_root;
    let catalog = TaskCatalog::load(Some(root))?;
    let arming = arming::load();
    let now = jiff::Timestamp::now();
    session_deliveries(&catalog, root, session.as_ref())
        .map(|(name, task)| {
            let entry = task.entry();
            let parsed = task.trigger().as_ref().map_err(Clone::clone)?;
            let duration = match &parsed.trigger {
                Trigger::Watch(_) => entry.timeout.as_deref(),
                Trigger::Schedule(_) => entry
                    .wait_meta
                    .as_ref()
                    .and_then(|meta| meta.delay.as_deref()),
                Trigger::Signal { .. } | Trigger::Condition { .. } => None,
            };
            if let Some(duration) = duration {
                parse_task_timeout(duration).map_err(anyhow::Error::msg)?;
            }
            let state = ArmState::resolve(arming.get(&task.key(name)), task.source(), now);
            let pid = if entry.watch.is_some() {
                watcher_info(ctx.runtime(), name)?.map(|info| info.pid)
            } else {
                None
            };
            Ok(WakeRow::new(name, entry, state, pid, agents, now))
        })
        .collect()
}

fn room_rows(ctx: &Ctx, agents: &[AgentState]) -> Result<Vec<WakeRow>> {
    let root = &ctx.workspace.project_root;
    let catalog = TaskCatalog::load_room(root)?;
    let arming = arming::load();
    let now = jiff::Timestamp::now();
    Ok(catalog
        .visible()
        .iter()
        .filter(|(_, task)| {
            task.kind() == TaskKind::WakeUp && task.entry().resolved_root() == *root
        })
        .map(|(name, task)| {
            let entry = task.entry();
            let state = ArmState::resolve(arming.get(&task.key(name)), task.source(), now);
            let pid = entry
                .watch
                .as_ref()
                .and_then(|_| watcher_info(ctx.runtime(), name).ok().flatten())
                .map(|info| info.pid);
            WakeRow::new(name, entry, state, pid, agents, now)
        })
        .collect())
}

pub(super) fn write_rows(
    out: &mut impl Write,
    ctx: &Ctx,
    agent: Option<&AgentState>,
    agents: &[AgentState],
    rows: Vec<WakeRow>,
) -> Result<()> {
    let now = jiff::Timestamp::now();
    if let Some(agent) = agent {
        return wake_view::write_self(out, agent, agents, &rows, &ctx.workspace.worktree_root, now);
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
    let agents = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .map(|projection| projection.agents)
        .unwrap_or_default();
    writeln!(out)?;
    wake_view::write_room(out, &rows, &agents, None, &ctx.workspace.worktree_root, now)
}
