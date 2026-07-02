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

pub struct AddOutput {
    pub label: String,
    pub message_id: MessageId,
    pub status: MessageStatus,
}

pub struct AddResult {
    pub outputs: Vec<AddOutput>,
    pub compacted: Vec<String>,
}

pub struct AddContext<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub ledger: &'a Ledger,
    pub snapshot: &'a SidebarSnapshot,
    pub pending: &'a mut Vec<MessageRecord>,
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
    raw: &str,
    worktree: Option<&str>,
    channel: Option<&str>,
    rollup_only: bool,
) -> std::result::Result<Vec<QueueTarget<'a>>, TargetErr> {
    if rollup_only {
        let agents = crate::harness::target::resolve_many(snapshot, raw, worktree, channel)?;
        return Ok(combine_queue_targets(snapshot, agents, Vec::new()));
    }
    let agent_result = crate::harness::target::resolve_many(snapshot, raw, worktree, channel);
    let pane_result = crate::harness::target::resolve_targets(snapshot, raw, worktree, channel);
    match (agent_result, pane_result) {
        (Ok(agents), Ok(panes)) => Ok(combine_queue_targets(snapshot, agents, panes)),
        (Ok(agents), Err(_)) => Ok(combine_queue_targets(snapshot, agents, Vec::new())),
        (Err(_), Ok(panes)) => Ok(combine_queue_targets(snapshot, Vec::new(), panes)),
        (Err(err), Err(_)) => Err(err),
    }
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
        let mut park = spec.not_before.is_some()
            || !target.receivable_now(ctx.snapshot, ctx.pending, spec.gate, spec.force, now);
        if !park && let Some(pane) = target.pane {
            let bound = target.bound(ctx.snapshot);
            let message = send::message_for_target(
                ctx.workspace.workspace_id.clone(),
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
            match send::send_prompt_to_live_pane(
                ctx.workspace,
                ctx.ledger,
                ctx.snapshot,
                pane,
                bound,
                &message,
                &mut live_send,
            ) {
                Ok(sent) => match sent.outcome {
                    send::Outcome::Sent { message_id, .. } => {
                        if sent.compacted.is_some() {
                            compacted.push(handle.clone());
                        }
                        deliver::register_message_wake(ctx.workspace, ctx.ledger)?;
                        outputs.push(AddOutput {
                            label: handle,
                            message_id,
                            status: MessageStatus::Sent,
                        });
                        continue;
                    }
                    send::Outcome::SkippedPending { .. } => park = true,
                },
                Err(err) => {
                    if deliver::message_recorded_as_sent(ctx.ledger, &message.message_id)? {
                        deliver::register_message_wake(ctx.workspace, ctx.ledger)?;
                        outputs.push(AddOutput {
                            label: handle,
                            message_id: message.message_id.clone(),
                            status: MessageStatus::Sent,
                        });
                        continue;
                    }
                    if deliver::is_mux_timeout(&err) && target.agent.is_some() {
                        park = true;
                    } else {
                        ctx.ledger.record_send_error(
                            &message,
                            &err.to_string(),
                            &ctx.workspace.session_name,
                        )?;
                        deliver::register_message_wake(ctx.workspace, ctx.ledger)?;
                        return Err(err.into());
                    }
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
            channel: None,
            status,
            phase,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                "terminal_3",
            ))),
            agent_pid: None,
            agent_process_start: None,
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
