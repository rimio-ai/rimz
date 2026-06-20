//! `rimz steer` — send state-gated text to live agent panes.

use anyhow::{Result, bail};
use clap::Args;

use super::send::{self, SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_ledger};
use rimz::feed::{AgentState, pending_ask_for};
use rimz::message::{AutoCompact, MessageSender};
use rimz::mux::MuxBackend;
use rimz::schema::event::{AgentSteeredPayload, EventEnvelope};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};
use rimz::{PaneAgent, SidebarSnapshot};

#[derive(Debug, Args)]
pub struct SteerArgs {
    /// Agent mention: `@codex-2`, `@swift-otter`, `@codex` (every codex), `@all`,
    /// optionally `#worktree`; or a pane id (`tmux:%1`).
    target: String,
    #[command(flatten)]
    send: SendFlags,
    /// Text to type into the agent pane. `\n` is a soft newline; `\\` a literal
    /// backslash. Omit it and pass `--file` to send a file's contents verbatim.
    #[arg(last = true)]
    text: Vec<String>,
}

/// What happened to one agent in a fan-out. Every resolved target carries a live
/// pane (it came from the producer's pane fold), so the only skip is a pending
/// ask reserving the next input.
enum Outcome {
    Sent { label: String, compacted: bool },
    SkippedPending { label: String, request_id: String },
}

/// How a steer is delivered: send past a pending ask, submit with Enter, and an
/// optional compact-first threshold.
struct SteerSend {
    force: bool,
    enter: bool,
    auto_compact: Option<AutoCompact>,
    sender: MessageSender,
}

pub fn run(args: SteerArgs, globals: &GlobalFlags) -> Result<()> {
    rimz::target::require_mention(&args.target)?;
    let SendFlags {
        worktree,
        no_enter,
        force,
        all,
        create,
        yes,
        smart_compact,
        file,
        no_from,
    } = args.send;
    let auto_compact = smart_compact.or_else(|| super::machine_config().harness.smart_compact);
    let text = resolve_message(&args.text, file.as_deref())?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let mut snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
    // `--smart-compact` reads context fill, which the resolution snapshot does not
    // carry; fold the disposable context sidecars in for the freshest gauge.
    if auto_compact.is_some()
        && let Ok(runtime) = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
    {
        snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    }
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), no_from);
    let targets = match super::resolve_pane_targets(
        &snapshot,
        &args.target,
        worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(targets) => targets,
        // Create-on-miss: a kind/profile address with --create launches a fresh
        // agent with this text as its first prompt, so no separate steer follows.
        Err(_) if create => {
            return super::agents_cmd::create_on_miss(
                &args.target,
                worktree.as_deref(),
                channel.as_deref(),
                &text,
                globals,
            );
        }
        Err(err) => return Err(err),
    };

    if targets.len() > 1 {
        let labels: Vec<String> = targets.iter().map(|target| target.label()).collect();
        if !all && !rimz::target::is_broadcast(&args.target) {
            return Err(super::ambiguous_fanout("steer", &args.target, &labels));
        }
        if !yes {
            super::confirm_fanout("Steer", &args.target, &labels)?;
        }
    }

    let send = SteerSend {
        force,
        enter: !no_enter,
        auto_compact,
        sender,
    };
    let peers: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .collect();
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in &targets {
        outcomes.push(steer_one(
            &workspace, &ledger, &snapshot, target, &text, &send, &peers,
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
    send: &SteerSend,
    peers: &[&AgentState],
) -> Result<Outcome> {
    let label = target.label();
    // A pending ask reserves the next input — but only a bound session can hold
    // one; a lazy pane has no feed item, so it always sends.
    if !send.force
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
    let compacted = compact_if_full(backend.as_ref(), snapshot, target, send.auto_compact)?;
    let payload =
        match rimz::target::sender_prefix(&send.sender, peers, target.channel().as_deref()) {
            Some(prefix) => format!("{prefix}{text}"),
            None => text.to_owned(),
        };
    super::pane::paste_text(backend.as_ref(), pane_id, &payload)?;
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
            send.force,
            send.sender.attributed(),
            text.len(),
        ),
    );
    ledger.append_event(&event)?;
    if send.enter {
        super::pane::send_enter(backend.as_ref(), pane_id)?;
    }
    Ok(Outcome::Sent { label, compacted })
}

/// Submit the agent's `/compact` ahead of the steer when `--smart-compact` is set
/// and a bound agent's context has reached the threshold. A lazy pane carries no
/// context, and an agent kind with no compaction command can't compact, so both
/// pass through untouched. Returns whether a compaction was sent.
fn compact_if_full(
    backend: &dyn MuxBackend,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    auto_compact: Option<AutoCompact>,
) -> Result<bool> {
    let Some(threshold) = auto_compact else {
        return Ok(false);
    };
    let Some(agent) = bound_agent(snapshot, target) else {
        return Ok(false);
    };
    if !threshold.triggered(agent) {
        return Ok(false);
    }
    let Some(command) = rimz::agents::find_adapter(target.kind.as_str())
        .and_then(|adapter| adapter.compact_command())
    else {
        return Ok(false);
    };
    super::pane::send_command(backend, &target.pane_id, command)?;
    Ok(true)
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
        Outcome::Sent { label, .. } => Some(label.as_str()),
        _ => None,
    });
    let compacted = labels(|outcome| match outcome {
        Outcome::Sent {
            label,
            compacted: true,
        } => Some(label.as_str()),
        _ => None,
    });
    if total == 1 {
        if !sent.is_empty() {
            // A single steer stays quiet on success, but a compaction it ran on
            // the user's behalf is reported so the extra turn is never silent.
            if let Some(label) = compacted.first() {
                #[expect(clippy::print_stdout, reason = "steer compaction notice")]
                {
                    println!("compacted {label}, then steered");
                }
            }
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
    if !compacted.is_empty() {
        line.push_str(&format!("; compacted first: {}", compacted.join(", ")));
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
