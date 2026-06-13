//! `rimz steer` — send state-gated text to live agent panes.

use anyhow::{Context, Result, bail};
use clap::Args;

use super::{GlobalFlags, current_channel, open_ledger};
use rimz::feed::{AgentState, pending_ask_for};
use rimz::schema::event::{AgentSteeredPayload, EventEnvelope};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct SteerArgs {
    /// Agent mention: `@codex-2`, `@swift-otter`, `@codex` (every codex), `@all`,
    /// optionally `#worktree`; or a pane id (`tmux:%1`).
    target: String,
    /// Restrict matches to one worktree branch, name, or path (the channel).
    #[arg(long)]
    worktree: Option<String>,
    /// Type the text without pressing Enter.
    #[arg(long)]
    no_enter: bool,
    /// Send even when a pending ask is attached to the agent.
    #[arg(long)]
    force: bool,
    /// Broadcast to more than one agent without the confirmation prompt.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Text to type into the agent pane.
    #[arg(last = true)]
    text: Vec<String>,
}

/// What happened to one agent in a fan-out.
enum Outcome {
    Sent(String),
    SkippedPending { label: String, request_id: String },
    SkippedNoPane { label: String },
}

pub fn run(args: SteerArgs, globals: &GlobalFlags) -> Result<()> {
    if args.text.is_empty() {
        bail!("expected non-empty text");
    }
    rimz::target::require_mention(&args.target)?;
    let text = args.text.join(" ");
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let channel = current_channel(&workspace);
    let agents = super::resolve_agent_many(
        &snapshot,
        &args.target,
        args.worktree.as_deref(),
        channel.as_deref(),
    )?;

    if agents.len() > 1 && !args.yes {
        super::confirm_fanout("Steer", &args.target, &agents)?;
    }

    let mut outcomes = Vec::with_capacity(agents.len());
    for agent in &agents {
        outcomes.push(steer_one(
            &workspace,
            &ledger,
            &snapshot,
            agent,
            &text,
            args.force,
            !args.no_enter,
        )?);
    }

    report(&args.target, agents.len(), &outcomes)
}

/// Type into one agent's pane, recording the steer between the paste and the
/// submit Enter. A pending ask or a missing pane skips the agent rather than
/// aborting a broadcast; only a mux failure returns an error.
fn steer_one(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    snapshot: &rimz::SidebarSnapshot,
    agent: &AgentState,
    text: &str,
    force: bool,
    enter: bool,
) -> Result<Outcome> {
    let label = super::agent_label(agent);
    if !force
        && let Some(ask) = pending_ask_for(
            agent,
            snapshot
                .needs_attention
                .iter()
                .chain(snapshot.resolver_working.iter()),
        )
    {
        return Ok(Outcome::SkippedPending {
            label,
            request_id: ask.request_id.to_string(),
        });
    }
    let Some(pane) = agent.pane.as_ref() else {
        return Ok(Outcome::SkippedNoPane { label });
    };
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    super::pane::paste_text(backend.as_ref(), &pane.pane_id, text)?;
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
            force,
            text.len(),
        ),
    );
    ledger.append_event(&event)?;
    if enter {
        super::pane::send_enter(backend.as_ref(), &pane.pane_id)?;
    }
    Ok(Outcome::Sent(label))
}

/// Report a fan-out. A lone agent that was skipped fails with the same message
/// the single-target path always returned; a broadcast always prints its
/// sent/skipped summary and succeeds — a blocked agent never aborts the rest.
fn report(target: &str, total: usize, outcomes: &[Outcome]) -> Result<()> {
    let labels = |pick: fn(&Outcome) -> Option<&str>| -> Vec<&str> {
        outcomes.iter().filter_map(pick).collect()
    };
    let sent = labels(|outcome| match outcome {
        Outcome::Sent(label) => Some(label.as_str()),
        _ => None,
    });
    if total == 1 {
        if !sent.is_empty() {
            return Ok(());
        }
        match outcomes.first() {
            Some(Outcome::SkippedPending { label, request_id }) => {
                bail!("{label} has pending ask {request_id}; resolve it or pass --force")
            }
            Some(Outcome::SkippedNoPane { label }) => {
                bail!("{label} has no bound pane; run `rimz pane list`")
            }
            _ => bail!("no agent matches `{target}`"),
        }
    }
    let pending = labels(|outcome| match outcome {
        Outcome::SkippedPending { label, .. } => Some(label.as_str()),
        _ => None,
    });
    let no_pane = labels(|outcome| match outcome {
        Outcome::SkippedNoPane { label } => Some(label.as_str()),
        _ => None,
    });
    let mut line = format!("steered {} agent(s)", sent.len());
    if !sent.is_empty() {
        line.push_str(&format!(": {}", sent.join(", ")));
    }
    let mut skips = Vec::new();
    if !pending.is_empty() {
        skips.push(format!("pending ask: {}", pending.join(", ")));
    }
    if !no_pane.is_empty() {
        skips.push(format!("no pane: {}", no_pane.join(", ")));
    }
    if !skips.is_empty() {
        line.push_str(&format!("; skipped {}", skips.join("; ")));
    }
    #[expect(clippy::print_stdout, reason = "steer fan-out summary")]
    {
        println!("{line}");
    }
    Ok(())
}
