//! `rimz steer` — send state-gated text to a live agent pane.

use anyhow::{Context, Result, bail};
use clap::Args;

use super::{GlobalFlags, open_ledger};
use rimz::feed::pending_ask_for;
use rimz::schema::event::{AgentSteeredPayload, EventEnvelope};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct SteerArgs {
    /// Agent target: pane id (`tmux:%1`), agent kind (`claude`), or session id.
    target: String,
    /// Restrict kind/session matches to one worktree branch, name, or path.
    #[arg(long)]
    worktree: Option<String>,
    /// Type the text without pressing Enter.
    #[arg(long)]
    no_enter: bool,
    /// Send even when a pending ask is attached to the agent.
    #[arg(long)]
    force: bool,
    /// Text to type into the agent pane.
    #[arg(last = true)]
    text: Vec<String>,
}

pub fn run(args: SteerArgs, globals: &GlobalFlags) -> Result<()> {
    if args.text.is_empty() {
        bail!("expected non-empty text");
    }
    let text = args.text.join(" ");
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let agent = super::resolve_agent_card(&snapshot, &args.target, args.worktree.as_deref())?;
    if !args.force
        && let Some(ask) = pending_ask_for(
            agent,
            snapshot
                .needs_attention
                .iter()
                .chain(snapshot.resolver_working.iter()),
        )
    {
        bail!(
            "agent {}:{} has pending ask {}; resolve it or pass --force",
            agent.kind,
            agent.agent_id,
            ask.request_id
        );
    }
    let pane = agent.pane.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "agent {}:{} has no bound pane; run `rimz pane list`",
            agent.kind,
            agent.agent_id
        )
    })?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    let text_len = text.len();
    super::pane::send_text(backend.as_ref(), &pane.pane_id, &text)?;
    // Record the steer once the text lands and before the submit keystroke, so a
    // submitted steer is always preceded by its audit event. A failed Enter then
    // returns an error over text that is already accounted for, never untracked.
    let event = EventEnvelope::agent_steered(
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        AgentSteeredPayload::new(
            agent.kind.clone(),
            agent.agent_id.clone(),
            pane.pane_id.clone(),
            args.force,
            text_len,
        ),
    );
    ledger.append_event(&event)?;
    if !args.no_enter {
        super::pane::send_enter(backend.as_ref(), &pane.pane_id)?;
    }
    Ok(())
}
