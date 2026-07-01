//! `rimz message` — immediate or parked per-agent text delivery.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use serde::Serialize;

use super::send::{self, SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::agents::AgentState;
use rimz::feed::pending_ask_for;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, PaneId};
use rimz::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, gate_open,
    max_delivery_attempts_from_env, message_interval_from_env, parse_schedule_at, queue_head,
    settle_duration_from_env,
};
use rimz::mux::MuxErr;
use rimz::schema::event::{EventEnvelope, EventKind, MessageEventPayload};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};
use rimz::{PaneAgent, RuntimePaths, SidebarSnapshot};

#[derive(Debug, Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    command: Option<MessageSubcmd>,
    /// Agent mention for the bare message form.
    target: Option<String>,
    /// The message, as one quoted argument. Omit it and pass `--file` to deliver
    /// a file's contents verbatim.
    text: Option<String>,
    /// Deliver after a successful/idle turn (`done`) or after success/idle/failure (`any`).
    #[arg(long, value_parser = parse_gate, default_value = "done", conflicts_with = "steer")]
    on: DeliveryGate,
    /// Interrupt the live pane now instead of parking for a turn boundary.
    #[arg(long, conflicts_with_all = ["schedule", "on"])]
    steer: bool,
    /// Park the message until at least this duration or configured-zone `HH:MM`.
    #[arg(long, value_name = "DUR|HH:MM", conflicts_with = "steer")]
    schedule: Option<String>,
    #[command(flatten)]
    send: SendFlags,
}

#[derive(Debug, Subcommand)]
enum MessageSubcmd {
    /// List queued message records.
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Include every channel and archived messages.
        #[arg(long)]
        all: bool,
        /// Exact status filter.
        #[arg(long, value_name = "STATUS", value_parser = parse_status)]
        status: Option<MessageStatus>,
        /// Filter by channel name.
        #[arg(long, value_name = "NAME")]
        channel: Option<String>,
        /// Optional target filter.
        target: Option<String>,
    },
    /// Show one message record.
    Status { message_id: MessageId },
    /// Remove one queued message.
    Remove { message_id: MessageId },
    /// Remove every queued message for an agent.
    Clear {
        target: String,
        #[arg(long, conflicts_with = "channel")]
        worktree: Option<String>,
        #[arg(long, value_name = "NAME", conflicts_with = "worktree")]
        channel: Option<String>,
    },
    /// Deliver one queued message. Spawned by lifecycle hooks.
    #[command(hide = true)]
    Deliver {
        #[arg(long)]
        message_id: MessageId,
    },
    /// Deliver due scheduled messages. Spawned by the sidebar elder.
    #[command(hide = true)]
    Sweep,
}

pub fn run(args: MessageArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(MessageSubcmd::List {
            json,
            all,
            status,
            channel,
            target,
        }) => list_messages(json, all, status, channel, target, globals),
        Some(MessageSubcmd::Status { message_id }) => status_message(message_id, globals),
        Some(MessageSubcmd::Remove { message_id }) => remove_message(message_id, globals),
        Some(MessageSubcmd::Clear {
            target,
            worktree,
            channel,
        }) => clear_messages(target, worktree, channel, globals),
        Some(MessageSubcmd::Deliver { message_id }) => deliver_message(message_id, globals),
        Some(MessageSubcmd::Sweep) => sweep_messages(globals),
        None => {
            let target = args.target.ok_or_else(|| {
                anyhow::anyhow!("expected a target, or `rimz message list|remove|clear`")
            })?;
            let text = args.text.into_iter().collect();
            if args.steer {
                steer_message(target, args.send, text, globals)
            } else {
                message_add(target, args.on, args.schedule, args.send, text, globals)
            }
        }
    }
}

/// Shared enqueue for parked messages: resolve the prompt from inline argv or
/// `--file`, then split the mirrored `SendFlags` into the delivery spec and the
/// fan-out controls and hand off.
fn message_add(
    target: String,
    gate: DeliveryGate,
    schedule: Option<String>,
    send: SendFlags,
    text: Vec<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let SendFlags {
        worktree,
        channel,
        no_enter,
        force,
        all,
        create,
        smart_compact,
        file,
        no_from,
        wait,
    } = send;
    if schedule.is_some() && create {
        bail!("--schedule needs an existing agent; remove --create");
    }
    let wait = send::wait_duration(wait);
    send::validate_wait(!no_enter, wait)?;
    let machine_config = super::machine_config();
    let auto_compact = smart_compact.or(machine_config.harness.smart_compact);
    let text = resolve_message(&text, file.as_deref())?;
    let now = Timestamp::now().to_zoned(machine_config.time_zone());
    let not_before = schedule
        .as_deref()
        .map(|raw| parse_schedule_at(raw, &now).map_err(anyhow::Error::msg))
        .transpose()?;
    add_message(
        target,
        worktree,
        channel,
        text,
        MessageSpec {
            enter: !no_enter,
            gate,
            force,
            auto_compact,
            no_from,
            wait,
            not_before,
            stamp_channel: true,
        },
        FanoutFlags { all, create },
        globals,
    )
}

pub(crate) fn to_session(
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
        None,
        text,
        MessageSpec {
            enter: true,
            gate,
            force: false,
            auto_compact: None,
            no_from: false,
            wait: None,
            not_before: None,
            stamp_channel: false,
        },
        FanoutFlags {
            all: false,
            create: false,
        },
        &globals,
    )
}

fn steer_message(
    target: String,
    send: SendFlags,
    text: Vec<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    rimz::target::require_mention(&target)?;
    let SendFlags {
        worktree,
        channel: channel_flag,
        no_enter,
        force,
        all,
        create,
        smart_compact,
        file,
        no_from,
        wait,
    } = send;
    let wait = send::wait_duration(wait);
    send::validate_wait(!no_enter, wait)?;
    let auto_compact = smart_compact.or_else(|| super::machine_config().harness.smart_compact);
    let text = resolve_message(&text, file.as_deref())?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let mut snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
    if auto_compact.is_some()
        && let Ok(runtime) = RuntimePaths::for_workspace(workspace.workspace_id.clone())
    {
        snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    }
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), no_from);
    let targets = match super::resolve_pane_targets(
        &snapshot,
        &target,
        worktree.as_deref().or(channel_flag.as_deref()),
        channel.as_deref(),
    ) {
        Ok(targets) => targets,
        Err(_) if create => {
            return super::agents_cmd::create_on_miss(
                &target,
                worktree.as_deref(),
                channel_flag.as_deref(),
                channel.as_deref(),
                &text,
                globals,
            );
        }
        Err(err) => return message_miss(&snapshot, channel.as_deref(), &err),
    };

    if targets.len() > 1 && !all && !rimz::target::is_broadcast(&target) {
        let labels: Vec<String> = targets.iter().map(|target| target.label()).collect();
        return Err(super::ambiguous_fanout("message --steer", &target, &labels));
    }
    let text = if targets.len() > 1 || rimz::target::is_broadcast(&target) {
        rimz::target::group_prefixed(&target, &text)
    } else {
        text
    };
    let mut live_send = send::LiveSend {
        force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut compacted = Vec::new();
    for target in &targets {
        let bound = send::bound_agent(&snapshot, target);
        let handle = send::handle_for_pane_target(&snapshot, target, bound);
        let message = send::message_for_target(
            workspace.workspace_id.clone(),
            target,
            bound,
            channel.as_deref(),
            send::MessageDraft {
                text: text.clone(),
                body: rimz::message::MessageBody::Prompt,
                enter: !no_enter,
                gate: DeliveryGate::Any,
                sender: sender.clone(),
                force,
                auto_compact,
            },
        );
        let sent = match send::send_prompt_to_live_pane(
            &workspace,
            &ledger,
            &snapshot,
            target,
            bound,
            &message,
            &mut live_send,
        ) {
            Ok(sent) => sent,
            Err(err) => {
                if message_recorded_as_sent(&ledger, &message.message_id)? {
                    register_message_wake(&workspace, &ledger)?;
                    outcomes.push(send::Outcome::Sent {
                        label: handle,
                        message_id: message.message_id.clone(),
                    });
                    continue;
                }
                ledger.record_send_error(&message, &err.to_string(), &workspace.session_name)?;
                register_message_wake(&workspace, &ledger)?;
                return Err(err);
            }
        };
        if sent.compacted.is_some() {
            compacted.push(handle);
        }
        if sent_prompt_has_sent_record(&sent) {
            register_message_wake(&workspace, &ledger)?;
        }
        outcomes.push(sent.outcome);
    }

    report_steer(
        &ledger,
        &workspace.session_name,
        wait,
        &target,
        targets.len(),
        &outcomes,
        &compacted,
    )
}

/// The fan-out / create flags shared by parked message delivery.
struct FanoutFlags {
    all: bool,
    create: bool,
}

fn message_miss(
    snapshot: &SidebarSnapshot,
    channel: Option<&str>,
    err: &anyhow::Error,
) -> Result<()> {
    let mut out = render::err();
    writeln!(out, "{err:#}")?;
    let agents: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| channel.is_none_or(|filter| rimz::target::agent_in_worktree(agent, filter)))
        .collect();
    if agents.is_empty() {
        writeln!(out, "no agents are running")?;
    } else {
        writeln!(out, "available agents:")?;
        super::agents_cmd::render_agents_table(&mut out, snapshot, &agents, Timestamp::now())?;
    }
    out.flush().ok();
    std::process::exit(1);
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
    not_before: Option<Timestamp>,
    stamp_channel: bool,
}

/// One logical message target. `pane` is present when the agent can be reached
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

#[derive(Clone, Debug, Serialize)]
struct MessageListRow {
    message_id: MessageId,
    kind: AgentKind,
    agent_id: AgentSessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    sender: MessageSender,
    body: MessageBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    enter: bool,
    gate: DeliveryGate,
    force: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_id: Option<PaneId>,
    status: MessageStatus,
    enqueued_at: Timestamp,
    updated_at: Timestamp,
    attempts: u32,
    unconfirmed_sends: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_attempt_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivered_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    not_before: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_compact: Option<AutoCompact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compacted_context_tokens: Option<u64>,
}

impl MessageListRow {
    fn from_record(message: MessageRecord) -> Self {
        Self {
            message_id: message.message_id,
            kind: message.kind,
            agent_id: message.agent_id,
            agent_name: message.agent_name,
            channel: message.channel,
            sender: message.sender,
            body: message.body,
            text: Some(message.text),
            enter: message.enter,
            gate: message.gate,
            force: message.force,
            pane_id: message.pane_id,
            status: message.status,
            enqueued_at: message.enqueued_at,
            updated_at: message.updated_at,
            attempts: message.attempts,
            unconfirmed_sends: message.unconfirmed_sends,
            last_attempt_at: message.last_attempt_at,
            last_error: message.last_error,
            delivered_at: message.delivered_at,
            not_before: message.not_before,
            retry_after: message.retry_after,
            auto_compact: message.auto_compact,
            compacted_context_tokens: message.compacted_context_tokens,
        }
    }

    fn from_terminal_event(event: &EventEnvelope, payload: MessageEventPayload) -> Option<Self> {
        if !payload.status.is_terminal() {
            return None;
        }
        let delivered_at = payload
            .delivered_at
            .or_else(|| (payload.status == MessageStatus::Delivered).then_some(event.timestamp));
        Some(Self {
            message_id: payload.message_id,
            kind: payload.kind,
            agent_id: payload.agent_id,
            agent_name: payload.agent_name,
            channel: payload.channel,
            sender: payload.sender.unwrap_or_default(),
            body: payload.body,
            text: None,
            enter: payload.enter,
            gate: payload.gate,
            force: payload.forced,
            pane_id: payload.pane_id,
            status: payload.status,
            enqueued_at: payload.enqueued_at.unwrap_or(event.timestamp),
            updated_at: event.timestamp,
            attempts: payload.attempts,
            unconfirmed_sends: payload.unconfirmed_sends,
            last_attempt_at: None,
            last_error: payload.reason,
            delivered_at,
            not_before: None,
            retry_after: None,
            auto_compact: None,
            compacted_context_tokens: payload.compacted_context_tokens,
        })
    }

    fn same_agent_card(&self, agent: &AgentState) -> bool {
        self.kind == agent.kind
            && (self.agent_id == agent.agent_id
                || (agent.name.is_some() && self.agent_name.as_deref() == agent.name.as_deref()))
    }
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
        now: Timestamp,
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
                now,
            )
            .is_none()
        })
    }
}

fn handle_for_target(snapshot: &SidebarSnapshot, target: &QueueTarget<'_>) -> String {
    if let Some(agent) = target.agent {
        let peers: Vec<&AgentState> = snapshot
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .collect();
        rimz::target::agent_handle(agent, &peers, true)
    } else if let Some(pane) = target.pane {
        format!("@{}", pane.label())
    } else {
        "@agent".to_owned()
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
    let now = Timestamp::now();
    agents
        .iter()
        .all(|agent| !agent_needs_live_queue_resolution(snapshot, pending, agent, gate, force, now))
}

fn agent_needs_live_queue_resolution(
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    agent: &AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
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
                now,
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
    channel_flag: Option<String>,
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
    let now = Timestamp::now();
    let rollup_only = rollup_targets_all_park_without_live(
        &snapshot,
        &target,
        worktree.as_deref().or(channel_flag.as_deref()),
        channel.as_deref(),
        &pending,
        spec.gate,
        spec.force,
    );
    if !rollup_only {
        snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
        // Smart compaction reads context fill. Immediate message sends share the
        // live path, so fold the disposable context sidecars before any send-now
        // decision that might compact first.
        if spec.auto_compact.is_some()
            && let Ok(runtime) = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        {
            snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
        }
    }
    let targets = if rollup_only {
        let agents = super::resolve_agent_many(
            &snapshot,
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
        )?;
        combine_queue_targets(&snapshot, agents, Vec::new())
    } else {
        let agent_result = super::resolve_agent_many(
            &snapshot,
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
        );
        let pane_result = super::resolve_pane_targets(
            &snapshot,
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
        );
        match (agent_result, pane_result) {
            (Ok(agents), Ok(panes)) => combine_queue_targets(&snapshot, agents, panes),
            (Ok(agents), Err(_)) => combine_queue_targets(&snapshot, agents, Vec::new()),
            (Err(_), Ok(panes)) => combine_queue_targets(&snapshot, Vec::new(), panes),
            (Err(err), Err(_)) => {
                // Create-on-miss launches a fresh agent with this text as its first
                // prompt, so the launch carries the work and no message record is made.
                if flags.create {
                    return super::agents_cmd::create_on_miss(
                        &target,
                        worktree.as_deref(),
                        channel_flag.as_deref(),
                        channel.as_deref(),
                        &text,
                        globals,
                    );
                }
                return message_miss(&snapshot, channel.as_deref(), &err);
            }
        }
    };
    if targets.len() > 1 && !flags.all && !rimz::target::is_broadcast(&target) {
        let labels: Vec<String> = targets.iter().map(QueueTarget::label).collect();
        return Err(super::ambiguous_fanout("deliver to", &target, &labels));
    }
    let text = if targets.len() > 1 || rimz::target::is_broadcast(&target) {
        rimz::target::group_prefixed(&target, &text)
    } else {
        text
    };
    let mut live_send = send::LiveSend {
        force: spec.force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut kinds_seen = std::collections::BTreeSet::new();
    let mut compacted = Vec::new();
    let mut outputs = Vec::new();
    for target in &targets {
        let handle = handle_for_target(&snapshot, target);
        let mut park = spec.not_before.is_some()
            || !target.receivable_now(&snapshot, &pending, spec.gate, spec.force, now);
        if !park && let Some(pane) = target.pane {
            let bound = target.bound(&snapshot);
            let message = send::message_for_target(
                workspace.workspace_id.clone(),
                pane,
                bound,
                channel.as_deref(),
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
                            compacted.push(handle.clone());
                        }
                        register_message_wake(&workspace, &ledger)?;
                        outputs.push(AddOutput {
                            label: handle,
                            message_id,
                            status: MessageStatus::Sent,
                        });
                        continue;
                    }
                    send::Outcome::SkippedPending { .. } => park = true,
                },
                Err(err) => {
                    if message_recorded_as_sent(&ledger, &message.message_id)? {
                        register_message_wake(&workspace, &ledger)?;
                        outputs.push(AddOutput {
                            label: handle,
                            message_id: message.message_id.clone(),
                            status: MessageStatus::Sent,
                        });
                        continue;
                    }
                    if is_mux_timeout(&err) && target.agent.is_some() {
                        park = true;
                    } else {
                        ledger.record_send_error(
                            &message,
                            &err.to_string(),
                            &workspace.session_name,
                        )?;
                        register_message_wake(&workspace, &ledger)?;
                        return Err(err);
                    }
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
        .with_channel(
            spec.stamp_channel
                .then(|| rimz::target::agent_channel(agent))
                .flatten(),
        )
        .with_sender(sender.clone())
        .with_auto_compact(spec.auto_compact)
        .with_not_before(spec.not_before);
        let message_id = message.message_id.clone();
        ledger.queue_message(&message, &workspace.session_name)?;
        pending.push(message);
        outputs.push(AddOutput {
            label: handle,
            message_id,
            status: MessageStatus::Queued,
        });
    }
    register_message_wake(&workspace, &ledger)?;
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

fn list_messages(
    json: bool,
    all: bool,
    status: Option<MessageStatus>,
    channel: Option<String>,
    target: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let (workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let mut messages = projected_messages(&ledger)?;
    let ambient_channel = current_channel(&workspace);
    let default_channel = if all {
        None
    } else {
        ambient_channel.as_deref()
    };
    let filter_channel = channel.as_deref().or(default_channel);
    if let Some(filter) = filter_channel {
        messages.retain(|message| message.channel.as_deref() == Some(filter));
    }
    if let Some(status) = status {
        messages.retain(|message| message.status == status);
    } else if !all {
        messages.retain(|message| message.status != MessageStatus::Archived);
    }
    if let Some(raw) = target {
        rimz::target::require_mention(&raw)?;
        let agent = super::resolve_agent_one(&snapshot, &raw, None, filter_channel)?;
        messages.retain(|message| message.same_agent_card(agent));
    }
    messages.sort_by(|a, b| {
        b.enqueued_at
            .cmp(&a.enqueued_at)
            .then_with(|| b.message_id.as_str().cmp(a.message_id.as_str()))
    });
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
        let now = Timestamp::now();
        let mut table = render::Table::new([
            "ID",
            "STATUS",
            "TARGET",
            "FROM",
            "CREATED",
            "DELIVERED",
            "TEXT",
        ]);
        for message in messages {
            let target = message_target(&message, &agents);
            table.row([
                render::cell(message.message_id.to_string()).fg(render::palette::ACCENT),
                render::cell(message.status.as_str()).fg(render::status::message(message.status)),
                render::cell(target).fg(render::palette::META),
                render::cell(message.sender.render()).fg(render::palette::META),
                render::cell(render::rel_age(message.enqueued_at, now)),
                render::cell(
                    message
                        .delivered_at
                        .map(|delivered| render::rel_age(delivered, now))
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
                render::cell(
                    message
                        .text
                        .as_deref()
                        .map(preview)
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
            ]);
        }
        table.render(&mut render::out())?;
    }
    Ok(())
}

fn projected_messages(ledger: &rimz::Ledger) -> Result<Vec<MessageListRow>> {
    let mut rows = std::collections::BTreeMap::new();
    for event in ledger.read_events()? {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        let Some(row) = MessageListRow::from_terminal_event(&event, payload) else {
            continue;
        };
        rows.insert(row.message_id.to_string(), row);
    }
    for message in ledger.list_messages()? {
        let row = MessageListRow::from_record(message);
        rows.insert(row.message_id.to_string(), row);
    }
    Ok(rows.into_values().collect())
}

fn status_message(message_id: MessageId, globals: &GlobalFlags) -> Result<()> {
    let (_workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let Some(message) = projected_messages(&ledger)?
        .into_iter()
        .find(|message| message.message_id == message_id)
    else {
        bail!("message {message_id} not found");
    };
    let agents: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .collect();
    let now = Timestamp::now();
    let mut kv = render::KeyVals::new();
    kv.push("id", render::cell(message.message_id.to_string()));
    kv.push(
        "status",
        render::cell(message.status.as_str()).fg(render::status::message(message.status)),
    );
    kv.push(
        "target",
        render::cell(message_target(&message, &agents)).fg(render::palette::META),
    );
    kv.push(
        "from",
        render::cell(message.sender.render()).fg(render::palette::META),
    );
    kv.push(
        "channel",
        render::cell(message.channel.clone().unwrap_or_else(|| "-".to_owned())).dash(),
    );
    kv.push(
        "created",
        render::cell(render::rel_age(message.enqueued_at, now)),
    );
    kv.push(
        "delivered",
        render::cell(
            message
                .delivered_at
                .map(|delivered| render::rel_age(delivered, now))
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    kv.push("attempts", render::cell(message.attempts.to_string()));
    kv.push(
        "unconfirmed_sends",
        render::cell(message.unconfirmed_sends.to_string()),
    );
    kv.push(
        "last_error",
        render::cell(message.last_error.clone().unwrap_or_else(|| "-".to_owned())).dash(),
    );
    kv.push(
        "text",
        render::cell(
            message
                .text
                .as_deref()
                .map(preview)
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    kv.render(&mut render::out())?;
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

fn clear_messages(
    target: String,
    worktree: Option<String>,
    channel_flag: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    rimz::target::require_mention(&target)?;
    let (workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let channel = current_channel(&workspace);
    let agent = super::resolve_agent_one(
        &snapshot,
        &target,
        worktree.as_deref().or(channel_flag.as_deref()),
        channel.as_deref(),
    )?;
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
        return format!(
            "{} {} ({})",
            wait_status_label(status),
            output.label,
            output.message_id
        );
    }
    match status {
        MessageStatus::Sent => format!("sent to {} ({})", output.label, output.message_id),
        MessageStatus::Queued => format!("queued for {} ({})", output.label, output.message_id),
        other => format!(
            "{} {} ({})",
            other.as_str(),
            output.label,
            output.message_id
        ),
    }
}

/// Report a `--steer` fan-out. A lone agent that was skipped fails with the
/// same message the single-target path always returned; a broadcast prints its
/// sent/skipped summary and succeeds.
fn report_steer(
    ledger: &rimz::Ledger,
    session_name: &str,
    wait: Option<Duration>,
    target: &str,
    total: usize,
    outcomes: &[send::Outcome],
    compacted: &[String],
) -> Result<()> {
    let sent = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            send::Outcome::Sent { label, message_id } => Some(format!("{label} ({message_id})")),
            _ => None,
        })
        .collect::<Vec<_>>();
    let sent_labels = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            send::Outcome::Sent { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let pending = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            send::Outcome::SkippedPending {
                label, message_id, ..
            } => Some(format!("{label} ({message_id})")),
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some(timeout) = wait {
        let mut failed = false;
        let deadline = std::time::Instant::now() + timeout;
        for outcome in outcomes {
            if let send::Outcome::Sent { label, message_id } = outcome {
                print_compacted_if_needed(label, compacted);
                let status =
                    send::wait_for_message_until(ledger, message_id, session_name, deadline)?;
                if status != MessageStatus::Delivered {
                    failed = true;
                }
                #[expect(clippy::print_stdout, reason = "wait status")]
                {
                    println!("{} {label} ({message_id})", wait_status_label(status));
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    if total == 1 {
        if !sent.is_empty() {
            let label = sent_labels[0];
            print_compacted_if_needed(label, compacted);
            #[expect(clippy::print_stdout, reason = "message confirmation")]
            {
                println!("sent to {}", sent[0]);
            }
            return Ok(());
        }
        match outcomes.first() {
            Some(send::Outcome::SkippedPending {
                label,
                message_id,
                request_id,
            }) => {
                bail!(
                    "{label} ({message_id}) has pending ask {request_id}; resolve it or pass --force"
                )
            }
            _ => bail!("no agent matches `{target}`"),
        }
    }
    let mut line = format!("sent {} agent(s)", sent.len());
    if !sent.is_empty() {
        line.push_str(&format!(": {}", sent.join(", ")));
    }
    if !compacted.is_empty() {
        line.push_str(&format!("; compacted: {}", compacted.join(", ")));
    }
    if !pending.is_empty() {
        line.push_str(&format!("; skipped pending ask: {}", pending.join(", ")));
    }
    #[expect(clippy::print_stdout, reason = "message fan-out summary")]
    {
        println!("{line}");
    }
    Ok(())
}

fn print_compacted_if_needed(label: &str, compacted: &[String]) {
    if compacted.iter().any(|compacted| compacted == label) {
        #[expect(clippy::print_stdout, reason = "message compact confirmation")]
        {
            println!("compacted {label}");
        }
    }
}

fn wait_status_label(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Delivered => "delivered",
        MessageStatus::Errored => "errored",
        MessageStatus::TimedOut => "timed out",
        MessageStatus::Removed => "removed",
        MessageStatus::Abandoned => "abandoned",
        MessageStatus::Archived => "archived",
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

fn sweep_messages(globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let now = Timestamp::now();
    let delivery_window = rimz::message::delivery_window_from_env();
    ledger.reconcile_stale_sent_messages(
        &workspace.session_name,
        now,
        delivery_window,
        max_delivery_attempts_from_env(),
    )?;
    let pending = ledger.list_pending_messages()?;
    let mut heads_seen = std::collections::BTreeSet::new();
    for message in pending.iter().filter(|message| message.is_ready(now)) {
        let Some(head) = queue_head(
            pending.iter(),
            &message.kind,
            &message.agent_id,
            message.agent_name.as_deref(),
            now,
        ) else {
            continue;
        };
        if heads_seen.insert(head.message_id.to_string()) {
            let delivered = deliver_one(
                &workspace,
                &ledger,
                &head.message_id,
                Duration::ZERO,
                globals,
            )?;
            if !delivered {
                ledger.defer_message_wake(&head.message_id, now + delivery_window)?;
            }
        }
    }
    register_message_wake(&workspace, &ledger)?;
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
        }) => {
            register_message_wake(workspace, ledger)?;
            Ok(true)
        }
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
            if message_recorded_as_sent(ledger, &message.message_id)? {
                register_message_wake(workspace, ledger)?;
                return Ok(false);
            }
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
            register_message_wake(workspace, ledger)?;
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
    let now = Timestamp::now();
    if !message.is_ready(now) {
        return Ok(None);
    }
    let Some(head) = queue_head(
        pending.iter(),
        &message.kind,
        &message.agent_id,
        message.agent_name.as_deref(),
        now,
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
    // unless the message was queued with `--force`, mirroring `message --steer --force`.
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

fn register_message_wake(workspace: &ResolvedWorkspace, ledger: &rimz::Ledger) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing message wake cache")?;
    refresh_wake_stamp(&runtime, ledger, Timestamp::now())
}

fn refresh_wake_stamp(runtime: &RuntimePaths, ledger: &rimz::Ledger, now: Timestamp) -> Result<()> {
    let path = wake_stamp_path(runtime);
    let next = ledger.earliest_message_wake(now, rimz::message::delivery_window_from_env())?;
    match next {
        Some(not_before) => {
            rimz::ledger::atomic::write_temp_then_rename_cache(&path, &Some(not_before))
                .with_context(|| format!("writing message wake cache `{}`", path.display()))?;
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("removing message wake cache `{}`", path.display()));
            }
        },
    }
    Ok(())
}

fn sent_prompt_has_sent_record(sent: &send::SentPrompt) -> bool {
    sent.compacted.is_some() || matches!(sent.outcome, send::Outcome::Sent { .. })
}

fn message_recorded_as_sent(ledger: &rimz::Ledger, message_id: &MessageId) -> Result<bool> {
    Ok(ledger
        .list_messages()?
        .iter()
        .any(|message| message.message_id == *message_id && message.status == MessageStatus::Sent))
}

fn is_mux_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<MuxErr>()
            .is_some_and(|err| matches!(err, MuxErr::Timeout { .. }))
    })
}

fn wake_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(rimz::message::MESSAGE_WAKE_FILE)
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

fn parse_status(raw: &str) -> std::result::Result<MessageStatus, String> {
    match raw {
        "created" => Ok(MessageStatus::Created),
        "queued" | "pending" => Ok(MessageStatus::Queued),
        "claimed" => Ok(MessageStatus::Claimed),
        "sent" => Ok(MessageStatus::Sent),
        "delivered" => Ok(MessageStatus::Delivered),
        "timed_out" => Ok(MessageStatus::TimedOut),
        "errored" => Ok(MessageStatus::Errored),
        "removed" => Ok(MessageStatus::Removed),
        "abandoned" => Ok(MessageStatus::Abandoned),
        "archived" => Ok(MessageStatus::Archived),
        other => Err(format!("unknown message status `{other}`")),
    }
}

fn message_target(message: &MessageListRow, agents: &[&AgentState]) -> String {
    agents
        .iter()
        .copied()
        .find(|agent| message.same_agent_card(agent))
        .map(|agent| rimz::target::agent_handle(agent, agents, true))
        .unwrap_or_else(|| format!("{}:{}", message.kind, message.agent_id))
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
        let timestamp = now();
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
            .receivable_now(&idle_snapshot, &[], DeliveryGate::Done, false, timestamp)
        );

        assert!(
            QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&idle_snapshot, &[], DeliveryGate::Done, false, timestamp)
        );

        let running_pane = bound_pane(&running, "terminal_5");
        let running_snapshot =
            snapshot_with_panes(vec![running.clone()], vec![running_pane.clone()]);
        assert!(
            !QueueTarget {
                pane: Some(&running_pane),
                agent: Some(&running),
            }
            .receivable_now(
                &running_snapshot,
                &[],
                DeliveryGate::Done,
                false,
                timestamp
            )
        );

        let ask_snapshot = snapshot_with_ask(idle.clone(), pane.clone());
        assert!(
            !QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&ask_snapshot, &[], DeliveryGate::Done, false, timestamp)
        );
        assert!(
            QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(&ask_snapshot, &[], DeliveryGate::Done, true, timestamp)
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
            .receivable_now(
                &idle_snapshot,
                &[older],
                DeliveryGate::Done,
                false,
                timestamp
            )
        );
    }

    #[test]
    fn rendered_agent_handles_keep_single_sigil() {
        let mut coder = agent("sess-coder", AgentStatus::Idle);
        coder.role = Some("coder".to_owned());
        let snapshot = snapshot_with_panes(vec![coder], Vec::new());
        let target = QueueTarget {
            pane: None,
            agent: Some(&snapshot.agents[0]),
        };
        assert_eq!(handle_for_target(&snapshot, &target), "@coder#project");

        let message = MessageRecord::new(
            workspace_id(),
            &snapshot.agents[0],
            "work".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let message = MessageListRow::from_record(message);
        let agents: Vec<&AgentState> = snapshot.agents.iter().collect();
        assert_eq!(message_target(&message, &agents), "@coder#project");
    }

    #[test]
    fn mux_timeout_detection_walks_error_context() {
        let err = anyhow::Error::new(MuxErr::Timeout {
            program: "tmux".to_owned(),
            args: "send-keys %1".to_owned(),
            seconds: 30,
        })
        .context("sending prompt");

        assert!(is_mux_timeout(&err));
        assert!(!is_mux_timeout(&anyhow::anyhow!("ordinary failure")));
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
            channel: None,
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
            origin: None,
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
            channel: None,
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
            channel: None,
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
