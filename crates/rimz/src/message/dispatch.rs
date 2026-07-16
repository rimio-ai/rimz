//! Resolve one owned message request and dispatch it durably.
//!
//! This module owns live-plus-durable target resolution, rollup-only selection,
//! context folding, condition binding, hook preflight, reply causality, record
//! construction, and park-vs-live delivery.

use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, MessageId, MuxName};
use crate::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    WhenCondition, gate_open_for_agent, message_interval_from_env, queue_head,
};
use crate::workspace::ResolvedWorkspace;
use crate::{PaneAgent, SidebarSnapshot, Store, TargetErr};

use super::reply::{PreparationTarget, ReplyJoin, ReplyPreparation, ReplyPrepareErr, ReplyWait};
use super::{deliver, send};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AfterRequest {
    pub address: String,
}

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
        auto_compact: Option<AutoCompact>,
    },
    Boundary {
        enter: bool,
        gate: DeliveryGate,
        force: bool,
        auto_compact: Option<AutoCompact>,
        not_before: Option<Timestamp>,
        after: Vec<AfterRequest>,
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
    request: DispatchRequest,
) -> Result<DispatchResult> {
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
    let targets = resolve_targets(
        &snapshot,
        Some(&durable_agents),
        &request.target,
        request.target_scope.as_deref(),
        request.current_channel.as_deref(),
        rollup_only,
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
    let mode = prepare_mode(request.mode, resolution, &targets, &pending)?;
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
        sender: &request.sender,
        automated: request.automated,
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
            let peers = snapshot.root_agents().collect::<Vec<_>>();
            crate::harness::target::agent_handle(agent, &peers, true)
        } else if let Some(pane) = self.pane.as_ref() {
            send::handle_for_pane_target(snapshot, pane, None)
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
                gate_open_for_agent(gate, agent, force, now)
                    && (force || !agent.is_awaiting_input())
            }
        };
        if !open {
            return false;
        }
        self.agent.as_ref().is_none_or(|agent| {
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
            .or_else(|err| durable_targets(durable_agents, raw, scope, channel, err))?;
        return Ok(combine_targets(snapshot, agents, Vec::new()));
    }
    let agent_result = crate::harness::target::resolve_many(snapshot, raw, scope, channel);
    let pane_result = crate::harness::target::resolve_targets(snapshot, raw, scope, channel);
    match (agent_result, pane_result) {
        (Ok(agents), Ok(panes)) => Ok(combine_targets(snapshot, agents, panes)),
        (Ok(agents), Err(_)) => Ok(combine_targets(snapshot, agents, Vec::new())),
        (Err(_), Ok(panes)) => Ok(combine_targets(snapshot, Vec::new(), panes)),
        (Err(err), Err(_)) => durable_targets(durable_agents, raw, scope, channel, err)
            .map(|agents| combine_targets(snapshot, agents, Vec::new())),
    }
}

fn durable_target_agents(store: &Store) -> Result<Vec<AgentState>> {
    Ok(store
        .runtime_projection(crate::RuntimeScope::Audit)?
        .agents
        .into_iter()
        .filter(|agent| agent.parent_agent_id.is_none() && agent.ended_at.is_none())
        .collect())
}

fn durable_targets<'a>(
    durable_agents: Option<&'a [AgentState]>,
    raw: &str,
    scope: Option<&str>,
    channel: Option<&str>,
    live_err: TargetErr,
) -> std::result::Result<Vec<&'a AgentState>, TargetErr> {
    let Some(durable_agents) = durable_agents else {
        return Err(live_err);
    };
    let candidates = durable_agents.iter().collect::<Vec<_>>();
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
        || crate::agents::descriptor_by_kind(agent.kind.as_str())
            .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
        || (gate_open_for_agent(gate, agent, force, now)
            && (force || !agent.is_awaiting_input())
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
    enter: bool,
    force: bool,
    auto_compact: Option<AutoCompact>,
    gate: DeliveryGate,
    not_before: Option<Timestamp>,
    after: Vec<AfterCondition>,
    when: Vec<WhenCondition>,
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
) -> Result<PreparedMode> {
    match mode {
        DispatchMode::Steer {
            enter,
            force,
            auto_compact,
        } => Ok(PreparedMode {
            steer: true,
            enter,
            force,
            auto_compact,
            gate: DeliveryGate::Any,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
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
            enter,
            force,
            auto_compact,
            gate,
            not_before,
            after: resolve_after(resolution, recipients, &after, gate, pending)?,
            when: resolve_when(resolution, &when)?,
        }),
    }
}

fn resolve_after(
    resolution: ResolutionView<'_>,
    recipients: &[ResolvedTarget],
    requests: &[AfterRequest],
    gate: DeliveryGate,
    pending: &[MessageRecord],
) -> Result<Vec<AfterCondition>> {
    let now = Timestamp::now();
    requests
        .iter()
        .map(|request| {
            let target = resolve_condition_target(
                resolution,
                ConditionKind::After,
                &request.address,
                &request.address,
            )?;
            // Condition target resolution rejects pane-only targets.
            let agent = target.agent.as_ref().expect("condition target validated");
            if recipients.iter().any(|recipient| {
                recipient
                    .agent
                    .as_ref()
                    .is_some_and(|recipient| agent.card_ref().matches(recipient.card_ref()))
            }) {
                return Err(ConditionErr::RecipientSelfReference {
                    address: request.address.clone(),
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
    sender: &'a MessageSender,
    automated: bool,
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
        let message = draft(self, text, mode, handle)
            .into_record(
                self.workspace.workspace_id.clone(),
                recipient,
                self.scope_channel,
            )
            .with_reply_wait(self.reply_wait)
            .with_in_reply_to(self.in_reply_to.to_vec());
        self.store
            .queue_message(&message, &self.workspace.session_name)?;
        Ok(message)
    }
}

enum LiveAttempt {
    Sent {
        message_id: MessageId,
        compacted: bool,
    },
    SkippedWaiting {
        message_id: MessageId,
    },
    CompactionPending {
        message_id: MessageId,
    },
    ParkInstead,
}

fn dispatch_targets(
    state: &mut DispatchState<'_>,
    targets: &[ResolvedTarget],
    text: &str,
    mode: &PreparedMode,
) -> Result<(Vec<DispatchOutcome>, Vec<String>)> {
    let now = Timestamp::now();
    let parks = targets
        .iter()
        .map(|target| should_park(state, target, mode, now))
        .collect::<Vec<_>>();
    let mut live_send = send::LiveSend {
        force: mode.force,
        steer: mode.steer,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut preflighted_kinds = BTreeSet::new();
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut compacted = Vec::new();
    for (target, park) in targets.iter().zip(parks) {
        if park {
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
            park,
        )?);
    }
    deliver::register_message_wake(state.workspace, state.store)?;
    Ok((outcomes, compacted))
}

fn should_park(
    state: &DispatchState<'_>,
    target: &ResolvedTarget,
    mode: &PreparedMode,
    now: Timestamp,
) -> bool {
    if mode.steer {
        return target.pane.is_none();
    }
    mode.not_before.is_some()
        || !mode
            .after
            .iter()
            .all(|condition| condition.met_at.is_some())
        || !mode.when.iter().all(|condition| condition.met_at.is_some())
        || !target.receivable_now(state.snapshot, state.pending, mode.gate, mode.force, now)
}

fn dispatch_one(
    state: &mut DispatchState<'_>,
    live_send: &mut send::LiveSend,
    compacted: &mut Vec<String>,
    target: &ResolvedTarget,
    text: &str,
    mode: &PreparedMode,
    park: bool,
) -> Result<DispatchOutcome> {
    let handle = target.label(state.snapshot);
    if park {
        return dispatch_parked(state, target, text, mode, handle);
    }
    let Some(pane) = target.pane.as_ref() else {
        return Err(DispatchErr::NoDurableSession { label: handle });
    };
    let bound = target.bound(state.snapshot);
    let message = state.enqueue(target, Some(pane), text, mode, &handle)?;
    let message_id = message.message_id.clone();
    match send_live_with_recovery(
        state,
        live_send,
        target.agent.is_some(),
        pane,
        bound,
        &message,
    )? {
        LiveAttempt::Sent {
            message_id,
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
        LiveAttempt::SkippedWaiting { message_id } if mode.steer => {
            state.store.record_send_error(
                &message,
                "agent is waiting on input in its pane",
                &state.workspace.session_name,
            )?;
            Ok(DispatchOutcome::SkippedWaiting {
                label: handle,
                message_id,
            })
        }
        LiveAttempt::SkippedWaiting { message_id } => {
            state.store.record_message_delivery_failure(
                &message_id,
                "agent is waiting on input in its pane",
                &state.workspace.session_name,
            )?;
            push_pending(state, message);
            Ok(DispatchOutcome::Queued {
                label: handle,
                message_id,
            })
        }
        LiveAttempt::ParkInstead => {
            push_pending(state, message);
            Ok(DispatchOutcome::Queued {
                label: handle,
                message_id,
            })
        }
        LiveAttempt::CompactionPending { message_id } => {
            let released = state
                .store
                .release_message_claim(
                    &message_id,
                    "parked: waiting for compaction to finish",
                    &state.workspace.session_name,
                )?
                .unwrap_or(message);
            push_pending(state, released);
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
) -> Result<DispatchOutcome> {
    let message = state.enqueue(target, None, text, mode, &handle)?;
    let message_id = message.message_id.clone();
    push_pending(state, message);
    Ok(DispatchOutcome::Queued {
        label: handle,
        message_id,
    })
}

fn draft(
    state: &DispatchState<'_>,
    text: &str,
    mode: &PreparedMode,
    handle: &str,
) -> send::MessageDraft {
    send::MessageDraft {
        text: text.to_owned(),
        body: MessageBody::Prompt,
        address: Some(handle.to_owned()),
        enter: mode.enter,
        gate: mode.gate,
        sender: state.sender.clone(),
        automated: state.automated,
        force: mode.force,
        auto_compact: mode.auto_compact,
        not_before: mode.not_before,
        after: mode.after.clone(),
        when: mode.when.clone(),
    }
}

fn push_pending(state: &mut DispatchState<'_>, message: MessageRecord) {
    if state.track_pending {
        state.pending.push(message);
    }
}

fn send_live_with_recovery(
    state: &DispatchState<'_>,
    live_send: &mut send::LiveSend,
    park_on_failure: bool,
    pane: &PaneAgent,
    bound: Option<&AgentState>,
    message: &MessageRecord,
) -> Result<LiveAttempt> {
    let sent = match send::send_batch_to_live_pane(
        state.workspace,
        state.store,
        state.snapshot,
        pane,
        bound,
        std::slice::from_ref(message),
        live_send,
    ) {
        Ok(sent) => sent,
        Err(err) => {
            if deliver::message_recorded_as_sent(state.store, &message.message_id)? {
                return Ok(LiveAttempt::Sent {
                    message_id: message.message_id.clone(),
                    compacted: false,
                });
            }
            if park_on_failure {
                state.store.record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &state.workspace.session_name,
                )?;
                deliver::register_message_wake(state.workspace, state.store)?;
                return Ok(LiveAttempt::ParkInstead);
            }
            state.store.record_send_error(
                message,
                &err.to_string(),
                &state.workspace.session_name,
            )?;
            deliver::register_message_wake(state.workspace, state.store)?;
            return Err(err.into());
        }
    };
    match sent.outcome {
        send::Outcome::Sent { message_id, .. } => Ok(LiveAttempt::Sent {
            message_id,
            compacted: sent.compacted.is_some(),
        }),
        send::Outcome::SkippedWaiting { message_id, .. } => {
            Ok(LiveAttempt::SkippedWaiting { message_id })
        }
        send::Outcome::CompactionPending { message_id, .. } => {
            Ok(LiveAttempt::CompactionPending { message_id })
        }
    }
}

fn preflight_queue_hooks(agent: &AgentState) -> Result<()> {
    let Some(adapter) = crate::agents::find_adapter(agent.kind.as_str()) else {
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
        .root_agents()
        .find(|agent| agent.kind == *kind && agent.name.as_deref() == Some(name))
        .and_then(|agent| agent.context.as_ref())
        .map(|context| context.turn_opened_by.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::{AgentStatus, TurnPhase};
    use crate::ids::{AgentKind, MuxName, PaneId, WorkspaceId};
    use crate::pane::PaneRef;

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
            ResolvedTarget {
                pane: Some(lazy),
                agent: None,
            }
            .receivable_now(&idle_snapshot, &[], DeliveryGate::Done, false, timestamp)
        );
        assert!(
            ResolvedTarget {
                pane: Some(pane.clone()),
                agent: Some(idle.clone()),
            }
            .receivable_now(&idle_snapshot, &[], DeliveryGate::Done, false, timestamp)
        );

        let running_pane = bound_pane(&running, "terminal_5");
        let running_snapshot =
            snapshot_with_panes(vec![running.clone()], vec![running_pane.clone()]);
        assert!(
            !ResolvedTarget {
                pane: Some(running_pane),
                agent: Some(running),
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
        let ask_target = ResolvedTarget {
            pane: Some(pane.clone()),
            agent: Some(idle.clone()),
        };
        assert!(!ask_target.receivable_now(
            &ask_snapshot,
            &[],
            DeliveryGate::Done,
            false,
            timestamp
        ));
        assert!(ask_target.receivable_now(&ask_snapshot, &[], DeliveryGate::Done, true, timestamp));

        let future = MessageRecord::new(
            workspace_id(),
            &idle,
            "future".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_not_before(Some(timestamp + jiff::SignedDuration::from_secs(60)));
        let target = ResolvedTarget {
            pane: Some(pane),
            agent: Some(idle.clone()),
        };
        assert!(target.receivable_now(
            &idle_snapshot,
            &[future],
            DeliveryGate::Done,
            false,
            timestamp
        ));
        let older = MessageRecord::new(
            workspace_id(),
            &idle,
            "older".to_owned(),
            true,
            DeliveryGate::Done,
        );
        assert!(!target.receivable_now(
            &idle_snapshot,
            &[older],
            DeliveryGate::Done,
            false,
            timestamp
        ));
    }

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
    fn agent_sender_inherits_exact_named_sessions_turn_openers() {
        let opener = MessageId::parse("msg_0123456789abcdef").unwrap();
        let mut agent = agent("sess-1", AgentStatus::Running);
        agent.name = Some("coder".to_owned());
        let mut context = crate::store::agent_context::empty_context("codex", now());
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

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn snapshot_with_panes(agents: Vec<AgentState>, panes: Vec<PaneAgent>) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), agents, now());
        snapshot.agent_panes = panes;
        snapshot
    }

    fn snapshot_with_ask(mut agent: AgentState, pane: PaneAgent) -> SidebarSnapshot {
        agent.status = AgentStatus::Waiting;
        agent.phase = TurnPhase::Idle;
        agent.waiting_since = Some(agent.last_activity);
        let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now());
        snapshot.agent_panes = vec![pane];
        snapshot
    }

    fn agent(id: &str, status: AgentStatus) -> AgentState {
        let mut agent = AgentState::stub("claude", id, status);
        agent.pane = Some(PaneRef::from_id(PaneId::from_parts(
            MuxName::Zellij,
            "terminal_3",
        )));
        agent.worktree_path = Some("/repo/project".to_owned());
        agent.worktree_branch = Some("project".to_owned());
        agent
    }

    fn bound_pane(agent: &AgentState, raw: &str) -> PaneAgent {
        PaneAgent {
            kind: agent.kind.clone(),
            kind_ordinal: agent.kind_ordinal,
            name: agent.name.clone(),
            name_explicit: agent.name_explicit,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            pane_pid: None,
            worktree_path: agent.worktree_path.clone(),
            worktree_branch: agent.worktree_branch.clone(),
        }
    }

    fn lazy_pane(kind: &str, raw: &str) -> PaneAgent {
        PaneAgent {
            kind: AgentKind::new_unchecked(kind),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: None,
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            pane_pid: None,
            worktree_path: Some("/repo/project".to_owned()),
            worktree_branch: Some("project".to_owned()),
        }
    }

    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
}
