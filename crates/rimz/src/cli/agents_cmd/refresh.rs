//! User-facing agent-card context refresh (`rimz agents refresh`).
//!
//! Forces one or more live agent cards through the local transcript refresh
//! path and any adapter-owned detached rich-context helper.

use std::io::Write;

use anyhow::{Context, Result};
use clap::Args;

use rimz::agents::AgentState;
use rimz::sidebar::refresh::force_refresh_session_context;

use crate::cli::{Ctx, GlobalFlags, render};

#[derive(Debug, Args)]
pub(super) struct RefreshArgs {
    /// Agent to refresh (@handle, bare selector, or pane id); defaults to every live agent in the current channel.
    pub(super) reference: Option<String>,
    /// Refresh every live agent in the workspace, ignoring channel scope.
    #[arg(long, conflicts_with = "reference")]
    pub(super) all: bool,
}

pub(super) fn run_refresh(args: RefreshArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx
        .resolution_snapshot()
        .context("reading agent snapshot")?;
    let runtime = ctx.runtime();
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    let current_channel = ctx.channel();
    let targets = match (args.reference.as_deref(), args.all) {
        (Some(reference), _) => vec![crate::cli::resolve_agent_one(
            &snapshot,
            reference,
            None,
            current_channel,
        )?],
        (None, true) => refresh_targets(&snapshot, None),
        (None, false) => refresh_targets(&snapshot, current_channel),
    };
    if targets.is_empty() {
        writeln!(render::err(), "no matching agents to refresh")?;
        return Ok(());
    }

    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let mut failed = false;
    let mut out = render::out();
    for agent in targets {
        let label = rimz::harness::target::agent_handle(agent, &peers, true);
        let kind = agent.kind.as_str();
        let model_hint = agent.model.as_deref().or_else(|| {
            agent
                .context
                .as_ref()
                .and_then(|context| context.model_id.as_deref())
        });
        match force_refresh_session_context(
            &snapshot,
            runtime,
            kind,
            agent.agent_id.as_str(),
            model_hint,
        ) {
            Ok(refresh) if refresh.transcript_refreshed => {
                writeln!(out, "refreshed {label}")?;
            }
            Ok(refresh) if refresh.helper_spawned => {
                writeln!(out, "refreshed {label} (rich-context helper spawned)")?;
            }
            Ok(_) => {
                writeln!(
                    out,
                    "{label}: nothing to refresh ({kind} has no local context channel)"
                )?;
            }
            Err(err) => {
                failed = true;
                writeln!(out, "error {label}: {err:#}")?;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

pub(super) fn refresh_targets<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    channel: Option<&str>,
) -> Vec<&'a AgentState> {
    rimz::harness::target::addressable_agents(snapshot)
        .into_iter()
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| rimz::agents::find_definition(agent.kind.as_str()).is_some())
        .filter(|agent| {
            channel.is_none_or(|filter| rimz::harness::target::agent_in_worktree(agent, filter))
        })
        .collect()
}
