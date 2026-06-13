//! `rimz queue` — durable per-agent text delivery.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::feed::{AgentState, pending_ask_for};
use rimz::ids::{MessageId, PaneId};
use rimz::message::{
    DeliveryGate, MessageRecord, MessageStatus, gate_open, queue_head, settle_duration_from_env,
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
    /// Queue text without pressing Enter after delivery.
    #[arg(long)]
    no_enter: bool,
    /// Restrict matches to one worktree branch, name, or path (the channel).
    #[arg(long)]
    worktree: Option<String>,
    /// Queue for more than one agent without the confirmation prompt.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Text to deliver.
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
    #[arg(long, value_parser = parse_gate, default_value = "done")]
    on: DeliveryGate,
    #[arg(long)]
    no_enter: bool,
    #[arg(long)]
    worktree: Option<String>,
    #[arg(long, short = 'y')]
    yes: bool,
    #[arg(last = true)]
    text: Vec<String>,
}

pub fn run(args: QueueArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(QueueSubcmd::Add(add)) => add_message(
            add.target,
            add.worktree,
            join_text(add.text)?,
            !add.no_enter,
            add.on,
            add.yes,
            globals,
        ),
        Some(QueueSubcmd::List { json, target }) => list_messages(json, target, globals),
        Some(QueueSubcmd::Remove { message_id }) => remove_message(message_id, globals),
        Some(QueueSubcmd::Clear { target, worktree }) => clear_messages(target, worktree, globals),
        Some(QueueSubcmd::Deliver { message_id }) => deliver_message(message_id, globals),
        None => {
            let target = args.target.ok_or_else(|| {
                anyhow::anyhow!("expected a target, or `rimz queue list|remove|clear`")
            })?;
            let text = join_text(args.text)?;
            add_message(
                target,
                args.worktree,
                text,
                !args.no_enter,
                args.on,
                args.yes,
                globals,
            )
        }
    }
}

fn join_text(text: Vec<String>) -> Result<String> {
    if text.is_empty() {
        bail!("expected text after `--`");
    }
    Ok(text.join(" "))
}

fn add_message(
    target: String,
    worktree: Option<String>,
    text: String,
    enter: bool,
    gate: DeliveryGate,
    yes: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    if text.is_empty() {
        bail!("expected non-empty text");
    }
    rimz::target::require_mention(&target)?;
    let (workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let channel = current_channel(&workspace);
    let agents =
        super::resolve_agent_many(&snapshot, &target, worktree.as_deref(), channel.as_deref())?;
    if agents.len() > 1 && !yes {
        super::confirm_fanout("Queue for", &target, &agents)?;
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
            enter,
            gate,
        );
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
        let mut table =
            render::Table::new(["ID", "STATUS", "TARGET", "ATTEMPTS", "TEXT"]).right(&[3]);
        for message in messages {
            table.row([
                render::cell(message.message_id.to_string()).fg(render::palette::ACCENT),
                render::cell(message.status.as_str()).fg(render::status::message(message.status)),
                render::cell(format!("{}:{}", message.kind, message.agent_id))
                    .fg(render::palette::META),
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
    match super::pane::submit_message(
        backend.as_ref(),
        &candidate.pane_id,
        &message.text,
        message.enter,
    ) {
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
    let Some(head) = queue_head(pending.iter(), &message.kind, &message.agent_id) else {
        return Ok(None);
    };
    if head.message_id != *message_id {
        return Ok(None);
    }
    let snapshot = ledger
        .snapshot_cached()
        .context("reading delivery snapshot")?;
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
    if pending_ask_for(
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
    Ok(Some(DeliveryCandidate {
        message,
        pane_id: pane.pane_id.clone(),
    }))
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
