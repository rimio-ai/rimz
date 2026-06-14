//! `rimz steer` — send state-gated text to live agent panes.

use anyhow::{Result, bail};
use clap::Args;

use super::{GlobalFlags, current_channel, open_ledger};
use rimz::feed::{AgentState, pending_ask_for};
use rimz::schema::event::{AgentSteeredPayload, EventEnvelope};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};
use rimz::{PaneAgent, SidebarSnapshot};

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
    /// Fan out to every agent the address matches. Without it, a selector that
    /// matches more than one agent is an error that lists the handles to pick one.
    #[arg(long)]
    all: bool,
    /// Launch the agent if the address matches none: a kind (`@codex`) or a role
    /// (`@planner`) opens a fresh agent in the channel with this text as its first
    /// prompt. An instance handle (pet name, ordinal) cannot create.
    #[arg(long)]
    create: bool,
    /// Skip the fan-out confirmation prompt when broadcasting (`@all` or --all).
    #[arg(long, short = 'y')]
    yes: bool,
    /// Text to type into the agent pane.
    #[arg(last = true)]
    text: Vec<String>,
}

/// What happened to one agent in a fan-out. Every resolved target carries a live
/// pane (it came from the producer's pane fold), so the only skip is a pending
/// ask reserving the next input.
enum Outcome {
    Sent(String),
    SkippedPending { label: String, request_id: String },
}

pub fn run(args: SteerArgs, globals: &GlobalFlags) -> Result<()> {
    if args.text.is_empty() {
        bail!("expected non-empty text");
    }
    rimz::target::require_mention(&args.target)?;
    let text = args.text.join(" ");
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
    let channel = current_channel(&workspace);
    let targets = match super::resolve_pane_targets(
        &snapshot,
        &args.target,
        args.worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(targets) => targets,
        // Create-on-miss: a kind/role address with --create launches a fresh
        // agent with this text as its first prompt, so no separate steer follows.
        Err(_) if args.create => {
            return super::agents_cmd::create_on_miss(
                &args.target,
                args.worktree.as_deref(),
                channel.as_deref(),
                &text,
                globals,
            );
        }
        Err(err) => return Err(err),
    };

    if targets.len() > 1 {
        let labels: Vec<String> = targets.iter().map(|target| target.label()).collect();
        if !args.all && !rimz::target::is_broadcast(&args.target) {
            return Err(super::ambiguous_fanout("steer", &args.target, &labels));
        }
        if !args.yes {
            super::confirm_fanout("Steer", &args.target, &labels)?;
        }
    }

    let mut outcomes = Vec::with_capacity(targets.len());
    for target in &targets {
        outcomes.push(steer_one(
            &workspace,
            &ledger,
            &snapshot,
            target,
            &text,
            args.force,
            !args.no_enter,
        )?);
    }

    report(&args.target, targets.len(), &outcomes)
}

/// Type into one agent's pane, recording the steer between the paste and the
/// submit Enter. A pending ask or a missing pane skips the agent rather than
/// aborting a broadcast; only a mux failure returns an error.
fn steer_one(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    text: &str,
    force: bool,
    enter: bool,
) -> Result<Outcome> {
    let label = target.label();
    // A pending ask reserves the next input — but only a bound session can hold
    // one; a lazy pane has no feed item, so it always sends.
    if !force
        && let Some(agent) = bound_agent(snapshot, target)
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
    let pane_id = &target.pane_id;
    let backend = rimz::mux::backend_for(pane_id.mux());
    super::pane::paste_text(backend.as_ref(), pane_id, text)?;
    // Record the steer once the text lands and before the submit keystroke, so a
    // submitted steer is always preceded by its audit event. A failed Enter then
    // returns an error over text that is already accounted for, never untracked.
    // A lazy pane has no session id yet, so the record names only kind and pane.
    let event = EventEnvelope::agent_steered(
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        AgentSteeredPayload::new(
            target.kind.clone(),
            target.agent_id.clone(),
            pane_id.clone(),
            force,
            text.len(),
        ),
    );
    ledger.append_event(&event)?;
    if enter {
        super::pane::send_enter(backend.as_ref(), pane_id)?;
    }
    Ok(Outcome::Sent(label))
}

/// The rollup session behind a bound pane target, for the pending-ask gate.
/// A lazy pane carries no session, so it never gates.
fn bound_agent<'a>(snapshot: &'a SidebarSnapshot, target: &PaneAgent) -> Option<&'a AgentState> {
    let agent_id = target.agent_id.as_ref()?;
    snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == target.kind && &agent.agent_id == agent_id)
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
            _ => bail!("no agent matches `{target}`"),
        }
    }
    let pending = labels(|outcome| match outcome {
        Outcome::SkippedPending { label, .. } => Some(label.as_str()),
        _ => None,
    });
    let mut line = format!("steered {} agent(s)", sent.len());
    if !sent.is_empty() {
        line.push_str(&format!(": {}", sent.join(", ")));
    }
    if !pending.is_empty() {
        line.push_str(&format!("; skipped pending ask: {}", pending.join(", ")));
    }
    #[expect(clippy::print_stdout, reason = "steer fan-out summary")]
    {
        println!("{line}");
    }
    Ok(())
}
