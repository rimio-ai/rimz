//! User-facing agent-card context refresh (`rimz agents refresh`).
//!
//! Forces one or more live agent cards through the local transcript refresh
//! path and any adapter-owned detached rich-context helper.

use std::io::Write;

use anyhow::{Context, Result};
use clap::Args;

use rimz::agents::AgentState;
use rimz::sidebar::refresh::force_refresh_session_context;

use crate::cli::{Ctx, GlobalFlags, UsageError, render, select};

#[derive(Debug, Args)]
pub(super) struct RefreshArgs {
    /// Agent to refresh (@handle, bare selector, or pane id); `-` reads one address per line from
    /// stdin; defaults to every live agent in the current channel.
    pub(super) reference: Option<String>,
    /// Refresh the agents matching `field=value` terms, as `agents list --filter` reads them;
    /// repeatable, `--filter help` lists fields.
    #[arg(
        long = "filter",
        value_name = "QUERY",
        add = clap_complete::ArgValueCompleter::new(crate::cli::complete::filter_terms)
    )]
    pub(super) filters: Vec<String>,
    /// Refresh every live agent in the workspace, ignoring channel scope; with `--filter`,
    /// every match in the room. Beside `-` it changes nothing.
    #[arg(long)]
    pub(super) all: bool,
}

pub(super) fn run_refresh(args: RefreshArgs, globals: &GlobalFlags) -> Result<()> {
    if select::filter_help(&args.filters)? {
        return Ok(());
    }
    if args.all
        && args
            .reference
            .as_deref()
            .is_some_and(|reference| reference != select::STDIN)
    {
        return Err(UsageError::new(
            "--all refreshes every live agent, so it takes no reference but `-`".to_owned(),
        )
        .into());
    }
    let ctx = Ctx::open(globals)?;
    let runtime = ctx.runtime();
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    let references = Vec::from_iter(args.reference.clone());
    if select::selects(&references, &args.filters) {
        // Refresh never asks for consent: it fans out bare and with `--all` today.
        let flags = select::TargetFlags {
            references: &references,
            filters: &args.filters,
            all: true,
            room: args.all,
        };
        return match select::select_targets(&ctx, "refresh", flags)? {
            select::Targets::Selected(selected) => refresh_selected(&ctx, selected),
            select::Targets::References(_) | select::Targets::Done => Ok(()),
        };
    }
    let snapshot = refresh_snapshot(&ctx)?;
    let current_channel = ctx.channel();
    let targets = match (args.reference.as_deref(), args.all) {
        (Some(reference), _) => vec![crate::cli::resolve_agent_one(
            &ctx.store,
            &snapshot,
            reference,
            None,
            &ctx.address_context(),
        )?],
        (None, true) => refresh_targets(&snapshot, None),
        (None, false) => {
            refresh_targets(&snapshot, current_channel.map(rimz::ids::LaneName::as_str))
        }
    };
    if targets.is_empty() {
        writeln!(render::err(), "no matching agents to refresh")?;
        return Ok(());
    }

    let peers = rimz::address::addressable_agents(&snapshot);
    let mut failed = 0;
    let mut out = render::out();
    for agent in targets {
        let label = rimz::address::agent_handle(agent, &peers, true);
        failed += usize::from(!refresh_one(&mut out, &snapshot, runtime, agent, &label)?);
    }
    select::batch_result(failed)
}

/// Refresh each selected agent in turn from a snapshot read just before it.
fn refresh_selected(ctx: &Ctx, mut selected: Vec<select::Selected>) -> Result<()> {
    select::identify(ctx, &refresh_snapshot(ctx)?, &mut selected);
    let mut failed = 0;
    let mut out = render::out();
    for target in &selected {
        let snapshot = refresh_snapshot(ctx)?;
        let refreshed = match select::reread(ctx, &snapshot, target) {
            Ok(agent) => refresh_one(&mut out, &snapshot, ctx.runtime(), agent, &target.address)?,
            Err(err) => {
                writeln!(out, "error {}: {err:#}", target.address)?;
                false
            }
        };
        failed += usize::from(!refreshed);
    }
    select::batch_result(failed)
}

fn refresh_snapshot(ctx: &Ctx) -> Result<rimz::store::snapshot::SidebarSnapshot> {
    let mut snapshot = ctx
        .resolution_snapshot()
        .context("reading agent snapshot")?;
    // A resolution snapshot lists the room's own session without recording its
    // name, and a turn-death pane read needs the pane's session.
    snapshot.pane_session_name = Some(ctx.workspace.session_name.clone());
    Ok(snapshot)
}

/// Refresh one agent and print its receipt line; `false` when it failed.
fn refresh_one(
    out: &mut impl Write,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    runtime: &rimz::RuntimePaths,
    agent: &AgentState,
    label: &str,
) -> Result<bool> {
    let kind = agent.kind.as_str();
    match force_refresh_session_context(snapshot, runtime, agent) {
        Ok(refresh) if refresh.transcript_refreshed => writeln!(out, "refreshed {label}")?,
        Ok(refresh) if refresh.helper_spawned => {
            writeln!(out, "refreshed {label} (rich-context helper spawned)")?;
        }
        Ok(_) => writeln!(
            out,
            "{label}: nothing to refresh ({kind} has no local context channel)"
        )?,
        Err(err) => {
            writeln!(out, "error {label}: {err:#}")?;
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn refresh_targets<'a>(
    snapshot: &'a rimz::store::snapshot::SidebarSnapshot,
    channel: Option<&str>,
) -> Vec<&'a AgentState> {
    let lane = channel.map(|filter| rimz::address::LaneSelector::parse(Some(filter)));
    rimz::address::addressable_agents(snapshot)
        .into_iter()
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| rimz::agents::find_definition(agent.kind.as_str()).is_some())
        .filter(|agent| {
            lane.as_ref()
                .is_none_or(|lane| rimz::address::agent_in_lane(agent, lane))
        })
        .collect()
}
