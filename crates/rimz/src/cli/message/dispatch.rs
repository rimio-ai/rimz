//! Parse direct message policy, invoke domain dispatch, and render its result.

use std::io::Write;

use anyhow::{Result, bail};
use jiff::Timestamp;

use super::*;
use rimz::address::TargetErr;
use rimz::message::dispatch::{
    ConditionErr, ConditionKind, DispatchErr, DispatchMode, DispatchRequest, DispatchResult,
    WhenRequest,
};
use rimz::store::message::Premise;

pub(super) enum SendKind {
    Steer,
    Interrupt,
    Boundary {
        gate: DeliveryGate,
        schedule: Option<String>,
        after: Vec<String>,
        when: Vec<WhenRequest>,
        ttl: Option<std::time::Duration>,
        unless_prompted: bool,
    },
}

/// Who a send goes to: an address dispatch resolves, or recipients selected
/// one by one.
pub(super) enum Recipients {
    Target(String),
    Selected(Vec<crate::cli::select::Selected>),
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
    send_message_to(
        Recipients::Target(target),
        mode,
        flags,
        text,
        piped,
        globals,
    )
}

/// [`send_message`] to either kind of recipients. A selection prints one
/// error line for each address that named no single agent, and exits 1
/// after the others are sent.
pub(super) fn send_message_to(
    recipients: Recipients,
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
        reply_wanted,
    } = flags;
    if reply_wanted {
        refuse_unmarkable(no_from, create)?;
    }
    let ctx = Ctx::open(globals)?;
    let caller = send::resolve_caller(&ctx.store)?;
    let text = resolve_message(&text, file.as_deref(), piped.as_deref())?;
    let premises = user_premises(&mode)?;
    let mode = dispatch_mode(mode, create)?;
    let (target, selected) = match recipients {
        Recipients::Target(target) => {
            rimz::address::require_mention(&target)?;
            (target, Vec::new())
        }
        Recipients::Selected(selected) => (String::new(), selected),
    };
    let selecting = !selected.is_empty();
    let mut unresolved = Vec::new();
    let mut resolved = Vec::new();
    for target in selected {
        match self_address(&ctx, target)? {
            Ok(target) => resolved.push(target),
            Err(miss) => unresolved.push(miss),
        }
    }
    let selected = resolved;
    let target = if target == "@me" {
        let snapshot = ctx.cached_snapshot()?;
        let agent = crate::cli::resolve_agent_one(
            &ctx.store,
            &snapshot,
            &target,
            None,
            &ctx.address_context(),
        )?;
        format!("@{}", agent.agent_id)
    } else {
        target
    };
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let current_channel = ctx.channel().cloned();
    let sender = send::sender_for(caller.as_ref(), current_channel.as_ref(), no_from);
    if reply_wanted && !matches!(sender, MessageSender::Agent { .. }) {
        bail!(
            "--reply-wanted asks for a message back, and a user shell has no inbox one could reach; send without --reply-wanted"
        );
    }
    let kind = mode.kind();
    let request = || DispatchRequest {
        about_run: None,
        reply_wanted,
        premises: premises.clone(),
        resuming_recipient: None,
        selected: selected.clone(),
        target: target.clone(),
        text: text.clone(),
        target_scope: worktree.clone().or_else(|| channel_flag.clone()),
        current_channel: ctx.address_context(),
        caller: caller.clone(),
        sender: sender.clone(),
        automated: false,
        allow_fanout: all,
        mux: globals.mux,
        enter: !no_enter,
        force,
        auto_compact: smart_compact,
        mode: mode.clone(),
    };
    let miss = || RecipientMiss {
        target: &target,
        text: &text,
        sender: &sender,
        worktree: worktree.as_deref(),
        channel_flag: channel_flag.as_deref(),
        current_channel: current_channel.as_ref(),
        create,
    };
    if selecting {
        let mut out = crate::cli::render::out();
        for (address, reason) in &unresolved {
            writeln!(out, "error {address}: {reason}")?;
        }
        if selected.is_empty() {
            return crate::cli::select::batch_result(unresolved.len());
        }
        let result = rimz::message::dispatch::dispatch(workspace, store, request())
            .map_err(|err| map_dispatch_err(err, &ctx))?;
        // A reporter can refuse on a skip, so it runs after the misses print.
        for (address, reason) in &result.missed {
            writeln!(out, "error {address}: {reason}")?;
        }
        if !result.outcomes.is_empty() {
            send::report_dispatch(
                kind,
                "the selected agents",
                &result.outcomes,
                &result.compacted,
            )?;
        }
        return crate::cli::select::batch_result(unresolved.len() + result.missed.len());
    }
    let result = match rimz::message::dispatch::dispatch(workspace, store, request()) {
        Ok(result) => result,
        Err(DispatchErr::Recipient(err)) => {
            match recipient_miss(&ctx, miss(), err, globals, Some(request()))? {
                RecipientMissOutcome::Complete => return Ok(()),
                RecipientMissOutcome::Dispatched(result) => *result,
                RecipientMissOutcome::Retry => {
                    match rimz::message::dispatch::dispatch(workspace, store, request()) {
                        Ok(result) => result,
                        Err(DispatchErr::Recipient(err)) => {
                            recipient_miss(&ctx, miss(), err, globals, None)?;
                            return Ok(());
                        }
                        Err(err) => return Err(map_dispatch_err(err, &ctx)),
                    }
                }
            }
        }
        Err(err) => return Err(map_dispatch_err(err, &ctx)),
    };
    send::report_dispatch(kind, &target, &result.outcomes, &result.compacted)
}

/// A stdin `@me` line as the caller's own address, which the library resolver
/// cannot read, pinned to the caller; any other line as it came. An `@me` that
/// names no caller is that line's miss: its address and the resolver's reason.
fn self_address(
    ctx: &Ctx,
    target: crate::cli::select::Selected,
) -> Result<Result<crate::cli::select::Selected, (String, String)>> {
    if target.address != "@me" {
        return Ok(Ok(target));
    }
    let snapshot = ctx.cached_snapshot()?;
    let agent = match crate::cli::resolve_agent_one(
        &ctx.store,
        &snapshot,
        "@me",
        None,
        &ctx.address_context(),
    ) {
        Ok(agent) => agent,
        Err(err) => return Ok(Err((target.address, format!("{err:#}")))),
    };
    Ok(Ok(crate::cli::select::Selected {
        address: format!("@{}", agent.agent_id),
        agent: Some((agent.kind.clone(), agent.agent_id.clone())),
    }))
}

/// Refuse `--reply-wanted` where no header would carry the mark: `--create` may
/// launch with the text as a bare first prompt whether or not the target exists.
fn refuse_unmarkable(no_from: bool, create: bool) -> Result<()> {
    if no_from {
        return Err(UsageError::new(
            "--reply-wanted needs the message header to carry it, and --no-from sends none; send without --reply-wanted"
                .to_owned(),
        )
        .into());
    }
    if create {
        return Err(UsageError::new(
            "--reply-wanted cannot mark a first prompt --create launches an agent with; send without --reply-wanted"
                .to_owned(),
        )
        .into());
    }
    Ok(())
}

/// The premises `--ttl` and `--unless-prompted` put on a boundary send, both measured from the
/// send: a prompt the receiver takes after it, whoever typed it, withdraws the message.
fn user_premises(mode: &SendKind) -> Result<Vec<Premise>> {
    let SendKind::Boundary {
        ttl,
        unless_prompted,
        ..
    } = mode
    else {
        return Ok(Premise::NONE);
    };
    let now = Timestamp::now();
    let lifespan = ttl
        .map(|ttl| {
            jiff::SignedDuration::try_from(ttl)
                .ok()
                .and_then(|ttl| now.checked_add(ttl).ok())
                .map(|until| Premise::Lifespan { until })
                .ok_or_else(|| anyhow::anyhow!("--ttl is too long"))
        })
        .transpose()?;
    let unprompted = unless_prompted.then_some(Premise::NotPromptedSince {
        last_prompt_at: Some(now),
    });
    Ok(lifespan.into_iter().chain(unprompted).collect())
}

/// Validate the flag combinations a send mode forbids, then resolve it into the
/// domain dispatch mode.
fn dispatch_mode(mode: SendKind, create: bool) -> Result<DispatchMode> {
    let machine_config = crate::cli::machine_config();
    let (gate, schedule, after, when) = match mode {
        SendKind::Interrupt => {
            if create {
                bail!("--interrupt needs an existing recipient; remove --create");
            }
            return Ok(DispatchMode::Interrupt);
        }
        SendKind::Steer => {
            return Ok(DispatchMode::Steer);
        }
        SendKind::Boundary {
            gate,
            schedule,
            after,
            when,
            ..
        } => (gate, schedule, after, when),
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
        gate,
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
    current_channel: Option<&'a LaneName>,
    create: bool,
}

enum RecipientMissOutcome {
    Complete,
    Retry,
    Dispatched(Box<DispatchResult>),
}

/// Recover from an address that matched no recipient: create the agent on
/// demand, record the bounce, then present the miss with the available agents.
fn recipient_miss(
    ctx: &Ctx,
    miss: RecipientMiss<'_>,
    err: TargetErr,
    globals: &GlobalFlags,
    resume: Option<DispatchRequest>,
) -> Result<RecipientMissOutcome> {
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    if miss.create {
        return crate::cli::agents_cmd::create_on_miss(
            miss.target,
            miss.worktree,
            miss.channel_flag,
            ctx.current_channel(),
            miss.text,
            globals,
        )
        .map(|()| RecipientMissOutcome::Complete);
    }
    let mut resume_error = None;
    let mut refusal = None;
    if matches!(
        err,
        TargetErr::NoMatch { .. }
            | TargetErr::NoMatchInChannel { .. }
            | TargetErr::PaneUnbound { .. }
    ) {
        if let Some(request) = resume {
            use crate::cli::subagents::ResumeChildOutcome;
            match crate::cli::subagents::resume_child(
                ctx,
                miss.target,
                miss.worktree.or(miss.channel_flag),
                &ctx.address_context(),
                request,
            ) {
                Ok(ResumeChildOutcome::AlreadyBound) => return Ok(RecipientMissOutcome::Retry),
                Ok(ResumeChildOutcome::Dispatched(result)) => {
                    return Ok(RecipientMissOutcome::Dispatched(Box::new(result)));
                }
                Ok(ResumeChildOutcome::DispatchFailed(error)) => {
                    return Err(map_dispatch_err(error, ctx));
                }
                Ok(ResumeChildOutcome::DeliveryFailed(error)) => return Err(error),
                Ok(ResumeChildOutcome::NotApplicable) => {}
                Ok(ResumeChildOutcome::LimitRefusal(reason)) => refusal = Some(reason),
                Err(err) => resume_error = Some(format!("{err:#}")),
            }
        }
        // A caller with no current channel is at trunk, so its bounce is trunk's.
        let trunk = LaneName::trunk();
        store.record_unresolved_message(rimz::store::writer::UnresolvedMessage {
            workspace_id: workspace.workspace_id.clone(),
            session_name: &workspace.session_name,
            address: miss.target,
            channel: miss.current_channel.or(Some(&trunk)),
            sender: miss.sender,
            text_len: miss.text.len(),
            reason: refusal
                .as_deref()
                .or(resume_error.as_deref())
                .unwrap_or("receiver not found"),
        })?;
    }
    if let Some(refusal) = refusal {
        bail!("{refusal}");
    }
    let snapshot = store.snapshot_cached()?;
    let mapped = map_queue_target_err(miss.target, err);
    let mapped = match resume_error {
        Some(reason) => mapped.context(reason),
        None => mapped,
    };
    let demotion = rimz::harness::park_demotion(
        Some(store),
        store.paths(),
        ctx.runtime(),
        &crate::cli::machine_config(),
        &snapshot.agents,
        snapshot.resume_outcomes.as_deref().unwrap_or_default(),
        Timestamp::now(),
    );
    message_miss(
        &demotion,
        &snapshot,
        miss.current_channel.map(LaneName::as_str),
        &ctx.address_context(),
        &mapped,
    )
    .map(|()| RecipientMissOutcome::Complete)
}

fn map_dispatch_err(err: DispatchErr, ctx: &Ctx) -> anyhow::Error {
    match err {
        DispatchErr::Fanout {
            target,
            matches,
            recorded,
        } => match ctx.cached_snapshot() {
            Ok(snapshot) => crate::cli::render::AmbiguousAgents::from_matches(
                &target, &matches, recorded, &snapshot,
            )
            .into(),
            Err(err) => err,
        },
        DispatchErr::Condition(err) => map_condition_err(err),
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

pub(super) fn message_miss(
    demotion: &rimz::agents::ParkDemotion,
    snapshot: &SidebarSnapshot,
    channel: Option<&str>,
    reader: &rimz::address::AddressContext,
    err: &anyhow::Error,
) -> Result<()> {
    render::report(err);
    let mut out = render::err();
    let agents = rimz::address::addressable_agents(snapshot)
        .into_iter()
        .filter(|agent| {
            channel.is_none_or(|filter| {
                rimz::address::agent_in_lane(
                    agent,
                    &rimz::address::LaneSelector::parse(Some(filter)),
                )
            })
        })
        .collect::<Vec<_>>();
    if agents.is_empty() {
        writeln!(out, "no agents are running")?;
    } else {
        writeln!(out, "available agents:")?;
        crate::cli::agents_cmd::render_agents_table(
            demotion,
            &mut out,
            snapshot,
            &agents,
            Timestamp::now(),
            render::terminal_columns(120),
            &crate::cli::machine_config().theme,
            reader,
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
