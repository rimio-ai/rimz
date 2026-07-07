//! Park-vs-live target resolution and add-message dispatch.

use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::ids::MessageId;
use crate::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, gate_open_for_agent,
    message_interval_from_env, queue_head,
};
use crate::workspace::ResolvedWorkspace;
use crate::{PaneAgent, SidebarSnapshot, Store, TargetErr};

use super::{deliver, send};

#[derive(Clone, Copy)]
pub struct QueueTarget<'a> {
    pane: Option<&'a PaneAgent>,
    agent: Option<&'a AgentState>,
}

#[derive(Clone, Copy)]
pub enum SendMode {
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
    },
}

impl SendMode {
    fn enter(&self) -> bool {
        match self {
            Self::Steer { enter, .. } | Self::Boundary { enter, .. } => *enter,
        }
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

    fn auto_compact(&self) -> Option<AutoCompact> {
        match self {
            Self::Steer { auto_compact, .. } | Self::Boundary { auto_compact, .. } => *auto_compact,
        }
    }

    fn not_before(&self) -> Option<Timestamp> {
        match self {
            Self::Steer { .. } => None,
            Self::Boundary { not_before, .. } => *not_before,
        }
    }

    fn is_steer(&self) -> bool {
        matches!(self, Self::Steer { .. })
    }
}

pub struct DispatchResult {
    pub outcomes: Vec<DispatchOutcome>,
    pub compacted: Vec<String>,
}

pub enum DispatchOutcome {
    Sent {
        label: String,
        message_id: MessageId,
    },
    Queued {
        label: String,
        message_id: MessageId,
    },
    SkippedWaiting {
        label: String,
        message_id: MessageId,
    },
}

pub struct DispatchContext<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    pub snapshot: &'a SidebarSnapshot,
    pub pending: Option<&'a mut Vec<MessageRecord>>,
    pub scope_channel: Option<&'a str>,
    pub sender: &'a MessageSender,
}

pub type Result<T> = std::result::Result<T, DispatchErr>;

#[derive(Debug, thiserror::Error)]
pub enum DispatchErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Send(#[from] send::SendErr),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverErr),
    #[error("unknown agent kind `{0}`")]
    UnknownAgentKind(crate::ids::AgentKind),
    #[error(
        "queued delivery requires {kind} hooks so messages can deliver at turn boundaries; run `rimz hooks install {kind}`"
    )]
    HooksMissing { kind: crate::ids::AgentKind },
    #[error("{kind} hooks are installed but not trusted ({hooks}); {fix}")]
    HooksUntrusted {
        kind: crate::ids::AgentKind,
        hooks: String,
        fix: String,
    },
    #[error("`{label}` cannot receive now and has no durable session to park")]
    NoDurableSession { label: String },
}

enum LiveAttempt {
    Sent {
        message_id: MessageId,
        compacted: bool,
    },
    SkippedWaiting {
        message_id: MessageId,
    },
    ParkInstead,
}

struct LiveRecovery<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    snapshot: &'a SidebarSnapshot,
    live_send: &'a mut send::LiveSend,
    park_on_failure: bool,
}

impl QueueTarget<'_> {
    pub fn label(&self, snapshot: &SidebarSnapshot) -> String {
        handle_for_target(snapshot, self)
    }

    pub fn bound<'a>(&self, snapshot: &'a SidebarSnapshot) -> Option<&'a AgentState> {
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
                gate_open_for_agent(gate, agent, force) && (force || !agent.is_awaiting_input())
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

/// Resolve queue targets. `rollup_only` selects the cheap path after
/// [`rollup_targets_all_park_without_live`] proves no live pane is needed.
pub fn queue_targets<'a>(
    snapshot: &'a SidebarSnapshot,
    durable_agents: Option<&'a [AgentState]>,
    raw: &str,
    worktree: Option<&str>,
    channel: Option<&str>,
    rollup_only: bool,
) -> std::result::Result<Vec<QueueTarget<'a>>, TargetErr> {
    if rollup_only {
        let agents = crate::harness::target::resolve_many(snapshot, raw, worktree, channel)
            .or_else(|err| {
                durable_targets(snapshot, durable_agents, raw, worktree, channel, err)
            })?;
        return Ok(combine_queue_targets(snapshot, agents, Vec::new()));
    }
    let agent_result = crate::harness::target::resolve_many(snapshot, raw, worktree, channel);
    let pane_result = crate::harness::target::resolve_targets(snapshot, raw, worktree, channel);
    match (agent_result, pane_result) {
        (Ok(agents), Ok(panes)) => Ok(combine_queue_targets(snapshot, agents, panes)),
        (Ok(agents), Err(_)) => Ok(combine_queue_targets(snapshot, agents, Vec::new())),
        (Err(_), Ok(panes)) => Ok(combine_queue_targets(snapshot, Vec::new(), panes)),
        (Err(err), Err(_)) => {
            durable_targets(snapshot, durable_agents, raw, worktree, channel, err)
                .map(|agents| combine_queue_targets(snapshot, agents, Vec::new()))
        }
    }
}

pub fn durable_target_agents(store: &Store) -> Result<Vec<AgentState>> {
    Ok(store
        .runtime_projection(crate::RuntimeScope::Audit)?
        .agents
        .into_iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .collect())
}

fn durable_targets<'a>(
    _snapshot: &'a SidebarSnapshot,
    durable_agents: Option<&'a [AgentState]>,
    raw: &str,
    worktree: Option<&str>,
    channel: Option<&str>,
    live_err: TargetErr,
) -> std::result::Result<Vec<&'a AgentState>, TargetErr> {
    let Some(durable_agents) = durable_agents else {
        return Err(live_err);
    };
    let candidates = durable_agents.iter().collect::<Vec<_>>();
    crate::harness::target::resolve_agents(raw, worktree, channel, &candidates)
}

pub fn handle_for_target(snapshot: &SidebarSnapshot, target: &QueueTarget<'_>) -> String {
    if let Some(agent) = target.agent {
        let peers: Vec<&AgentState> = snapshot.root_agents().collect();
        crate::harness::target::agent_handle(agent, &peers, true)
    } else if let Some(pane) = target.pane {
        send::handle_for_pane_target(snapshot, pane, None)
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

pub(crate) fn pane_matches_agent(pane: &PaneAgent, agent: &AgentState) -> bool {
    if pane.kind != agent.kind {
        return false;
    }
    if pane.agent_id.as_ref() == Some(&agent.agent_id) {
        return true;
    }
    pane.agent_id.is_none()
        && agent.agent_id.is_provisional()
        && pane.channel() == crate::harness::target::agent_channel(agent)
}

fn provisional_agent_for_pane<'a>(
    snapshot: &'a SidebarSnapshot,
    pane: &PaneAgent,
) -> Option<&'a AgentState> {
    snapshot.root_agents().find(|agent| {
        agent.kind == pane.kind
            && agent.agent_id.is_provisional()
            && crate::harness::target::agent_channel(agent) == pane.channel()
    })
}

pub fn rollup_targets_all_park_without_live(
    snapshot: &SidebarSnapshot,
    raw: &str,
    worktree: Option<&str>,
    channel: Option<&str>,
    pending: &[MessageRecord],
    gate: DeliveryGate,
    force: bool,
) -> bool {
    if crate::harness::target::is_broadcast(raw) {
        return false;
    }
    let Ok(agents) = crate::harness::target::resolve_many(snapshot, raw, worktree, channel) else {
        return false;
    };
    let now = Timestamp::now();
    agents
        .iter()
        .all(|agent| !agent_needs_live_queue_resolution(snapshot, pending, agent, gate, force, now))
}

fn agent_needs_live_queue_resolution(
    _snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    agent: &AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
) -> bool {
    agent.agent_id.is_provisional()
        || agent_kind_registers_lazily(agent)
        || (gate_open_for_agent(gate, agent, force)
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

fn agent_kind_registers_lazily(agent: &AgentState) -> bool {
    crate::agents::descriptor_by_kind(agent.kind.as_str())
        .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
}

fn live_prompt_for_target(
    workspace_id: crate::ids::WorkspaceId,
    target: &QueueTarget<'_>,
    pane: &PaneAgent,
    bound: Option<&AgentState>,
    scope_channel: Option<&str>,
    draft: send::MessageDraft,
) -> MessageRecord {
    let send::MessageDraft {
        text,
        body,
        address,
        enter,
        gate,
        sender,
        force,
        auto_compact,
    } = draft;
    if let Some(agent) = target.agent {
        return MessageRecord::new(workspace_id, agent, text, enter, gate)
            .with_body(body)
            .with_force(force)
            .with_address(address)
            .with_channel(
                crate::harness::target::agent_channel(agent).or_else(|| {
                    crate::harness::target::recipient_channel(pane, bound, scope_channel)
                }),
            )
            .with_sender(sender)
            .with_pane_id(pane.pane_id.clone())
            .with_auto_compact(auto_compact);
    }
    send::message_for_target(
        workspace_id,
        pane,
        bound,
        scope_channel,
        send::MessageDraft {
            text,
            body,
            address,
            enter,
            gate,
            sender,
            force,
            auto_compact,
        },
    )
}

pub fn dispatch_for_targets(
    mut ctx: DispatchContext<'_>,
    targets: &[QueueTarget<'_>],
    text: &str,
    mode: SendMode,
) -> Result<DispatchResult> {
    let mut live_send = send::LiveSend {
        force: mode.force(),
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut kinds_seen = BTreeSet::new();
    let mut compacted = Vec::new();
    let now = Timestamp::now();
    for target in targets {
        let handle = handle_for_target(ctx.snapshot, target);
        let park = match mode {
            SendMode::Steer { .. } => target.pane.is_none(),
            SendMode::Boundary { .. } => {
                let pending = ctx
                    .pending
                    .as_ref()
                    .map_or(&[][..], |pending| pending.as_slice());
                mode.not_before().is_some()
                    || !target.receivable_now(ctx.snapshot, pending, mode.gate(), mode.force(), now)
            }
        };

        if !park && let Some(pane) = target.pane {
            let bound = target.bound(ctx.snapshot);
            let message = live_prompt_for_target(
                ctx.workspace.workspace_id.clone(),
                target,
                pane,
                bound,
                ctx.scope_channel,
                send::MessageDraft {
                    text: text.to_owned(),
                    body: MessageBody::Prompt,
                    address: Some(handle.clone()),
                    enter: mode.enter(),
                    gate: mode.gate(),
                    sender: ctx.sender.clone(),
                    force: mode.force(),
                    auto_compact: mode.auto_compact(),
                },
            );
            let message_id = message.message_id.clone();
            ctx.store
                .queue_message(&message, &ctx.workspace.session_name)?;
            let mut recovery = LiveRecovery {
                workspace: ctx.workspace,
                store: ctx.store,
                snapshot: ctx.snapshot,
                live_send: &mut live_send,
                park_on_failure: target.agent.is_some(),
            };
            match send_live_with_recovery(&mut recovery, pane, bound, &message)? {
                LiveAttempt::Sent {
                    message_id,
                    compacted: was_compacted,
                } => {
                    if was_compacted {
                        compacted.push(handle.clone());
                    }
                    outcomes.push(DispatchOutcome::Sent {
                        label: handle,
                        message_id,
                    });
                    continue;
                }
                LiveAttempt::SkippedWaiting { message_id } => {
                    if mode.is_steer() {
                        ctx.store.record_send_error(
                            &message,
                            "agent is waiting on input in its pane",
                            &ctx.workspace.session_name,
                        )?;
                        outcomes.push(DispatchOutcome::SkippedWaiting {
                            label: handle,
                            message_id,
                        });
                    } else {
                        ctx.store.record_message_delivery_failure(
                            &message_id,
                            "agent is waiting on input in its pane",
                            &ctx.workspace.session_name,
                        )?;
                        push_pending(&mut ctx.pending, message);
                        outcomes.push(DispatchOutcome::Queued {
                            label: handle,
                            message_id,
                        });
                    }
                    continue;
                }
                LiveAttempt::ParkInstead => {
                    if !mode.is_steer() {
                        push_pending(&mut ctx.pending, message);
                    }
                    outcomes.push(DispatchOutcome::Queued {
                        label: handle,
                        message_id,
                    });
                    continue;
                }
            }
        }
        if !park {
            continue;
        }
        let Some(agent) = target.agent else {
            return Err(DispatchErr::NoDurableSession { label: handle });
        };
        if kinds_seen.insert(agent.kind.as_str().to_owned()) {
            preflight_queue_hooks(agent)?;
        }
        let message = MessageRecord::new(
            ctx.workspace.workspace_id.clone(),
            agent,
            text.to_owned(),
            mode.enter(),
            mode.gate(),
        )
        .with_force(mode.force())
        .with_address(Some(handle.clone()))
        .with_channel(crate::harness::target::agent_channel(agent))
        .with_sender(ctx.sender.clone())
        .with_auto_compact(mode.auto_compact())
        .with_not_before(mode.not_before());
        let message_id = message.message_id.clone();
        ctx.store
            .queue_message(&message, &ctx.workspace.session_name)?;
        push_pending(&mut ctx.pending, message);
        outcomes.push(DispatchOutcome::Queued {
            label: handle,
            message_id,
        });
    }
    deliver::register_message_wake(ctx.workspace, ctx.store)?;
    Ok(DispatchResult {
        outcomes,
        compacted,
    })
}

fn push_pending(pending: &mut Option<&mut Vec<MessageRecord>>, message: MessageRecord) {
    if let Some(pending) = pending.as_mut() {
        pending.push(message);
    }
}

fn send_live_with_recovery(
    recovery: &mut LiveRecovery<'_>,
    pane: &PaneAgent,
    bound: Option<&AgentState>,
    message: &MessageRecord,
) -> Result<LiveAttempt> {
    let sent = match send::send_batch_to_live_pane(
        recovery.workspace,
        recovery.store,
        recovery.snapshot,
        pane,
        bound,
        std::slice::from_ref(message),
        recovery.live_send,
    ) {
        Ok(sent) => sent,
        Err(err) => {
            if deliver::message_recorded_as_sent(recovery.store, &message.message_id)? {
                return Ok(LiveAttempt::Sent {
                    message_id: message.message_id.clone(),
                    compacted: false,
                });
            }
            if recovery.park_on_failure {
                recovery.store.record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &recovery.workspace.session_name,
                )?;
                deliver::register_message_wake(recovery.workspace, recovery.store)?;
                return Ok(LiveAttempt::ParkInstead);
            }
            recovery.store.record_send_error(
                message,
                &err.to_string(),
                &recovery.workspace.session_name,
            )?;
            deliver::register_message_wake(recovery.workspace, recovery.store)?;
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
    }
}

fn preflight_queue_hooks(agent: &AgentState) -> Result<()> {
    let Some(adapter) = crate::agents::find_adapter(agent.kind.as_str()) else {
        return Err(DispatchErr::UnknownAgentKind(agent.kind.clone()));
    };
    if !adapter.hooks_installed() {
        return Err(DispatchErr::HooksMissing {
            kind: agent.kind.clone(),
        });
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        return Err(DispatchErr::HooksUntrusted {
            kind: agent.kind.clone(),
            hooks: untrusted.join(", "),
            fix: crate::agents::hook_trust_fix(agent.kind.as_str()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::{AgentStatus, TurnPhase};
    use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
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

        let future = MessageRecord::new(
            workspace_id(),
            &idle,
            "future".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_not_before(Some(timestamp + jiff::SignedDuration::from_secs(60)));
        assert!(
            QueueTarget {
                pane: Some(&pane),
                agent: Some(&idle),
            }
            .receivable_now(
                &idle_snapshot,
                &[future],
                DeliveryGate::Done,
                false,
                timestamp
            )
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
    }

    #[test]
    fn queue_targets_fall_back_to_durable_agents_after_live_miss() {
        let mut reviewer = agent("sess-reviewer", AgentStatus::Idle);
        reviewer.role = Some("reviewer".to_owned());
        let durable = vec![reviewer.clone()];
        let snapshot = snapshot_with_panes(Vec::new(), Vec::new());

        let targets = queue_targets(
            &snapshot,
            Some(&durable),
            "@reviewer",
            None,
            Some("project"),
            false,
        )
        .expect("durable target resolves");

        assert_eq!(targets.len(), 1);
        assert!(targets[0].pane.is_none());
        assert_eq!(
            targets[0].agent.expect("durable agent").agent_id,
            reviewer.agent_id
        );
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

    fn snapshot_with_ask(mut agent: AgentState, pane: PaneAgent) -> SidebarSnapshot {
        agent.status = AgentStatus::Waiting;
        agent.phase = TurnPhase::Idle;
        agent.waiting_since = Some(agent.last_activity);
        let mut snapshot = SidebarSnapshot::build_with_agents(
            workspace_id(),
            Vec::<()>::new(),
            vec![agent],
            now(),
        );
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
            name_explicit: false,
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

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }
}
