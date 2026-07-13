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
use super::send::{self, SendFlags, WaitSpec, resolve_message};
use super::{GlobalFlags, current_channel, open_store};
use crate::cli::render;
use rimz::SidebarSnapshot;
use rimz::agents::AgentState;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, PaneId};
use rimz::message::dispatch::{DispatchContext, DispatchOutcome, SendMode};
use rimz::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    MessageStatus, WhenCondition, parse_schedule_at,
};
use rimz::message::{deliver, dispatch as message_dispatch};
use rimz::store::event::{EventEnvelope, EventKind, MessageEventPayload};
use rimz::store::{EditOutcome, MessageEdit};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    command: Option<MessageSubcmd>,
    /// Agent mention for the bare message form.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::message_targets
    ))]
    target: Option<String>,
    /// The message, as one quoted argument. Omit it and pass `--stdin` or
    /// `--file` to deliver external contents verbatim.
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
    /// Hold delivery until this agent finishes its queued work (repeatable; all must finish).
    #[arg(long, value_name = "ADDR", conflicts_with_all = ["steer", "wait"])]
    after: Vec<String>,
    /// Hold delivery until an agent stays in a status for a duration (repeatable; all must match).
    #[arg(
        long,
        value_name = "'ADDR STATUS DUR'",
        conflicts_with_all = ["steer", "wait"]
    )]
    when: Vec<String>,
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
        #[arg(
            long,
            value_name = "NAME",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::channels)
        )]
        channel: Option<String>,
        /// Max rows to show, newest first. 0 lists all.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Optional target filter.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::message_targets
        ))]
        target: Option<String>,
    },
    /// Show one message record with timeline and delivery diagnosis.
    #[command(alias = "status")]
    Show {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::all_message_ids
        ))]
        message_id: MessageId,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change a queued message.
    Edit {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::queued_message_ids
        ))]
        message_id: MessageId,
        #[command(flatten)]
        edit: EditFlags,
    },
    /// Deliver a queued message now.
    Steer {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::queued_message_ids
        ))]
        message_id: MessageId,
        /// Send even when the agent is Waiting.
        #[arg(long)]
        force: bool,
    },
    /// Queue a new copy of a finished message.
    Requeue {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::all_message_ids
        ))]
        message_id: MessageId,
        #[command(flatten)]
        edit: EditFlags,
    },
    /// Remove queued messages.
    Remove {
        #[arg(
            value_name = "MESSAGE_ID",
            num_args = 1..,
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::queued_message_ids)
        )]
        message_ids: Vec<MessageId>,
    },
    /// Remove queued messages for an agent, or in the scoped channel.
    Clear {
        /// Optional agent address whose queued messages are removed.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::message_targets
        ))]
        target: Option<String>,
        /// Remove queued messages in this worktree or lane.
        #[arg(
            long,
            conflicts_with = "channel",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
        )]
        worktree: Option<String>,
        /// Remove queued messages in this channel.
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with = "worktree",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::channels)
        )]
        channel: Option<String>,
    },
    /// Deliver one queued message. Spawned by lifecycle hooks.
    #[command(hide = true)]
    Deliver {
        #[arg(long)]
        message_id: MessageId,
    },
    /// Deliver due scheduled messages and cross-agent triggers.
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
            let piped = if args.send.stdin {
                send::read_stdin_prompt()?
            } else {
                send::warn_ignored_stdin();
                None
            };
            let text = args.text.into_iter().collect();
            if args.steer {
                send_message(target, SendKind::Steer, args.send, text, piped, globals)
            } else {
                send_message(
                    target,
                    SendKind::Boundary {
                        gate: args.on,
                        schedule: args.schedule,
                        after: args.after,
                        when: args.when,
                    },
                    args.send,
                    text,
                    piped,
                    globals,
                )
            }
        }
    }
}

const DEFAULT_MESSAGE_LIST_LIMIT: usize = 200;

mod dispatch;
mod edit;
mod list;
mod reply;
mod show;

use dispatch::*;
use edit::*;
use list::*;
use show::*;

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
            automated: true,
            wait: WaitSpec::OFF,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
        },
        FanoutFlags {
            all: false,
            create: false,
        },
        &globals,
    )
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
