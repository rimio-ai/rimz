//! `rimz queue` — durable per-agent text delivery.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::send::{SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::feed::{AgentState, pending_ask_for};
use rimz::ids::{MessageId, PaneId};
use rimz::message::{
    AutoCompact, DeliveryGate, MessageRecord, MessageStatus, gate_open, queue_head,
    settle_duration_from_env,
};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct QueueArgs {
    #[command(subcommand)]
    command: Option<QueueSubcmd>,
    /// Agent target for the bare add form.
    target: Option<String>,
    /// Deliver after a successful/idle turn (`done`) or after success/idle/failure (`any`).
    #[arg(long, value_parser = parse_gate, default_value = "done")]
    on: DeliveryGate,
    #[command(flatten)]
    send: SendFlags,
    /// Text to deliver. `\n` is a soft newline; `\\` a literal backslash. Omit it
    /// and pass `--file` to deliver a file's contents verbatim.
    #[arg(last = true)]
    text: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum QueueSubcmd {
    /// Queue text for an agent.
    Add(AddArgs),
    /// List queued message records.
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Optional target filter.
        target: Option<String>,
    },
    /// Remove one pending message.
    Remove { message_id: MessageId },
    /// Remove every pending message for an agent.
    Clear {
        target: String,
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Deliver one queued message. Spawned by lifecycle hooks.
    #[command(hide = true)]
    Deliver {
        #[arg(long)]
        message_id: MessageId,
    },
}

#[derive(Debug, Args)]
struct AddArgs {
    target: String,
    /// Deliver after a successful/idle turn (`done`) or after success/idle/failure (`any`).
    #[arg(long, value_parser = parse_gate, default_value = "done")]
    on: DeliveryGate,
    #[command(flatten)]
    send: SendFlags,
    /// Text to deliver. `\n` is a soft newline; `\\` a literal backslash. Omit it
    /// and pass `--file` to deliver a file's contents verbatim.
    #[arg(last = true)]
    text: Vec<String>,
}

pub fn run(args: QueueArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(QueueSubcmd::Add(add)) => queue_add(add.target, add.on, add.send, add.text, globals),
        Some(QueueSubcmd::List { json, target }) => list_messages(json, target, globals),
        Some(QueueSubcmd::Remove { message_id }) => remove_message(message_id, globals),
        Some(QueueSubcmd::Clear { target, worktree }) => clear_messages(target, worktree, globals),
        Some(QueueSubcmd::Deliver { message_id }) => deliver_message(message_id, globals),
        None => {
            let target = args.target.ok_or_else(|| {
                anyhow::anyhow!("expected a target, or `rimz queue list|remove|clear`")
            })?;
            queue_add(target, args.on, args.send, args.text, globals)
        }
    }
}

/// Shared enqueue for the `queue add` and bare `queue` forms: resolve the prompt
/// from inline argv or `--file`, then split the mirrored `SendFlags` into the
/// delivery spec and the fan-out controls and hand off.
fn queue_add(
    target: String,
    gate: DeliveryGate,
    send: SendFlags,
    text: Vec<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let SendFlags {
        worktree,
        no_enter,
        force,
        all,
        create,
        yes,
        auto_compact,
        file,
    } = send;
    let text = resolve_message(&text, file.as_deref())?;
    add_message(
        target,
        worktree,
        text,
        MessageSpec {
            enter: !no_enter,
            gate,
            force,
            auto_compact,
        },
        FanoutFlags { all, create, yes },
        globals,
    )
}

/// The fan-out / create / confirm flags shared by both queue-add forms.
struct FanoutFlags {
    all: bool,
    create: bool,
    yes: bool,
}

/// How a queued message delivers: submit with Enter, the turn-boundary gate,
/// whether to deliver past a pending ask, and an optional compact-first threshold.
struct MessageSpec {
    enter: bool,
    gate: DeliveryGate,
    force: bool,
    auto_compact: Option<AutoCompact>,
}

fn add_message(
    target: String,
    worktree: Option<String>,
    text: String,
    spec: MessageSpec,
    flags: FanoutFlags,
    globals: &GlobalFlags,
) -> Result<()> {
    rimz::target::require_mention(&target)?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
    let channel = current_channel(&workspace);
    // queue records are durable and keyed on a session id, so they address bound
    // agents. A match that is only a live, sessionless pane has no key — point it
    // at steer, which reaches the pane directly.
    let agents = match super::resolve_agent_many(
        &snapshot,
        &target,
        worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(agents) => agents,
        Err(err) => {
            if rimz::target::unbound_pane_in_channel(
                &snapshot,
                &target,
                worktree.as_deref(),
                channel.as_deref(),
            ) {
                bail!(
                    "`{target}` matched an agent pane with no session yet; `rimz steer {target}` to start it, then queue"
                );
            }
            // Create-on-miss launches a fresh agent with this text as its first
            // prompt, so the launch carries the work and no queue entry is made.
            if flags.create {
                return super::agents_cmd::create_on_miss(
                    &target,
                    worktree.as_deref(),
                    channel.as_deref(),
                    &text,
                    globals,
                );
            }
            return Err(err);
        }
    };
    if agents.len() > 1 {
        let labels: Vec<String> = agents
            .iter()
            .map(|agent| super::agent_label(agent))
            .collect();
        if !flags.all && !rimz::target::is_broadcast(&target) {
            return Err(super::ambiguous_fanout("queue for", &target, &labels));
        }
        if !flags.yes {
            super::confirm_fanout("Queue for", &target, &labels)?;
        }
    }
    // Preflight hooks once per distinct kind, before queuing anything — the hard
    // hooks precondition is all-or-nothing across the fan-out.
    let mut kinds_seen = std::collections::BTreeSet::new();
    for &agent in &agents {
        if kinds_seen.insert(agent.kind.as_str().to_owned()) {
            preflight_queue_hooks(agent)?;
        }
    }
    let mut ids = Vec::with_capacity(agents.len());
    for &agent in &agents {
        let message = MessageRecord::new(
            workspace.workspace_id.clone(),
            agent,
            text.clone(),
            spec.enter,
            spec.gate,
        )
        .with_force(spec.force)
        .with_auto_compact(spec.auto_compact);
        let message_id = message.message_id.clone();
        ledger.queue_message(&message, &workspace.session_name)?;
        let _ = deliver_one(&workspace, &ledger, &message_id, Duration::ZERO);
        ids.push(message_id);
    }
    #[expect(clippy::print_stdout, reason = "command result is message id(s)")]
    {
        for id in &ids {
            println!("{id}");
        }
    }
    Ok(())
}

fn list_messages(json: bool, target: Option<String>, globals: &GlobalFlags) -> Result<()> {
    let (workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let mut messages = ledger.list_messages()?;
    if let Some(raw) = target {
        rimz::target::require_mention(&raw)?;
        let channel = current_channel(&workspace);
        let agent = super::resolve_agent_one(&snapshot, &raw, None, channel.as_deref())?;
        messages.retain(|message| message.same_agent_card(agent));
    }
    if json {
        let rendered = serde_json::to_string_pretty(&messages)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
    } else {
        // Address each message by the live agent's canonical handle; a message
        // whose agent has since left falls back to the durable `kind:id`.
        let agents: Vec<&AgentState> = snapshot
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .collect();
        let mut table =
            render::Table::new(["ID", "STATUS", "TARGET", "ATTEMPTS", "TEXT"]).right(&[3]);
        for message in messages {
            let target = agents
                .iter()
                .copied()
                .find(|agent| message.same_agent_card(agent))
                .map(|agent| rimz::target::agent_handle(agent, &agents, true))
                .unwrap_or_else(|| format!("{}:{}", message.kind, message.agent_id));
            table.row([
                render::cell(message.message_id.to_string()).fg(render::palette::ACCENT),
                render::cell(message.status.as_str()).fg(render::status::message(message.status)),
                render::cell(target).fg(render::palette::META),
                render::cell(message.attempts.to_string()),
                render::cell(preview(&message.text)),
            ]);
        }
        table.render(&mut render::out())?;
    }
    Ok(())
}

fn remove_message(message_id: MessageId, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    if !ledger.remove_message(&message_id, &workspace.session_name, "remove")? {
        bail!("message {message_id} is not pending or claimed");
    }
    Ok(())
}

fn clear_messages(target: String, worktree: Option<String>, globals: &GlobalFlags) -> Result<()> {
    rimz::target::require_mention(&target)?;
    let (workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let channel = current_channel(&workspace);
    let agent =
        super::resolve_agent_one(&snapshot, &target, worktree.as_deref(), channel.as_deref())?;
    let count = ledger.clear_messages_for(
        &agent.kind,
        &agent.agent_id,
        agent.name.as_deref(),
        &workspace.session_name,
    )?;
    #[expect(clippy::print_stdout, reason = "command result is removal count")]
    {
        println!("{count}");
    }
    Ok(())
}

fn deliver_message(message_id: MessageId, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    deliver_one(&workspace, &ledger, &message_id, settle_duration_from_env())?;
    Ok(())
}

fn deliver_one(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    message_id: &MessageId,
    settle: Duration,
) -> Result<bool> {
    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    let Some(candidate) = delivery_candidate(ledger, message_id)? else {
        return Ok(false);
    };
    let Some(message) = ledger.claim_message_for_delivery(message_id, jiff::Timestamp::now())?
    else {
        return Ok(false);
    };
    debug_assert!(message.same_agent(&candidate.message.kind, &candidate.message.agent_id));
    debug_assert_eq!(message.message_id, candidate.message.message_id);
    let backend = rimz::mux::backend_for(candidate.pane_id.mux());
    // A `--auto-compact` message types `/compact` ahead of the text, so the
    // prompt lands against a fresh window. A failed compaction fails the whole
    // delivery through the same retry path as a failed message send.
    let send = (|| {
        if let Some(command) = candidate.compact {
            super::pane::send_command(backend.as_ref(), &candidate.pane_id, command)?;
        }
        super::pane::submit_message(
            backend.as_ref(),
            &candidate.pane_id,
            &message.text,
            message.enter,
        )
    })();
    match send {
        Ok(()) => {
            ledger.settle_message(
                &message.message_id,
                MessageStatus::Delivered,
                &workspace.session_name,
                None,
            )?;
            Ok(true)
        }
        Err(err) => {
            ledger.record_message_delivery_failure(
                &message.message_id,
                &err.to_string(),
                &workspace.session_name,
            )?;
            Ok(false)
        }
    }
}

struct DeliveryCandidate {
    message: MessageRecord,
    pane_id: PaneId,
    /// The `/compact` to type ahead of the text, set when `--auto-compact`'s
    /// threshold is met at this delivery boundary. `None` leaves delivery as a
    /// plain message send.
    compact: Option<&'static str>,
}

fn delivery_candidate(
    ledger: &rimz::Ledger,
    message_id: &MessageId,
) -> Result<Option<DeliveryCandidate>> {
    let pending = ledger.list_pending_messages()?;
    let Some(message) = pending
        .iter()
        .find(|message| message.message_id == *message_id)
        .cloned()
    else {
        return Ok(None);
    };
    let Some(head) = queue_head(
        pending.iter(),
        &message.kind,
        &message.agent_id,
        message.agent_name.as_deref(),
    ) else {
        return Ok(None);
    };
    if head.message_id != *message_id {
        return Ok(None);
    }
    let mut snapshot = ledger
        .snapshot_cached()
        .context("reading delivery snapshot")?;
    // `--auto-compact` reads context fill, which the cached snapshot does not
    // carry; fold the disposable context sidecars in for the freshest gauge.
    if message.auto_compact.is_some()
        && let Ok(runtime) = rimz::RuntimePaths::for_workspace(message.workspace_id.clone())
    {
        snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    }
    let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| message.same_agent_card(agent))
    else {
        return Ok(None);
    };
    if !gate_open(message.gate, agent.status) {
        return Ok(None);
    }
    // A pending ask reserves the agent's next input, so it defers delivery —
    // unless the message was queued with `--force`, mirroring `steer --force`.
    if !message.force
        && pending_ask_for(
            agent,
            snapshot
                .needs_attention
                .iter()
                .chain(snapshot.resolver_working.iter()),
        )
        .is_some()
    {
        return Ok(None);
    }
    let Some(pane) = agent.pane.as_ref() else {
        return Ok(None);
    };
    let compact = compact_command_if_full(&message, agent);
    let pane_id = pane.pane_id.clone();
    Ok(Some(DeliveryCandidate {
        message,
        pane_id,
        compact,
    }))
}

/// The agent's `/compact` when a `--auto-compact` message's threshold is met by
/// the agent's current fill, else `None`. An agent kind with no compaction
/// command can't compact, so it passes through as a plain send.
fn compact_command_if_full(message: &MessageRecord, agent: &AgentState) -> Option<&'static str> {
    let threshold = message.auto_compact?;
    threshold
        .triggered(agent)
        .then(|| rimz::agents::find_adapter(message.kind.as_str())?.compact_command())
        .flatten()
}

fn workspace_ledger_snapshot(
    globals: &GlobalFlags,
) -> Result<(ResolvedWorkspace, rimz::Ledger, rimz::SidebarSnapshot)> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    Ok((workspace, ledger, snapshot))
}

fn preflight_queue_hooks(agent: &AgentState) -> Result<()> {
    let adapter = rimz::agents::find_adapter(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    if !adapter.hooks_installed() {
        bail!(
            "`rimz queue` requires {} hooks so queued messages can deliver at turn boundaries; run `rimz hooks install {}`",
            agent.kind,
            agent.kind
        );
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        bail!(
            "{} hooks are installed but not trusted ({}); {}",
            agent.kind,
            untrusted.join(", "),
            rimz::agents::hook_trust_fix(agent.kind.as_str())
        );
    }
    Ok(())
}

fn parse_gate(raw: &str) -> std::result::Result<DeliveryGate, String> {
    match raw {
        "done" => Ok(DeliveryGate::Done),
        "any" => Ok(DeliveryGate::Any),
        other => Err(format!(
            "unknown delivery gate `{other}`; expected done or any"
        )),
    }
}

fn preview(text: &str) -> String {
    const MAX: usize = 80;
    let preview = text.replace(['\r', '\n', '\t'], " ");
    let mut chars = preview.chars();
    let short: String = chars.by_ref().take(MAX).collect();
    if chars.next().is_some() {
        let mut shortened = preview.chars().take(MAX - 3).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        short
    }
}
