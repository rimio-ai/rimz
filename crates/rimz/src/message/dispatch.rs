//! Park-vs-live target resolution and add-message dispatch.

use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::feed::pending_ask_in_snapshot;
use crate::ids::MessageId;
use crate::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, gate_open,
    message_interval_from_env, queue_head,
};
use crate::workspace::ResolvedWorkspace;
use crate::{Ledger, PaneAgent, SidebarSnapshot, TargetErr};

use super::{deliver, send};

#[derive(Clone, Copy)]
pub struct QueueTarget<'a> {
    pane: Option<&'a PaneAgent>,
    agent: Option<&'a AgentState>,
}

pub struct AddSpec {
    pub enter: bool,
    pub gate: DeliveryGate,
    pub force: bool,
    pub auto_compact: Option<AutoCompact>,
    pub not_before: Option<Timestamp>,
    pub stamp_channel: bool,
}

pub struct SteerSpec {
    pub enter: bool,
    pub force: bool,
    pub auto_compact: Option<AutoCompact>,
}

pub struct AddOutput {
    pub label: String,
    pub message_id: MessageId,
    pub status: MessageStatus,
}

pub struct AddResult {
    pub outputs: Vec<AddOutput>,
    pub compacted: Vec<String>,
}

pub struct SteerResult {
    pub outcomes: Vec<SteerOutcome>,
    pub compacted: Vec<String>,
}

pub enum SteerOutcome {
    Sent {
        label: String,
        message_id: MessageId,
    },
    Queued {
        label: String,
        message_id: MessageId,
    },
    SkippedPending {
        label: String,
        message_id: MessageId,
        request_id: String,
    },
}

pub struct AddContext<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub ledger: &'a Ledger,
    pub snapshot: &'a SidebarSnapshot,
    pub pending: &'a mut Vec<MessageRecord>,
    pub scope_channel: Option<&'a str>,
    pub sender: &'a MessageSender,
}

pub struct SteerContext<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub ledger: &'a Ledger,
    pub snapshot: &'a SidebarSnapshot,
    pub scope_channel: Option<&'a str>,
    pub sender: &'a MessageSender,
}

pub type Result<T> = std::result::Result<T, DispatchErr>;

#[derive(Debug, thiserror::Error)]
pub enum DispatchErr {
    #[error(transparent)]
    Ledger(#[from] crate::ledger::LedgerErr),
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
    SkippedPending {
        message_id: MessageId,
        request_id: String,
    },
    ParkInstead,
}

struct LiveRecovery<'a> {
    workspace: &'a ResolvedWorkspace,
    ledger: &'a Ledger,
    snapshot: &'a SidebarSnapshot,
    live_send: &'a mut send::LiveSend,
    park_on_failure: bool,
}

impl QueueTarget<'_> {
    pub fn label(&self) -> String {
        self.agent
            .map(agent_label)
            .or_else(|| self.pane.map(PaneAgent::label))
            .unwrap_or_else(|| "agent".to_owned())
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
                gate_open(gate, agent.effective_status())
                    && (force || pending_ask_in_snapshot(agent, snapshot).is_none())
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

pub fn durable_target_agents(ledger: &Ledger) -> Result<Vec<AgentState>> {
    Ok(ledger
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
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    agent: &AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
) -> bool {
    agent.agent_id.is_provisional()
        || agent_kind_registers_lazily(agent)
        || (gate_open(gate, agent.effective_status())
            && (force || pending_ask_in_snapshot(agent, snapshot).is_none())
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
            enter,
            gate,
            sender,
            force,
            auto_compact,
        },
    )
}

pub fn add_for_targets(
    ctx: AddContext<'_>,
    targets: &[QueueTarget<'_>],
    text: &str,
    spec: AddSpec,
) -> Result<AddResult> {
    let mut live_send = send::LiveSend {
        force: spec.force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut kinds_seen = BTreeSet::new();
    let mut compacted = Vec::new();
    let mut outputs = Vec::new();
    let now = Timestamp::now();
    for target in targets {
        let handle = handle_for_target(ctx.snapshot, target);
        let park = spec.not_before.is_some()
            || !target.receivable_now(ctx.snapshot, ctx.pending, spec.gate, spec.force, now);
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
                    enter: spec.enter,
                    gate: spec.gate,
                    sender: ctx.sender.clone(),
                    force: spec.force,
                    auto_compact: spec.auto_compact,
                },
            );
            let message_id = message.message_id.clone();
            ctx.ledger
                .queue_message(&message, &ctx.workspace.session_name)?;
            let mut recovery = LiveRecovery {
                workspace: ctx.workspace,
                ledger: ctx.ledger,
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
                    outputs.push(AddOutput {
                        label: handle,
                        message_id,
                        status: MessageStatus::Sent,
                    });
                    continue;
                }
                LiveAttempt::SkippedPending { request_id, .. } => {
                    ctx.ledger.record_message_delivery_failure(
                        &message_id,
                        &format!("pending ask {request_id} reserves input"),
                        &ctx.workspace.session_name,
                    )?;
                    ctx.pending.push(message);
                    outputs.push(AddOutput {
                        label: handle,
                        message_id,
                        status: MessageStatus::Queued,
                    });
                    continue;
                }
                LiveAttempt::ParkInstead => {
                    ctx.pending.push(message);
                    outputs.push(AddOutput {
                        label: handle,
                        message_id,
                        status: MessageStatus::Queued,
                    });
                    continue;
                }
            }
        }
        if !park {
            continue;
        }
        let Some(agent) = target.agent else {
            return Err(DispatchErr::NoDurableSession {
                label: target.label(),
            });
        };
        if kinds_seen.insert(agent.kind.as_str().to_owned()) {
            preflight_queue_hooks(agent)?;
        }
        let message = MessageRecord::new(
            ctx.workspace.workspace_id.clone(),
            agent,
            text.to_owned(),
            spec.enter,
            spec.gate,
        )
        .with_force(spec.force)
        .with_channel(
            spec.stamp_channel
                .then(|| crate::harness::target::agent_channel(agent))
                .flatten(),
        )
        .with_sender(ctx.sender.clone())
        .with_auto_compact(spec.auto_compact)
        .with_not_before(spec.not_before);
        let message_id = message.message_id.clone();
        ctx.ledger
            .queue_message(&message, &ctx.workspace.session_name)?;
        ctx.pending.push(message);
        outputs.push(AddOutput {
            label: handle,
            message_id,
            status: MessageStatus::Queued,
        });
    }
    deliver::register_message_wake(ctx.workspace, ctx.ledger)?;
    Ok(AddResult { outputs, compacted })
}

pub fn steer_for_targets(
    ctx: SteerContext<'_>,
    targets: &[QueueTarget<'_>],
    text: &str,
    spec: SteerSpec,
) -> Result<SteerResult> {
    let mut live_send = send::LiveSend {
        force: spec.force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut compacted = Vec::new();
    for target in targets {
        let handle = handle_for_target(ctx.snapshot, target);
        let Some(pane) = target.pane else {
            let Some(agent) = target.agent else {
                return Err(DispatchErr::NoDurableSession {
                    label: target.label(),
                });
            };
            preflight_queue_hooks(agent)?;
            let message = MessageRecord::new(
                ctx.workspace.workspace_id.clone(),
                agent,
                text.to_owned(),
                spec.enter,
                DeliveryGate::Any,
            )
            .with_force(spec.force)
            .with_channel(crate::harness::target::agent_channel(agent))
            .with_sender(ctx.sender.clone())
            .with_auto_compact(spec.auto_compact);
            let message_id = message.message_id.clone();
            ctx.ledger
                .queue_message(&message, &ctx.workspace.session_name)?;
            outcomes.push(SteerOutcome::Queued {
                label: handle,
                message_id,
            });
            continue;
        };
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
                enter: spec.enter,
                gate: DeliveryGate::Any,
                sender: ctx.sender.clone(),
                force: spec.force,
                auto_compact: spec.auto_compact,
            },
        );
        let message_id = message.message_id.clone();
        ctx.ledger
            .queue_message(&message, &ctx.workspace.session_name)?;
        let mut recovery = LiveRecovery {
            workspace: ctx.workspace,
            ledger: ctx.ledger,
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
                outcomes.push(SteerOutcome::Sent {
                    label: handle,
                    message_id,
                });
            }
            LiveAttempt::SkippedPending {
                message_id,
                request_id,
            } => {
                ctx.ledger.record_message_delivery_failure(
                    &message_id,
                    &format!("pending ask {request_id} reserves input"),
                    &ctx.workspace.session_name,
                )?;
                outcomes.push(SteerOutcome::SkippedPending {
                    label: handle,
                    message_id,
                    request_id,
                });
            }
            LiveAttempt::ParkInstead => outcomes.push(SteerOutcome::Queued {
                label: handle,
                message_id,
            }),
        }
    }
    deliver::register_message_wake(ctx.workspace, ctx.ledger)?;
    Ok(SteerResult {
        outcomes,
        compacted,
    })
}

fn send_live_with_recovery(
    recovery: &mut LiveRecovery<'_>,
    pane: &PaneAgent,
    bound: Option<&AgentState>,
    message: &MessageRecord,
) -> Result<LiveAttempt> {
    let sent = match send::send_batch_to_live_pane(
        recovery.workspace,
        recovery.ledger,
        recovery.snapshot,
        pane,
        bound,
        std::slice::from_ref(message),
        recovery.live_send,
    ) {
        Ok(sent) => sent,
        Err(err) => {
            if deliver::message_recorded_as_sent(recovery.ledger, &message.message_id)? {
                return Ok(LiveAttempt::Sent {
                    message_id: message.message_id.clone(),
                    compacted: false,
                });
            }
            if recovery.park_on_failure {
                recovery.ledger.record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &recovery.workspace.session_name,
                )?;
                deliver::register_message_wake(recovery.workspace, recovery.ledger)?;
                return Ok(LiveAttempt::ParkInstead);
            }
            recovery.ledger.record_send_error(
                message,
                &err.to_string(),
                &recovery.workspace.session_name,
            )?;
            deliver::register_message_wake(recovery.workspace, recovery.ledger)?;
            return Err(err.into());
        }
    };
    match sent.outcome {
        send::Outcome::Sent { message_id, .. } => Ok(LiveAttempt::Sent {
            message_id,
            compacted: sent.compacted.is_some(),
        }),
        send::Outcome::SkippedPending {
            message_id,
            request_id,
            ..
        } => Ok(LiveAttempt::SkippedPending {
            message_id,
            request_id,
        }),
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

fn agent_label(agent: &AgentState) -> String {
    agent
        .name
        .clone()
        .unwrap_or_else(|| match agent.kind_ordinal {
            Some(ordinal) => format!("{}-{}", agent.kind, ordinal),
            None => agent.kind.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::agents::{AgentStatus, TurnPhase};
    use crate::feed::{FeedItem, FeedKind, Surface};
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

    fn snapshot_with_ask(agent: AgentState, pane: PaneAgent) -> SidebarSnapshot {
        let mut item = FeedItem::new(
            workspace_id(),
            Surface::NativeUi,
            FeedKind::Permission,
            "approve?",
            agent.kind.as_str(),
            "agent-hook",
        );
        item.payload = json!({ "session_id": agent.agent_id.as_str() });
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id(), vec![item], vec![agent], now());
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

    fn bound_pane(agent: &AgentState, raw: &str) -> PaneAgent {
        PaneAgent {
            kind: agent.kind.clone(),
            kind_ordinal: agent.kind_ordinal,
            name: agent.name.clone(),
            profile: None,
            role: None,
            team: None,
            channel: None,
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            worktree_path: agent.worktree_path.clone(),
            worktree_branch: agent.worktree_branch.clone(),
        }
    }

    fn lazy_pane(kind: &str, raw: &str) -> PaneAgent {
        PaneAgent {
            kind: AgentKind::new_unchecked(kind),
            kind_ordinal: None,
            name: None,
            profile: None,
            role: None,
            team: None,
            channel: None,
            agent_id: None,
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            worktree_path: Some("/repo/project".to_owned()),
            worktree_branch: Some("project".to_owned()),
        }
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }
}
