//! `rimz message` — parse command input, call message domain, and render output.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use serde::Serialize;

use super::send::{self, SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::SidebarSnapshot;
use rimz::agents::AgentState;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, PaneId};
use rimz::ledger::event::{EventEnvelope, EventKind, MessageEventPayload};
use rimz::message::dispatch::{AddContext, AddOutput, AddSpec, SteerContext, SteerSpec};
use rimz::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus,
    card_matches, parse_schedule_at,
};
use rimz::message::{deliver, dispatch};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

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
        /// Max rows to show, newest first. 0 lists all.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Optional target filter.
        target: Option<String>,
    },
    /// Show one message record with timeline and delivery diagnosis.
    #[command(alias = "status")]
    Show {
        message_id: MessageId,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove queued messages.
    Remove {
        #[arg(value_name = "MESSAGE_ID", num_args = 1..)]
        message_ids: Vec<MessageId>,
    },
    /// Remove queued messages for an agent, or in the scoped channel.
    Clear {
        target: Option<String>,
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
            limit,
            target,
        }) => list_messages(json, all, status, channel, limit, target, globals),
        Some(MessageSubcmd::Show { message_id, json }) => show_message(message_id, json, globals),
        Some(MessageSubcmd::Remove { message_ids }) => remove_messages(message_ids, globals),
        Some(MessageSubcmd::Clear {
            target,
            worktree,
            channel,
        }) => clear_messages(target, worktree, channel, globals),
        Some(MessageSubcmd::Deliver { message_id }) => deliver_message(message_id, globals),
        Some(MessageSubcmd::Sweep) => sweep_messages(globals),
        None => {
            let Some(target) = args.target else {
                return list_messages(false, false, None, None, None, None, globals);
            };
            if !target.starts_with('@') && !target.contains(':') {
                if target.starts_with("msg_") {
                    bail!("did you mean `rimz message show {target}`?");
                }
                bail!(
                    "unknown subcommand `{target}`; expected list, show <id>, remove <id>..., clear [target], or an @agent target"
                );
            }
            let text = args.text.into_iter().collect();
            if args.steer {
                steer_message(target, args.send, text, globals)
            } else {
                message_add(target, args.on, args.schedule, args.send, text, globals)
            }
        }
    }
}

const DEFAULT_MESSAGE_LIST_LIMIT: usize = 200;

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
    rimz::harness::target::require_mention(&target)?;
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
        && let Ok(runtime) = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
    {
        snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    }
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), no_from);
    let durable_agents = dispatch::durable_target_agents(&ledger)?;
    let targets = match dispatch::queue_targets(
        &snapshot,
        Some(&durable_agents),
        &target,
        worktree.as_deref().or(channel_flag.as_deref()),
        channel.as_deref(),
        false,
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
        Err(err) => {
            record_resolution_bounce(
                &ledger,
                &workspace,
                &target,
                channel.as_deref(),
                &sender,
                text.len(),
                &err,
            )?;
            let err = map_queue_target_err(&target, err);
            return message_miss(&snapshot, channel.as_deref(), &err);
        }
    };

    if targets.len() > 1 && !all && !rimz::harness::target::is_broadcast(&target) {
        let labels: Vec<String> = targets.iter().map(dispatch::QueueTarget::label).collect();
        return Err(super::ambiguous_fanout("message --steer", &target, &labels));
    }
    let text = if targets.len() > 1 || rimz::harness::target::is_broadcast(&target) {
        rimz::harness::target::group_prefixed(&target, &text)
    } else {
        text
    };
    let wait = if let Some(timeout) = wait {
        Some((timeout, ledger.wait_fold_base()?))
    } else {
        None
    };
    let result = dispatch::steer_for_targets(
        SteerContext {
            workspace: &workspace,
            ledger: &ledger,
            snapshot: &snapshot,
            scope_channel: channel.as_deref(),
            sender: &sender,
        },
        &targets,
        &text,
        SteerSpec {
            enter: !no_enter,
            force,
            auto_compact,
        },
    )?;

    report_steer(
        &ledger,
        &workspace.session_name,
        wait,
        &target,
        targets.len(),
        &result.outcomes,
        &result.compacted,
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
        .root_agents()
        .filter(|agent| {
            channel.is_none_or(|filter| rimz::harness::target::agent_in_worktree(agent, filter))
        })
        .collect();
    if agents.is_empty() {
        writeln!(out, "no agents are running")?;
    } else {
        writeln!(out, "available agents:")?;
        super::agents_cmd::render_agents_table(
            &mut out,
            snapshot,
            &agents,
            Timestamp::now(),
            render::terminal_columns(120),
        )?;
    }
    out.flush().ok();
    std::process::exit(1);
}

fn map_queue_target_err(target: &str, err: rimz::TargetErr) -> anyhow::Error {
    let mapped: Result<()> = super::map_resolve(target, Err(err.clone()));
    match mapped {
        Ok(_) => unreachable!("mapping an error cannot succeed"),
        Err(mapped) => mapped,
    }
}

fn record_resolution_bounce(
    ledger: &rimz::Ledger,
    workspace: &ResolvedWorkspace,
    target: &str,
    channel: Option<&str>,
    sender: &MessageSender,
    text_len: usize,
    err: &rimz::TargetErr,
) -> Result<()> {
    if !matches!(
        err,
        rimz::TargetErr::NoMatch { .. }
            | rimz::TargetErr::NoMatchInChannel { .. }
            | rimz::TargetErr::PaneUnbound { .. }
    ) {
        return Ok(());
    }
    ledger.record_unresolved_message(rimz::ledger::UnresolvedMessage {
        workspace_id: workspace.workspace_id.clone(),
        session_name: &workspace.session_name,
        address: target,
        channel,
        sender,
        text_len,
        reason: "receiver not found",
    })?;
    Ok(())
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

#[derive(Clone, Debug, Serialize)]
struct MessageListRow {
    message_id: MessageId,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
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
            address: None,
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
            address: payload.address,
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
        card_matches(
            &self.kind,
            &self.agent_id,
            self.agent_name.as_deref(),
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
        )
    }
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
    rimz::harness::target::require_mention(&target)?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let mut snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), spec.no_from);
    let mut pending = ledger.list_pending_messages()?;
    let rollup_only = dispatch::rollup_targets_all_park_without_live(
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
    let durable_agents = dispatch::durable_target_agents(&ledger)?;
    let targets = match super::map_resolve(
        &target,
        dispatch::queue_targets(
            &snapshot,
            Some(&durable_agents),
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
            rollup_only,
        ),
    ) {
        Ok(targets) => targets,
        Err(err) => {
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
            if let Some(target_err) = err.downcast_ref::<rimz::TargetErr>() {
                record_resolution_bounce(
                    &ledger,
                    &workspace,
                    &target,
                    channel.as_deref(),
                    &sender,
                    text.len(),
                    target_err,
                )?;
            }
            return message_miss(&snapshot, channel.as_deref(), &err);
        }
    };
    if targets.len() > 1 && !flags.all && !rimz::harness::target::is_broadcast(&target) {
        let labels: Vec<String> = targets.iter().map(dispatch::QueueTarget::label).collect();
        return Err(super::ambiguous_fanout("deliver to", &target, &labels));
    }
    let text = if targets.len() > 1 || rimz::harness::target::is_broadcast(&target) {
        rimz::harness::target::group_prefixed(&target, &text)
    } else {
        text
    };
    let wait_base = if spec.wait.is_some() {
        Some(ledger.wait_fold_base()?)
    } else {
        None
    };
    let result = dispatch::add_for_targets(
        AddContext {
            workspace: &workspace,
            ledger: &ledger,
            snapshot: &snapshot,
            pending: &mut pending,
            scope_channel: channel.as_deref(),
            sender: &sender,
        },
        &targets,
        &text,
        AddSpec {
            enter: spec.enter,
            gate: spec.gate,
            force: spec.force,
            auto_compact: spec.auto_compact,
            not_before: spec.not_before,
            stamp_channel: spec.stamp_channel,
        },
    )?;
    for label in &result.compacted {
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("compacted {label}");
        }
    }
    let mut failed = false;
    let wait_deadline = spec.wait.map(|timeout| std::time::Instant::now() + timeout);
    for output in &result.outputs {
        if let Some(deadline) = wait_deadline {
            if !wait_and_print_message(
                &ledger,
                &workspace.session_name,
                &output.label,
                &output.message_id,
                wait_base.unwrap_or(0),
                deadline,
            )? {
                failed = true;
            }
        } else {
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("{}", render_add_output(output, output.status));
            }
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
    limit: Option<usize>,
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
        messages.retain(|message| {
            message
                .channel
                .as_deref()
                .is_some_and(|channel| rimz::harness::target::channel_in_lane(channel, filter))
        });
    }
    if let Some(status) = status {
        messages.retain(|message| message.status == status);
    } else if !all {
        messages.retain(|message| message.status != MessageStatus::Archived);
    }
    if let Some(raw) = target {
        rimz::harness::target::require_mention(&raw)?;
        let agent = super::resolve_agent_one(&snapshot, &raw, None, filter_channel)?;
        messages.retain(|message| message.same_agent_card(agent));
    }
    messages.sort_by(|a, b| {
        b.enqueued_at
            .cmp(&a.enqueued_at)
            .then_with(|| b.message_id.as_str().cmp(a.message_id.as_str()))
    });
    let limit = limit.unwrap_or(DEFAULT_MESSAGE_LIST_LIMIT);
    let hidden = if limit == 0 {
        0
    } else {
        messages.len().saturating_sub(limit)
    };
    if limit != 0 {
        messages.truncate(limit);
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
        let agents: Vec<&AgentState> = snapshot.root_agents().collect();
        let now = Timestamp::now();
        let show_channel = filter_channel.is_none();
        let mut headers = vec![
            "ID",
            "FROM",
            "TO",
            "STATUS",
            "CREATED",
            "DELIVERED",
            "MESSAGE",
        ];
        if show_channel {
            headers.insert(3, "CHANNEL");
        }
        let mut table = render::Table::new(headers);
        for message in messages {
            let target = scoped_handle(message_target(&message, &agents), filter_channel);
            let sender = scoped_handle(message.sender.render(), filter_channel);
            let message_cell = message_cell(&message);
            let mut row = vec![
                render::cell(message.message_id.to_string()).fg(render::palette::ACCENT),
                render::cell(sender).fg(render::palette::META),
                render::cell(target).fg(render::palette::META),
                render::cell(message.status.as_str()).fg(render::status::message(message.status)),
                render::cell(render::rel_age(message.enqueued_at, now)),
                render::cell(
                    message
                        .delivered_at
                        .map(|delivered| render::rel_age(delivered, now))
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
                message_cell,
            ];
            if show_channel {
                row.insert(
                    3,
                    render::cell(message.channel.as_deref().unwrap_or("-"))
                        .fg(render::palette::META)
                        .dash(),
                );
            }
            table.row(row);
        }
        let mut out = render::out();
        table.render(&mut out)?;
        if hidden > 0 {
            writeln!(
                out,
                "... {hidden} older messages hidden (--limit 0 for all)"
            )?;
        }
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
    for message in ledger.list_message_history()? {
        let row = MessageListRow::from_record(message);
        rows.insert(row.message_id.to_string(), row);
    }
    for message in ledger.list_messages()? {
        let row = MessageListRow::from_record(message);
        rows.insert(row.message_id.to_string(), row);
    }
    Ok(rows.into_values().collect())
}

#[derive(Clone, Debug, Serialize)]
struct MessageTimelineRow {
    method: String,
    at: Timestamp,
    attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct MessageDeliveryJson {
    check: deliver::DeliveryCheck,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct MessageShowJson {
    message: MessageListRow,
    timeline: Vec<MessageTimelineRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<MessageDeliveryJson>,
}

fn show_message(message_id: MessageId, json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let cached_snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    let Some(message) = projected_messages(&ledger)?
        .into_iter()
        .find(|message| message.message_id == message_id)
    else {
        bail!("message {message_id} not found");
    };
    let timeline = message_timeline(&ledger, &message_id)?;
    let live_messages = ledger.list_messages()?;
    let now = Timestamp::now();
    let delivery = if message.status.is_open() {
        match live_messages
            .iter()
            .find(|record| record.message_id == message.message_id)
        {
            Some(record) => {
                let mut snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
                if let Ok(runtime) = rimz::RuntimePaths::for_workspace(record.workspace_id.clone())
                {
                    snapshot = snapshot
                        .with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
                }
                let check = deliver::explain(record, &live_messages, &snapshot, now);
                let agents: Vec<&AgentState> = snapshot.root_agents().collect();
                let target = message_target(&message, &agents);
                Some(MessageDeliveryJson {
                    verdict: delivery_verdict(&check, &target, now),
                    check,
                })
            }
            None => None,
        }
    } else {
        None
    };
    if json {
        let rendered = serde_json::to_string_pretty(&MessageShowJson {
            message,
            timeline,
            delivery,
        })?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }

    let agents: Vec<&AgentState> = cached_snapshot.root_agents().collect();
    let raw_target = message_target(&message, &agents);
    let target = scoped_handle(raw_target.clone(), message.channel.as_deref());
    let sender = scoped_handle(message.sender.render(), message.channel.as_deref());
    let mut out = render::out();
    writeln!(
        out,
        "{} — {}",
        render::paint(render::palette::ACCENT.bold(), message.message_id.as_str()),
        render::paint(
            render::status::message(message.status),
            message.status.as_str()
        )
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("from", render::cell(sender).fg(render::palette::META));
    kv.push("to", render::cell(target.clone()).fg(render::palette::META));
    kv.push(
        "channel",
        render::cell(message.channel.clone().unwrap_or_else(|| "-".to_owned())).dash(),
    );
    if message.body != MessageBody::Prompt {
        kv.push("body", render::cell(message.body.as_str()));
    }
    if message.gate != DeliveryGate::Done {
        kv.push("gate", render::cell(message.gate.as_str()));
    }
    if !message.enter {
        kv.push("enter", render::cell("false"));
    }
    if message.force {
        kv.push("force", render::cell("true"));
    }
    kv.push(
        "created",
        render::cell(time_with_absolute(message.enqueued_at, now)),
    );
    if let Some(delivered) = message.delivered_at {
        kv.push(
            "delivered",
            render::cell(time_with_absolute(delivered, now)),
        );
    }
    if let Some(not_before) = message.not_before {
        kv.push(
            "schedule",
            render::cell(time_until_with_absolute(not_before, now)),
        );
    }
    if message.attempts > 0 {
        kv.push("attempts", render::cell(message.attempts.to_string()));
    }
    if message.unconfirmed_sends > 0 {
        kv.push(
            "unconfirmed_sends",
            render::cell(message.unconfirmed_sends.to_string()),
        );
    }
    if let Some(last_error) = message.last_error.as_deref() {
        kv.push("last_error", render::cell(last_error));
    }
    kv.render(&mut out)?;
    writeln!(out)?;
    writeln!(out, "{}", render::paint(render::palette::HEADER, "TEXT"))?;
    if let Some(text) = message.text.as_deref() {
        write_indented_block(&mut out, text)?;
    } else {
        writeln!(out, "  ({})", textless_location(&message, &raw_target))?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::HEADER, "TIMELINE")
    )?;
    if timeline.is_empty() {
        writeln!(out, "  -")?;
    } else {
        let show_attempts = timeline.iter().any(|event| event.attempts > 0);
        let show_note = timeline.iter().any(|event| {
            event
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty())
        });
        let mut headers = vec!["EVENT", "WHEN"];
        if show_attempts {
            headers.push("ATTEMPT");
        }
        if show_note {
            headers.push("NOTE");
        }
        let mut table = render::Table::new(headers).indent(2);
        for event in &timeline {
            let label = event
                .method
                .strip_prefix("message.")
                .unwrap_or(&event.method);
            let mut row = vec![
                render::cell(label.to_owned()),
                render::cell(time_with_absolute(event.at, now)),
            ];
            if show_attempts {
                row.push(render::cell(event.attempts.to_string()));
            }
            if show_note {
                let reason = event
                    .reason
                    .as_deref()
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("-");
                row.push(render::cell(reason).dash());
            }
            table.row(row);
        }
        table.render(&mut out)?;
    }
    if let Some(delivery) = delivery {
        render_delivery_check(&mut out, &delivery.check, &delivery.verdict, now)?;
    }
    Ok(())
}

fn message_timeline(
    ledger: &rimz::Ledger,
    message_id: &MessageId,
) -> Result<Vec<MessageTimelineRow>> {
    let mut rows = Vec::new();
    for event in ledger.read_events()? {
        let EventKind::Message { method, payload } = event.kind() else {
            continue;
        };
        if payload.message_id != *message_id {
            continue;
        }
        rows.push(MessageTimelineRow {
            method: method.as_str().to_owned(),
            at: event.timestamp,
            attempts: payload.attempts,
            reason: payload.reason,
        });
    }
    Ok(rows)
}

fn remove_messages(message_ids: Vec<MessageId>, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let mut failed = false;
    for message_id in message_ids {
        if ledger.remove_message(&message_id, &workspace.session_name, "remove")? {
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("removed {message_id}");
            }
        } else {
            failed = true;
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("{message_id} is not queued or claimed");
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn clear_messages(
    target: Option<String>,
    worktree: Option<String>,
    channel_flag: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let (workspace, ledger, snapshot) = workspace_ledger_snapshot(globals)?;
    let channel = current_channel(&workspace);
    if let Some(target) = target {
        rimz::harness::target::require_mention(&target)?;
        let agent = super::resolve_agent_one(
            &snapshot,
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
        )?;
        let removed = ledger.clear_messages_for(
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
            &workspace.session_name,
        )?;
        print_removed_summary(&format!("for {target}"), &removed);
        return Ok(());
    }
    let lane = worktree
        .as_deref()
        .or(channel_flag.as_deref())
        .or(channel.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "message clear needs an @agent target or scoped channel; pass --channel NAME or run from a Rimz channel"
            )
        })?;
    let removed = ledger.clear_channel_messages(lane, &workspace.session_name)?;
    print_removed_summary(&format!("in #{lane}"), &removed);
    Ok(())
}

fn print_removed_summary(scope: &str, removed: &[MessageRecord]) {
    let ids: Vec<String> = removed
        .iter()
        .map(|message| message.message_id.to_string())
        .collect();
    #[expect(clippy::print_stdout, reason = "final user-facing message")]
    {
        if ids.is_empty() {
            println!("removed 0 message(s) {scope}");
        } else {
            println!(
                "removed {} message(s) {scope}: {}",
                ids.len(),
                ids.join(", ")
            );
        }
    }
}

fn render_add_output(output: &AddOutput, status: MessageStatus) -> String {
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

fn wait_and_print_message(
    ledger: &rimz::Ledger,
    session_name: &str,
    label: &str,
    message_id: &MessageId,
    wait_base: u64,
    deadline: std::time::Instant,
) -> Result<bool> {
    let status =
        send::wait_for_message_until(ledger, message_id, session_name, wait_base, deadline)?;
    #[expect(clippy::print_stdout, reason = "wait status")]
    {
        println!("{} {label} ({message_id})", wait_status_label(status));
    }
    Ok(status == MessageStatus::Delivered)
}

/// Report a `--steer` fan-out. A lone agent that was skipped fails with the
/// same message the single-target path always returned; a broadcast prints its
/// sent/skipped summary and succeeds.
fn report_steer(
    ledger: &rimz::Ledger,
    session_name: &str,
    wait: Option<(Duration, u64)>,
    target: &str,
    total: usize,
    outcomes: &[dispatch::SteerOutcome],
    compacted: &[String],
) -> Result<()> {
    let sent = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            dispatch::SteerOutcome::Sent { label, message_id } => {
                Some(format!("{label} ({message_id})"))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let sent_labels = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            dispatch::SteerOutcome::Sent { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let queued = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            dispatch::SteerOutcome::Queued { label, message_id } => {
                Some(format!("{label} ({message_id})"))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let pending = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            dispatch::SteerOutcome::SkippedPending {
                label, message_id, ..
            } => Some(format!("{label} ({message_id})")),
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some((timeout, wait_base)) = wait {
        let mut failed = false;
        let deadline = std::time::Instant::now() + timeout;
        for outcome in outcomes {
            let (label, message_id, compactable) = match outcome {
                dispatch::SteerOutcome::Sent { label, message_id } => (label, message_id, true),
                dispatch::SteerOutcome::Queued { label, message_id } => (label, message_id, false),
                dispatch::SteerOutcome::SkippedPending { .. } => continue,
            };
            if compactable {
                print_compacted_if_needed(label, compacted);
            }
            if !wait_and_print_message(
                ledger,
                session_name,
                label,
                message_id,
                wait_base,
                deadline,
            )? {
                failed = true;
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
        if !queued.is_empty() {
            #[expect(clippy::print_stdout, reason = "message confirmation")]
            {
                println!("queued for {}", queued[0]);
            }
            return Ok(());
        }
        match outcomes.first() {
            Some(dispatch::SteerOutcome::SkippedPending {
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
    if !queued.is_empty() {
        line.push_str(&format!("; queued: {}", queued.join(", ")));
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
    deliver::deliver_one(
        &workspace,
        &ledger,
        &message_id,
        rimz::message::settle_duration_from_env(),
        globals.mux,
    )?;
    Ok(())
}

fn sweep_messages(globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    deliver::sweep(&workspace, &ledger, globals.mux)?;
    Ok(())
}

fn workspace_ledger_snapshot(
    globals: &GlobalFlags,
) -> Result<(ResolvedWorkspace, rimz::Ledger, rimz::SidebarSnapshot)> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
    Ok((workspace, ledger, snapshot))
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
    if let Some(address) = &message.address {
        return address.clone();
    }
    agents
        .iter()
        .copied()
        .find(|agent| message.same_agent_card(agent))
        .map(|agent| rimz::harness::target::agent_handle(agent, agents, true))
        .unwrap_or_else(|| format!("{}:{}", message.kind, message.agent_id))
}

fn scoped_handle(rendered: String, filter_channel: Option<&str>) -> String {
    let Some(filter) = filter_channel else {
        return rendered;
    };
    let Some((base, channel)) = rendered.rsplit_once('#') else {
        return rendered;
    };
    if rimz::harness::target::channel_in_lane(channel, filter) {
        base.to_owned()
    } else {
        rendered
    }
}

fn message_cell(message: &MessageListRow) -> render::Cell {
    if let Some(text) = message.text.as_deref() {
        return render::cell(preview(text));
    }
    if let Some(reason) = message
        .last_error
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        return render::cell(reason.to_owned()).fg(render::palette::FAINT);
    }
    render::cell("-").dash()
}

fn time_with_absolute(ts: Timestamp, now: Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%dT%H:%M:%SZ");
    format!("{} ({absolute})", render::rel_age(ts, now))
}

fn time_until_with_absolute(ts: Timestamp, now: Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%dT%H:%M:%SZ");
    format!("{} ({absolute})", render::rel_until(ts, now))
}

fn write_indented_block(out: &mut impl Write, text: &str) -> Result<()> {
    if text.is_empty() {
        writeln!(out, "  ")?;
        return Ok(());
    }
    for line in text.split('\n') {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

fn textless_location(message: &MessageListRow, target: &str) -> String {
    if let Some(reason) = message
        .last_error
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        return format!("content not retained in the event log; {reason}");
    }
    if message.status == MessageStatus::Delivered {
        format!("content in `rimz transcript {target}`")
    } else {
        "content not retained in the event log".to_owned()
    }
}

fn render_delivery_check(
    out: &mut impl Write,
    check: &deliver::DeliveryCheck,
    verdict: &str,
    now: Timestamp,
) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::HEADER, "DELIVERY CHECK")
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    let schedule = if check.schedule.ready {
        match check.schedule.retry_after {
            Some(retry_after) if retry_after > now => {
                format!(
                    "ok; retry wake {}",
                    time_until_with_absolute(retry_after, now)
                )
            }
            Some(retry_after) => format!("ok; retry wake {}", time_with_absolute(retry_after, now)),
            None => "ok".to_owned(),
        }
    } else {
        check
            .schedule
            .not_before
            .map(|not_before| format!("opens {}", time_until_with_absolute(not_before, now)))
            .unwrap_or_else(|| "not ready".to_owned())
    };
    kv.push("schedule", condition_cell(check.schedule.ready, schedule));
    let fifo = if check.fifo.head {
        "ok".to_owned()
    } else {
        check
            .fifo
            .blocker
            .as_ref()
            .map(|blocker| format!("behind {blocker}"))
            .unwrap_or_else(|| "head unavailable".to_owned())
    };
    kv.push("fifo", condition_cell(check.fifo.head, fifo));
    kv.push(
        "agent",
        condition_cell(
            check.agent.present,
            if check.agent.present {
                "ok".to_owned()
            } else {
                "receiver gone".to_owned()
            },
        ),
    );
    let gate_ready = gate_ready(check);
    let gate = if check.gate.resume_recovered == Some(false) {
        "waiting for provider recovery".to_owned()
    } else if check.gate.open {
        match check.gate.status {
            Some(status) => format!("ok (status {})", status.as_str()),
            None => "ok".to_owned(),
        }
    } else {
        match check.gate.status {
            Some(status) => format!(
                "closed (status {}, gate {})",
                status.as_str(),
                check.gate.gate
            ),
            None => format!("closed (gate {})", check.gate.gate),
        }
    };
    kv.push("gate", condition_cell(gate_ready, gate));
    let ask = if check.ask.clear {
        if check.ask.force {
            "ok (--force)".to_owned()
        } else {
            "ok".to_owned()
        }
    } else {
        check
            .ask
            .request_id
            .as_ref()
            .map(|request_id| format!("pending ask {request_id}"))
            .unwrap_or_else(|| "pending ask".to_owned())
    };
    kv.push("ask", condition_cell(check.ask.clear, ask));
    let pane = if check.pane.present {
        check
            .pane
            .pane_id
            .as_ref()
            .map(|pane_id| format!("ok ({pane_id})"))
            .unwrap_or_else(|| "ok".to_owned())
    } else if let Some(pane_id) = &check.pane.pinned_pane_id {
        format!("pinned pane {pane_id} not live")
    } else {
        "no live pane".to_owned()
    };
    kv.push("pane", condition_cell(check.pane.present, pane));
    kv.render(out)?;
    writeln!(out, "  {verdict}")?;
    Ok(())
}

fn condition_cell(ok: bool, text: String) -> render::Cell {
    let style = if ok {
        render::palette::GOOD
    } else {
        render::palette::WARN
    };
    render::cell(text).fg(style)
}

fn delivery_verdict(check: &deliver::DeliveryCheck, target: &str, now: Timestamp) -> String {
    if !check.schedule.ready {
        return check
            .schedule
            .not_before
            .map(|not_before| {
                format!(
                    "scheduled: opens {}",
                    time_until_with_absolute(not_before, now)
                )
            })
            .unwrap_or_else(|| "scheduled: waiting for readiness floor".to_owned());
    }
    if !check.fifo.head {
        return check
            .fifo
            .blocker
            .as_ref()
            .map(|blocker| format!("blocked: behind {blocker}"))
            .unwrap_or_else(|| "blocked: FIFO head unavailable".to_owned());
    }
    if !check.agent.present {
        return format!("stuck: receiver {target} is gone");
    }
    if !check.gate.open {
        let status = check
            .gate
            .status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        if check.gate.gate == DeliveryGate::Resume {
            return format!(
                "waiting: {target} is {status}; resume gate opens when the agent is paused and provider recovery passes"
            );
        }
        return format!(
            "waiting: {target} is {status}; gate '{}' opens at next turn end",
            check.gate.gate
        );
    }
    if check.gate.resume_recovered == Some(false) {
        return format!("waiting: {target} is paused; resume gate opens after provider recovery");
    }
    if let Some(request_id) = &check.ask.request_id {
        return format!("waiting: pending ask {request_id} reserves input");
    }
    if !check.pane.present {
        return match &check.pane.pinned_pane_id {
            Some(pane_id) => format!("stuck: pinned pane {pane_id} is not live for {target}"),
            None => format!("stuck: no live pane for {target}"),
        };
    }
    "ready: delivery conditions pass".to_owned()
}

fn gate_ready(check: &deliver::DeliveryCheck) -> bool {
    check.gate.open && check.gate.resume_recovered != Some(false)
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

    use rimz::agents::{AgentStatus, TurnPhase};
    use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
    use rimz::pane::PaneRef;

    #[test]
    fn message_target_keeps_single_sigil() {
        let mut coder = agent("sess-coder", AgentStatus::Idle);
        coder.role = Some("coder".to_owned());
        let snapshot =
            SidebarSnapshot::build_with_agents(workspace_id(), Vec::new(), vec![coder], now());
        let message = MessageRecord::new(
            workspace_id(),
            &snapshot.agents[0],
            "work".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let message = MessageListRow::from_record(message);
        let agents: Vec<&AgentState> = snapshot.root_agents().collect();
        assert_eq!(message_target(&message, &agents), "@coder#project");
    }

    #[test]
    fn scoped_handle_drops_matching_lane_suffix() {
        assert_eq!(
            scoped_handle("@coder#project".to_owned(), Some("project")),
            "@coder"
        );
        assert_eq!(
            scoped_handle("@coder#project/forge".to_owned(), Some("project")),
            "@coder"
        );
        assert_eq!(
            scoped_handle("@coder#ops".to_owned(), Some("project")),
            "@coder#ops"
        );
        assert_eq!(scoped_handle("you".to_owned(), Some("project")), "you");
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
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
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status,
            phase,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                "terminal_3",
            ))),
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
            last_compact_command_tokens: None,
            last_seen: timestamp,
            last_activity: timestamp,
            registered_at: Some(timestamp),
        }
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }
}
