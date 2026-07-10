//! Queued-message delivery, sweeping, and wake-cache maintenance.

use std::fs::File;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;

use crate::agents::AgentStatus;
use crate::ids::{MessageId, MuxName, PaneId};
use crate::message::{
    AfterCondition, DeliveryGate, MessageRecord, MessageStatus, after_condition_open, card_matches,
    delivery_window_from_env, gate_open_for_agent, max_delivery_attempts_from_env,
    message_interval_from_env, queue_batch_tail, queue_head, queue_head_for_message,
};
use crate::workspace::ResolvedWorkspace;
use crate::{PaneAgent, RuntimePaths, SidebarSnapshot, Store};

use super::{dispatch, send};

pub type Result<T> = std::result::Result<T, DeliverErr>;

#[derive(Debug, thiserror::Error)]
pub enum DeliverErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Path(#[from] crate::store::paths::PathErr),
    #[error(transparent)]
    Atomic(#[from] crate::store::atomic::AtomicErr),
    #[error(transparent)]
    Produce(#[from] crate::sidebar::produce::ProduceErr),
    #[error(transparent)]
    Send(#[from] send::SendErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryPolicy {
    Boundary,
    Steer { force: bool },
}

pub fn deliver_one(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    settle: Duration,
    mux: Option<MuxName>,
    policy: DeliveryPolicy,
) -> Result<bool> {
    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    let Some(candidate) = delivery_candidate(workspace, store, message_id, mux, policy)? else {
        return Ok(false);
    };
    let claimed_head = match policy {
        DeliveryPolicy::Boundary => {
            store.claim_message_for_delivery(message_id, jiff::Timestamp::now())?
        }
        DeliveryPolicy::Steer { .. } => {
            store.claim_message_for_steer(message_id, jiff::Timestamp::now())?
        }
    };
    let Some(message) = claimed_head else {
        return Ok(false);
    };
    debug_assert!(
        message.kind == candidate.message.kind && message.agent_id == candidate.message.agent_id
    );
    debug_assert_eq!(message.message_id, candidate.message.message_id);
    let mut claimed = vec![message];
    for tail in &candidate.batch_tail {
        match store.claim_message_for_delivery(&tail.message_id, jiff::Timestamp::now())? {
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
        force: claimed[0].force || matches!(policy, DeliveryPolicy::Steer { force: true }),
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let send_messages: Vec<MessageRecord> = claimed
        .iter()
        .cloned()
        .map(|message| message.with_pane_id(candidate.target.pane_id.clone()))
        .collect();
    let sent = send::send_batch_to_live_pane(
        workspace,
        store,
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
            register_message_wake(workspace, store)?;
            Ok(true)
        }
        Ok(send::SentPrompt {
            outcome: send::Outcome::SkippedWaiting { .. },
            ..
        }) => {
            for message in &claimed {
                store.record_message_delivery_failure(
                    &message.message_id,
                    "agent is waiting on input in its pane",
                    &workspace.session_name,
                )?;
            }
            Ok(false)
        }
        Err(err) => {
            let mut head_failure_recorded = true;
            for message in &claimed {
                if message_recorded_as_sent(store, &message.message_id)? {
                    continue;
                }
                let recorded = store.record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &workspace.session_name,
                )?;
                if message.message_id == claimed[0].message_id {
                    head_failure_recorded = recorded.is_some();
                }
            }
            if !head_failure_recorded {
                store.record_send_error(
                    &send_messages[0],
                    &err.to_string(),
                    &workspace.session_name,
                )?;
            }
            register_message_wake(workspace, store)?;
            Ok(false)
        }
    }
}

pub fn sweep(workspace: &ResolvedWorkspace, store: &Store, mux: Option<MuxName>) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let Some(_guard) = try_start_sweep(&runtime)? else {
        return Ok(());
    };
    let now = Timestamp::now();
    let delivery_window = delivery_window_from_env();
    store.reconcile_stale_sent_messages(
        &workspace.session_name,
        now,
        delivery_window,
        max_delivery_attempts_from_env(),
    )?;
    let live = store.list_messages()?;
    if live
        .iter()
        .any(|message| message.status == MessageStatus::Queued && !message.after_met())
    {
        let mut snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
        snapshot = snapshot.with_agent_context(crate::store::agent_context::read_all(&runtime));
        evaluate_after_conditions(workspace, store, &snapshot, &live, now)?;
    }
    let pending = store.list_pending_messages()?;
    let mut heads_seen = std::collections::BTreeSet::new();
    for message in pending.iter().filter(|message| message.is_deliverable(now)) {
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
            let delivered = deliver_one(
                workspace,
                store,
                &head.message_id,
                Duration::ZERO,
                mux,
                DeliveryPolicy::Boundary,
            )?;
            if !delivered {
                store.defer_message_wake(&head.message_id, now + delivery_window)?;
            }
        }
    }
    register_message_wake(workspace, store)?;
    Ok(())
}

fn evaluate_after_conditions(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    now: Timestamp,
) -> Result<()> {
    let mut stamps = Vec::new();
    let mut deferred = Vec::new();
    for message in pending
        .iter()
        .filter(|message| message.status == MessageStatus::Queued && !message.after_met())
    {
        let met = message
            .after
            .iter()
            .enumerate()
            .filter(|(_, condition)| condition.met_at.is_none())
            .filter_map(|(index, condition)| {
                after_condition_open(condition, message.gate, &snapshot.agents, pending, now)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let unmet_count = message
            .after
            .iter()
            .filter(|condition| condition.met_at.is_none())
            .count();
        if met.len() < unmet_count {
            deferred.push(message.message_id.clone());
        }
        if !met.is_empty() {
            stamps.push((message.message_id.clone(), met));
        }
    }
    if !stamps.is_empty() {
        store.stamp_after_conditions(&stamps, now, &workspace.session_name)?;
    }
    let retry_at = now + delivery_window_from_env();
    for message_id in deferred {
        store.defer_message_wake(&message_id, retry_at)?;
    }
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
    pub after: Vec<AfterConditionCheck>,
    pub fifo: FifoCheck,
    pub agent: AgentCheck,
    pub gate: GateCheck,
    pub ask: AskCheck,
    pub pane: PaneCheck,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AfterConditionCheck {
    pub address: String,
    pub met: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub met_at: Option<Timestamp>,
    pub agent_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
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
    pub waiting: bool,
    pub force: bool,
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
    let after = message
        .after
        .iter()
        .map(|condition| after_condition_check(condition, message.gate, pending, snapshot, now))
        .collect::<Vec<_>>();
    let after_ready = after.iter().all(|condition| condition.met);
    let fifo = if message.status == MessageStatus::Queued && schedule.ready && after_ready {
        let mut candidate = message.clone();
        for condition in &mut candidate.after {
            condition.met_at.get_or_insert(now);
        }
        let candidates = std::iter::once(&candidate).chain(
            pending
                .iter()
                .filter(|pending| pending.message_id != message.message_id),
        );
        match queue_head_for_message(candidates, &candidate, now) {
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
    let open = agent.is_some_and(|agent| gate_open_for_agent(message.gate, agent, message.force));
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
    let waiting = !message.force && agent.is_some_and(crate::agents::AgentState::is_awaiting_input);
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
        after,
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
            waiting,
            force: message.force,
        },
        pane: PaneCheck {
            present: pane.is_some(),
            pane_id: pane.map(|pane| pane.pane_id.clone()),
            pinned_pane_id: message.pane_id.clone(),
        },
    }
}

fn after_condition_check(
    condition: &AfterCondition,
    gate: DeliveryGate,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    now: Timestamp,
) -> AfterConditionCheck {
    let agent = snapshot.agents.iter().find(|agent| {
        card_matches(
            &condition.kind,
            &condition.agent_id,
            condition.agent_name.as_deref(),
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
        )
    });
    AfterConditionCheck {
        address: condition.address.clone(),
        met: condition.met_at.is_some()
            || after_condition_open(condition, gate, &snapshot.agents, pending, now),
        met_at: condition.met_at,
        agent_present: agent.is_some(),
        status: agent.map(crate::agents::AgentState::effective_status),
    }
}

fn delivery_candidate(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    mux: Option<MuxName>,
    policy: DeliveryPolicy,
) -> Result<Option<DeliveryCandidate>> {
    let pending = store.list_pending_messages()?;
    let Some(message) = pending
        .iter()
        .find(|message| message.message_id == *message_id)
        .cloned()
    else {
        return Ok(None);
    };
    let now = Timestamp::now();
    if matches!(policy, DeliveryPolicy::Boundary) {
        if !message.is_deliverable(now) {
            return Ok(None);
        }
        let Some(head) = queue_head_for_message(pending.iter(), &message, now) else {
            return Ok(None);
        };
        if head.message_id != *message_id {
            return Ok(None);
        }
    }
    let mut snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
    // The resolution snapshot carries live panes but not rich context sidecars.
    // Fold them here for smart-compact gauges and parked-status delivery gates.
    let runtime = RuntimePaths::for_workspace(message.workspace_id.clone()).ok();
    if let Some(runtime) = runtime.as_ref() {
        snapshot = snapshot.with_agent_context(crate::store::agent_context::read_all(runtime));
    }
    let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| message.same_agent_card(agent))
    else {
        return Ok(None);
    };
    let status = agent.effective_status();
    if matches!(policy, DeliveryPolicy::Boundary) {
        if !gate_open_for_agent(message.gate, agent, message.force) {
            return Ok(None);
        }
        if message.gate == DeliveryGate::Resume
            && !runtime
                .as_ref()
                .is_some_and(|runtime| crate::agents::resume_gate_recovered(runtime, agent, now))
        {
            return Ok(None);
        }
    }
    // A waiting agent reserves the next input, so it defers delivery —
    // unless the message was queued with `--force`, mirroring `message --steer --force`.
    let force_waiting = match policy {
        DeliveryPolicy::Boundary => message.force,
        DeliveryPolicy::Steer { force } => message.force || force,
    };
    if !force_waiting && agent.is_awaiting_input() {
        return Ok(None);
    }
    let batch_tail = match policy {
        DeliveryPolicy::Boundary => queue_batch_tail(pending.iter(), &message, status, now)
            .into_iter()
            .cloned()
            .collect(),
        DeliveryPolicy::Steer { .. } => Vec::new(),
    };
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

pub fn register_message_wake(workspace: &ResolvedWorkspace, store: &Store) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    refresh_wake_stamp(&runtime, store, Timestamp::now())
}

pub fn refresh_wake_stamp(runtime: &RuntimePaths, store: &Store, now: Timestamp) -> Result<()> {
    let path = wake_stamp_path(runtime);
    let next = store.earliest_message_wake(now, delivery_window_from_env())?;
    match next {
        Some(not_before) => {
            crate::store::atomic::write_temp_then_rename_cache(&path, &Some(not_before))?;
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

pub fn message_recorded_as_sent(store: &Store, message_id: &MessageId) -> Result<bool> {
    Ok(store
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

    use crate::agents::AgentState;
    use crate::ids::WorkspaceId;
    use crate::store::snapshot::PaneAgent;

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
    fn explain_reports_actionable_delivery_blockers() {
        let now = Timestamp::now();
        let receiver = agent("sess-receiver", AgentStatus::Idle);
        let live = snapshot(receiver.clone(), true, now);

        let older = message(&receiver, 1, "first");
        let newer = message(&receiver, 2, "second");
        let check = explain(&newer, &[newer.clone(), older.clone()], &live, now);
        assert!(!check.fifo.head);
        assert_eq!(check.fifo.blocker, Some(older.message_id));

        let mut running = receiver.clone();
        running.status = AgentStatus::Running;
        let running = snapshot(running, true, now);
        let candidate = message(&receiver, 1, "wait");
        let check = explain(&candidate, std::slice::from_ref(&candidate), &running, now);
        assert_eq!(check.gate.status, Some(AgentStatus::Running));
        assert!(!check.gate.open);

        let mut waiting = receiver.clone();
        waiting.status = AgentStatus::Waiting;
        waiting.waiting_since = Some(waiting.last_activity);
        let waiting = snapshot(waiting, true, now);
        let check = explain(&candidate, std::slice::from_ref(&candidate), &waiting, now);
        assert!(check.ask.waiting);

        let no_pane = snapshot(receiver.clone(), false, now);
        let check = explain(&candidate, std::slice::from_ref(&candidate), &no_pane, now);
        assert!(!check.pane.present);

        let not_before = now + jiff::SignedDuration::from_secs(60);
        let scheduled = message(&receiver, 1, "later").with_not_before(Some(not_before));
        let check = explain(&scheduled, std::slice::from_ref(&scheduled), &live, now);
        assert!(!check.schedule.ready);
        assert_eq!(check.schedule.not_before, Some(not_before));

        let mut upstream = agent("sess-planner", AgentStatus::Running);
        upstream.role = Some("planner".to_owned());
        let mut dependency = live;
        dependency.agents.push(upstream.clone());
        let after = message(&receiver, 1, "wait for plan").with_after(vec![AfterCondition {
            kind: upstream.kind.clone(),
            agent_id: upstream.agent_id.clone(),
            agent_name: upstream.name.clone(),
            address: "@planner".to_owned(),
            met_at: None,
        }]);
        let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
        assert_eq!(check.after[0].address, "@planner");
        assert!(!check.after[0].met);
        assert!(check.after[0].agent_present);
        assert_eq!(check.after[0].status, Some(AgentStatus::Running));

        dependency.agents[1].status = AgentStatus::Idle;
        let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
        assert!(check.after[0].met);
    }

    fn snapshot(agent: AgentState, with_pane: bool, now: Timestamp) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now);
        if with_pane {
            let agent = &snapshot.agents[0];
            snapshot.agent_panes = vec![PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: agent.kind_ordinal,
                name: agent.name.clone(),
                name_explicit: agent.name_explicit,
                profile: agent.profile.clone(),
                role: agent.role.clone(),
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
        AgentState::stub("claude", id, status)
    }
}
