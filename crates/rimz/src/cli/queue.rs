//! `rimz queue` — send-now or parked per-agent text delivery.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::send::{self, SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::agents::AgentState;
use rimz::feed::pending_ask_for;
use rimz::ids::MessageId;
use rimz::message::{
    AutoCompact, DeliveryGate, MessageRecord, MessageStatus, gate_open, message_interval_from_env,
    queue_head, settle_duration_from_env,
};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};
use rimz::{PaneAgent, SidebarSnapshot};

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
    /// Remove one queued message.
    Remove { message_id: MessageId },
    /// Remove every queued message for an agent.
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
pub(crate) fn queue_add(
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
        smart_compact,
        file,
        no_from,
        wait,
    } = send;
    let wait = send::wait_duration(wait);
    send::validate_wait(!no_enter, wait)?;
    let auto_compact = smart_compact.or_else(|| super::machine_config().harness.smart_compact);
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
            no_from,
            wait,
        },
        FanoutFlags { all, create, yes },
        globals,
    )
}

pub(crate) fn queue_to_session(
    root: &Path,
    kind: &str,
    session: &str,
    text: String,
    gate: DeliveryGate,
    globals: &GlobalFlags,
) -> Result<()> {
    let mut globals = globals.clone();
    globals.root = Some(root.to_path_buf());
    tracing::debug!(kind, session, "queueing loop wake-up");
    add_message(
        format!("@{session}"),
        None,
        text,
        MessageSpec {
            enter: true,
            gate,
            force: false,
            auto_compact: None,
            no_from: false,
            wait: None,
        },
        FanoutFlags {
            all: false,
            create: false,
            yes: true,
        },
        &globals,
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
    no_from: bool,
    wait: Option<Duration>,
}

/// One logical queue target. `pane` is present when the agent can be reached
/// through the live pane fold now; `agent` is the durable rollup identity used
/// for FIFO and parked records. Lazy panes may have a pane and no identity.
#[derive(Clone, Copy)]
struct QueueTarget<'a> {
    pane: Option<&'a PaneAgent>,
    agent: Option<&'a AgentState>,
}

struct AddOutput {
    label: String,
    message_id: MessageId,
    status: MessageStatus,
}

impl QueueTarget<'_> {
    fn label(&self) -> String {
        self.agent
            .map(super::agent_label)
            .or_else(|| self.pane.map(PaneAgent::label))
            .unwrap_or_else(|| "agent".to_owned())
    }

    fn bound<'a>(&self, snapshot: &'a SidebarSnapshot) -> Option<&'a AgentState> {
        self.pane.and_then(|pane| send::bound_agent(snapshot, pane))
    }

    fn receivable_now(
        &self,
        snapshot: &SidebarSnapshot,
        pending: &[MessageRecord],
        gate: DeliveryGate,
        force: bool,
    ) -> bool {
        if self.pane.is_none() {
            return false;
        }
        let open = match self.bound(snapshot) {
            None => true,
            Some(agent) => {
                gate_open(gate, agent.status)
                    && (force
                        || pending_ask_for(
                            agent,
                            snapshot
                                .needs_attention
                                .iter()
                                .chain(snapshot.resolver_working.iter()),
                        )
                        .is_none())
            }
        };
        if !open {
            return false;
        }
        self.agent.is_none_or(|agent| {
            queue_head(
                pending.iter(),
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
            )
            .is_none()
        })
    }
}

fn combine_queue_targets<'a>(
    snapshot: &'a SidebarSnapshot,
    agents: Vec<&'a AgentState>,
    panes: Vec<&'a PaneAgent>,
) -> Vec<QueueTarget<'a>> {
    let mut used_panes = vec![false; panes.len()];
    let mut targets = Vec::new();
    for agent in agents {
        let pane_index = panes
            .iter()
            .enumerate()
            .find(|(index, pane)| !used_panes[*index] && pane_matches_agent(pane, agent))
            .map(|(index, _)| index);
        let pane = pane_index.map(|index| {
            used_panes[index] = true;
            panes[index]
        });
        targets.push(QueueTarget {
            pane,
            agent: Some(agent),
        });
    }
    for (index, pane) in panes.into_iter().enumerate() {
        if used_panes[index] {
            continue;
        }
        targets.push(QueueTarget {
            pane: Some(pane),
            agent: send::bound_agent(snapshot, pane)
                .or_else(|| provisional_agent_for_pane(snapshot, pane)),
        });
    }
    targets
}

fn pane_matches_agent(pane: &PaneAgent, agent: &AgentState) -> bool {
    if pane.kind != agent.kind {
        return false;
    }
    if pane.agent_id.as_ref() == Some(&agent.agent_id) {
        return true;
    }
    pane.agent_id.is_none()
        && agent.agent_id.is_provisional()
        && pane.channel() == rimz::target::agent_channel(agent)
}

fn provisional_agent_for_pane<'a>(
    snapshot: &'a SidebarSnapshot,
    pane: &PaneAgent,
) -> Option<&'a AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .find(|agent| {
            agent.kind == pane.kind
                && agent.agent_id.is_provisional()
                && rimz::target::agent_channel(agent) == pane.channel()
        })
}

fn rollup_targets_all_park_without_live(
    snapshot: &SidebarSnapshot,
    raw: &str,
    worktree: Option<&str>,
    channel: Option<&str>,
    pending: &[MessageRecord],
    gate: DeliveryGate,
    force: bool,
) -> bool {
    if rimz::target::is_broadcast(raw) {
        return false;
    }
    let Ok(agents) = super::resolve_agent_many(snapshot, raw, worktree, channel) else {
        return false;
    };
    agents
        .iter()
        .all(|agent| !agent_needs_live_queue_resolution(snapshot, pending, agent, gate, force))
}

fn agent_needs_live_queue_resolution(
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    agent: &AgentState,
    gate: DeliveryGate,
    force: bool,
) -> bool {
    agent.agent_id.is_provisional()
        || agent_kind_registers_lazily(agent)
        || (gate_open(gate, agent.status)
            && (force
                || pending_ask_for(
                    agent,
                    snapshot
                        .needs_attention
                        .iter()
                        .chain(snapshot.resolver_working.iter()),
                )
                .is_none())
            && queue_head(
                pending.iter(),
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
            )
            .is_none())
}

fn agent_kind_registers_lazily(agent: &AgentState) -> bool {
    rimz::agents::descriptor_by_kind(agent.kind.as_str())
        .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
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
    let mut snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), spec.no_from);
    let mut pending = ledger.list_pending_messages()?;
    let rollup_only = rollup_targets_all_park_without_live(
        &snapshot,
        &target,
        worktree.as_deref(),
        channel.as_deref(),
        &pending,
        spec.gate,
        spec.force,
    );
    if !rollup_only {
        snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
        // Smart compaction reads context fill. Immediate queue sends share steer's
        // live path, so fold the disposable context sidecars before any send-now
        // decision that might compact first.
        if spec.auto_compact.is_some()
            && let Ok(runtime) = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        {
            snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
        }
    }
    let targets = if rollup_only {
        let agents =
            super::resolve_agent_many(&snapshot, &target, worktree.as_deref(), channel.as_deref())?;
        combine_queue_targets(&snapshot, agents, Vec::new())
    } else {
        let agent_result =
            super::resolve_agent_many(&snapshot, &target, worktree.as_deref(), channel.as_deref());
        let pane_result = super::resolve_pane_targets(
            &snapshot,
            &target,
            worktree.as_deref(),
            channel.as_deref(),
        );
        match (agent_result, pane_result) {
            (Ok(agents), Ok(panes)) => combine_queue_targets(&snapshot, agents, panes),
            (Ok(agents), Err(_)) => combine_queue_targets(&snapshot, agents, Vec::new()),
            (Err(_), Ok(panes)) => combine_queue_targets(&snapshot, Vec::new(), panes),
            (Err(err), Err(_)) => {
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
        }
    };
    if targets.len() > 1 {
        let labels: Vec<String> = targets.iter().map(QueueTarget::label).collect();
        if !flags.all && !rimz::target::is_broadcast(&target) {
            return Err(super::ambiguous_fanout("deliver to", &target, &labels));
        }
        if !flags.yes {
            super::confirm_fanout("Deliver to", &target, &labels)?;
        }
    }
    let mut live_send = send::LiveSend {
        force: spec.force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut kinds_seen = std::collections::BTreeSet::new();
    let mut compacted = Vec::new();
    let mut outputs = Vec::new();
    for target in &targets {
        let label = target.label();
        let mut park = !target.receivable_now(&snapshot, &pending, spec.gate, spec.force);
        if !park && let Some(pane) = target.pane {
            let bound = target.bound(&snapshot);
            let message = send::message_for_target(
                workspace.workspace_id.clone(),
                pane,
                bound,
                send::MessageDraft {
                    text: text.clone(),
                    body: rimz::message::MessageBody::Prompt,
                    enter: spec.enter,
                    gate: spec.gate,
                    sender: sender.clone(),
                    force: spec.force,
                    auto_compact: spec.auto_compact,
                },
            );
            match send::send_prompt_to_live_pane(
                &workspace,
                &ledger,
                &snapshot,
                pane,
                bound,
                &message,
                &mut live_send,
            ) {
                Ok(sent) => match sent.outcome {
                    send::Outcome::Sent { message_id, .. } => {
                        if sent.compacted.is_some() {
                            compacted.push(label.clone());
                        }
                        outputs.push(AddOutput {
                            label,
                            message_id,
                            status: MessageStatus::Sent,
                        });
                        continue;
                    }
                    send::Outcome::SkippedPending { .. } => park = true,
                },
                Err(err) => {
                    ledger.record_send_error(
                        &message,
                        &err.to_string(),
                        &workspace.session_name,
                    )?;
                    return Err(err);
                }
            }
        }
        if !park {
            continue;
        }
        let Some(agent) = target.agent else {
            bail!(
                "`{}` cannot receive now and has no durable session to park",
                target.label()
            );
        };
        if kinds_seen.insert(agent.kind.as_str().to_owned()) {
            preflight_queue_hooks(agent)?;
        }
        let message = MessageRecord::new(
            workspace.workspace_id.clone(),
            agent,
            text.clone(),
            spec.enter,
            spec.gate,
        )
        .with_force(spec.force)
        .with_sender(sender.clone())
        .with_auto_compact(spec.auto_compact);
        let message_id = message.message_id.clone();
        ledger.queue_message(&message, &workspace.session_name)?;
        pending.push(message);
        outputs.push(AddOutput {
            label,
            message_id,
            status: MessageStatus::Queued,
        });
    }
    for label in &compacted {
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("compacted {label}");
        }
    }
    let mut failed = false;
    let wait_deadline = spec.wait.map(|timeout| std::time::Instant::now() + timeout);
    for output in &outputs {
        let mut status = output.status;
        if let Some(deadline) = wait_deadline {
            status = send::wait_for_message_until(
                &ledger,
                &output.message_id,
                &workspace.session_name,
                deadline,
            )?;
            if status != MessageStatus::Delivered {
                failed = true;
            }
        }
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("{}", render_add_output(output, status, spec.wait.is_some()));
        }
    }
    if failed {
        std::process::exit(1);
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
            render::Table::new(["ID", "STATUS", "TARGET", "FROM", "ATTEMPTS", "TEXT"]).right(&[4]);
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
                render::cell(message.sender.render()).fg(render::palette::META),
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
        bail!("message {message_id} is not queued or claimed");
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

fn render_add_output(output: &AddOutput, status: MessageStatus, waited: bool) -> String {
    if waited {
        return format!("{} {}", wait_status_label(status), output.label);
    }
    match status {
        MessageStatus::Sent => format!("sent {}", output.label),
        MessageStatus::Queued => format!("queued {} ({})", output.label, output.message_id),
        other => format!("{} {}", other.as_str(), output.label),
    }
}

fn wait_status_label(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Delivered => "delivered",
        MessageStatus::Errored => "errored",
        MessageStatus::TimedOut => "timed out",
        MessageStatus::Removed => "removed",
        MessageStatus::Abandoned => "abandoned",
        MessageStatus::Created
        | MessageStatus::Queued
        | MessageStatus::Claimed
        | MessageStatus::Sent => "timed out",
    }
}

fn deliver_message(message_id: MessageId, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    deliver_one(
        &workspace,
        &ledger,
        &message_id,
        settle_duration_from_env(),
        globals,
    )?;
    Ok(())
}

fn deliver_one(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    message_id: &MessageId,
    settle: Duration,
    globals: &GlobalFlags,
) -> Result<bool> {
    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    let Some(candidate) = delivery_candidate(workspace, ledger, message_id, globals)? else {
        return Ok(false);
    };
    let Some(message) = ledger.claim_message_for_delivery(message_id, jiff::Timestamp::now())?
    else {
        return Ok(false);
    };
    debug_assert!(message.same_agent(&candidate.message.kind, &candidate.message.agent_id));
    debug_assert_eq!(message.message_id, candidate.message.message_id);
    // Hook delivery handles one claimed message; settle above owns any
    // pre-delivery spacing, so this pacer's first tick stays a no-op.
    let mut live_send = send::LiveSend {
        force: message.force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let send_message = message
        .clone()
        .with_pane_id(candidate.target.pane_id.clone());
    let send = send::send_prompt_to_live_pane(
        workspace,
        ledger,
        &candidate.snapshot,
        &candidate.target,
        send::bound_agent(&candidate.snapshot, &candidate.target),
        &send_message,
        &mut live_send,
    );
    match send {
        Ok(send::SentPrompt {
            outcome: send::Outcome::Sent { .. },
            ..
        }) => Ok(true),
        Ok(send::SentPrompt {
            outcome: send::Outcome::SkippedPending { request_id, .. },
            ..
        }) => {
            ledger.record_message_delivery_failure(
                &message.message_id,
                &format!("pending ask {request_id} reserves input"),
                &workspace.session_name,
            )?;
            Ok(false)
        }
        Err(err) => {
            if ledger
                .record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &workspace.session_name,
                )?
                .is_none()
            {
                ledger.record_send_error(
                    &send_message,
                    &err.to_string(),
                    &workspace.session_name,
                )?;
            }
            Ok(false)
        }
    }
}

struct DeliveryCandidate {
    message: MessageRecord,
    snapshot: SidebarSnapshot,
    target: PaneAgent,
}

fn delivery_candidate(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    message_id: &MessageId,
    globals: &GlobalFlags,
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
    let mut snapshot = super::resolution_snapshot(workspace, ledger, globals)
        .context("reading delivery snapshot")?;
    // `--smart-compact` reads context fill, which the resolution snapshot does
    // not carry; fold the disposable context sidecars in for the freshest gauge.
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
    let Some(target) = snapshot
        .agent_panes
        .iter()
        .find(|pane| pane_matches_agent(pane, agent))
        .cloned()
    else {
        return Ok(None);
    };
    Ok(Some(DeliveryCandidate {
        message,
        snapshot,
        target,
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
            "queued delivery requires {} hooks so messages can deliver at turn boundaries; run `rimz hooks install {}`",
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

pub(crate) fn parse_gate(raw: &str) -> std::result::Result<DeliveryGate, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use rimz::agents::{AgentStatus, TurnPhase};
    use rimz::feed::{FeedItem, FeedKind, Surface};
    use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
    use rimz::pane::PaneRef;

    #[test]
    fn receivable_now_decision_table() {
        let idle = agent("sess-idle", AgentStatus::Idle);
        let running = agent("sess-running", AgentStatus::Running);
        let pane = bound_pane(&idle, "terminal_3");
        let lazy = lazy_pane("codex", "terminal_4");
        let idle_snapshot =
            snapshot_with_panes(vec![idle.clone(), running.clone()], vec![pane.clone()]);

        assert!(
            QueueTarget {
                pane: Some(&lazy),
                agent: None,
            }
            .receivable_now(&idle_snapshot, &[], DeliveryGate::Done, false)
        );

        assert!(
            QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&idle_snapshot, &[], DeliveryGate::Done, false)
        );

        let running_pane = bound_pane(&running, "terminal_5");
        let running_snapshot =
            snapshot_with_panes(vec![running.clone()], vec![running_pane.clone()]);
        assert!(
            !QueueTarget {
                pane: Some(&running_pane),
                agent: Some(&running),
            }
            .receivable_now(&running_snapshot, &[], DeliveryGate::Done, false)
        );

        let ask_snapshot = snapshot_with_ask(idle.clone(), pane.clone());
        assert!(
            !QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&ask_snapshot, &[], DeliveryGate::Done, false)
        );
        assert!(
            QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&ask_snapshot, &[], DeliveryGate::Done, true)
        );

        let older = MessageRecord::new(
            workspace_id(),
            &idle,
            "older".to_owned(),
            true,
            DeliveryGate::Done,
        );
        assert!(
            !QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&idle_snapshot, &[older], DeliveryGate::Done, false)
        );
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn snapshot_with_panes(agents: Vec<AgentState>, panes: Vec<PaneAgent>) -> SidebarSnapshot {
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id(), Vec::new(), agents, now());
        snapshot.agent_panes = panes;
        snapshot
    }

    fn snapshot_with_ask(agent: AgentState, pane: PaneAgent) -> SidebarSnapshot {
        let mut item = FeedItem::new(
            workspace_id(),
            Surface::NativeUi,
            FeedKind::Permission,
            "approve?",
            agent.kind.as_str(),
            "agent-hook",
        );
        item.payload = json!({ "session_id": agent.agent_id.as_str() });
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id(), vec![item], vec![agent], now());
        snapshot.agent_panes = vec![pane];
        snapshot
    }

    fn agent(id: &str, status: AgentStatus) -> AgentState {
        let timestamp = now();
        let phase = match status {
            AgentStatus::Running => TurnPhase::Reasoning,
            _ => TurnPhase::Idle,
        };
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked("claude"),
            name: Some(format!("{id}-name")),
            kind_ordinal: Some(1),
            profile: None,
            role: None,
            team: None,
            status,
            phase,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                "terminal_3",
            ))),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: Some("/repo/project".to_owned()),
            worktree_branch: Some("project".to_owned()),
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: timestamp,
            last_activity: timestamp,
            registered_at: Some(timestamp),
        }
    }

    fn bound_pane(agent: &AgentState, raw: &str) -> PaneAgent {
        PaneAgent {
            kind: agent.kind.clone(),
            kind_ordinal: agent.kind_ordinal,
            name: agent.name.clone(),
            profile: None,
            role: None,
            team: None,
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            worktree_path: agent.worktree_path.clone(),
            worktree_branch: agent.worktree_branch.clone(),
        }
    }

    fn lazy_pane(kind: &str, raw: &str) -> PaneAgent {
        PaneAgent {
            kind: AgentKind::new_unchecked(kind),
            kind_ordinal: None,
            name: None,
            profile: None,
            role: None,
            team: None,
            agent_id: None,
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            worktree_path: Some("/repo/project".to_owned()),
            worktree_branch: Some("project".to_owned()),
        }
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }
}
