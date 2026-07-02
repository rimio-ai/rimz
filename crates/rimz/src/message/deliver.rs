//! Queued-message delivery, sweeping, and wake-cache maintenance.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;

use crate::feed::pending_ask_in_snapshot;
use crate::ids::{MessageId, MuxName};
use crate::message::{
    DeliveryGate, MessageRecord, MessageStatus, delivery_window_from_env, gate_open,
    max_delivery_attempts_from_env, message_interval_from_env, queue_head, queue_head_for_message,
};
use crate::mux::MuxErr;
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
    // Hook delivery handles one claimed message; settle above owns any
    // pre-delivery spacing, so this pacer's first tick stays a no-op.
    let mut live_send = send::LiveSend {
        force: message.force,
        pacer: send::Pacer::new(message_interval_from_env()),
    };
    let send_message = message
        .clone()
        .with_pane_id(candidate.target.pane_id.clone());
    let sent = send::send_prompt_to_live_pane(
        workspace,
        ledger,
        &candidate.snapshot,
        &candidate.target,
        send::bound_agent(&candidate.snapshot, &candidate.target),
        &send_message,
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
            ledger.record_message_delivery_failure(
                &message.message_id,
                &format!("pending ask {request_id} reserves input"),
                &workspace.session_name,
            )?;
            Ok(false)
        }
        Err(err) => {
            if message_recorded_as_sent(ledger, &message.message_id)? {
                register_message_wake(workspace, ledger)?;
                return Ok(false);
            }
            if ledger
                .record_message_delivery_failure(
                    &message.message_id,
                    &err.to_string(),
                    &workspace.session_name,
                )?
                .is_none()
            {
                ledger.record_send_error(
                    &send_message,
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

struct DeliveryCandidate {
    message: MessageRecord,
    snapshot: SidebarSnapshot,
    target: PaneAgent,
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
        && !runtime.as_ref().is_some_and(|runtime| {
            crate::sidebar::enrich::resume_gate_recovered(runtime, agent, now)
        })
    {
        return Ok(None);
    }
    // A pending ask reserves the agent's next input, so it defers delivery —
    // unless the message was queued with `--force`, mirroring `message --steer --force`.
    if !message.force && pending_ask_in_snapshot(agent, &snapshot).is_some() {
        return Ok(None);
    }
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

pub fn sent_prompt_has_sent_record(sent: &send::SentPrompt) -> bool {
    sent.compacted.is_some() || matches!(sent.outcome, send::Outcome::Sent { .. })
}

pub fn message_recorded_as_sent(ledger: &Ledger, message_id: &MessageId) -> Result<bool> {
    Ok(ledger
        .list_messages()?
        .iter()
        .any(|message| message.message_id == *message_id && message.status == MessageStatus::Sent))
}

pub fn is_mux_timeout(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(err);
    while let Some(current) = cause {
        if current
            .downcast_ref::<MuxErr>()
            .is_some_and(|err| matches!(err, MuxErr::Timeout { .. }))
        {
            return true;
        }
        cause = current.source();
    }
    false
}

pub(crate) fn wake_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(crate::message::MESSAGE_WAKE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("sending prompt")]
    struct WrappedMuxTimeout(#[source] MuxErr);

    #[test]
    fn mux_timeout_detection_walks_error_context() {
        let err = WrappedMuxTimeout(MuxErr::Timeout {
            program: "tmux".to_owned(),
            args: "send-keys %1".to_owned(),
            seconds: 30,
        });

        assert!(is_mux_timeout(&err));
        assert!(!is_mux_timeout(&std::io::Error::other("ordinary failure")));
    }
}
