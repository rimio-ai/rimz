//! `rimz agents auto-continue` — the hidden helper the sidebar producer spawns to
//! resume a parked agent when its class-specific condition is due.
//!
//! The producer decides *which* agent and *when* (`sidebar::enrich`
//! auto-continue, opt-in via `[resume] auto_continue*`); this helper performs the
//! two side effects the sidebar's read-only import graph must not: it types the
//! nudge into the agent's live pane through the shared pane-send primitive and
//! writes the `agent.resumed` audit record. Best-effort by contract — it inherits
//! the producer's frame-validated target, so a vanished pane just fails the send
//! and no audit record is written.

use anyhow::{Context, Result};
use clap::Args;

use rimz::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use rimz::ledger::{wakeup, workspace_record};
use rimz::schema::event::EventEnvelope;
use rimz::{Ledger, RuntimePaths, StatePaths};

#[derive(Debug, Args)]
pub struct AutoContinueArgs {
    #[arg(long)]
    workspace_id: String,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    agent_id: String,
    /// Normalized pane id (`tmux:%3`, `zellij:terminal_3`).
    #[arg(long)]
    pane: String,
    #[arg(long)]
    text: String,
    #[arg(long)]
    reason: String,
}

pub fn run_auto_continue(args: AutoContinueArgs) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let pane_id = PaneId::parse(&args.pane).context("parsing pane id")?;
    let kind = AgentKind::new_unchecked(args.kind);
    let agent_id = AgentSessionId::from(args.agent_id.as_str());
    let text = args.text.trim();
    if text.is_empty() {
        return Ok(());
    }

    let paths =
        StatePaths::for_workspace(workspace_id.clone()).context("preparing ledger paths")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    let session_name = workspace_record::read(&paths.workspace_record)
        .map(|record| record.session_name)
        .unwrap_or_default();
    let ledger = Ledger::open(paths, runtime).context("opening ledger")?;

    // Type the nudge into the live pane, then submit — the same bracketed-paste
    // path `steer` uses, so the agent composer takes the text and the discrete
    // Enter submits it rather than folding a newline into the composer.
    let backend = rimz::mux::backend_for(pane_id.mux());
    crate::cli::pane::submit_message(backend.as_ref(), &pane_id, text, true)
        .context("sending auto-continue nudge")?;

    // Audit only after a successful send, so a vanished pane leaves no false
    // `resumed` record. The nudge text never enters the log, mirroring `steer`.
    let event = EventEnvelope::agent_resumed(
        workspace_id,
        session_name,
        &kind,
        &agent_id,
        &pane_id,
        &args.reason,
    );
    ledger
        .append_event(&event)
        .context("recording agent.resumed")?;
    let _ = wakeup::wake_sidebars(ledger.runtime_paths());
    Ok(())
}
