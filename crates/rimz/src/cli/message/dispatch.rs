//! Parse direct message policy, invoke domain dispatch, and render its result.

use std::io::Write;

use anyhow::{Result, bail};
use jiff::Timestamp;

use super::*;
use rimz::TargetErr;
use rimz::message::dispatch::{
    ConditionErr, ConditionKind, DispatchErr, DispatchMode, DispatchRequest, ReplyRequest,
    WhenRequest,
};
use rimz::message::reply::{ReplyJoin, ReplyPrepareErr};

pub(super) enum SendKind {
    Steer,
    Boundary {
        gate: DeliveryGate,
        schedule: Option<String>,
        after: Vec<String>,
        when: Vec<WhenRequest>,
    },
}

/// Resolve command-side flags and dispatch a steer or turn-boundary message.
pub(super) fn send_message(
    target: String,
    mode: SendKind,
    flags: SendFlags,
    text: Vec<String>,
    piped: Option<String>,
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
        stdin: _,
        no_from,
        wait,
        json,
        any,
    } = flags;
    let wait = send::WaitSpec {
        mode: send::reply_wait(wait, send::agent_caller()),
        any,
        json,
    };
    let scheduled = matches!(
        &mode,
        SendKind::Boundary {
            schedule: Some(_),
            ..
        }
    );
    send::validate_reply_wait(wait, !no_enter, create, scheduled)?;
    let text = resolve_message(&text, file.as_deref(), piped.as_deref())?;
    let mode = dispatch_mode(mode, !no_enter, force, create, smart_compact)?;
    rimz::harness::target::require_mention(&target)?;
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let current_channel = ctx.channel().map(ToOwned::to_owned);
    let sender = send::sender_from_env(current_channel.as_deref(), no_from);
    let steer = matches!(mode, DispatchMode::Steer { .. });
    let wait_started = std::time::Instant::now();
    let request = DispatchRequest {
        target: target.clone(),
        text: text.clone(),
        target_scope: worktree.clone().or_else(|| channel_flag.clone()),
        current_channel: current_channel.clone(),
        sender: sender.clone(),
        automated: false,
        allow_fanout: all,
        reply: wait.is_on().then(|| ReplyRequest {
            join: if wait.any {
                ReplyJoin::Any
            } else {
                ReplyJoin::All
            },
            caller_identity: send::agent_caller_identity(),
        }),
        mux: globals.mux,
        mode,
    };
    let result = match rimz::message::dispatch::dispatch(workspace, store, request) {
        Ok(result) => result,
        Err(DispatchErr::Recipient(err)) => {
            return recipient_miss(
                &ctx,
                RecipientMiss {
                    target: &target,
                    text: &text,
                    sender: &sender,
                    worktree: worktree.as_deref(),
                    channel_flag: channel_flag.as_deref(),
                    current_channel: current_channel.as_deref(),
                    create,
                },
                err,
                globals,
            );
        }
        Err(err) => return Err(map_dispatch_err(err)),
    };
    if let Some(reply_wait) = result.reply {
        return reply::wait_for_replies(
            store,
            &workspace.session_name,
            reply_wait,
            wait,
            wait.deadline_from(wait_started),
        );
    }
    send::report_dispatch(
        if steer {
            send::ReportMode::Steer
        } else {
            send::ReportMode::Boundary
        },
        &target,
        &result.outcomes,
        &result.compacted,
    )
}

/// Validate the flag combinations a send mode forbids, then resolve it into the
/// domain dispatch mode.
fn dispatch_mode(
    mode: SendKind,
    enter: bool,
    force: bool,
    create: bool,
    smart_compact: Option<AutoCompact>,
) -> Result<DispatchMode> {
    let machine_config = crate::cli::machine_config();
    let auto_compact = smart_compact.or(machine_config.harness.smart_compact);
    let SendKind::Boundary {
        gate,
        schedule,
        after,
        when,
    } = mode
    else {
        return Ok(DispatchMode::Steer {
            enter,
            force,
            auto_compact,
        });
    };
    if create {
        if schedule.is_some() {
            bail!("--schedule needs an existing agent; remove --create");
        }
        if !after.is_empty() {
            bail!("--after needs an existing recipient; remove --create");
        }
        if !when.is_empty() {
            bail!("--when needs an existing recipient; remove --create");
        }
    }
    let now = Timestamp::now().to_zoned(machine_config.time_zone());
    Ok(DispatchMode::Boundary {
        enter,
        gate,
        force,
        auto_compact,
        not_before: schedule
            .as_deref()
            .map(|raw| parse_schedule_at(raw, &now).map_err(anyhow::Error::msg))
            .transpose()?,
        after,
        when,
    })
}

/// The addressing context a recipient miss needs to recover or explain itself.
struct RecipientMiss<'a> {
    target: &'a str,
    text: &'a str,
    sender: &'a MessageSender,
    worktree: Option<&'a str>,
    channel_flag: Option<&'a str>,
    current_channel: Option<&'a str>,
    create: bool,
}

/// Recover from an address that matched no recipient: create the agent on
/// demand, record the bounce, then present the miss with the available agents.
fn recipient_miss(
    ctx: &Ctx,
    miss: RecipientMiss<'_>,
    err: TargetErr,
    globals: &GlobalFlags,
) -> Result<()> {
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    if miss.create {
        return crate::cli::agents_cmd::create_on_miss(
            miss.target,
            miss.worktree,
            miss.channel_flag,
            miss.current_channel,
            miss.text,
            globals,
        );
    }
    if matches!(
        err,
        TargetErr::NoMatch { .. }
            | TargetErr::NoMatchInChannel { .. }
            | TargetErr::PaneUnbound { .. }
    ) {
        store.record_unresolved_message(rimz::store::UnresolvedMessage {
            workspace_id: workspace.workspace_id.clone(),
            session_name: &workspace.session_name,
            address: miss.target,
            channel: miss.current_channel,
            sender: miss.sender,
            text_len: miss.text.len(),
            reason: "receiver not found",
        })?;
    }
    let snapshot = store.snapshot_cached()?;
    let mapped = map_queue_target_err(miss.target, err);
    message_miss(&snapshot, miss.current_channel, &mapped)
}

fn map_dispatch_err(err: DispatchErr) -> anyhow::Error {
    match err {
        DispatchErr::Fanout {
            target,
            labels,
            steer,
        } => crate::cli::ambiguous_fanout(
            if steer {
                "message --steer"
            } else {
                "deliver to"
            },
            &target,
            &labels,
        ),
        DispatchErr::Condition(err) => map_condition_err(err),
        DispatchErr::ReplyPreparation(err) => map_reply_prepare_err(err),
        other => anyhow::Error::new(other),
    }
}

fn map_condition_err(err: ConditionErr) -> anyhow::Error {
    match err {
        ConditionErr::Broadcast {
            kind,
            address,
            expression,
        } => match kind {
            ConditionKind::After => anyhow::anyhow!(
                "--after `{address}` must name one agent; broadcasts are not supported"
            ),
            ConditionKind::When => anyhow::anyhow!(
                "--when `{expression}` must name one agent; broadcasts are not supported"
            ),
        },
        ConditionErr::Arity {
            kind,
            address,
            expression,
            matched,
        } => match kind {
            ConditionKind::After => anyhow::anyhow!(
                "--after `{address}` must resolve to exactly one agent; matched {matched}"
            ),
            ConditionKind::When => anyhow::anyhow!(
                "--when `{expression}` must resolve to exactly one agent; matched {matched}"
            ),
        },
        ConditionErr::NoLifecycle {
            kind,
            address,
            expression,
        } => match kind {
            ConditionKind::After => {
                anyhow::anyhow!("--after `{address}` must resolve to an agent with lifecycle state")
            }
            ConditionKind::When => anyhow::anyhow!(
                "--when `{expression}` must resolve to an agent with lifecycle state"
            ),
        },
        ConditionErr::RecipientSelfReference { address } => anyhow::anyhow!(
            "--after `{address}` names the message recipient; use --on to gate on the recipient's turn"
        ),
        ConditionErr::Target {
            address, source, ..
        } => map_queue_target_err(&address, *source),
    }
}

fn map_reply_prepare_err(err: ReplyPrepareErr) -> anyhow::Error {
    match err {
        ReplyPrepareErr::PaneOnly { label } => anyhow::anyhow!(
            "--wait requires an agent with lifecycle state; `{label}` is only a pane target"
        ),
        ReplyPrepareErr::NotLive { label } => anyhow::anyhow!(
            "--wait requires a live agent with lifecycle state; `{label}` is not running"
        ),
        ReplyPrepareErr::TurnLifecycleUnsupported { kind, reason } => anyhow::anyhow!(
            "--wait cannot use {kind}: a verified executable turn-lifecycle signal is required; {reason}"
        ),
        ReplyPrepareErr::HooksMissing { kind } => anyhow::anyhow!(
            "--wait requires {kind} hooks so the reply turn can report its boundaries; run `rimz hooks install {kind}`"
        ),
        ReplyPrepareErr::HooksUntrusted { kind, hooks, fix } => {
            anyhow::anyhow!("{kind} hooks are installed but not trusted ({hooks}); {fix}")
        }
        ReplyPrepareErr::DependencyCycle {
            target,
            first_handle,
            first_message_id,
            chain,
        } => {
            let fix =
                "finish your turn to answer it, or resend without --wait or with --wait=<duration>";
            let (Some(handle), Some(message_id)) = (first_handle, first_message_id) else {
                return anyhow::anyhow!("--wait would deadlock: {target} is your own agent; {fix}");
            };
            match chain {
                Some(chain) => anyhow::anyhow!(
                    "--wait would deadlock: {chain} is an active reply-wait chain ({message_id}); {fix}"
                ),
                None => anyhow::anyhow!(
                    "--wait would deadlock: {handle} is waiting on your reply ({message_id}); {fix}"
                ),
            }
        }
        other => anyhow::Error::new(other),
    }
}

pub(super) fn message_miss(
    snapshot: &SidebarSnapshot,
    channel: Option<&str>,
    err: &anyhow::Error,
) -> Result<()> {
    let mut out = render::err();
    writeln!(out, "{err:#}")?;
    let agents = snapshot
        .root_agents()
        .filter(|agent| {
            channel.is_none_or(|filter| rimz::harness::target::agent_in_worktree(agent, filter))
        })
        .collect::<Vec<_>>();
    if agents.is_empty() {
        writeln!(out, "no agents are running")?;
    } else {
        writeln!(out, "available agents:")?;
        crate::cli::agents_cmd::render_agents_table(
            &mut out,
            snapshot,
            &agents,
            Timestamp::now(),
            render::terminal_columns(120),
            &crate::cli::machine_config().theme,
        )?;
    }
    out.flush().ok();
    std::process::exit(1);
}

pub(super) fn map_queue_target_err(target: &str, err: TargetErr) -> anyhow::Error {
    let mapped: Result<()> = crate::cli::map_resolve(target, Err(err.clone()));
    match mapped {
        Ok(_) => unreachable!("mapping an error cannot succeed"),
        Err(mapped) => mapped,
    }
}
