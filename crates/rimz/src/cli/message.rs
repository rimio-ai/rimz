//! `rimz message` — parse command input, call message domain, and render output.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use serde::Serialize;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::address;
use super::send::{self, SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_store};
use crate::cli::render;
use rimz::SidebarSnapshot;
use rimz::agents::AgentState;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, PaneId};
use rimz::store::event::{EventEnvelope, EventKind, MessageEventPayload};
use rimz::store::{EditOutcome, MessageEdit};
use rimz::message::dispatch::{DispatchContext, DispatchOutcome, SendMode};
use rimz::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus,
    parse_schedule_at,
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
    /// Change a queued message.
    Edit {
        message_id: MessageId,
        #[command(flatten)]
        edit: EditFlags,
    },
    /// Deliver a queued message now.
    Steer {
        message_id: MessageId,
        /// Send even when the agent is Waiting.
        #[arg(long)]
        force: bool,
    },
    /// Queue a new copy of a finished message.
    Requeue {
        message_id: MessageId,
        #[command(flatten)]
        edit: EditFlags,
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

#[derive(Debug, Args)]
struct EditFlags {
    /// Replace the message text.
    #[arg(long, value_name = "TEXT", conflicts_with = "file")]
    text: Option<String>,
    /// Replace the message text from a file.
    #[arg(long, value_name = "PATH", conflicts_with = "text")]
    file: Option<PathBuf>,
    /// Deliver after a successful/idle turn (`done`) or after success/idle/failure (`any`).
    #[arg(long, value_parser = parse_gate)]
    on: Option<DeliveryGate>,
    /// Park the message until at least this duration or configured-zone `HH:MM`.
    #[arg(long, value_name = "DUR|HH:MM", conflicts_with = "no_schedule")]
    schedule: Option<String>,
    /// Clear the earliest-delivery floor.
    #[arg(long, conflicts_with = "schedule")]
    no_schedule: bool,
    /// Send even when the agent is Waiting.
    #[arg(long, conflicts_with = "no_force")]
    force: bool,
    /// Restore normal Waiting deferral.
    #[arg(long, conflicts_with = "force")]
    no_force: bool,
    /// Submit the message with Enter after paste.
    #[arg(long, conflicts_with = "no_enter")]
    enter: bool,
    /// Paste the text but leave it unsubmitted.
    #[arg(long, conflicts_with = "enter")]
    no_enter: bool,
    /// Compact first when context is at least this full.
    #[arg(long, value_name = "PCT|TOKENS", value_parser = AutoCompact::parse, conflicts_with = "no_smart_compact")]
    smart_compact: Option<AutoCompact>,
    /// Clear smart compact.
    #[arg(long, conflicts_with = "smart_compact")]
    no_smart_compact: bool,
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
        Some(MessageSubcmd::Edit { message_id, edit }) => edit_message(message_id, edit, globals),
        Some(MessageSubcmd::Steer { message_id, force }) => {
            steer_queued_message(message_id, force, globals)
        }
        Some(MessageSubcmd::Requeue { message_id, edit }) => {
            requeue_message(message_id, edit, globals)
        }
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
                    "unknown subcommand `{target}`; expected list, show <id>, edit <id>, steer <id>, requeue <id>, remove <id>..., clear [target], or an @agent target"
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

enum LaneScope {
    All,
    Main,
    Named(String),
}

impl LaneScope {
    fn named(&self) -> Option<&str> {
        match self {
            Self::Named(channel) => Some(channel),
            Self::All | Self::Main => None,
        }
    }

    fn includes_archived(&self) -> bool {
        matches!(self, Self::All)
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
    dispatch_message(
        target,
        worktree,
        channel,
        text,
        MessageDispatchMode::Boundary,
        MessageSpec {
            enter: !no_enter,
            gate,
            force,
            auto_compact,
            no_from,
            wait,
            not_before,
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
    dispatch_message(
        format!("@{session}"),
        None,
        None,
        text,
        MessageDispatchMode::Boundary,
        MessageSpec {
            enter: true,
            gate,
            force: false,
            auto_compact: None,
            no_from: false,
            wait: None,
            not_before: None,
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
    dispatch_message(
        target,
        worktree,
        channel_flag,
        text,
        MessageDispatchMode::Steer,
        MessageSpec {
            enter: !no_enter,
            gate: DeliveryGate::Any,
            force,
            auto_compact,
            no_from,
            wait,
            not_before: None,
        },
        FanoutFlags { all, create },
        globals,
    )
}

fn edit_from_flags(flags: EditFlags) -> Result<MessageEdit> {
    let EditFlags {
        text,
        file,
        on,
        schedule,
        no_schedule,
        force,
        no_force,
        enter,
        no_enter,
        smart_compact,
        no_smart_compact,
    } = flags;
    let text = match (text, file) {
        (Some(text), None) => Some(resolve_message(&[text], None)?),
        (None, Some(path)) => Some(resolve_message(&[], Some(path.as_path()))?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap enforces --text/--file conflicts"),
    };
    let machine_config = super::machine_config();
    let now = Timestamp::now().to_zoned(machine_config.time_zone());
    let not_before = if no_schedule {
        Some(None)
    } else {
        schedule
            .as_deref()
            .map(|raw| parse_schedule_at(raw, &now).map_err(anyhow::Error::msg))
            .transpose()?
            .map(Some)
    };
    let force = force.then_some(true).or_else(|| no_force.then_some(false));
    let enter = enter.then_some(true).or_else(|| no_enter.then_some(false));
    let auto_compact = smart_compact
        .map(Some)
        .or_else(|| no_smart_compact.then_some(None));
    Ok(MessageEdit {
        text,
        gate: on,
        not_before,
        force,
        enter,
        auto_compact,
    })
}

/// The fan-out / create flags shared by parked message delivery.
struct FanoutFlags {
    all: bool,
    create: bool,
}

#[derive(Clone, Copy)]
enum MessageDispatchMode {
    Steer,
    Boundary,
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
    store: &rimz::Store,
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
    store.record_unresolved_message(rimz::store::UnresolvedMessage {
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
/// whether to deliver past Waiting, and an optional compact-first threshold.
struct MessageSpec {
    enter: bool,
    gate: DeliveryGate,
    force: bool,
    auto_compact: Option<AutoCompact>,
    no_from: bool,
    wait: Option<Duration>,
    not_before: Option<Timestamp>,
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
            address: message.address,
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
}

#[allow(clippy::too_many_arguments)]
fn dispatch_message(
    target: String,
    worktree: Option<String>,
    channel_flag: Option<String>,
    text: String,
    mode: MessageDispatchMode,
    spec: MessageSpec,
    flags: FanoutFlags,
    globals: &GlobalFlags,
) -> Result<()> {
    rimz::harness::target::require_mention(&target)?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), spec.no_from);
    let mut pending = Vec::new();
    let rollup_only = match mode {
        MessageDispatchMode::Steer => false,
        MessageDispatchMode::Boundary => {
            pending = store.list_pending_messages()?;
            let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
            let rollup_only = dispatch::rollup_targets_all_park_without_live(
                &snapshot,
                &target,
                worktree.as_deref().or(channel_flag.as_deref()),
                channel.as_deref(),
                &pending,
                spec.gate,
                spec.force,
            );
            if rollup_only {
                let durable_agents = dispatch::durable_target_agents(&store)?;
                let Some(targets) = resolve_message_targets(
                    &store,
                    &workspace,
                    &snapshot,
                    &sender,
                    &target,
                    worktree.as_deref(),
                    channel_flag.as_deref(),
                    channel.as_deref(),
                    &text,
                    flags.create,
                    globals,
                    &durable_agents,
                    true,
                )?
                else {
                    return Ok(());
                };
                return dispatch_resolved_message(
                    mode, &workspace, &store, &snapshot, pending, &sender, target, text, spec,
                    flags, targets, channel,
                );
            }
            false
        }
    };
    let mut snapshot = super::resolution_snapshot(&workspace, &store, globals)?;
    // Smart compaction reads context fill. Immediate message sends share the
    // live path, so fold the disposable context sidecars before any send-now
    // decision that might compact first.
    if spec.auto_compact.is_some()
        && let Ok(runtime) = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
    {
        snapshot = snapshot.with_agent_context(rimz::store::agent_context::read_all(&runtime));
    }
    let durable_agents = dispatch::durable_target_agents(&store)?;
    let Some(targets) = resolve_message_targets(
        &store,
        &workspace,
        &snapshot,
        &sender,
        &target,
        worktree.as_deref(),
        channel_flag.as_deref(),
        channel.as_deref(),
        &text,
        flags.create,
        globals,
        &durable_agents,
        rollup_only,
    )?
    else {
        return Ok(());
    };
    dispatch_resolved_message(
        mode, &workspace, &store, &snapshot, pending, &sender, target, text, spec, flags, targets,
        channel,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_message_targets<'a>(
    store: &rimz::Store,
    workspace: &ResolvedWorkspace,
    snapshot: &'a SidebarSnapshot,
    sender: &MessageSender,
    target: &str,
    worktree: Option<&str>,
    channel_flag: Option<&str>,
    channel: Option<&str>,
    text: &str,
    create: bool,
    globals: &GlobalFlags,
    durable_agents: &'a [AgentState],
    rollup_only: bool,
) -> Result<Option<Vec<dispatch::QueueTarget<'a>>>> {
    match dispatch::queue_targets(
        snapshot,
        Some(durable_agents),
        target,
        worktree.or(channel_flag),
        channel,
        rollup_only,
    ) {
        Ok(targets) => Ok(Some(targets)),
        Err(err) => {
            // Create-on-miss launches a fresh agent with this text as its first
            // prompt, so the launch carries the work and no message record is made.
            if create {
                return super::agents_cmd::create_on_miss(
                    target,
                    worktree,
                    channel_flag,
                    channel,
                    text,
                    globals,
                )
                .map(|()| None);
            }
            record_resolution_bounce(store, workspace, target, channel, sender, text.len(), &err)?;
            let err = map_queue_target_err(target, err);
            message_miss(snapshot, channel, &err).map(|()| None)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_resolved_message(
    mode: MessageDispatchMode,
    workspace: &ResolvedWorkspace,
    store: &rimz::Store,
    snapshot: &SidebarSnapshot,
    mut pending: Vec<MessageRecord>,
    sender: &MessageSender,
    target: String,
    text: String,
    spec: MessageSpec,
    flags: FanoutFlags,
    targets: Vec<dispatch::QueueTarget<'_>>,
    channel: Option<String>,
) -> Result<()> {
    if targets.len() > 1 && !flags.all && !rimz::harness::target::is_broadcast(&target) {
        let labels: Vec<String> = targets
            .iter()
            .map(|target| target.label(snapshot))
            .collect();
        let verb = match mode {
            MessageDispatchMode::Steer => "message --steer",
            MessageDispatchMode::Boundary => "deliver to",
        };
        return Err(super::ambiguous_fanout(verb, &target, &labels));
    }
    let text = if targets.len() > 1 || rimz::harness::target::is_broadcast(&target) {
        rimz::harness::target::group_prefixed(&target, &text)
    } else {
        text
    };
    let wait_base = if spec.wait.is_some() {
        Some(store.wait_fold_base()?)
    } else {
        None
    };
    let result = dispatch::dispatch_for_targets(
        DispatchContext {
            workspace,
            store,
            snapshot,
            pending: matches!(mode, MessageDispatchMode::Boundary).then_some(&mut pending),
            scope_channel: channel.as_deref(),
            sender,
        },
        &targets,
        &text,
        match mode {
            MessageDispatchMode::Steer => SendMode::Steer {
                enter: spec.enter,
                force: spec.force,
                auto_compact: spec.auto_compact,
            },
            MessageDispatchMode::Boundary => SendMode::Boundary {
                enter: spec.enter,
                gate: spec.gate,
                force: spec.force,
                auto_compact: spec.auto_compact,
                not_before: spec.not_before,
            },
        },
    )?;
    let wait = spec.wait.map(|timeout| (timeout, wait_base.unwrap_or(0)));
    report_dispatch(
        match mode {
            MessageDispatchMode::Steer => ReportMode::Steer,
            MessageDispatchMode::Boundary => ReportMode::Boundary,
        },
        store,
        &workspace.session_name,
        wait,
        &target,
        targets.len(),
        &result.outcomes,
        &result.compacted,
    )
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
    let (workspace, store, snapshot) = workspace_store_snapshot(globals)?;
    let mut messages = projected_messages(&store)?;
    let ambient_channel = current_channel(&workspace);
    let lane_scope = if all {
        LaneScope::All
    } else if let Some(channel) = channel {
        LaneScope::Named(channel)
    } else if let Some(channel) = ambient_channel {
        LaneScope::Named(channel)
    } else {
        LaneScope::Main
    };
    match &lane_scope {
        LaneScope::All => {}
        LaneScope::Main => messages.retain(|message| message.channel.is_none()),
        LaneScope::Named(channel) => {
            messages.retain(|message| message.channel.as_deref() == Some(channel.as_str()));
        }
    }
    if let Some(status) = status {
        messages.retain(|message| message.status == status);
    } else if !lane_scope.includes_archived() {
        messages.retain(|message| message.status != MessageStatus::Archived);
    }
    if let Some(raw) = target {
        rimz::harness::target::require_mention(&raw)?;
        let agent = super::resolve_agent_one(&snapshot, &raw, None, lane_scope.named())?;
        messages.retain(|message| {
            rimz::message::card_matches(
                &message.kind,
                &message.agent_id,
                message.agent_name.as_deref(),
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
            )
        });
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
        let agents: Vec<&AgentState> = snapshot.root_agents().collect();
        let mut out = render::out();
        render_message_digest(&mut out, messages, &agents, &lane_scope, hidden, status)?;
    }
    Ok(())
}

fn projected_messages(store: &rimz::Store) -> Result<Vec<MessageListRow>> {
    let mut rows = std::collections::BTreeMap::new();
    for event in store.read_events()? {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        let Some(row) = MessageListRow::from_terminal_event(&event, payload) else {
            continue;
        };
        rows.insert(row.message_id.to_string(), row);
    }
    for message in store.list_message_history()? {
        let row = MessageListRow::from_record(message);
        rows.insert(row.message_id.to_string(), row);
    }
    for message in store.list_messages()? {
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
    let store = open_store(&workspace)?;
    let cached_snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let Some(message) = projected_messages(&store)?
        .into_iter()
        .find(|message| message.message_id == message_id)
    else {
        bail!("message {message_id} not found");
    };
    let timeline = message_timeline(&store, &message_id)?;
    let live_messages = store.list_messages()?;
    let now = Timestamp::now();
    let delivery = if message.status.is_open() {
        match live_messages
            .iter()
            .find(|record| record.message_id == message.message_id)
        {
            Some(record) => {
                let mut snapshot = super::resolution_snapshot(&workspace, &store, globals)?;
                if let Ok(runtime) = rimz::RuntimePaths::for_workspace(record.workspace_id.clone())
                {
                    snapshot = snapshot
                        .with_agent_context(rimz::store::agent_context::read_all(&runtime));
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
        render_delivery_check(
            &mut out,
            &message.message_id,
            &delivery.check,
            &delivery.verdict,
            now,
        )?;
    }
    Ok(())
}

fn message_timeline(
    store: &rimz::Store,
    message_id: &MessageId,
) -> Result<Vec<MessageTimelineRow>> {
    let mut rows = Vec::new();
    for event in store.read_events()? {
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

fn edit_message(message_id: MessageId, flags: EditFlags, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let edit = edit_from_flags(flags)?;
    if edit.is_empty() {
        bail!("nothing to edit; pass --text, --file, --on, --schedule, or another edit flag");
    }
    let fields = edit.changed_fields();
    match store.edit_message(&message_id, edit, &workspace.session_name)? {
        EditOutcome::Edited(_) => {
            deliver::register_message_wake(&workspace, &store)?;
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("edited {message_id} ({})", fields.join(", "));
            }
            Ok(())
        }
        EditOutcome::NotOpen(MessageStatus::Claimed) => {
            bail!("{message_id} delivery in progress; retry in a moment")
        }
        EditOutcome::NotOpen(status) if status.is_terminal() => {
            bail!("{message_id} is {status}; use `rimz message requeue {message_id}`")
        }
        EditOutcome::NotOpen(status) => {
            bail!("{message_id} is {status}; only queued messages can be edited")
        }
        EditOutcome::NotFound => bail!("message {message_id} not found"),
    }
}

fn steer_queued_message(message_id: MessageId, force: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let messages = store.list_messages()?;
    let Some(record) = messages
        .iter()
        .find(|record| record.message_id == message_id)
    else {
        if let Some(history) = store
            .list_message_history()?
            .into_iter()
            .find(|record| record.message_id == message_id)
        {
            bail!(
                "{message_id} is {}; use `rimz message requeue {message_id}`",
                history.status
            );
        }
        bail!("message {message_id} not found");
    };
    match record.status {
        MessageStatus::Queued => {}
        MessageStatus::Claimed => bail!("{message_id} delivery in progress; retry in a moment"),
        status if status.is_terminal() => {
            bail!("{message_id} is {status}; use `rimz message requeue {message_id}`")
        }
        status => bail!("{message_id} is {status}; only queued messages can be steered"),
    }
    let mut snapshot = super::resolution_snapshot(&workspace, &store, globals)?;
    if let Ok(runtime) = rimz::RuntimePaths::for_workspace(record.workspace_id.clone()) {
        snapshot = snapshot.with_agent_context(rimz::store::agent_context::read_all(&runtime));
    }
    let label = message_target_for_record(record, &snapshot);
    let delivered = deliver::deliver_one(
        &workspace,
        &store,
        &message_id,
        Duration::ZERO,
        globals.mux,
        deliver::DeliveryPolicy::Steer { force },
    )?;
    if delivered {
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("sent to {label} ({message_id})");
        }
        return Ok(());
    }
    let messages = store.list_messages()?;
    let Some(record) = messages
        .iter()
        .find(|record| record.message_id == message_id)
    else {
        bail!("message {message_id} is no longer queued");
    };
    let check = deliver::explain(record, &messages, &snapshot, Timestamp::now());
    bail!("{}", steer_failure(&check, &label, &message_id))
}

fn requeue_message(message_id: MessageId, flags: EditFlags, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let record = if let Some(record) = store
        .list_message_history()?
        .into_iter()
        .find(|record| record.message_id == message_id)
    {
        record
    } else if let Some(record) = store
        .list_messages()?
        .into_iter()
        .find(|record| record.message_id == message_id)
    {
        record
    } else if projected_messages(&store)?
        .into_iter()
        .any(|row| row.message_id == message_id)
    {
        bail!("message {message_id} content is not retained; send a new message instead");
    } else {
        bail!("message {message_id} not found");
    };
    if !record.status.is_terminal() {
        if matches!(
            record.status,
            MessageStatus::Queued | MessageStatus::Claimed
        ) {
            bail!("{message_id} is still queued; use `rimz message edit` or `rimz message steer`");
        }
        bail!(
            "{message_id} is {}; wait for it to finish before requeueing",
            record.status
        );
    }
    if record.text.is_empty() {
        bail!("message {message_id} content is not retained; send a new message instead");
    }
    let edit = edit_from_flags(flags)?;
    let mut copy = MessageRecord::new_for_card(
        record.workspace_id.clone(),
        record.kind.clone(),
        record.agent_id.clone(),
        record.agent_name.clone(),
        record.text.clone(),
        record.enter,
        record.gate,
    )
    .with_address(record.address.clone())
    .with_channel(record.channel.clone())
    .with_sender(record.sender.clone())
    .with_force(record.force)
    .with_auto_compact(record.auto_compact)
    .with_body(record.body)
    .with_not_before(record.not_before);
    apply_edit_to_record(&mut copy, edit);
    let new_id = copy.message_id.clone();
    store.queue_message(&copy, &workspace.session_name)?;
    deliver::register_message_wake(&workspace, &store)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let label = message_target_for_record(&copy, &snapshot);
    #[expect(clippy::print_stdout, reason = "command result")]
    {
        println!("queued for {label} ({new_id})  (from {message_id})");
    }
    Ok(())
}

fn apply_edit_to_record(record: &mut MessageRecord, edit: MessageEdit) {
    if let Some(text) = edit.text {
        record.text = text;
    }
    if let Some(gate) = edit.gate {
        record.gate = gate;
    }
    if let Some(not_before) = edit.not_before {
        record.not_before = not_before;
    }
    if let Some(force) = edit.force {
        record.force = force;
    }
    if let Some(enter) = edit.enter {
        record.enter = enter;
    }
    if let Some(auto_compact) = edit.auto_compact {
        record.auto_compact = auto_compact;
        record.compacted_context_tokens = None;
    }
}

fn message_target_for_record(record: &MessageRecord, snapshot: &SidebarSnapshot) -> String {
    let row = MessageListRow::from_record(record.clone());
    let agents: Vec<&AgentState> = snapshot.root_agents().collect();
    scoped_handle(message_target(&row, &agents), row.channel.as_deref())
}

fn steer_failure(check: &deliver::DeliveryCheck, target: &str, message_id: &MessageId) -> String {
    if check.ask.waiting {
        return format!(
            "{target} ({message_id}) is waiting on your input in its pane; answer it or pass --force"
        );
    }
    if !check.agent.present {
        return format!("receiver {target} is gone; cannot steer {message_id}");
    }
    if !check.pane.present {
        return match &check.pane.pinned_pane_id {
            Some(pane_id) => {
                format!("pinned pane {pane_id} is not live for {target}; cannot steer {message_id}")
            }
            None => format!("no live pane for {target}; cannot steer {message_id}"),
        };
    }
    if delivery_conditions_pass(check) {
        return format!(
            "{message_id} has a recent delivery attempt in progress; retry in a few seconds"
        );
    }
    delivery_verdict(check, target, Timestamp::now())
}

fn delivery_conditions_pass(check: &deliver::DeliveryCheck) -> bool {
    check.schedule.ready
        && check.fifo.head
        && check.agent.present
        && gate_ready(check)
        && !check.ask.waiting
        && check.pane.present
}

fn remove_messages(message_ids: Vec<MessageId>, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let mut failed = false;
    for message_id in message_ids {
        if store.remove_message(&message_id, &workspace.session_name, "remove")? {
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
    let (workspace, store, snapshot) = workspace_store_snapshot(globals)?;
    let channel = current_channel(&workspace);
    if let Some(target) = target {
        rimz::harness::target::require_mention(&target)?;
        let agent = super::resolve_agent_one(
            &snapshot,
            &target,
            worktree.as_deref().or(channel_flag.as_deref()),
            channel.as_deref(),
        )?;
        let removed = store.clear_messages_for(
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
    let removed = store.clear_channel_messages(lane, &workspace.session_name)?;
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

fn wait_and_print_message(
    store: &rimz::Store,
    session_name: &str,
    label: &str,
    message_id: &MessageId,
    wait_base: u64,
    deadline: std::time::Instant,
) -> Result<bool> {
    let status =
        send::wait_for_message_until(store, message_id, session_name, wait_base, deadline)?;
    #[expect(clippy::print_stdout, reason = "wait status")]
    {
        println!("{} {label} ({message_id})", wait_status_label(status));
    }
    Ok(status == MessageStatus::Delivered)
}

#[derive(Clone, Copy)]
enum ReportMode {
    Steer,
    Boundary,
}

fn render_dispatch_outcome(outcome: &DispatchOutcome) -> Option<String> {
    match outcome {
        DispatchOutcome::Sent { label, message_id } => {
            Some(format!("sent to {label} ({message_id})"))
        }
        DispatchOutcome::Queued { label, message_id } => {
            Some(format!("queued for {label} ({message_id})"))
        }
        DispatchOutcome::SkippedWaiting { .. } => None,
    }
}

/// Report a unified dispatch. Boundary sends keep the old one-line-per-target
/// output; steer fan-out keeps the summary line and pending-ask bail.
#[allow(clippy::too_many_arguments)]
fn report_dispatch(
    mode: ReportMode,
    store: &rimz::Store,
    session_name: &str,
    wait: Option<(Duration, u64)>,
    target: &str,
    total: usize,
    outcomes: &[DispatchOutcome],
    compacted: &[String],
) -> Result<()> {
    if matches!(mode, ReportMode::Boundary) {
        for label in compacted {
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("compacted {label}");
            }
        }
        if let Some((timeout, wait_base)) = wait {
            let mut failed = false;
            let deadline = std::time::Instant::now() + timeout;
            for outcome in outcomes {
                let (label, message_id) = match outcome {
                    DispatchOutcome::Sent { label, message_id }
                    | DispatchOutcome::Queued { label, message_id } => (label, message_id),
                    DispatchOutcome::SkippedWaiting { .. } => continue,
                };
                if !wait_and_print_message(
                    store,
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
        for outcome in outcomes {
            if let Some(line) = render_dispatch_outcome(outcome) {
                #[expect(clippy::print_stdout, reason = "command result")]
                {
                    println!("{line}");
                }
            }
        }
        return Ok(());
    }

    let sent = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            DispatchOutcome::Sent { label, message_id } => Some(format!("{label} ({message_id})")),
            _ => None,
        })
        .collect::<Vec<_>>();
    let sent_labels = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            DispatchOutcome::Sent { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let queued = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            DispatchOutcome::Queued { label, message_id } => {
                Some(format!("{label} ({message_id})"))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let pending = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            DispatchOutcome::SkippedWaiting { label, message_id } => {
                Some(format!("{label} ({message_id})"))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some((timeout, wait_base)) = wait {
        let mut failed = false;
        let deadline = std::time::Instant::now() + timeout;
        for outcome in outcomes {
            let (label, message_id, compactable) = match outcome {
                DispatchOutcome::Sent { label, message_id } => (label, message_id, true),
                DispatchOutcome::Queued { label, message_id } => (label, message_id, false),
                DispatchOutcome::SkippedWaiting { .. } => continue,
            };
            if compactable {
                print_compacted_if_needed(label, compacted);
            }
            if !wait_and_print_message(
                store,
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
            Some(DispatchOutcome::SkippedWaiting { label, message_id }) => {
                bail!(
                    "{label} ({message_id}) is waiting on your input in its pane; answer it or pass --force"
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
        line.push_str(&format!("; waiting in pane: {}", pending.join(", ")));
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
        MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent => "timed out",
    }
}

fn deliver_message(message_id: MessageId, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    deliver::deliver_one(
        &workspace,
        &store,
        &message_id,
        rimz::message::settle_duration_from_env(),
        globals.mux,
        deliver::DeliveryPolicy::Boundary,
    )?;
    Ok(())
}

fn sweep_messages(globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    deliver::sweep(&workspace, &store, globals.mux)?;
    Ok(())
}

fn workspace_store_snapshot(
    globals: &GlobalFlags,
) -> Result<(ResolvedWorkspace, rimz::Store, rimz::SidebarSnapshot)> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    Ok((workspace, store, snapshot))
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

fn render_message_digest(
    out: &mut impl Write,
    messages: Vec<MessageListRow>,
    agents: &[&AgentState],
    lane_scope: &LaneScope,
    hidden: usize,
    status: Option<MessageStatus>,
) -> Result<()> {
    if messages.is_empty() {
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::FAINT,
                &empty_message_digest(lane_scope, status)
            )
        )?;
        return Ok(());
    }

    let now = Timestamp::now();
    if matches!(lane_scope, LaneScope::All) {
        for (index, (channel, rows)) in message_digest_groups(messages).into_iter().enumerate() {
            if index > 0 {
                writeln!(out)?;
            }
            writeln!(
                out,
                "{}",
                render::paint(render::palette::HEADER, &lane_header(channel.as_deref()))
            )?;
            render_message_rows(out, rows, agents, now, 2, 4)?;
        }
    } else {
        render_message_rows(out, messages, agents, now, 0, 2)?;
    }
    if hidden > 0 {
        writeln!(
            out,
            "... {hidden} older messages hidden (--limit 0 for all)"
        )?;
    }
    Ok(())
}

fn render_message_rows(
    out: &mut impl Write,
    messages: Vec<MessageListRow>,
    agents: &[&AgentState],
    now: Timestamp,
    row_indent: usize,
    snippet_indent: usize,
) -> Result<()> {
    let row_pad = " ".repeat(row_indent);
    let snippet_pad = " ".repeat(snippet_indent);
    let snippet_width = render::terminal_columns(120).saturating_sub(snippet_indent);
    for message in messages {
        let target = scoped_handle(message_target(&message, agents), message.channel.as_deref());
        let sender = scoped_handle(message.sender.render(), message.channel.as_deref());
        writeln!(
            out,
            "{row_pad}{}{}{}  {}  {}  {}",
            rendered_sender(&message.sender, &sender),
            render::paint(render::palette::FAINT, " → "),
            render::paint(render::palette::META.bold(), &target),
            render::paint(
                render::status::message(message.status),
                message.status.as_str()
            ),
            render::rel_age(message.enqueued_at, now),
            render::paint(render::palette::FAINT, message.message_id.as_str())
        )?;
        writeln!(
            out,
            "{snippet_pad}{}",
            message_snippet(&message, snippet_width)
        )?;
    }
    Ok(())
}

fn empty_message_digest(lane_scope: &LaneScope, status: Option<MessageStatus>) -> String {
    let qualifier = status.map(MessageStatus::as_str).unwrap_or_default();
    let kind = if qualifier.is_empty() {
        "messages".to_owned()
    } else {
        format!("{qualifier} messages")
    };
    let mut line = match lane_scope {
        LaneScope::All => format!("no {kind}"),
        LaneScope::Main => format!("no {kind} in the main lane"),
        LaneScope::Named(channel) => format!("no {kind} in {}", lane_header(Some(channel))),
    };
    if !matches!(lane_scope, LaneScope::All) {
        line.push_str(" — rimz message list --all shows every channel");
    }
    line
}

fn message_digest_groups(
    messages: Vec<MessageListRow>,
) -> Vec<(Option<String>, Vec<MessageListRow>)> {
    let mut groups: Vec<(Option<String>, Vec<MessageListRow>)> = Vec::new();
    for message in messages {
        if let Some((_, rows)) = groups
            .iter_mut()
            .find(|(channel, _)| channel == &message.channel)
        {
            rows.push(message);
        } else {
            groups.push((message.channel.clone(), vec![message]));
        }
    }
    groups
}

fn rendered_sender(sender: &MessageSender, rendered: &str) -> String {
    match sender {
        MessageSender::Human => render::paint(render::palette::COOL, rendered),
        MessageSender::Agent { .. } => render::paint(render::palette::META.bold(), rendered),
    }
}

fn lane_header(channel: Option<&str>) -> String {
    channel
        .filter(|channel| !channel.is_empty())
        .map(|channel| format!("#{channel}"))
        .unwrap_or_else(|| "(main)".to_owned())
}

fn message_target(message: &MessageListRow, agents: &[&AgentState]) -> String {
    address::message_target(
        message.address.as_deref(),
        &message.kind,
        &message.agent_id,
        message.agent_name.as_deref(),
        message.channel.as_deref(),
        agents,
    )
}

fn scoped_handle(rendered: String, filter_channel: Option<&str>) -> String {
    let Some(filter) = filter_channel else {
        return rendered;
    };
    let Some((base, channel)) = rendered.rsplit_once('#') else {
        return rendered;
    };
    if channel == filter {
        base.to_owned()
    } else {
        rendered
    }
}

fn message_snippet(message: &MessageListRow, width: usize) -> String {
    if let Some(text) = message.text.as_deref() {
        return preview(&collapse_home_in_snippet(text), width);
    }
    if let Some(reason) = message
        .last_error
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        return render::paint(
            render::palette::FAINT,
            &preview(&collapse_home_in_snippet(reason), width),
        );
    }
    render::paint(render::palette::FAINT, "-")
}

fn collapse_home_in_snippet(text: &str) -> String {
    let home = std::env::var("HOME").ok();
    collapse_home_in_snippet_to(home.as_deref(), text)
}

fn collapse_home_in_snippet_to(home: Option<&str>, text: &str) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty() && *home != "/")
    else {
        return text.to_owned();
    };
    let mut collapsed = String::new();
    let mut rest = text;
    let mut changed = false;
    while let Some(index) = rest.find(home) {
        let (before, matched) = rest.split_at(index);
        let after = &matched[home.len()..];
        if home_match_boundary(before.chars().next_back(), after.chars().next()) {
            collapsed.push_str(before);
            collapsed.push('~');
            rest = after;
            changed = true;
        } else {
            let (head, tail) = matched.split_at(matched.chars().next().unwrap().len_utf8());
            collapsed.push_str(before);
            collapsed.push_str(head);
            rest = tail;
        }
    }
    if !changed {
        return text.to_owned();
    }
    collapsed.push_str(rest);
    collapsed
}

fn home_match_boundary(previous: Option<char>, next: Option<char>) -> bool {
    home_start_boundary(previous) && home_end_boundary(next)
}

fn home_start_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '(' | '[' | '{' | '<' | '=' | ':')
    })
}

fn home_end_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '/' | '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | ':'
            )
    })
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
    message_id: &MessageId,
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
    let ask = if !check.ask.waiting {
        if check.ask.force {
            "ok (--force)".to_owned()
        } else {
            "ok".to_owned()
        }
    } else {
        "waiting in pane".to_owned()
    };
    kv.push("ask", condition_cell(!check.ask.waiting, ask));
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
    if let Some(hint) = delivery_action_hint(check, message_id) {
        writeln!(out, "  {}", render::paint(render::palette::FAINT, &hint))?;
    }
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
    if check.ask.waiting {
        return format!("waiting: {target} is waiting on input in its pane");
    }
    if !check.pane.present {
        return match &check.pane.pinned_pane_id {
            Some(pane_id) => format!("stuck: pinned pane {pane_id} is not live for {target}"),
            None => format!("stuck: no live pane for {target}"),
        };
    }
    "ready: delivery conditions pass".to_owned()
}

fn delivery_action_hint(check: &deliver::DeliveryCheck, message_id: &MessageId) -> Option<String> {
    if !check.schedule.ready {
        return Some(format!(
            "force now: rimz message steer {message_id}  ·  or: rimz message edit {message_id} --no-schedule"
        ));
    }
    if !check.fifo.head || !gate_ready(check) {
        return Some(format!("force now: rimz message steer {message_id}"));
    }
    if check.ask.waiting {
        return Some(format!(
            "force now: rimz message steer {message_id} --force"
        ));
    }
    None
}

fn gate_ready(check: &deliver::DeliveryCheck) -> bool {
    check.gate.open && check.gate.resume_recovered != Some(false)
}

fn preview(text: &str, width: usize) -> String {
    let preview = text.replace(['\r', '\n', '\t'], " ");
    if preview.width() <= width {
        return preview;
    }
    if width == 0 {
        return String::new();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let mut shortened = String::new();
    let mut used = 0;
    for ch in preview.chars() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > width - 3 {
            break;
        }
        shortened.push(ch);
        used += char_width;
    }
    shortened.push_str("...");
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;

    use rimz::agents::{AgentStatus, TurnPhase};
    use rimz::ids::{AgentKind, AgentSessionId, MessageId, MuxName, PaneId, WorkspaceId};
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
    fn message_target_uses_stored_address_before_fallbacks() {
        let message = MessageRecord::new(
            workspace_id(),
            &agent("sess-coder", AgentStatus::Idle),
            "work".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_channel(Some("project".to_owned()))
        .with_address(Some("@saved#project".to_owned()));
        let message = MessageListRow::from_record(message);

        assert_eq!(message_target(&message, &[]), "@saved#project");
    }

    #[test]
    fn message_target_falls_back_to_agent_name_and_channel_when_agent_is_gone() {
        let message = MessageRecord::new(
            workspace_id(),
            &agent("sess-coder", AgentStatus::Idle),
            "work".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_channel(Some("project".to_owned()));
        let message = MessageListRow::from_record(message);

        assert_eq!(message_target(&message, &[]), "@sess-coder-name#project");
    }

    #[test]
    fn message_target_falls_back_to_kind_id_for_nameless_records() {
        let mut receiver = agent("sess-coder", AgentStatus::Idle);
        receiver.name = None;
        let message = MessageRecord::new(
            workspace_id(),
            &receiver,
            "work".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let message = MessageListRow::from_record(message);

        assert_eq!(message_target(&message, &[]), "claude:sess-coder");
    }

    #[test]
    fn scoped_handle_drops_matching_lane_suffix() {
        assert_eq!(
            scoped_handle("@coder#project".to_owned(), Some("project")),
            "@coder"
        );
        // Lane membership is exact: a team lane keeps its suffix under the
        // directory filter.
        assert_eq!(
            scoped_handle("@coder#project/forge".to_owned(), Some("project")),
            "@coder#project/forge"
        );
        assert_eq!(
            scoped_handle("@coder#ops".to_owned(), Some("project")),
            "@coder#ops"
        );
        assert_eq!(scoped_handle("you".to_owned(), Some("project")), "you");
    }

    #[test]
    fn preview_respects_width_and_flattens_control_whitespace() {
        assert_eq!(preview("a\nb\tc", 10), "a b c");
        assert_eq!(preview("abcdef", 4), "a...");
        assert_eq!(preview("abcdef", 3), "...");
    }

    #[test]
    fn steer_failure_ready_check_reports_recent_attempt() {
        let message_id = MessageId::parse("msg_0000000000000001").unwrap();
        let check = deliver::DeliveryCheck {
            schedule: deliver::ScheduleCheck {
                ready: true,
                not_before: None,
                retry_after: None,
            },
            fifo: deliver::FifoCheck {
                head: true,
                blocker: None,
            },
            agent: deliver::AgentCheck { present: true },
            gate: deliver::GateCheck {
                gate: DeliveryGate::Done,
                status: Some(AgentStatus::Idle),
                open: true,
                resume_recovered: None,
            },
            ask: deliver::AskCheck {
                waiting: false,
                force: false,
            },
            pane: deliver::PaneCheck {
                present: true,
                pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_3")),
                pinned_pane_id: None,
            },
        };

        let message = steer_failure(&check, "@claude", &message_id);

        assert!(message.contains("recent delivery attempt"), "{message}");
        assert!(!message.contains("ready: delivery conditions pass"));
    }

    #[test]
    fn message_digest_groups_all_lanes_once_by_latest_activity() {
        let output = render_digest(
            vec![
                message_row("sess-docs-new", Some("docs"), "new docs"),
                message_row("sess-ops", Some("ops"), "ops"),
                message_row("sess-docs-old", Some("docs"), "old docs"),
            ],
            LaneScope::All,
            None,
        );

        assert_eq!(output.matches("#docs").count(), 1);
        assert_eq!(output.matches("#ops").count(), 1);
        assert!(output.find("#docs").unwrap() < output.find("new docs").unwrap());
        assert!(output.find("new docs").unwrap() < output.find("old docs").unwrap());
        assert!(output.find("old docs").unwrap() < output.find("#ops").unwrap());

        let lines: Vec<&str> = output.lines().collect();
        let snippet = lines
            .iter()
            .position(|line| line.contains("new docs"))
            .unwrap();
        assert!(lines[snippet - 1].starts_with("  "));
        assert!(lines[snippet].starts_with("    "));
    }

    #[test]
    fn message_digest_scopes_handles_by_row_lane() {
        let output = render_digest(
            vec![
                message_row_with_sender(
                    "sess-same",
                    Some("main"),
                    "own lane",
                    agent_sender("planner", Some("main")),
                ),
                message_row_with_sender(
                    "sess-cross",
                    Some("main"),
                    "cross lane",
                    agent_sender("reviewer", Some("docs")),
                ),
            ],
            LaneScope::All,
            None,
        );

        assert!(output.contains("@planner"));
        assert!(!output.contains("@planner#main"));
        assert!(output.contains("@reviewer#docs"));
    }

    #[test]
    fn message_digest_empty_state_describes_scope_and_status() {
        let all = render_digest(Vec::new(), LaneScope::All, None);
        assert!(all.contains("no messages"));
        assert!(!all.contains("shows every channel"));

        let main = render_digest(Vec::new(), LaneScope::Main, None);
        assert!(main.contains("no messages in the main lane"));
        assert!(main.contains("rimz message list --all shows every channel"));

        let named = render_digest(
            Vec::new(),
            LaneScope::Named("ops".to_owned()),
            Some(MessageStatus::Queued),
        );
        assert!(named.contains("no queued messages in #ops"));
        assert!(named.contains("rimz message list --all shows every channel"));
    }

    #[test]
    fn collapse_home_in_snippet_handles_mid_text_home_and_no_home() {
        assert_eq!(
            collapse_home_in_snippet_to(
                Some("/home/dev"),
                "see /home/dev/worktree/plan.md then /tmp"
            ),
            "see ~/worktree/plan.md then /tmp"
        );
        assert_eq!(
            collapse_home_in_snippet_to(None, "see /home/dev/worktree"),
            "see /home/dev/worktree"
        );
        assert_eq!(
            collapse_home_in_snippet_to(Some("/home/dev"), "see /home/development/plan.md"),
            "see /home/development/plan.md"
        );
        assert_eq!(
            collapse_home_in_snippet_to(Some("/home/dev/"), "see /home/dev/worktree"),
            "see ~/worktree"
        );
        assert_eq!(collapse_home_in_snippet_to(Some("/"), "/tmp"), "/tmp");
        assert_eq!(collapse_home_in_snippet_to(Some(""), "/tmp"), "/tmp");
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn render_digest(
        messages: Vec<MessageListRow>,
        lane_scope: LaneScope,
        status: Option<MessageStatus>,
    ) -> String {
        let mut out = Vec::new();
        render_message_digest(&mut out, messages, &[], &lane_scope, 0, status).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn message_row(id: &str, channel: Option<&str>, text: &str) -> MessageListRow {
        message_row_with_sender(id, channel, text, MessageSender::Human)
    }

    fn message_row_with_sender(
        id: &str,
        channel: Option<&str>,
        text: &str,
        sender: MessageSender,
    ) -> MessageListRow {
        let message = MessageRecord::new(
            workspace_id(),
            &agent(id, AgentStatus::Idle),
            text.to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_channel(channel.map(ToOwned::to_owned))
        .with_sender(sender);
        MessageListRow::from_record(message)
    }

    fn agent_sender(role: &str, channel: Option<&str>) -> MessageSender {
        MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: Some(role.to_owned()),
            channel: channel.map(ToOwned::to_owned),
        }
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
            waiting_since: None,
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
