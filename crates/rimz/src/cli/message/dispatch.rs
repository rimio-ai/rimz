use std::collections::BTreeSet;

use super::*;

pub(super) enum SendKind {
    Steer,
    Boundary {
        gate: DeliveryGate,
        schedule: Option<String>,
        after: Vec<String>,
        when: Vec<String>,
    },
}

/// Resolve shared send flags and dispatch a steer or turn-boundary message.
pub(super) fn send_message(
    target: String,
    mode: SendKind,
    send: SendFlags,
    text: Vec<String>,
    piped: Option<String>,
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
        stdin: _,
        no_from,
        wait,
        json,
        any,
    } = send;
    let agent_caller = send::agent_caller();
    if let SendKind::Boundary {
        schedule,
        after,
        when,
        ..
    } = &mode
    {
        if schedule.is_some() && create {
            bail!("--schedule needs an existing agent; remove --create");
        }
        if !after.is_empty() && create {
            bail!("--after needs an existing recipient; remove --create");
        }
        if !when.is_empty() && create {
            bail!("--when needs an existing recipient; remove --create");
        }
    }
    let wait = send::WaitSpec {
        mode: send::reply_wait(wait, agent_caller),
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
    let machine_config = crate::cli::machine_config();
    let auto_compact = smart_compact.or(machine_config.harness.smart_compact);
    let text = resolve_message(&text, file.as_deref(), piped.as_deref())?;
    let (dispatch_mode, gate, not_before, after, when) = match mode {
        SendKind::Steer => (
            MessageDispatchMode::Steer,
            DeliveryGate::Any,
            None,
            Vec::new(),
            Vec::new(),
        ),
        SendKind::Boundary {
            gate,
            schedule,
            after,
            when,
        } => {
            let now = Timestamp::now().to_zoned(machine_config.time_zone());
            let not_before = schedule
                .as_deref()
                .map(|raw| parse_schedule_at(raw, &now).map_err(anyhow::Error::msg))
                .transpose()?;
            (MessageDispatchMode::Boundary, gate, not_before, after, when)
        }
    };
    dispatch_message(
        target,
        worktree,
        channel,
        text,
        dispatch_mode,
        MessageSpec {
            enter: !no_enter,
            gate,
            force,
            auto_compact,
            no_from,
            automated: false,
            wait,
            not_before,
            after,
            when,
        },
        FanoutFlags { all, create },
        globals,
    )
}

/// The fan-out / create flags shared by parked message delivery.
pub(super) struct FanoutFlags {
    pub(super) all: bool,
    pub(super) create: bool,
}

#[derive(Clone, Copy)]
pub(super) enum MessageDispatchMode {
    Steer,
    Boundary,
}

pub(super) fn message_miss(
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

pub(super) fn map_queue_target_err(target: &str, err: rimz::TargetErr) -> anyhow::Error {
    let mapped: Result<()> = crate::cli::map_resolve(target, Err(err.clone()));
    match mapped {
        Ok(_) => unreachable!("mapping an error cannot succeed"),
        Err(mapped) => mapped,
    }
}

pub(super) fn record_resolution_bounce(
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
pub(super) struct MessageSpec {
    pub(super) enter: bool,
    pub(super) gate: DeliveryGate,
    pub(super) force: bool,
    pub(super) auto_compact: Option<AutoCompact>,
    pub(super) no_from: bool,
    pub(super) automated: bool,
    pub(super) wait: WaitSpec,
    pub(super) not_before: Option<Timestamp>,
    pub(super) after: Vec<String>,
    pub(super) when: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_message(
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
    let needs_agent_context = spec.auto_compact.is_some()
        || !spec.after.is_empty()
        || !spec.when.is_empty()
        || matches!(sender, MessageSender::Agent { .. })
        || send::agent_caller();
    let agent_context = needs_agent_context
        .then(|| read_agent_context(&workspace))
        .flatten();
    let mut pending = Vec::new();
    let mut cached_snapshot = None;
    let rollup_only = match mode {
        MessageDispatchMode::Steer => false,
        MessageDispatchMode::Boundary => {
            pending = store.list_messages()?;
            let mut snapshot = store.snapshot_cached().context("reading agent snapshot")?;
            if ((!spec.after.is_empty() || !spec.when.is_empty())
                || matches!(sender, MessageSender::Agent { .. })
                || send::agent_caller())
                && let Some(records) = agent_context.as_ref()
            {
                snapshot = snapshot.with_agent_context(records.clone());
            }
            let rollup_only = message_dispatch::rollup_targets_all_park_without_live(
                &snapshot,
                &target,
                worktree.as_deref().or(channel_flag.as_deref()),
                channel.as_deref(),
                &pending,
                spec.gate,
                spec.force,
            );
            cached_snapshot = Some(snapshot);
            rollup_only
        }
    };
    let mut snapshot = match cached_snapshot.filter(|_| rollup_only) {
        Some(snapshot) => snapshot,
        None => crate::cli::resolution_snapshot(&workspace, &store, globals)?,
    };
    // Smart compaction reads context fill. Immediate message sends share the
    // live path, so fold the disposable context sidecars before any send-now
    // decision that might compact first.
    if !rollup_only && let Some(records) = agent_context {
        snapshot = snapshot.with_agent_context(records);
    }
    let durable_agents = message_dispatch::durable_target_agents(&store)?;
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
    let after = resolve_after_conditions(
        &snapshot,
        &durable_agents,
        &targets,
        &spec.after,
        worktree.as_deref().or(channel_flag.as_deref()),
        channel.as_deref(),
        rollup_only,
        spec.gate,
        &pending,
    )?;
    let when = resolve_when_conditions(
        &snapshot,
        &durable_agents,
        &spec.when,
        worktree.as_deref().or(channel_flag.as_deref()),
        channel.as_deref(),
        rollup_only,
    )?;
    dispatch_resolved_message(
        mode, &workspace, &store, &snapshot, pending, &sender, target, text, spec, flags, targets,
        channel, after, when,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_when_conditions(
    snapshot: &SidebarSnapshot,
    durable_agents: &[AgentState],
    expressions: &[String],
    scope: Option<&str>,
    channel: Option<&str>,
    rollup_only: bool,
) -> Result<Vec<rimz::message::WhenCondition>> {
    let now = Timestamp::now();
    expressions
        .iter()
        .map(|expression| {
            let fields = expression.split_whitespace().collect::<Vec<_>>();
            let [address, status, duration] = fields.as_slice() else {
                bail!(
                    "invalid --when `{expression}`; use `@handle <status> <duration>` (for example `@coder idle 58m`)"
                );
            };
            rimz::harness::target::require_mention(address)?;
            if rimz::harness::target::is_broadcast(address) {
                bail!("--when `{expression}` must name one agent; broadcasts are not supported");
            }
            let status = rimz::message::parse_when_status(status).map_err(anyhow::Error::msg)?;
            let dwell_secs =
                rimz::message::parse_when_duration(duration).map_err(anyhow::Error::msg)?;
            let targets = message_dispatch::queue_targets(
                snapshot,
                Some(durable_agents),
                address,
                scope,
                channel,
                rollup_only,
            )
            .map_err(|err| map_queue_target_err(address, err))?;
            if targets.len() != 1 {
                bail!(
                    "--when `{expression}` must resolve to exactly one agent; matched {}",
                    targets.len()
                );
            }
            let target = targets[0];
            let agent = target.agent().ok_or_else(|| {
                anyhow::anyhow!(
                    "--when `{expression}` must resolve to an agent with lifecycle state"
                )
            })?;
            let mut condition = rimz::message::WhenCondition {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                agent_name: agent.name.clone(),
                address: message_dispatch::handle_for_target(snapshot, &target),
                status,
                dwell_secs,
                met_at: None,
            };
            if rimz::message::when_condition_state(&condition, &snapshot.agents, now)
                == rimz::message::WhenState::Met
            {
                condition.met_at = Some(now);
            }
            Ok(condition)
        })
        .collect()
}

fn read_agent_context(
    workspace: &ResolvedWorkspace,
) -> Option<Vec<rimz::store::agent_context::AgentContextRecord>> {
    rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .ok()
        .map(|runtime| rimz::store::agent_context::read_all(&runtime))
}

#[allow(clippy::too_many_arguments)]
fn resolve_after_conditions(
    snapshot: &SidebarSnapshot,
    durable_agents: &[AgentState],
    recipients: &[message_dispatch::QueueTarget<'_>],
    addresses: &[String],
    scope: Option<&str>,
    channel: Option<&str>,
    rollup_only: bool,
    gate: DeliveryGate,
    pending: &[MessageRecord],
) -> Result<Vec<rimz::message::AfterCondition>> {
    let now = Timestamp::now();
    addresses
        .iter()
        .map(|address| {
            rimz::harness::target::require_mention(address)?;
            if rimz::harness::target::is_broadcast(address) {
                bail!("--after `{address}` must name one agent; broadcasts are not supported");
            }
            let targets = message_dispatch::queue_targets(
                snapshot,
                Some(durable_agents),
                address,
                scope,
                channel,
                rollup_only,
            )
            .map_err(|err| map_queue_target_err(address, err))?;
            if targets.len() != 1 {
                bail!(
                    "--after `{address}` must resolve to exactly one agent; matched {}",
                    targets.len()
                );
            }
            let target = targets[0];
            let agent = target.agent().ok_or_else(|| {
                anyhow::anyhow!(
                    "--after `{address}` must resolve to an agent with lifecycle state"
                )
            })?;
            if recipients.iter().any(|recipient| {
                recipient.agent().is_some_and(|recipient| {
                    rimz::message::card_matches(
                        &agent.kind,
                        &agent.agent_id,
                        agent.name.as_deref(),
                        &recipient.kind,
                        &recipient.agent_id,
                        recipient.name.as_deref(),
                    )
                })
            }) {
                bail!(
                    "--after `{address}` names the message recipient; use --on to gate on the recipient's turn"
                );
            }
            let mut condition = rimz::message::AfterCondition {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                agent_name: agent.name.clone(),
                address: message_dispatch::handle_for_target(snapshot, &target),
                met_at: None,
            };
            if rimz::message::after_condition_open(
                &condition,
                gate,
                &snapshot.agents,
                pending,
                now,
            ) {
                condition.met_at = Some(now);
            }
            Ok(condition)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_message_targets<'a>(
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
) -> Result<Option<Vec<message_dispatch::QueueTarget<'a>>>> {
    match message_dispatch::queue_targets(
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
                return crate::cli::agents_cmd::create_on_miss(
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
pub(super) fn dispatch_resolved_message(
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
    targets: Vec<message_dispatch::QueueTarget<'_>>,
    channel: Option<String>,
    after: Vec<rimz::message::AfterCondition>,
    when: Vec<rimz::message::WhenCondition>,
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
        return Err(crate::cli::ambiguous_fanout(verb, &target, &labels));
    }
    let caller_identity = send::agent_caller_identity();
    let reply_targets = if spec.wait.is_on() {
        Some(resolve_reply_targets(
            store,
            snapshot,
            &targets,
            caller_identity.as_ref(),
        )?)
    } else {
        None
    };
    let text = if targets.len() > 1 || rimz::harness::target::is_broadcast(&target) {
        rimz::harness::target::group_prefixed(&target, &text)
    } else {
        text
    };
    let wait_started = std::time::Instant::now();
    let wait_base = if spec.wait.is_on() {
        Some(store.wait_fold_base()?)
    } else {
        None
    };
    let in_reply_to = sender_turn_opened_by(snapshot, sender);
    let result = message_dispatch::dispatch_for_targets(
        DispatchContext {
            workspace,
            store,
            snapshot,
            pending: matches!(mode, MessageDispatchMode::Boundary).then_some(&mut pending),
            scope_channel: channel.as_deref(),
            sender,
            automated: spec.automated,
            reply_wait: spec.wait.is_on(),
            in_reply_to: &in_reply_to,
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
                after,
                when,
            },
        },
    )?;
    if let Some(reply_targets) = reply_targets {
        if reply_targets.len() != result.outcomes.len() {
            bail!("--wait requires one dispatched message per agent target");
        }
        let legs = reply_targets
            .into_iter()
            .zip(&result.outcomes)
            .map(|(target, outcome)| reply::Leg::new(target, outcome, wait_base.unwrap_or(0)))
            .collect();
        return reply::wait_for_replies(
            store,
            &workspace.session_name,
            legs,
            matches!(mode, MessageDispatchMode::Steer),
            spec.wait,
            spec.wait.deadline_from(wait_started),
            caller_identity,
        );
    }
    report_dispatch(
        match mode {
            MessageDispatchMode::Steer => ReportMode::Steer,
            MessageDispatchMode::Boundary => ReportMode::Boundary,
        },
        &target,
        targets.len(),
        &result.outcomes,
        &result.compacted,
    )
}

fn resolve_reply_targets(
    store: &rimz::Store,
    snapshot: &SidebarSnapshot,
    targets: &[message_dispatch::QueueTarget<'_>],
    caller_identity: Option<&(AgentKind, String)>,
) -> Result<Vec<reply::ReplyTarget>> {
    let mut reply_targets = Vec::with_capacity(targets.len());
    let mut checked_kinds = BTreeSet::new();
    let guard_records = if caller_identity.is_some() {
        Some((store.list_messages()?, store.list_message_history()?))
    } else {
        None
    };
    for resolved in targets {
        let label = resolved.label(snapshot);
        let identity = resolved.agent().ok_or_else(|| {
            anyhow::anyhow!(
                "--wait requires an agent with lifecycle state; `{label}` is only a pane target"
            )
        })?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| {
                rimz::message::card_matches(
                    &identity.kind,
                    &identity.agent_id,
                    identity.name.as_deref(),
                    &agent.kind,
                    &agent.agent_id,
                    agent.name.as_deref(),
                )
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--wait requires a live agent with lifecycle state; `{label}` is not running"
                )
            })?;
        let adapter = rimz::agents::find_adapter(agent.kind.as_str())
            .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
        if checked_kinds.insert(agent.kind.clone()) {
            preflight_reply_hooks(agent, adapter)?;
        }
        if let (Some((self_kind, self_name)), Some((live, history))) =
            (caller_identity, guard_records.as_ref())
            && let Some(cycle) = rimz::message::wait_guard::wait_cycle(
                live,
                history,
                &snapshot.agents,
                self_kind,
                self_name,
                agent,
            )
        {
            return Err(wait_cycle_error(&cycle, &label));
        }
        reply_targets.push(reply::ReplyTarget::new(agent, label, adapter));
    }
    Ok(reply_targets)
}

fn preflight_reply_hooks(
    agent: &AgentState,
    adapter: &dyn rimz::agents::AgentAdapter,
) -> Result<()> {
    if let Some(rimz::agents::ConcernCoverage::Unsupported { reason }) = adapter
        .descriptor()
        .concern_coverage(rimz::agents::IntegrationConcern::TurnLifecycle)
    {
        bail!(
            "--wait cannot use {}: a verified executable turn-lifecycle signal is required; {reason}",
            agent.kind
        );
    }
    if !adapter.hooks_installed() {
        bail!(
            "--wait requires {} hooks so the reply turn can report its boundaries; run `rimz hooks install {}`",
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

fn wait_cycle_error(
    cycle: &[rimz::message::wait_guard::WaitCycleHop],
    target: &str,
) -> anyhow::Error {
    let fix = "finish your turn to answer it, or resend without --wait or with --wait=<duration>";
    let Some(first) = cycle.first() else {
        return anyhow::anyhow!("--wait would deadlock: {target} is your own agent; {fix}");
    };
    if cycle.len() == 1 {
        return anyhow::anyhow!(
            "--wait would deadlock: {} is waiting on your reply ({}); {fix}",
            first.handle,
            first.message_id
        );
    }
    let mut chain = cycle
        .iter()
        .map(|hop| hop.handle.as_str())
        .collect::<Vec<_>>();
    chain.push("you");
    anyhow::anyhow!(
        "--wait would deadlock: {} is an active reply-wait chain ({}); {fix}",
        chain.join(" → "),
        first.message_id
    )
}

fn sender_turn_opened_by(snapshot: &SidebarSnapshot, sender: &MessageSender) -> Vec<MessageId> {
    let MessageSender::Agent {
        kind,
        name: Some(name),
        ..
    } = sender
    else {
        return Vec::new();
    };
    snapshot
        .root_agents()
        .find(|agent| agent.kind == *kind && agent.name.as_deref() == Some(name))
        .and_then(|agent| agent.context.as_ref())
        .map(|context| context.turn_opened_by.clone())
        .unwrap_or_default()
}

#[derive(Clone, Copy)]
pub(super) enum ReportMode {
    Steer,
    Boundary,
}

pub(super) fn render_dispatch_outcome(outcome: &DispatchOutcome) -> Option<String> {
    match outcome {
        DispatchOutcome::Sent { label, message_id } => {
            Some(format!("sent to {label} ({message_id})"))
        }
        DispatchOutcome::Queued { label, message_id } => {
            Some(format!("queued for {label} ({message_id})"))
        }
        DispatchOutcome::CompactionPending { label, message_id } => Some(format!(
            "compacting {label}; queued {message_id} (delivers when compaction completes)"
        )),
        DispatchOutcome::SkippedWaiting { .. } => None,
    }
}

/// Report a unified dispatch. Boundary sends keep the old one-line-per-target
/// output; steer fan-out keeps the summary line and pending-ask bail.
#[allow(clippy::too_many_arguments)]
pub(super) fn report_dispatch(
    mode: ReportMode,
    target: &str,
    total: usize,
    outcomes: &[DispatchOutcome],
    compacted: &[String],
) -> Result<()> {
    match mode {
        ReportMode::Boundary => report_boundary(outcomes, compacted),
        ReportMode::Steer => report_steer(target, total, outcomes, compacted),
    }
}

fn report_boundary(outcomes: &[DispatchOutcome], compacted: &[String]) -> Result<()> {
    for label in compacted {
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("compacted {label}");
        }
    }
    for outcome in outcomes {
        if let Some(line) = render_dispatch_outcome(outcome) {
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn report_steer(
    target: &str,
    total: usize,
    outcomes: &[DispatchOutcome],
    compacted: &[String],
) -> Result<()> {
    let mut sent = Vec::new();
    let mut sent_labels = Vec::new();
    let mut queued = Vec::new();
    let mut pending = Vec::new();
    for outcome in outcomes {
        match outcome {
            DispatchOutcome::Sent { label, message_id } => {
                sent.push(format!("{label} ({message_id})"));
                sent_labels.push(label.as_str());
            }
            DispatchOutcome::Queued { label, message_id } => {
                queued.push(format!("{label} ({message_id})"));
            }
            DispatchOutcome::CompactionPending { label, message_id } => {
                queued.push(format!("{label} ({message_id}; waiting for compaction)"));
            }
            DispatchOutcome::SkippedWaiting { label, message_id } => {
                pending.push(format!("{label} ({message_id})"));
            }
        }
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
            Some(DispatchOutcome::CompactionPending { .. }) => {
                unreachable!("compaction-pending outcomes are included in the queued summary")
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

pub(super) fn print_compacted_if_needed(label: &str, compacted: &[String]) {
    if compacted.iter().any(|compacted| compacted == label) {
        #[expect(clippy::print_stdout, reason = "message compact confirmation")]
        {
            println!("compacted {label}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_sender_inherits_exact_named_sessions_turn_openers() {
        let now = Timestamp::UNIX_EPOCH;
        let opener = MessageId::parse("msg_0123456789abcdef").unwrap();
        let mut agent = AgentState::stub("codex", "sess-1", rimz::agents::AgentStatus::Running);
        agent.name = Some("coder".to_owned());
        let mut context = rimz::store::agent_context::empty_context("codex", now);
        context.turn_opened_by = vec![opener.clone()];
        agent.context = Some(context);
        let snapshot = SidebarSnapshot::build_with_agents(
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/replies")),
            vec![agent],
            now,
        );

        let sender = MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: Some("coder".to_owned()),
            profile: None,
            role: None,
            channel: Some("chat".to_owned()),
        };
        assert_eq!(sender_turn_opened_by(&snapshot, &sender), vec![opener]);

        let unnamed = MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: None,
            channel: Some("chat".to_owned()),
        };
        assert!(sender_turn_opened_by(&snapshot, &unnamed).is_empty());
        assert!(sender_turn_opened_by(&snapshot, &MessageSender::Human).is_empty());
    }
}
