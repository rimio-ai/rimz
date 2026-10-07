//! Resolve one owned message request, persist it, and order target fan-out.
//!
//! This module owns live-plus-durable target resolution, rollup-only selection,
//! context folding, condition binding, hook preflight, reply causality, record
//! construction, and the park-vs-live decision. Live attempts delegate receiver
//! recovery to [`super::deliver`]. Agent-originated broadcasts exclude the
//! caller after address resolution, before any fan-out work begins.

use std::collections::BTreeSet;

use super::DeliveryKind;
use jiff::Timestamp;

use crate::Store;
use crate::address::{AddressContext, TargetErr};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, MessageId, MuxName};
use crate::message::{MessageDraft, Recipient};
use crate::store::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    WhenCondition, in_flight_claim, queue_head,
};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;

use super::reply::{PreparationTarget, ReplyJoin, ReplyPreparation, ReplyPrepareErr, ReplyWait};
use super::{deliver, send};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhenRequest {
    pub address: String,
    pub status: AgentStatus,
    pub dwell_secs: u64,
    pub expression: String,
}

#[derive(Clone, Debug)]
pub enum DispatchMode {
    Interrupt,
    Steer,
    Boundary {
        gate: DeliveryGate,
        not_before: Option<Timestamp>,
        after: Vec<String>,
        when: Vec<WhenRequest>,
    },
}

impl DispatchMode {
    pub fn kind(&self) -> DeliveryKind {
        match self {
            Self::Boundary { .. } => DeliveryKind::Boundary,
            Self::Steer => DeliveryKind::Steer,
            Self::Interrupt => DeliveryKind::Interrupt,
        }
    }

    fn gate(&self) -> DeliveryGate {
        match self {
            Self::Steer | Self::Interrupt => DeliveryGate::Any,
            Self::Boundary { gate, .. } => *gate,
        }
    }

    fn needs_agent_context(&self) -> bool {
        match self {
            Self::Interrupt => true,
            Self::Steer => false,
            Self::Boundary { after, when, .. } => !after.is_empty() || !when.is_empty(),
        }
    }
}

pub struct DispatchRequest {
    pub target: String,
    pub text: String,
    pub target_scope: Option<String>,
    pub current_channel: AddressContext,
    pub caller: Option<crate::harness::ancestry::CallerIdentity>,
    pub sender: MessageSender,
    pub automated: bool,
    pub allow_fanout: bool,
    pub reply: Option<ReplyJoin>,
    pub mux: Option<MuxName>,
    pub enter: bool,
    pub force: bool,
    /// An explicit threshold for this dispatch; `None` inherits the
    /// `[harness] smart_compact` machine default in [`dispatch`].
    pub auto_compact: Option<AutoCompact>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParkReason {
    Status(AgentStatus),
    WaitingOnPrompt,
    ProviderStarting,
    Scheduled(Timestamp),
    After(String),
    When {
        address: String,
        status: AgentStatus,
        dwell_secs: u64,
    },
    Behind(MessageId),
    NoPane,
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

type Result<T> = std::result::Result<T, DispatchErr>;

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
        kind: DeliveryKind,
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
    #[error("{label} is waiting on your input in its pane; answer it or pass --force")]
    WaitingOnInput { label: String },
    #[error(transparent)]
    Login(#[from] crate::agents::RoomLoginErr),
}

pub fn dispatch(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mut request: DispatchRequest,
) -> Result<DispatchResult> {
    request.auto_compact = request
        .auto_compact
        .or(crate::config::MachineConfig::load_lenient()
            .harness
            .smart_compact);
    let boundary = request.mode.kind() == DeliveryKind::Boundary;
    let pending = if boundary {
        store.list_messages()?
    } else {
        Vec::new()
    };
    let needs_context = request.auto_compact.is_some()
        || request.mode.needs_agent_context()
        || matches!(request.sender, MessageSender::Agent { .. })
        || request.reply.is_some();
    let agent_context =
        needs_context.then(|| crate::store::agent_context::read_all(store.runtime_paths()));

    let cached_snapshot = boundary.then(|| store.snapshot_cached()).transpose()?;
    let rollup_only = cached_snapshot.as_ref().is_some_and(|snapshot| {
        targets_all_park_without_live(
            snapshot,
            &request.target,
            request.target_scope.as_deref(),
            &request.current_channel,
            &pending,
            request.mode.gate(),
            request.force,
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
    // Ended children linger on their parent's card for presentation, not delivery.
    snapshot.agents.retain(|agent| agent.ended_at.is_none());

    let durable_agents = durable_target_agents(store)?;
    let resolution = ResolutionView {
        snapshot: &snapshot,
        durable_agents: &durable_agents,
        scope: request.target_scope.as_deref(),
        channel: &request.current_channel,
        rollup_only,
    };
    let mut targets = resolution.resolve(&request.target)?;
    exclude_broadcast_caller(
        &request.target,
        &mut targets,
        &durable_agents,
        request.caller.as_ref(),
        request
            .target_scope
            .as_deref()
            .or(request.current_channel.channel.as_deref()),
    )?;
    if targets.len() > 1 && !request.allow_fanout && !crate::address::is_broadcast(&request.target)
    {
        return Err(DispatchErr::Fanout {
            target: request.target,
            labels: targets
                .iter()
                .map(|target| target.label(&snapshot))
                .collect(),
            kind: request.mode.kind(),
        });
    }

    let mode = prepare_mode(&request, &resolution, &targets, &pending)?;
    let reply_preparation = request
        .reply
        .map(|_| {
            ReplyPreparation::new(
                store,
                &snapshot,
                targets.iter().map(|target| PreparationTarget {
                    agent: target.agent.as_ref(),
                    label: target.label(&snapshot),
                }),
                request
                    .caller
                    .as_ref()
                    .and_then(|caller| Some((caller.kind.clone(), caller.name.clone()?))),
            )
        })
        .transpose()?;
    let text = if targets.len() > 1 || crate::address::is_broadcast(&request.target) {
        crate::address::group_prefixed(&request.target, &request.text)
    } else {
        request.text
    };
    let in_reply_to = turn_openers_for_sender(&snapshot, &request.sender);
    let state = DispatchState {
        workspace,
        store,
        snapshot: &snapshot,
        pending: &pending,
        scope_channel: request.current_channel.channel.as_deref(),
        reply_wait: reply_preparation.is_some(),
        in_reply_to: &in_reply_to,
    };
    let (outcomes, compacted) = dispatch_targets(&state, &targets, &text, &mode)?;
    let reply = reply_preparation
        .zip(request.reply)
        .map(|(preparation, join)| preparation.attach(&outcomes, mode.kind, join))
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
            let peers = crate::address::addressable_agents(snapshot);
            crate::address::agent_handle(agent, &peers, true)
        } else if let Some(pane) = self.pane.as_ref() {
            format!("@{}", pane.label())
        } else {
            "@agent".to_owned()
        }
    }

    fn bound<'a>(&self, snapshot: &'a SidebarSnapshot) -> Option<&'a AgentState> {
        self.pane
            .as_ref()
            .and_then(|pane| crate::address::pane_binding(snapshot, pane, None))
            .and_then(|binding| binding.exact_agent)
    }
}

fn exclude_broadcast_caller(
    raw: &str,
    targets: &mut Vec<ResolvedTarget>,
    durable_agents: &[AgentState],
    caller_env: Option<&crate::harness::ancestry::CallerIdentity>,
    channel: Option<&str>,
) -> Result<()> {
    if !crate::address::is_broadcast(raw) {
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

fn durable_target_agents(store: &Store) -> Result<Vec<AgentState>> {
    Ok(store
        .runtime_projection(crate::RuntimeScope::Audit)?
        .agents
        .into_iter()
        .filter(|agent| !agent.is_provider_subagent() && agent.ended_at.is_none())
        .collect())
}

fn targets_all_park_without_live(
    snapshot: &SidebarSnapshot,
    raw: &str,
    scope: Option<&str>,
    channel: &AddressContext,
    pending: &[MessageRecord],
    gate: DeliveryGate,
    force: bool,
) -> bool {
    if crate::address::is_broadcast(raw) {
        return false;
    }
    let Ok(agents) = crate::address::resolve_many(snapshot, raw, scope, channel) else {
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
    kind: DeliveryKind,
    draft: MessageDraft,
}

struct ResolutionView<'a> {
    snapshot: &'a SidebarSnapshot,
    durable_agents: &'a [AgentState],
    scope: Option<&'a str>,
    channel: &'a AddressContext,
    rollup_only: bool,
}

impl ResolutionView<'_> {
    fn resolve(&self, raw: &str) -> std::result::Result<Vec<ResolvedTarget>, TargetErr> {
        if self.rollup_only {
            let agents = crate::address::resolve_many(self.snapshot, raw, self.scope, self.channel)
                .or_else(|_| self.durable_targets(raw))?;
            return Ok(self.combine_targets(agents, Vec::new()));
        }
        let agent_result =
            crate::address::resolve_many(self.snapshot, raw, self.scope, self.channel);
        let pane_result =
            crate::address::resolve_targets(self.snapshot, raw, self.scope, self.channel);
        match (agent_result, pane_result) {
            (Ok(agents), Ok(panes)) => Ok(self.combine_targets(agents, panes)),
            (Ok(agents), Err(_)) => Ok(self.combine_targets(agents, Vec::new())),
            (Err(_), Ok(panes)) => Ok(self.combine_targets(Vec::new(), panes)),
            (Err(_), Err(_)) => self
                .durable_targets(raw)
                .map(|agents| self.combine_targets(agents, Vec::new())),
        }
    }

    fn durable_targets<'a>(
        &'a self,
        raw: &str,
    ) -> std::result::Result<Vec<&'a AgentState>, TargetErr> {
        let candidates = self
            .durable_agents
            .iter()
            .filter(|agent| !crate::address::shadowed_by_pane_owner(self.snapshot, agent))
            .collect::<Vec<_>>();
        crate::address::resolve_agents(raw, self.scope, self.channel, &candidates)
    }

    fn combine_targets(
        &self,
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
                        && crate::address::pane_binding(self.snapshot, pane, None)
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
            let binding = crate::address::pane_binding(self.snapshot, pane, None);
            targets.push(ResolvedTarget {
                pane: Some(pane.clone()),
                agent: binding.and_then(|binding| binding.agent).cloned(),
            });
        }
        targets
    }

    fn condition_target(
        &self,
        kind: ConditionKind,
        address: &str,
        expression: &str,
    ) -> Result<ResolvedTarget> {
        if crate::address::is_broadcast(address) {
            return Err(ConditionErr::Broadcast {
                kind,
                address: address.to_owned(),
                expression: expression.to_owned(),
            }
            .into());
        }
        let targets = self
            .resolve(address)
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
}

fn prepare_mode(
    request: &DispatchRequest,
    resolution: &ResolutionView<'_>,
    recipients: &[ResolvedTarget],
    pending: &[MessageRecord],
) -> Result<PreparedMode> {
    let kind = request.mode.kind();
    if kind == DeliveryKind::Interrupt {
        for target in recipients {
            let label = target.label(resolution.snapshot);
            let target_kind = target
                .agent
                .as_ref()
                .map(|agent| &agent.kind)
                .or_else(|| target.pane.as_ref().map(|pane| &pane.kind))
                .ok_or_else(|| DispatchErr::NoDurableSession {
                    label: label.clone(),
                })?;
            send::interrupt_key(target_kind, &label).map_err(deliver::DeliverErr::from)?;
            let agent = if target.pane.is_some() {
                target.bound(resolution.snapshot)
            } else {
                target.agent.as_ref()
            }
            .ok_or_else(|| DispatchErr::NoDurableSession {
                label: label.clone(),
            })?;
            if !request.force && agent.effective_status() == AgentStatus::Waiting {
                return Err(DispatchErr::WaitingOnInput { label });
            }
        }
    }
    let gate = request.mode.gate();
    let (not_before, after, when) = match &request.mode {
        DispatchMode::Boundary {
            not_before,
            after,
            when,
            ..
        } => (*not_before, after.as_slice(), when.as_slice()),
        _ => (None, &[][..], &[][..]),
    };
    Ok(PreparedMode {
        kind,
        draft: MessageDraft {
            body: MessageBody::Prompt,
            enter: request.enter,
            gate,
            sender: request.sender.clone(),
            automated: request.automated,
            force: request.force,
            auto_compact: request.auto_compact,
            not_before,
            after: resolve_after(resolution, recipients, after, gate, pending)?,
            when: resolve_when(resolution, when)?,
        },
    })
}

fn resolve_after(
    resolution: &ResolutionView<'_>,
    recipients: &[ResolvedTarget],
    addresses: &[String],
    gate: DeliveryGate,
    pending: &[MessageRecord],
) -> Result<Vec<AfterCondition>> {
    let now = Timestamp::now();
    addresses
        .iter()
        .map(|address| {
            let target = resolution.condition_target(ConditionKind::After, address, address)?;
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
    resolution: &ResolutionView<'_>,
    requests: &[WhenRequest],
) -> Result<Vec<WhenCondition>> {
    let now = Timestamp::now();
    let delivery_window = MessageBody::Prompt.delivery_window();
    requests
        .iter()
        .map(|request| {
            let target = resolution.condition_target(
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

struct DispatchState<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    snapshot: &'a SidebarSnapshot,
    pending: &'a [MessageRecord],
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
            (Some(agent), pane) => Recipient::Agent { agent, pane },
            (None, Some(pane)) => Recipient::Pane {
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
        if pane.is_some() {
            return Ok(self.store.queue_claimed_message(
                &message,
                &self.workspace.session_name,
                Timestamp::now(),
            )?);
        }
        self.store
            .queue_message(&message, &self.workspace.session_name)?;
        Ok(message)
    }
}

fn dispatch_targets(
    state: &DispatchState<'_>,
    targets: &[ResolvedTarget],
    text: &str,
    mode: &PreparedMode,
) -> Result<(Vec<DispatchOutcome>, Vec<String>)> {
    let now = Timestamp::now();
    let decisions = targets
        .iter()
        .map(|target| dispatch_decision(state.snapshot, state.pending, target, mode, now))
        .collect::<Vec<_>>();
    let mut live_send = send::LiveSend::new(mode.draft.force, mode.kind);
    let mut preflighted_logins = BTreeSet::new();
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
            if preflighted_logins.insert(agent.login_key()) {
                preflight_queue_hooks(
                    agent,
                    &crate::agents::session_login_env(&agent.kind, agent.login.as_ref())?,
                )?;
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
    deliver::register_message_wake(state.workspace, state.store);
    Ok((outcomes, compacted))
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    if mode.kind != DeliveryKind::Boundary {
        return if target.pane.is_some() {
            DispatchDecision::Live
        } else {
            DispatchDecision::Parked {
                reason: Some(ParkReason::NoPane),
            }
        };
    }
    if let Some(not_before) = mode.draft.not_before {
        return DispatchDecision::Parked {
            reason: Some(ParkReason::Scheduled(not_before)),
        };
    }
    if let Some(condition) = mode
        .draft
        .after
        .iter()
        .find(|condition| condition.met_at.is_none())
    {
        return DispatchDecision::Parked {
            reason: Some(ParkReason::After(condition.address.clone())),
        };
    }
    if let Some(condition) = mode
        .draft
        .when
        .iter()
        .find(|condition| condition.met_at.is_none())
    {
        return DispatchDecision::Parked {
            reason: Some(ParkReason::When {
                address: condition.address.clone(),
                status: condition.status,
                dwell_secs: condition.dwell_secs,
            }),
        };
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
        if super::provider_start_pending(agent, now) {
            return DispatchDecision::Parked {
                reason: Some(ParkReason::ProviderStarting),
            };
        }
    }
    if target.pane.is_none() {
        return DispatchDecision::Parked {
            reason: Some(ParkReason::NoPane),
        };
    }
    if let Some(agent) = target.agent.as_ref()
        && let Some(blocker) = queue_head(
            pending.iter(),
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
            now,
        )
        .or_else(|| {
            in_flight_claim(
                pending.iter(),
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
                now,
            )
        })
    {
        return DispatchDecision::Parked {
            reason: Some(ParkReason::Behind(blocker.message_id.clone())),
        };
    }
    DispatchDecision::Live
}

fn dispatch_one(
    state: &DispatchState<'_>,
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
    deliver::register_message_wake(state.workspace, state.store);
    let message_id = message.message_id.clone();
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
        deliver::AttemptOutcome::Queued => Ok(DispatchOutcome::Queued {
            label: handle,
            message_id,
            reason: None,
        }),
        deliver::AttemptOutcome::CompactionPending => Ok(DispatchOutcome::CompactionPending {
            label: handle,
            message_id,
        }),
    }
}

fn dispatch_parked(
    state: &DispatchState<'_>,
    target: &ResolvedTarget,
    text: &str,
    mode: &PreparedMode,
    handle: String,
    reason: Option<ParkReason>,
) -> Result<DispatchOutcome> {
    let message_id = state.enqueue(target, None, text, mode, &handle)?.message_id;
    Ok(DispatchOutcome::Queued {
        label: handle,
        message_id,
        reason,
    })
}

fn preflight_queue_hooks(
    agent: &AgentState,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let Some(adapter) = crate::agents::find_definition(agent.kind.as_str()) else {
        return Err(DispatchErr::UnknownAgentKind(agent.kind.clone()));
    };
    match crate::agents::preflight_hooks(adapter, login_env, crate::agents::TurnLifecycleNeed::None)
    {
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
#[path = "dispatch/tests.rs"]
mod tests;
