//! Queued-message delivery, sweeping, and wake-cache maintenance.

use std::fs::File;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;

use crate::agents::AgentStatus;
use crate::feed::pending_ask_in_snapshot;
use crate::ids::{MessageId, MuxName, PaneId, RequestId};
use crate::message::{
    DeliveryGate, MessageRecord, MessageStatus, delivery_window_from_env, gate_open,
    max_delivery_attempts_from_env, message_interval_from_env, queue_batch_tail, queue_head,
    queue_head_for_message,
};
use crate::workspace::ResolvedWorkspace;
use crate::{Ledger, PaneAgent, RuntimePaths, SidebarSnapshot};

use super::{dispatch, send};

pub type Result<T> = std::result::Result<T, DeliverErr>;

#[derive(Debug, thiserror::Error)]
pub enum DeliverErr {
    #[error(transparent)]
    Ledger(#[from] crate::ledger::LedgerErr),
    #[error(transparent)]
    Path(#[from] crate::ledger::paths::PathErr),
    #[error(transparent)]
    Atomic(#[from] crate::ledger::atomic::AtomicErr),
    #[error(transparent)]
    Produce(#[from] crate::sidebar::produce::ProduceErr),
    #[error(transparent)]
    Send(#[from] send::SendErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn deliver_one(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    message_id: &MessageId,
    settle: Duration,
    mux: Option<MuxName>,
) -> Result<bool> {
    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    let Some(candidate) = delivery_candidate(workspace, ledger, message_id, mux)? else {
        return Ok(false);
    };
    let Some(message) = ledger.claim_message_for_delivery(message_id, jiff::Timestamp::now())?
    else {
        return Ok(false);
    };
    debug_assert!(message.same_agent(&candidate.message.kind, &candidate.message.agent_id));
    debug_assert_eq!(message.message_id, candidate.message.message_id);
    let mut claimed = vec![message];
    for tail in &candidate.batch_tail {
        match ledger.claim_message_for_delivery(&tail.message_id, jiff::Timestamp::now())? {
            Some(message) => claimed.push(message),
            None => break,
        }
    }
    if claimed.len() > 1 {
        let batch_id = claimed[0].message_id.clone();
        for message in &mut claimed {
            message.batch_id = Some(batch_id.clone());
        }
    }
    // Hook delivery handles one claimed message; settle above owns any
    // pre-delivery spacing, so this pacer's first tick stays a no-op.
    let mut live_send = send::LiveSend {
        force: claimed[0].force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let send_messages: Vec<MessageRecord> = claimed
        .iter()
        .cloned()
        .map(|message| message.with_pane_id(candidate.target.pane_id.clone()))
        .collect();
    let sent = send::send_batch_to_live_pane(
        workspace,
        ledger,
        &candidate.snapshot,
        &candidate.target,
        send::bound_agent(&candidate.snapshot, &candidate.target),
        &send_messages,
        &mut live_send,
    );
    match sent {
        Ok(send::SentPrompt {
            outcome: send::Outcome::Sent { .. },
            ..
        }) => {
            register_message_wake(workspace, ledger)?;
            Ok(true)
        }
        Ok(send::SentPrompt {
            outcome: send::Outcome::SkippedPending { request_id, .. },
            ..
        }) => {
            for message in &claimed {
                ledger.record_message_delivery_failure(
                    &message.message_id,
                    &format!("pending ask {request_id} reserves input"),
                    &workspace.session_name,
                )?;
            }
            Ok(false)
        }
        Err(err) => {
            let mut head_failure_recorded = true;
            for message in &claimed {
                if message_recorded_as_sent(ledger, &message.message_id)? {
                    continue;
                }
                let recorded = ledger.record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &workspace.session_name,
                )?;
                if message.message_id == claimed[0].message_id {
                    head_failure_recorded = recorded.is_some();
                }
            }
            if !head_failure_recorded {
                ledger.record_send_error(
                    &send_messages[0],
                    &err.to_string(),
                    &workspace.session_name,
                )?;
            }
            register_message_wake(workspace, ledger)?;
            Ok(false)
        }
    }
}

pub fn sweep(workspace: &ResolvedWorkspace, ledger: &Ledger, mux: Option<MuxName>) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let Some(_guard) = try_start_sweep(&runtime)? else {
        return Ok(());
    };
    let now = Timestamp::now();
    let delivery_window = delivery_window_from_env();
    ledger.reconcile_stale_sent_messages(
        &workspace.session_name,
        now,
        delivery_window,
        max_delivery_attempts_from_env(),
    )?;
    let pending = ledger.list_pending_messages()?;
    let mut heads_seen = std::collections::BTreeSet::new();
    for message in pending.iter().filter(|message| message.is_ready(now)) {
        let Some(head) = queue_head(
            pending.iter(),
            &message.kind,
            &message.agent_id,
            message.agent_name.as_deref(),
            now,
        ) else {
            continue;
        };
        if heads_seen.insert(head.message_id.to_string()) {
            let delivered = deliver_one(workspace, ledger, &head.message_id, Duration::ZERO, mux)?;
            if !delivered {
                ledger.defer_message_wake(&head.message_id, now + delivery_window)?;
            }
        }
    }
    register_message_wake(workspace, ledger)?;
    Ok(())
}

struct SweepRunGuard {
    file: File,
}

impl Drop for SweepRunGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn try_start_sweep(runtime: &RuntimePaths) -> Result<Option<SweepRunGuard>> {
    let path = runtime.root.join("message-sweep.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| DeliverErr::Io {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(Some(SweepRunGuard { file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(DeliverErr::Io { path, source }),
    }
}

struct DeliveryCandidate {
    message: MessageRecord,
    batch_tail: Vec<MessageRecord>,
    snapshot: SidebarSnapshot,
    target: PaneAgent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeliveryCheck {
    pub schedule: ScheduleCheck,
    pub fifo: FifoCheck,
    pub agent: AgentCheck,
    pub gate: GateCheck,
    pub ask: AskCheck,
    pub pane: PaneCheck,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleCheck {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FifoCheck {
    pub head: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<MessageId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentCheck {
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GateCheck {
    pub gate: DeliveryGate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_recovered: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AskCheck {
    pub clear: bool,
    pub force: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneCheck {
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_pane_id: Option<PaneId>,
}

pub fn explain(
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    now: Timestamp,
) -> DeliveryCheck {
    let schedule = ScheduleCheck {
        ready: message.is_ready(now),
        not_before: message.not_before,
        retry_after: message.retry_after,
    };
    let fifo = if message.status == MessageStatus::Queued && schedule.ready {
        match queue_head_for_message(pending.iter(), message, now) {
            Some(head) if head.message_id == message.message_id => FifoCheck {
                head: true,
                blocker: None,
            },
            Some(head) => FifoCheck {
                head: false,
                blocker: Some(head.message_id.clone()),
            },
            None => FifoCheck {
                head: false,
                blocker: None,
            },
        }
    } else {
        FifoCheck {
            head: true,
            blocker: None,
        }
    };
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| message.same_agent_card(agent));
    let status = agent.map(crate::agents::AgentState::effective_status);
    let open = status.is_some_and(|status| gate_open(message.gate, status));
    let resume_recovered =
        match (message.gate, agent, open) {
            (DeliveryGate::Resume, Some(agent), true) => {
                let runtime = RuntimePaths::for_workspace(message.workspace_id.clone()).ok();
                Some(runtime.as_ref().is_some_and(|runtime| {
                    crate::agents::resume_gate_recovered(runtime, agent, now)
                }))
            }
            _ => None,
        };
    let request_id = agent
        .and_then(|agent| (!message.force).then(|| pending_ask_in_snapshot(agent, snapshot)))
        .flatten()
        .map(|item| item.request_id.clone());
    let pane = agent.and_then(|agent| {
        snapshot.agent_panes.iter().find(|pane| {
            dispatch::pane_matches_agent(pane, agent)
                && message
                    .pane_id
                    .as_ref()
                    .is_none_or(|pane_id| pane.pane_id == *pane_id)
        })
    });
    DeliveryCheck {
        schedule,
        fifo,
        agent: AgentCheck {
            present: agent.is_some(),
        },
        gate: GateCheck {
            gate: message.gate,
            status,
            open,
            resume_recovered,
        },
        ask: AskCheck {
            clear: request_id.is_none(),
            force: message.force,
            request_id,
        },
        pane: PaneCheck {
            present: pane.is_some(),
            pane_id: pane.map(|pane| pane.pane_id.clone()),
            pinned_pane_id: message.pane_id.clone(),
        },
    }
}

fn delivery_candidate(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    message_id: &MessageId,
    mux: Option<MuxName>,
) -> Result<Option<DeliveryCandidate>> {
    let pending = ledger.list_pending_messages()?;
    let Some(message) = pending
        .iter()
        .find(|message| message.message_id == *message_id)
        .cloned()
    else {
        return Ok(None);
    };
    let now = Timestamp::now();
    if !message.is_ready(now) {
        return Ok(None);
    }
    let Some(head) = queue_head_for_message(pending.iter(), &message, now) else {
        return Ok(None);
    };
    if head.message_id != *message_id {
        return Ok(None);
    }
    let mut snapshot = crate::sidebar::produce::resolution_snapshot(workspace, ledger, mux)?;
    // The resolution snapshot carries live panes but not rich context sidecars.
    // Fold them here for smart-compact gauges and parked-status delivery gates.
    let runtime = RuntimePaths::for_workspace(message.workspace_id.clone()).ok();
    if let Some(runtime) = runtime.as_ref() {
        snapshot = snapshot.with_agent_context(crate::ledger::agent_context::read_all(runtime));
    }
    let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| message.same_agent_card(agent))
    else {
        return Ok(None);
    };
    let status = agent.effective_status();
    if !gate_open(message.gate, status) {
        return Ok(None);
    }
    if message.gate == DeliveryGate::Resume
        && !runtime
            .as_ref()
            .is_some_and(|runtime| crate::agents::resume_gate_recovered(runtime, agent, now))
    {
        return Ok(None);
    }
    // A pending ask reserves the agent's next input, so it defers delivery —
    // unless the message was queued with `--force`, mirroring `message --steer --force`.
    if !message.force && pending_ask_in_snapshot(agent, &snapshot).is_some() {
        return Ok(None);
    }
    let batch_tail = queue_batch_tail(pending.iter(), &message, status, now)
        .into_iter()
        .cloned()
        .collect();
    let Some(target) = snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            dispatch::pane_matches_agent(pane, agent)
                && message
                    .pane_id
                    .as_ref()
                    .is_none_or(|pane_id| pane.pane_id == *pane_id)
        })
        .cloned()
    else {
        return Ok(None);
    };
    Ok(Some(DeliveryCandidate {
        message,
        batch_tail,
        snapshot,
        target,
    }))
}

pub fn register_message_wake(workspace: &ResolvedWorkspace, ledger: &Ledger) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    refresh_wake_stamp(&runtime, ledger, Timestamp::now())
}

pub fn refresh_wake_stamp(runtime: &RuntimePaths, ledger: &Ledger, now: Timestamp) -> Result<()> {
    let path = wake_stamp_path(runtime);
    let next = ledger.earliest_message_wake(now, delivery_window_from_env())?;
    match next {
        Some(not_before) => {
            crate::ledger::atomic::write_temp_then_rename_cache(&path, &Some(not_before))?;
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(DeliverErr::Io { path, source: err });
            }
        },
    }
    Ok(())
}

pub fn message_recorded_as_sent(ledger: &Ledger, message_id: &MessageId) -> Result<bool> {
    Ok(ledger
        .list_messages()?
        .iter()
        .any(|message| message.message_id == *message_id && message.status == MessageStatus::Sent))
}

pub(crate) fn wake_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(crate::message::MESSAGE_WAKE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::agents::{AgentState, TurnPhase};
    use crate::feed::{FeedItem, FeedKind, Surface};
    use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
    use crate::ledger::snapshot::PaneAgent;

    #[test]
    fn sweep_guard_is_single_flight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/message-sweep")),
            dir.path(),
        )
        .expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");

        let first = try_start_sweep(&runtime)
            .expect("first guard")
            .expect("first guard starts");
        assert!(
            try_start_sweep(&runtime).expect("second guard").is_none(),
            "a running sweep keeps later helpers from entering delivery"
        );
        drop(first);
        assert!(
            try_start_sweep(&runtime).expect("third guard").is_some(),
            "the sweep lock releases when the helper exits"
        );
    }

    #[test]
    fn explain_reports_fifo_blocker() {
        let now = Timestamp::now();
        let agent = agent("sess-fifo", AgentStatus::Idle);
        let snapshot = snapshot(agent.clone(), true, Vec::new(), now);
        let older = message(&agent, 1, "first");
        let newer = message(&agent, 2, "second");

        let check = explain(&newer, &[newer.clone(), older.clone()], &snapshot, now);

        assert!(!check.fifo.head);
        assert_eq!(check.fifo.blocker, Some(older.message_id));
        assert!(check.gate.open);
        assert!(check.pane.present);
    }

    #[test]
    fn explain_reports_gate_closed() {
        let now = Timestamp::now();
        let agent = agent("sess-gate", AgentStatus::Running);
        let snapshot = snapshot(agent.clone(), true, Vec::new(), now);
        let message = message(&agent, 1, "wait");

        let check = explain(&message, std::slice::from_ref(&message), &snapshot, now);

        assert_eq!(check.gate.status, Some(AgentStatus::Running));
        assert!(!check.gate.open);
        assert!(check.pane.present);
    }

    #[test]
    fn explain_reports_pending_ask() {
        let now = Timestamp::now();
        let agent = agent("sess-ask", AgentStatus::Idle);
        let mut item = FeedItem::new(
            workspace_id(),
            Surface::NativeUi,
            FeedKind::Permission,
            "approve?",
            "claude",
            "agent-hook",
        );
        item.payload = json!({ "session_id": "sess-ask" });
        item.updated_at = agent.last_activity + jiff::SignedDuration::from_secs(1);
        let snapshot = snapshot(agent.clone(), true, vec![item], now);
        let message = message(&agent, 1, "blocked");

        let check = explain(&message, std::slice::from_ref(&message), &snapshot, now);

        assert!(!check.ask.clear);
        assert!(check.ask.request_id.is_some());
        assert!(check.pane.present);
    }

    #[test]
    fn explain_reports_no_live_pane() {
        let now = Timestamp::now();
        let agent = agent("sess-pane", AgentStatus::Idle);
        let snapshot = snapshot(agent.clone(), false, Vec::new(), now);
        let message = message(&agent, 1, "blocked");

        let check = explain(&message, std::slice::from_ref(&message), &snapshot, now);

        assert!(check.agent.present);
        assert!(check.gate.open);
        assert!(!check.pane.present);
    }

    #[test]
    fn explain_reports_scheduled_floor() {
        let now = Timestamp::now();
        let agent = agent("sess-scheduled", AgentStatus::Idle);
        let snapshot = snapshot(agent.clone(), true, Vec::new(), now);
        let mut message = message(&agent, 1, "later");
        let not_before = now + jiff::SignedDuration::from_secs(60);
        message.not_before = Some(not_before);

        let check = explain(&message, std::slice::from_ref(&message), &snapshot, now);

        assert!(!check.schedule.ready);
        assert_eq!(check.schedule.not_before, Some(not_before));
        assert!(check.fifo.head, "schedule is the first blocker");
    }

    fn snapshot(
        agent: AgentState,
        with_pane: bool,
        items: Vec<FeedItem>,
        now: Timestamp,
    ) -> SidebarSnapshot {
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id(), items, vec![agent], now);
        if with_pane {
            let agent = &snapshot.agents[0];
            snapshot.agent_panes = vec![PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: agent.kind_ordinal,
                name: agent.name.clone(),
                profile: agent.profile.clone(),
                role: agent.role.clone(),
                team: agent.team.clone(),
                channel: agent.channel.clone(),
                agent_id: Some(agent.agent_id.clone()),
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_3"),
                pane_pid: None,
                worktree_path: agent.worktree_path.clone(),
                worktree_branch: agent.worktree_branch.clone(),
            }];
        }
        snapshot
    }

    fn message(agent: &AgentState, id: u64, text: &str) -> MessageRecord {
        let mut message = MessageRecord::new(
            workspace_id(),
            agent,
            text.to_owned(),
            true,
            DeliveryGate::Done,
        );
        message.message_id = message_id(id);
        message
    }

    fn message_id(value: u64) -> MessageId {
        MessageId::parse(&format!("msg_{value:016}")).unwrap()
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn agent(id: &str, status: AgentStatus) -> AgentState {
        let now = Timestamp::now();
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
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
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
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
