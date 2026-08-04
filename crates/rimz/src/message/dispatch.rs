//! Resolve one owned message request, persist it, and order target fan-out.
//!
//! This module owns live-plus-durable target resolution, rollup-only selection,
//! context folding, condition binding, hook preflight, reply causality, record
//! construction, and the park-vs-live decision. Live attempts delegate receiver
//! recovery to [`super::deliver`]. Agent-originated broadcasts exclude the
//! caller after address resolution, before any fan-out work begins.

use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, MessageId, MuxName};
use crate::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    WhenCondition, command_submit_delay_from_env, message_interval_from_env, queue_head,
};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;
use crate::{Store, TargetErr};

use super::reply::{PreparationTarget, ReplyJoin, ReplyPreparation, ReplyPrepareErr, ReplyWait};
use super::{deliver, send};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhenRequest {
    pub address: String,
    pub status: AgentStatus,
    pub dwell_secs: u64,
    pub expression: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyRequest {
    pub join: ReplyJoin,
    pub caller_identity: Option<(AgentKind, String)>,
}

#[derive(Clone, Debug)]
pub enum DispatchMode {
    Steer {
        enter: bool,
        force: bool,
        /// An explicit threshold for this dispatch; `None` inherits the
        /// `[harness] smart_compact` machine default in [`dispatch`].
        auto_compact: Option<AutoCompact>,
    },
    Boundary {
        enter: bool,
        gate: DeliveryGate,
        force: bool,
        /// An explicit threshold for this dispatch; `None` inherits the
        /// `[harness] smart_compact` machine default in [`dispatch`].
        auto_compact: Option<AutoCompact>,
        not_before: Option<Timestamp>,
        after: Vec<String>,
        when: Vec<WhenRequest>,
    },
}

impl DispatchMode {
    fn steer(&self) -> bool {
        matches!(self, Self::Steer { .. })
    }

    fn gate(&self) -> DeliveryGate {
        match self {
            Self::Steer { .. } => DeliveryGate::Any,
            Self::Boundary { gate, .. } => *gate,
        }
    }

    fn force(&self) -> bool {
        match self {
            Self::Steer { force, .. } | Self::Boundary { force, .. } => *force,
        }
    }

    fn resolve_auto_compact(&mut self, default: Option<AutoCompact>) {
        let auto_compact = match self {
            Self::Steer { auto_compact, .. } | Self::Boundary { auto_compact, .. } => auto_compact,
        };
        *auto_compact = (*auto_compact).or(default);
    }

    fn needs_agent_context(&self) -> bool {
        match self {
            Self::Steer { auto_compact, .. } => auto_compact.is_some(),
            Self::Boundary {
                auto_compact,
                after,
                when,
                ..
            } => auto_compact.is_some() || !after.is_empty() || !when.is_empty(),
        }
    }
}

pub struct DispatchRequest {
    pub target: String,
    pub text: String,
    pub target_scope: Option<String>,
    pub current_channel: Option<String>,
    pub caller: Option<crate::harness::ancestry::LaunchCallerEnv>,
    pub sender: MessageSender,
    pub automated: bool,
    pub allow_fanout: bool,
    pub reply: Option<ReplyRequest>,
    pub mux: Option<MuxName>,
    pub mode: DispatchMode,
}

pub struct DispatchResult {
    pub outcomes: Vec<DispatchOutcome>,
    pub compacted: Vec<String>,
    pub reply: Option<ReplyWait>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Sent {
        label: String,
        message_id: MessageId,
    },
    Queued {
        label: String,
        message_id: MessageId,
        reason: Option<ParkReason>,
    },
    CompactionPending {
        label: String,
        message_id: MessageId,
    },
    SkippedWaiting {
        label: String,
        message_id: MessageId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkReason {
    Status(AgentStatus),
    WaitingOnPrompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionKind {
    After,
    When,
}

#[derive(Debug, thiserror::Error)]
pub enum ConditionErr {
    #[error("condition target `{address}` cannot be a broadcast")]
    Broadcast {
        kind: ConditionKind,
        address: String,
        expression: String,
    },
    #[error("condition target `{address}` resolved to {matched} agents")]
    Arity {
        kind: ConditionKind,
        address: String,
        expression: String,
        matched: usize,
    },
    #[error("condition target `{address}` has no lifecycle state")]
    NoLifecycle {
        kind: ConditionKind,
        address: String,
        expression: String,
    },
    #[error("after condition names the message recipient")]
    RecipientSelfReference { address: String },
    #[error("cannot resolve condition target `{address}`: {source}")]
    Target {
        kind: ConditionKind,
        address: String,
        expression: String,
        #[source]
        source: Box<TargetErr>,
    },
}

pub type Result<T> = std::result::Result<T, DispatchErr>;

#[derive(Debug, thiserror::Error)]
pub enum DispatchErr {
    #[error(transparent)]
    Recipient(#[from] TargetErr),
    #[error(
        "no other agents in the current channel{suffix}",
        suffix = channel
            .as_ref()
            .map(|channel| format!(" (`#{channel}`)"))
            .unwrap_or_default()
    )]
    NoPeers { channel: Option<String> },
    #[error("target `{target}` matched multiple agents")]
    Fanout {
        target: String,
        labels: Vec<String>,
        steer: bool,
    },
    #[error(transparent)]
    Condition(#[from] ConditionErr),
    #[error(transparent)]
    ReplyPreparation(#[from] ReplyPrepareErr),
    #[error(transparent)]
    Reply(#[from] super::reply::ReplyErr),
    #[error(transparent)]
    Resolution(#[from] crate::sidebar::produce::ProduceErr),
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Send(#[from] send::SendErr),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverErr),
    #[error("unknown agent kind `{0}`")]
    UnknownAgentKind(AgentKind),
    #[error(
        "queued delivery requires {kind} hooks so messages can deliver at turn boundaries; run `rimz hooks install {kind}`"
    )]
    HooksMissing { kind: AgentKind },
    #[error("{kind} hooks are installed but not trusted ({hooks}); {fix}")]
    HooksUntrusted {
        kind: AgentKind,
        hooks: String,
        fix: String,
    },
    #[error("`{label}` cannot receive now and has no durable session to park")]
    NoDurableSession { label: String },
}

pub fn dispatch(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mut request: DispatchRequest,
) -> Result<DispatchResult> {
    request.mode.resolve_auto_compact(
        crate::config::MachineConfig::load_lenient()
            .harness
            .smart_compact,
    );
    let boundary = !request.mode.steer();
    let mut pending = if boundary {
        store.list_messages()?
    } else {
        Vec::new()
    };
    let needs_context = request.mode.needs_agent_context()
        || matches!(request.sender, MessageSender::Agent { .. })
        || request.reply.is_some();
    let agent_context =
        needs_context.then(|| crate::store::agent_context::read_all(store.runtime_paths()));

    let mut cached_snapshot = boundary.then(|| store.snapshot_cached()).transpose()?;
    if let (Some(snapshot), Some(context)) = (&mut cached_snapshot, agent_context.as_ref()) {
        *snapshot = snapshot.clone().with_agent_context(context.clone());
    }
    let rollup_only = cached_snapshot.as_ref().is_some_and(|snapshot| {
        targets_all_park_without_live(
            snapshot,
            &request.target,
            request.target_scope.as_deref(),
            request.current_channel.as_deref(),
            &pending,
            request.mode.gate(),
            request.mode.force(),
        )
    });
    let mut snapshot = if rollup_only {
        // Rollup-only is computed solely from the cached snapshot above.
        cached_snapshot.expect("rollup-only proof requires cached snapshot")
    } else {
        crate::sidebar::produce::resolution_snapshot(workspace, store, request.mux)?
    };
    if !rollup_only && let Some(context) = agent_context {
        snapshot = snapshot.with_agent_context(context);
    }

    let durable_agents = durable_target_agents(store)?;
    let mut targets = resolve_targets(
        &snapshot,
        Some(&durable_agents),
        &request.target,
        request.target_scope.as_deref(),
        request.current_channel.as_deref(),
        rollup_only,
    )?;
    exclude_broadcast_caller(
        &request.target,
        &mut targets,
        &durable_agents,
        request.caller.as_ref(),
        request
            .target_scope
            .as_deref()
            .or(request.current_channel.as_deref()),
    )?;
    if targets.len() > 1
        && !request.allow_fanout
        && !crate::harness::target::is_broadcast(&request.target)
    {
        return Err(DispatchErr::Fanout {
            target: request.target,
            labels: targets
                .iter()
                .map(|target| target.label(&snapshot))
                .collect(),
            steer: request.mode.steer(),
        });
    }

    let resolution = ResolutionView {
        snapshot: &snapshot,
        durable_agents: &durable_agents,
        scope: request.target_scope.as_deref(),
        channel: request.current_channel.as_deref(),
        rollup_only,
    };
    let mode = prepare_mode(
        request.mode,
        resolution,
        &targets,
        &pending,
        &request.sender,
        request.automated,
    )?;
    let reply_join = request.reply.as_ref().map(|reply| reply.join);
    let reply_preparation = request
        .reply
        .map(|reply| {
            ReplyPreparation::new(
                store,
                &snapshot,
                targets.iter().map(|target| PreparationTarget {
                    agent: target.agent.as_ref(),
                    label: target.label(&snapshot),
                }),
                reply.caller_identity,
            )
        })
        .transpose()?;
    let text = if targets.len() > 1 || crate::harness::target::is_broadcast(&request.target) {
        crate::harness::target::group_prefixed(&request.target, &request.text)
    } else {
        request.text
    };
    let in_reply_to = turn_openers_for_sender(&snapshot, &request.sender);
    let mut state = DispatchState {
        workspace,
        store,
        snapshot: &snapshot,
        pending: &mut pending,
        track_pending: boundary,
        scope_channel: request.current_channel.as_deref(),
        reply_wait: reply_preparation.is_some(),
        in_reply_to: &in_reply_to,
    };
    let (outcomes, compacted) = dispatch_targets(&mut state, &targets, &text, &mode)?;
    let reply = reply_preparation
        .map(|preparation| {
            // Preparation exists only when the same request supplied a join mode.
            preparation.attach(
                &outcomes,
                mode.steer,
                reply_join.expect("reply preparation carries join mode"),
            )
        })
        .transpose()?;
    Ok(DispatchResult {
        outcomes,
        compacted,
        reply,
    })
}

#[derive(Clone, Debug)]
struct ResolvedTarget {
    pane: Option<PaneAgent>,
    agent: Option<AgentState>,
}

impl ResolvedTarget {
    fn label(&self, snapshot: &SidebarSnapshot) -> String {
        if let Some(agent) = self.agent.as_ref() {
            let peers = crate::harness::target::addressable_agents(snapshot);
            crate::harness::target::agent_handle(agent, &peers, true)
        } else if let Some(pane) = self.pane.as_ref() {
            format!("@{}", pane.label())
        } else {
            "@agent".to_owned()
        }
    }

    fn bound<'a>(&self, snapshot: &'a SidebarSnapshot) -> Option<&'a AgentState> {
        self.pane
            .as_ref()
            .and_then(|pane| crate::harness::target::pane_binding(snapshot, pane, None))
            .and_then(|binding| binding.exact_agent)
    }
}

fn exclude_broadcast_caller(
    raw: &str,
    targets: &mut Vec<ResolvedTarget>,
    durable_agents: &[AgentState],
    caller_env: Option<&crate::harness::ancestry::LaunchCallerEnv>,
    channel: Option<&str>,
) -> Result<()> {
    if !crate::harness::target::is_broadcast(raw) {
        return Ok(());
    }
    let Some(caller) = caller_env.and_then(|caller| {
        crate::harness::ancestry::resolve_launch_caller(durable_agents, caller).ok()
    }) else {
        return Ok(());
    };
    let caller_pane = caller.pane.as_ref().map(|pane| &pane.pane_id);
    targets.retain(|target| {
        if target
            .agent
            .as_ref()
            .is_some_and(|agent| caller.card_ref().matches(agent.card_ref()))
        {
            return false;
        }
        target.agent.is_some()
            || !target
                .pane
                .as_ref()
                .is_some_and(|pane| Some(&pane.pane_id) == caller_pane)
    });
    if targets.is_empty() {
        return Err(DispatchErr::NoPeers {
            channel: channel.map(ToOwned::to_owned),
        });
    }
    Ok(())
}

fn resolve_targets(
    snapshot: &SidebarSnapshot,
    durable_agents: Option<&[AgentState]>,
    raw: &str,
    scope: Option<&str>,
    channel: Option<&str>,
    rollup_only: bool,
) -> std::result::Result<Vec<ResolvedTarget>, TargetErr> {
    if rollup_only {
        let agents = crate::harness::target::resolve_many(snapshot, raw, scope, channel)
            .or_else(|err| durable_targets(snapshot, durable_agents, raw, scope, channel, err))?;
        return Ok(combine_targets(snapshot, agents, Vec::new()));
    }
    let agent_result = crate::harness::target::resolve_many(snapshot, raw, scope, channel);
    let pane_result = crate::harness::target::resolve_targets(snapshot, raw, scope, channel);
    match (agent_result, pane_result) {
        (Ok(agents), Ok(panes)) => Ok(combine_targets(snapshot, agents, panes)),
        (Ok(agents), Err(_)) => Ok(combine_targets(snapshot, agents, Vec::new())),
        (Err(_), Ok(panes)) => Ok(combine_targets(snapshot, Vec::new(), panes)),
        (Err(err), Err(_)) => durable_targets(snapshot, durable_agents, raw, scope, channel, err)
            .map(|agents| combine_targets(snapshot, agents, Vec::new())),
    }
}

fn durable_target_agents(store: &Store) -> Result<Vec<AgentState>> {
    Ok(store
        .runtime_projection(crate::RuntimeScope::Audit)?
        .agents
        .into_iter()
        .filter(|agent| !agent.is_provider_subagent() && agent.ended_at.is_none())
        .collect())
}

fn durable_targets<'a>(
    snapshot: &SidebarSnapshot,
    durable_agents: Option<&'a [AgentState]>,
    raw: &str,
    scope: Option<&str>,
    channel: Option<&str>,
    live_err: TargetErr,
) -> std::result::Result<Vec<&'a AgentState>, TargetErr> {
    let Some(durable_agents) = durable_agents else {
        return Err(live_err);
    };
    let candidates = durable_agents
        .iter()
        .filter(|agent| !crate::harness::target::shadowed_by_pane_owner(snapshot, agent))
        .collect::<Vec<_>>();
    crate::harness::target::resolve_agents(raw, scope, channel, &candidates)
}

fn combine_targets(
    snapshot: &SidebarSnapshot,
    agents: Vec<&AgentState>,
    panes: Vec<&PaneAgent>,
) -> Vec<ResolvedTarget> {
    let mut used_panes = vec![false; panes.len()];
    let mut targets = Vec::new();
    for agent in agents {
        let pane_index = panes
            .iter()
            .enumerate()
            .find(|(index, pane)| {
                !used_panes[*index]
                    && crate::harness::target::pane_binding(snapshot, pane, None)
                        .is_some_and(|binding| binding.matches_agent(agent))
            })
            .map(|(index, _)| index);
        let pane = pane_index.map(|index| {
            used_panes[index] = true;
            panes[index].clone()
        });
        targets.push(ResolvedTarget {
            pane,
            agent: Some(agent.clone()),
        });
    }
    for (index, pane) in panes.into_iter().enumerate() {
        if used_panes[index] {
            continue;
        }
        let binding = crate::harness::target::pane_binding(snapshot, pane, None);
        targets.push(ResolvedTarget {
            pane: Some(pane.clone()),
            agent: binding.and_then(|binding| binding.agent).cloned(),
        });
    }
    targets
}

fn targets_all_park_without_live(
    snapshot: &SidebarSnapshot,
    raw: &str,
    scope: Option<&str>,
    channel: Option<&str>,
    pending: &[MessageRecord],
    gate: DeliveryGate,
    force: bool,
) -> bool {
    if crate::harness::target::is_broadcast(raw) {
        return false;
    }
    let Ok(agents) = crate::harness::target::resolve_many(snapshot, raw, scope, channel) else {
        return false;
    };
    let now = Timestamp::now();
    agents
        .iter()
        .all(|agent| !agent_needs_live_resolution(pending, agent, gate, force, now))
}

fn agent_needs_live_resolution(
    pending: &[MessageRecord],
    agent: &AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
) -> bool {
    agent.agent_id.is_provisional()
        || crate::agents::spec_by_kind(agent.kind.as_str())
            .is_some_and(|definition| definition.capabilities.registers_lazily)
        || (deliver::receiver_readiness(agent, gate, force, now).accepts_prompt()
            && queue_head(
                pending.iter(),
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
                now,
            )
            .is_none())
}

struct PreparedMode {
    steer: bool,
    draft: send::MessageDraft,
}

#[derive(Clone, Copy)]
struct ResolutionView<'a> {
    snapshot: &'a SidebarSnapshot,
    durable_agents: &'a [AgentState],
    scope: Option<&'a str>,
    channel: Option<&'a str>,
    rollup_only: bool,
}

fn prepare_mode(
    mode: DispatchMode,
    resolution: ResolutionView<'_>,
    recipients: &[ResolvedTarget],
    pending: &[MessageRecord],
    sender: &MessageSender,
    automated: bool,
) -> Result<PreparedMode> {
    match mode {
        DispatchMode::Steer {
            enter,
            force,
            auto_compact,
        } => Ok(PreparedMode {
            steer: true,
            draft: send::MessageDraft {
                body: MessageBody::Prompt,
                enter,
                gate: DeliveryGate::Any,
                sender: sender.clone(),
                automated,
                force,
                auto_compact,
                not_before: None,
                after: Vec::new(),
                when: Vec::new(),
            },
        }),
        DispatchMode::Boundary {
            enter,
            gate,
            force,
            auto_compact,
            not_before,
            after,
            when,
        } => Ok(PreparedMode {
            steer: false,
            draft: send::MessageDraft {
                body: MessageBody::Prompt,
                enter,
                gate,
                sender: sender.clone(),
                automated,
                force,
                auto_compact,
                not_before,
                after: resolve_after(resolution, recipients, &after, gate, pending)?,
                when: resolve_when(resolution, &when)?,
            },
        }),
    }
}

fn resolve_after(
    resolution: ResolutionView<'_>,
    recipients: &[ResolvedTarget],
    addresses: &[String],
    gate: DeliveryGate,
    pending: &[MessageRecord],
) -> Result<Vec<AfterCondition>> {
    let now = Timestamp::now();
    addresses
        .iter()
        .map(|address| {
            let target =
                resolve_condition_target(resolution, ConditionKind::After, address, address)?;
            // Condition target resolution rejects pane-only targets.
            let agent = target.agent.as_ref().expect("condition target validated");
            if recipients.iter().any(|recipient| {
                recipient
                    .agent
                    .as_ref()
                    .is_some_and(|recipient| agent.card_ref().matches(recipient.card_ref()))
            }) {
                return Err(ConditionErr::RecipientSelfReference {
                    address: address.clone(),
                }
                .into());
            }
            let mut condition = AfterCondition {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                agent_name: agent.name.clone(),
                address: target.label(resolution.snapshot),
                met_at: None,
            };
            if deliver::evaluate_after_condition(
                &condition,
                gate,
                pending,
                resolution.snapshot,
                now,
            )
            .check
            .met
            {
                condition.met_at = Some(now);
            }
            Ok(condition)
        })
        .collect()
}

fn resolve_when(
    resolution: ResolutionView<'_>,
    requests: &[WhenRequest],
) -> Result<Vec<WhenCondition>> {
    let now = Timestamp::now();
    let delivery_window = crate::message::delivery_window_from_env();
    requests
        .iter()
        .map(|request| {
            let target = resolve_condition_target(
                resolution,
                ConditionKind::When,
                &request.address,
                &request.expression,
            )?;
            // Condition target resolution rejects pane-only targets.
            let agent = target.agent.as_ref().expect("condition target validated");
            let mut condition = WhenCondition {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                agent_name: agent.name.clone(),
                address: target.label(resolution.snapshot),
                status: request.status,
                dwell_secs: request.dwell_secs,
                met_at: None,
            };
            if deliver::evaluate_when_condition(
                &condition,
                resolution.snapshot,
                now,
                delivery_window,
            )
            .check
            .met
            {
                condition.met_at = Some(now);
            }
            Ok(condition)
        })
        .collect()
}

fn resolve_condition_target(
    resolution: ResolutionView<'_>,
    kind: ConditionKind,
    address: &str,
    expression: &str,
) -> Result<ResolvedTarget> {
    if crate::harness::target::is_broadcast(address) {
        return Err(ConditionErr::Broadcast {
            kind,
            address: address.to_owned(),
            expression: expression.to_owned(),
        }
        .into());
    }
    let targets = resolve_targets(
        resolution.snapshot,
        Some(resolution.durable_agents),
        address,
        resolution.scope,
        resolution.channel,
        resolution.rollup_only,
    )
    .map_err(|source| ConditionErr::Target {
        kind,
        address: address.to_owned(),
        expression: expression.to_owned(),
        source: Box::new(source),
    })?;
    if targets.len() != 1 {
        return Err(ConditionErr::Arity {
            kind,
            address: address.to_owned(),
            expression: expression.to_owned(),
            matched: targets.len(),
        }
        .into());
    }
    // Arity was checked immediately above.
    let target = targets.into_iter().next().expect("one condition target");
    if target.agent.is_none() {
        return Err(ConditionErr::NoLifecycle {
            kind,
            address: address.to_owned(),
            expression: expression.to_owned(),
        }
        .into());
    }
    Ok(target)
}

struct DispatchState<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    snapshot: &'a SidebarSnapshot,
    pending: &'a mut Vec<MessageRecord>,
    track_pending: bool,
    scope_channel: Option<&'a str>,
    reply_wait: bool,
    in_reply_to: &'a [MessageId],
}

impl DispatchState<'_> {
    fn enqueue(
        &self,
        target: &ResolvedTarget,
        pane: Option<&PaneAgent>,
        text: &str,
        mode: &PreparedMode,
        handle: &str,
    ) -> Result<MessageRecord> {
        let recipient = match (target.agent.as_ref(), pane) {
            (Some(agent), pane) => send::Recipient::Agent { agent, pane },
            (None, Some(pane)) => send::Recipient::Pane {
                pane,
                bound: target.bound(self.snapshot),
            },
            (None, None) => {
                return Err(DispatchErr::NoDurableSession {
                    label: handle.to_owned(),
                });
            }
        };
        let message = mode
            .draft
            .record(
                self.workspace.workspace_id.clone(),
                recipient,
                self.scope_channel,
                text,
                Some(handle),
            )
            .with_reply_wait(self.reply_wait)
            .with_in_reply_to(self.in_reply_to.to_vec());
        self.store
            .queue_message(&message, &self.workspace.session_name)?;
        Ok(message)
    }
}

fn dispatch_targets(
    state: &mut DispatchState<'_>,
    targets: &[ResolvedTarget],
    text: &str,
    mode: &PreparedMode,
) -> Result<(Vec<DispatchOutcome>, Vec<String>)> {
    let now = Timestamp::now();
    let decisions = targets
        .iter()
        .map(|target| {
            dispatch_decision(state.snapshot, state.pending.as_slice(), target, mode, now)
        })
        .collect::<Vec<_>>();
    let mut live_send = send::LiveSend {
        force: mode.draft.force,
        steer: mode.steer,
        pacer: send::Pacer::new(message_interval_from_env()),
        command_submit_delay: command_submit_delay_from_env(),
    };
    let mut preflighted_kinds = BTreeSet::new();
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut compacted = Vec::new();
    for (target, decision) in targets.iter().zip(decisions) {
        if matches!(decision, DispatchDecision::Parked { .. }) {
            let agent = target
                .agent
                .as_ref()
                .ok_or_else(|| DispatchErr::NoDurableSession {
                    label: target.label(state.snapshot),
                })?;
            if preflighted_kinds.insert(agent.kind.clone()) {
                preflight_queue_hooks(agent)?;
            }
        }
        outcomes.push(dispatch_one(
            state,
            &mut live_send,
            &mut compacted,
            target,
            text,
            mode,
            decision,
        )?);
    }
    deliver::register_message_wake(state.workspace, state.store)?;
    Ok((outcomes, compacted))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchDecision {
    Live,
    Parked { reason: Option<ParkReason> },
}

fn dispatch_decision(
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    target: &ResolvedTarget,
    mode: &PreparedMode,
    now: Timestamp,
) -> DispatchDecision {
    if mode.steer {
        return if target.pane.is_some() {
            DispatchDecision::Live
        } else {
            DispatchDecision::Parked { reason: None }
        };
    }
    if mode.draft.not_before.is_some()
        || !mode
            .draft
            .after
            .iter()
            .all(|condition| condition.met_at.is_some())
        || !mode
            .draft
            .when
            .iter()
            .all(|condition| condition.met_at.is_some())
    {
        return DispatchDecision::Parked { reason: None };
    }
    let readiness_agent = if target.pane.is_some() {
        target.bound(snapshot)
    } else {
        target.agent.as_ref()
    };
    if let Some(agent) = readiness_agent {
        let readiness = deliver::receiver_readiness(agent, mode.draft.gate, mode.draft.force, now);
        if !readiness.accepts_prompt() {
            let reason = if readiness.waiting {
                ParkReason::WaitingOnPrompt
            } else {
                ParkReason::Status(readiness.status)
            };
            return DispatchDecision::Parked {
                reason: Some(reason),
            };
        }
    }
    if target.pane.is_none() {
        return DispatchDecision::Parked { reason: None };
    }
    if target.agent.as_ref().is_some_and(|agent| {
        queue_head(
            pending.iter(),
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
            now,
        )
        .is_some()
    }) {
        return DispatchDecision::Parked { reason: None };
    }
    DispatchDecision::Live
}

fn dispatch_one(
    state: &mut DispatchState<'_>,
    live_send: &mut send::LiveSend,
    compacted: &mut Vec<String>,
    target: &ResolvedTarget,
    text: &str,
    mode: &PreparedMode,
    decision: DispatchDecision,
) -> Result<DispatchOutcome> {
    let handle = target.label(state.snapshot);
    if let DispatchDecision::Parked { reason } = decision {
        return dispatch_parked(state, target, text, mode, handle, reason);
    }
    let Some(pane) = target.pane.as_ref() else {
        return Err(DispatchErr::NoDurableSession { label: handle });
    };
    let bound = target.bound(state.snapshot);
    let message = state.enqueue(target, Some(pane), text, mode, &handle)?;
    let message_id = message.message_id.clone();
    let policy = if mode.steer {
        deliver::DeliveryPolicy::Steer {
            force: mode.draft.force,
        }
    } else {
        deliver::DeliveryPolicy::Boundary
    };
    match deliver::execute_attempt(
        deliver::Attempt {
            workspace: state.workspace,
            store: state.store,
            snapshot: state.snapshot,
            target: pane,
            bound,
            records: std::slice::from_ref(&message),
            source: deliver::AttemptSource::Fresh {
                durable_receiver: target.agent.is_some(),
            },
            policy,
        },
        live_send,
    )? {
        deliver::AttemptOutcome::Sent {
            compacted: was_compacted,
        } => {
            if was_compacted {
                compacted.push(handle.clone());
            }
            Ok(DispatchOutcome::Sent {
                label: handle,
                message_id,
            })
        }
        deliver::AttemptOutcome::SkippedWaiting => Ok(DispatchOutcome::SkippedWaiting {
            label: handle,
            message_id,
        }),
        deliver::AttemptOutcome::Queued => {
            push_pending(state, message);
            Ok(DispatchOutcome::Queued {
                label: handle,
                message_id,
                reason: None,
            })
        }
        deliver::AttemptOutcome::CompactionPending => {
            push_pending(state, message);
            Ok(DispatchOutcome::CompactionPending {
                label: handle,
                message_id,
            })
        }
    }
}

fn dispatch_parked(
    state: &mut DispatchState<'_>,
    target: &ResolvedTarget,
    text: &str,
    mode: &PreparedMode,
    handle: String,
    reason: Option<ParkReason>,
) -> Result<DispatchOutcome> {
    let message = state.enqueue(target, None, text, mode, &handle)?;
    let message_id = message.message_id.clone();
    push_pending(state, message);
    Ok(DispatchOutcome::Queued {
        label: handle,
        message_id,
        reason,
    })
}

fn push_pending(state: &mut DispatchState<'_>, message: MessageRecord) {
    if state.track_pending {
        state.pending.push(message);
    }
}

fn preflight_queue_hooks(agent: &AgentState) -> Result<()> {
    let Some(adapter) = crate::agents::find_definition(agent.kind.as_str()) else {
        return Err(DispatchErr::UnknownAgentKind(agent.kind.clone()));
    };
    match crate::agents::preflight_hooks(adapter, crate::agents::TurnLifecycleNeed::None) {
        Ok(()) => Ok(()),
        Err(crate::agents::HookPreflightErr::HooksMissing) => Err(DispatchErr::HooksMissing {
            kind: agent.kind.clone(),
        }),
        Err(crate::agents::HookPreflightErr::HooksUntrusted { hooks, fix }) => {
            Err(DispatchErr::HooksUntrusted {
                kind: agent.kind.clone(),
                hooks,
                fix,
            })
        }
        Err(crate::agents::HookPreflightErr::TurnLifecycleUnsupported { .. }) => {
            unreachable!("queue hook preflight requests no lifecycle coverage")
        }
    }
}

fn turn_openers_for_sender(snapshot: &SidebarSnapshot, sender: &MessageSender) -> Vec<MessageId> {
    let MessageSender::Agent {
        kind,
        name: Some(name),
        ..
    } = sender
    else {
        return Vec::new();
    };
    snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .find(|agent| agent.kind == *kind && agent.name.as_deref() == Some(name))
        .and_then(|agent| agent.context.as_ref())
        .map(|context| context.turn_opened_by.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::AgentStatus;
    use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
    use crate::pane::PaneRef;

    #[test]
    fn condition_broadcast_is_typed_before_resolution() {
        let snapshot = snapshot_with_panes(Vec::new(), Vec::new());
        let err = resolve_condition_target(
            ResolutionView {
                snapshot: &snapshot,
                durable_agents: &[],
                scope: None,
                channel: None,
                rollup_only: false,
            },
            ConditionKind::When,
            "@all",
            "@all idle 1m",
        )
        .expect_err("broadcast condition must fail");
        assert!(matches!(
            err,
            DispatchErr::Condition(ConditionErr::Broadcast {
                kind: ConditionKind::When,
                ..
            })
        ));
    }

    #[test]
    fn provisional_pane_skips_readiness_gate() {
        let launch = agent("launch_pending", AgentStatus::Running);
        let pane = pane_only("terminal_1", "coder");
        let snapshot = snapshot_with_panes(vec![launch.clone()], vec![pane.clone()]);
        let binding = crate::harness::target::pane_binding(&snapshot, &pane, None).unwrap();
        assert_eq!(
            binding.kind,
            crate::harness::target::PaneBindingKind::Provisional
        );
        let target = ResolvedTarget {
            pane: Some(pane),
            agent: Some(launch),
        };
        assert!(target.bound(&snapshot).is_none());
        let mode = PreparedMode {
            steer: false,
            draft: send::MessageDraft {
                body: MessageBody::Prompt,
                enter: true,
                gate: DeliveryGate::Done,
                sender: MessageSender::Human,
                automated: false,
                force: false,
                auto_compact: None,
                not_before: None,
                after: Vec::new(),
                when: Vec::new(),
            },
        };

        assert_eq!(
            dispatch_decision(&snapshot, &[], &target, &mode, now()),
            DispatchDecision::Live
        );
    }

    #[test]
    fn agent_sender_inherits_exact_named_sessions_turn_openers() {
        let opener = MessageId::parse("msg_0123456789abcdef").unwrap();
        let mut agent = agent("sess-1", AgentStatus::Running);
        agent.name = Some("coder".to_owned());
        let mut context = crate::agents::AgentContext::new("codex", now());
        context.turn_opened_by = vec![opener.clone()];
        agent.context = Some(context);
        let snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now());
        let sender = MessageSender::Agent {
            kind: AgentKind::new_unchecked("claude"),
            name: Some("coder".to_owned()),
            profile: None,
            role: None,
            channel: Some("chat".to_owned()),
        };
        assert_eq!(turn_openers_for_sender(&snapshot, &sender), vec![opener]);
        assert!(turn_openers_for_sender(&snapshot, &MessageSender::Human).is_empty());
    }

    #[test]
    fn co_resident_session_resolves_to_one_pane_backed_recipient() {
        let older = resident("sess-older", "terminal_1");
        let owner = resident("sess-owner", "terminal_1");
        let durable = vec![older.clone(), owner.clone()];
        let pane = owner_pane("sess-owner", Some("coder"));
        let snapshot = snapshot_with_panes(vec![older, owner], vec![pane]);

        let targets = resolve_targets(
            &snapshot,
            Some(&durable),
            "@coder",
            None,
            Some("project"),
            false,
        )
        .unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0]
                .agent
                .as_ref()
                .map(|agent| agent.agent_id.as_str()),
            Some("sess-owner")
        );
        assert!(targets[0].pane.is_some());
    }

    #[test]
    fn durable_fallback_drops_shadowed_co_resident_session() {
        let durable = vec![
            resident("sess-older", "terminal_1"),
            resident("sess-owner", "terminal_1"),
        ];
        // The live pane deliberately lacks the role so resolution falls back
        // to the audit-scope durable candidates.
        let snapshot = snapshot_with_panes(Vec::new(), vec![owner_pane("sess-owner", None)]);

        for rollup_only in [false, true] {
            let targets = resolve_targets(
                &snapshot,
                Some(&durable),
                "@coder",
                None,
                Some("project"),
                rollup_only,
            )
            .unwrap();
            assert_eq!(targets.len(), 1);
            assert_eq!(
                targets[0]
                    .agent
                    .as_ref()
                    .map(|agent| agent.agent_id.as_str()),
                Some("sess-owner")
            );
        }

        assert!(matches!(
            resolve_targets(&snapshot, Some(&durable), "@sess-older", None, None, false,),
            Err(TargetErr::NoMatch { .. })
        ));
    }

    #[test]
    fn agent_broadcast_excludes_only_the_caller_after_channel_resolution() {
        let mut caller = named_agent("caller", "planner", "project");
        caller.launch_id = Some(AgentSessionId::from("launch-planner"));
        let first_peer = named_agent("first-peer", "coder", "project");
        let second_peer = named_agent("second-peer", "reviewer", "project");
        let other_channel = named_agent("other-channel", "docs", "docs");
        let durable = vec![
            caller.clone(),
            first_peer.clone(),
            second_peer.clone(),
            other_channel,
        ];
        let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
        let mut targets =
            resolve_targets(&snapshot, Some(&durable), "@all#project", None, None, false).unwrap();

        exclude_broadcast_caller(
            "@all#project",
            &mut targets,
            &durable,
            Some(&launch_caller("launch-planner")),
            None,
        )
        .unwrap();

        let ids = targets
            .iter()
            .filter_map(|target| target.agent.as_ref())
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["first-peer", "second-peer"]);
    }

    #[test]
    fn exact_self_handle_is_not_broadcast_filtered() {
        let mut caller = named_agent("caller", "planner", "project");
        caller.launch_id = Some(AgentSessionId::from("launch-planner"));
        let durable = vec![caller];
        let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
        let mut targets = resolve_targets(
            &snapshot,
            Some(&durable),
            "@planner",
            None,
            Some("project"),
            false,
        )
        .unwrap();

        exclude_broadcast_caller(
            "@planner",
            &mut targets,
            &durable,
            Some(&launch_caller("launch-planner")),
            None,
        )
        .unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0]
                .agent
                .as_ref()
                .map(|agent| agent.agent_id.as_str()),
            Some("caller")
        );
    }

    #[test]
    fn explicit_selector_fanout_keeps_the_caller() {
        let mut caller = named_agent("caller", "planner", "project");
        caller.launch_id = Some(AgentSessionId::from("launch-planner"));
        let peer = named_agent("peer", "coder", "project");
        let durable = vec![caller, peer];
        let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
        let mut targets = resolve_targets(
            &snapshot,
            Some(&durable),
            "@claude",
            None,
            Some("project"),
            false,
        )
        .unwrap();

        exclude_broadcast_caller(
            "@claude",
            &mut targets,
            &durable,
            Some(&launch_caller("launch-planner")),
            None,
        )
        .unwrap();

        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn broadcast_excludes_a_legacy_pane_only_caller() {
        let caller_pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let mut caller = named_agent("caller", "planner", "project");
        caller.pane = Some(PaneRef::from_id(caller_pane.clone()));
        let durable = vec![caller];
        let snapshot = snapshot_with_panes(
            Vec::new(),
            vec![
                pane_only("terminal_1", "planner"),
                pane_only("terminal_2", "coder"),
            ],
        );
        let legacy = crate::harness::ancestry::LaunchCallerEnv {
            kind: AgentKind::new_unchecked("claude"),
            launch_id: None,
            pane_id: Some(caller_pane),
        };
        let mut targets = resolve_targets(
            &snapshot,
            Some(&durable),
            "@all",
            None,
            Some("project"),
            false,
        )
        .unwrap();

        exclude_broadcast_caller("@all", &mut targets, &durable, Some(&legacy), None).unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].pane.as_ref().map(|pane| pane.pane_id.as_str()),
            Some("zellij:terminal_2")
        );
    }

    #[test]
    fn solo_agent_broadcast_reports_no_peers() {
        let mut caller = named_agent("caller", "planner", "project");
        caller.launch_id = Some(AgentSessionId::from("launch-planner"));
        let durable = vec![caller];
        let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
        let mut targets = resolve_targets(
            &snapshot,
            Some(&durable),
            "@all",
            None,
            Some("project"),
            false,
        )
        .unwrap();
        let channel = "project".to_owned();

        let err = exclude_broadcast_caller(
            "@all",
            &mut targets,
            &durable,
            Some(&launch_caller("launch-planner")),
            Some(channel.as_str()),
        )
        .expect_err("the caller is not its own peer");

        assert!(matches!(
            &err,
            DispatchErr::NoPeers {
                channel: Some(channel)
            } if channel == "project"
        ));
        assert!(
            err.to_string()
                .starts_with("no other agents in the current channel")
        );
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn snapshot_with_panes(agents: Vec<AgentState>, panes: Vec<PaneAgent>) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), agents, now());
        snapshot.agent_panes = panes;
        snapshot
    }

    fn agent(id: &str, status: AgentStatus) -> AgentState {
        let mut agent = AgentState::stub("claude", id, status);
        agent.worktree_path = Some("/repo/project".to_owned());
        agent.worktree_branch = Some("project".to_owned());
        agent
    }

    fn named_agent(id: &str, name: &str, channel: &str) -> AgentState {
        let mut agent = agent(id, AgentStatus::Running);
        agent.name = Some(name.to_owned());
        agent.channel = Some(channel.to_owned());
        agent.worktree_branch = Some(channel.to_owned());
        agent
    }

    fn launch_caller(launch_id: &str) -> crate::harness::ancestry::LaunchCallerEnv {
        crate::harness::ancestry::LaunchCallerEnv {
            kind: AgentKind::new_unchecked("claude"),
            launch_id: Some(AgentSessionId::from(launch_id)),
            pane_id: None,
        }
    }

    fn resident(id: &str, pane: &str) -> AgentState {
        let mut agent = agent(id, AgentStatus::Running);
        agent.role = Some("coder".to_owned());
        agent.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, pane)));
        agent
    }

    fn owner_pane(id: &str, role: Option<&str>) -> PaneAgent {
        PaneAgent {
            kind: AgentKind::new_unchecked("claude"),
            kind_ordinal: Some(2),
            name: Some("owner".to_owned()),
            name_explicit: false,
            profile: None,
            role: role.map(ToOwned::to_owned),
            channel: None,
            agent_id: Some(AgentSessionId::from(id)),
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            pane_pid: None,
            worktree_path: Some("/repo/project".to_owned()),
            worktree_branch: Some("project".to_owned()),
        }
    }

    fn pane_only(pane: &str, role: &str) -> PaneAgent {
        PaneAgent {
            pane_id: PaneId::from_parts(MuxName::Zellij, pane),
            agent_id: None,
            role: Some(role.to_owned()),
            worktree_path: Some("/repo/project".to_owned()),
            worktree_branch: Some("project".to_owned()),
            ..owner_pane("", None)
        }
    }

    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
}
