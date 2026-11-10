//! Run delivery stamping and enrollment and settlement of launcher-opened persistent peer turns.

use std::path::Path;

use crate::agents::{AgentDefinition, AgentState, PermissionMode, TurnCompletion};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::AgentSessionId;
use crate::store::Store;
use crate::store::message::{MessageRecord, MessageSender};
use crate::store::run::{PeerRun, ReportTo, RunStatus, RunStoreErr};
use crate::store::writer::DeliveryAckMatch;

use super::{RecordMutation, Result, RunRecord};

pub fn peer_can_report(adapter: &AgentDefinition) -> bool {
    let hooks = &adapter.spec().lifecycle_hooks;
    hooks.turn_started.is_native() && hooks.turn_ended.is_native()
}

/// `reader` is the launcher's handle, whose `out/` directory receives the response.
/// `report_to` is stamped on a record this call creates; an open run it reuses keeps its own.
pub fn create_peer_prompt(
    paths: &StatePaths,
    peer: &AgentState,
    reader: Option<&str>,
    adapter: &AgentDefinition,
    prompt: &str,
    cwd: &Path,
    report_to: ReportTo,
) -> Result<Option<RunRecord>> {
    if peer.is_team_seat() {
        return super::team::create_team_run(paths, peer, reader, adapter, prompt, cwd, report_to);
    }
    let Some(launch_id) = peer_launch_id(peer) else {
        return Ok(None);
    };
    if prompt.trim().is_empty() || !peer_can_report(adapter) {
        return Ok(None);
    }
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    if let Some(record) = open_peer_run(paths, peer)? {
        return Ok(Some(record));
    }
    let mut record = new_peer_record(paths, peer, reader, launch_id, prompt.to_owned(), cwd);
    record.report_to = report_to;
    crate::store::run::write(&paths.runs_dir, &record)?;
    Ok(Some(record))
}

pub fn open_peer_run(paths: &StatePaths, peer: &AgentState) -> Result<Option<RunRecord>> {
    let Some(launch_id) = peer_launch_id(peer) else {
        return Ok(None);
    };
    Ok(super::list(paths)?.into_iter().find(|record| {
        !record.status.is_terminal()
            && record.kind == peer.kind
            && record
                .peer
                .as_ref()
                .is_some_and(|turn| &turn.launch_id == launch_id)
    }))
}

/// Record confirmed delivery: stamp the answer of the hook's subagent run, or enroll a peer turn
/// for any other card (a foreground `-p` peer carries a non-subagent hook run id).
/// Called inside `Store::confirm_delivered_for_card_with`'s workspace lock; never takes the lock itself.
pub fn record_run_delivery(
    paths: &StatePaths,
    peer: &AgentState,
    adapter: &AgentDefinition,
    records: &[MessageRecord],
    selection: DeliveryAckMatch,
    cwd: &Path,
    run_id: Option<&crate::ids::RunId>,
) -> Result<Option<RunRecord>> {
    if let Some(run_id) = run_id {
        // gc removes terminal records while a foreground `-p` peer's pane stays open.
        match super::load(paths, run_id) {
            Ok(record) if record.subagent => {
                return stamp_subagent_delivery(paths, record, peer, records);
            }
            Ok(_) | Err(RunStoreErr::NotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }
    if selection != DeliveryAckMatch::PromptCorrelated || !peer_can_report(adapter) {
        return Ok(None);
    }
    let Some(launch_id) = peer_launch_id(peer) else {
        return Ok(None);
    };
    let Some(launcher) = peer.launched_by.as_ref() else {
        return Ok(None);
    };
    let openers = records
        .iter()
        .filter(|record| {
            matches!(&record.sender,
                MessageSender::Agent { kind, agent_id: Some(id), .. }
                    if kind == &launcher.kind && id == &launcher.agent_id
            )
        })
        .collect::<Vec<_>>();
    if openers.is_empty() {
        return Ok(None);
    }
    let mut record = match open_peer_run(paths, peer)? {
        Some(record) => record,
        None => {
            let prompt = openers
                .iter()
                .map(|record| record.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let reader = openers.iter().find_map(|record| match &record.sender {
                MessageSender::Agent { name, .. } => name.as_deref(),
                _ => None,
            });
            let mut record = new_peer_record(paths, peer, reader, launch_id, prompt, cwd);
            record.agent_id = Some(peer.agent_id.clone());
            record.status = RunStatus::Running;
            record
        }
    };
    // Both the lookup and constructor above return peer records.
    let turn = record
        .peer
        .as_mut()
        .expect("peer run selected or constructed");
    let mut changed = false;
    for opener in openers {
        if !turn.opened_by.contains(&opener.message_id) {
            turn.opened_by.push(opener.message_id.clone());
            changed = true;
        }
    }
    if changed {
        record.updated_at = jiff::Timestamp::now();
        crate::store::run::write(&paths.runs_dir, &record)?;
    }
    Ok(Some(record))
}

fn stamp_subagent_delivery(
    paths: &StatePaths,
    mut record: RunRecord,
    card: &AgentState,
    records: &[MessageRecord],
) -> Result<Option<RunRecord>> {
    let openers = records
        .iter()
        .filter(|record| record.sender.is_conversation())
        .collect::<Vec<_>>();
    if openers.is_empty() {
        return Ok(None);
    }
    if record.status.is_terminal()
        || record.peer.is_some()
        || record.team.is_some()
        || record.kind != card.kind
        || record.agent_id.as_ref() != Some(&card.agent_id)
    {
        return Ok(None);
    }
    let mut changed = false;
    if let Some(turn) = record.follow_up.as_mut()
        && turn.prompt.is_none()
    {
        turn.prompt = Some(
            openers
                .iter()
                .map(|opener| opener.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
        changed = true;
    }
    for opener in openers {
        if !record.opened_by.contains(&opener.message_id) {
            record.opened_by.push(opener.message_id.clone());
            changed = true;
        }
    }
    if changed {
        record.updated_at = jiff::Timestamp::now();
        crate::store::run::write(&paths.runs_dir, &record)?;
    }
    Ok(Some(record))
}

pub fn fail_peer_run(store: &Store, peer: &AgentState, reason: &str) -> Result<Option<RunRecord>> {
    let open = match open_peer_run(store.paths(), peer)? {
        Some(record) => Some(record),
        None => super::team::open_team_run(store.paths(), peer)?,
    };
    let Some(record) = open else {
        return Ok(None);
    };
    let (record, wrote) = super::update_record(store.paths(), &record.run_id, |record, now| {
        if !record.mark_terminal(RunStatus::Failed, now) {
            return Ok(RecordMutation::Keep(false));
        }
        record.failure_tail = Some(reason.to_owned());
        Ok(RecordMutation::Write(true))
    })?;
    if !wrote {
        return Ok(None);
    }
    crate::store::run::wake_run(store.runtime_paths(), &record);
    Ok(Some(record))
}

/// Whether a terminal peer run's answer is `peer`'s latest turn: the peer is at
/// rest by `completion` (as `TurnWaitView::completion` reads it), and no request
/// has closed on its row since the run completed. The row stamps a close at the
/// hook's frame time, before the drainer settles the run under the lock, so a
/// tie counts as covered.
pub fn describes_latest_turn(
    record: &RunRecord,
    peer: &AgentState,
    completion: TurnCompletion,
) -> bool {
    record.peer.is_some()
        && record.status.is_terminal()
        && completion != TurnCompletion::Open
        && peer
            .turn_ended_at
            .zip(record.completed_at)
            .is_some_and(|(ended, completed)| ended <= completed)
}

fn peer_launch_id(peer: &AgentState) -> Option<&AgentSessionId> {
    if peer.launched_by.is_none() || peer.parent_agent_id.is_some() || peer.is_team_seat() {
        return None;
    }
    peer.launch_id.as_ref()
}

fn new_peer_record(
    paths: &StatePaths,
    peer: &AgentState,
    reader: Option<&str>,
    launch_id: &AgentSessionId,
    prompt: String,
    cwd: &Path,
) -> RunRecord {
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        peer.kind.clone(),
        peer.mode.unwrap_or(PermissionMode::Auto),
        prompt,
        peer.worktree_path
            .as_deref()
            .map_or_else(|| cwd.to_path_buf(), Into::into),
    );
    record.agent_name = peer.name.clone();
    record.reader = reader.map(str::to_owned);
    record.peer = Some(PeerRun {
        launch_id: launch_id.clone(),
        opened_by: Vec::new(),
    });
    record
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::agents::AgentStatus;

    #[test]
    fn a_terminal_peer_run_describes_the_turn_its_row_last_closed() {
        let completed = jiff::Timestamp::from_second(1_000_000).unwrap();
        let mut record = RunRecord::new(
            crate::ids::WorkspaceId::from_project_root(Path::new("/tmp/peer-latest")),
            crate::ids::AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "task".into(),
            "/tmp/peer-latest".into(),
        );
        record.peer = Some(PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        record.status = RunStatus::Completed;
        record.completed_at = Some(completed);
        let mut peer = AgentState::stub("claude", "peer-session", AgentStatus::Idle);
        let rest = TurnCompletion::Completed;
        let second = Duration::from_secs(1);
        let covered = |record: &RunRecord, peer: &AgentState, completion| {
            describes_latest_turn(record, peer, completion)
        };

        for (ended, expected) in [
            (Some(completed), true),
            (Some(completed - second), true),
            (Some(completed + second), false),
            (None, false),
        ] {
            peer.turn_ended_at = ended;
            assert_eq!(covered(&record, &peer, rest), expected, "{ended:?}");
        }
        peer.turn_ended_at = Some(completed);
        assert!(covered(&record, &peer, TurnCompletion::Failed));
        assert!(!covered(&record, &peer, TurnCompletion::Open));
        let mut open = record.clone();
        open.status = RunStatus::Running;
        assert!(!covered(&open, &peer, rest));
        let mut plain = record.clone();
        plain.peer = None;
        assert!(!covered(&plain, &peer, rest));
        let mut unstamped = record;
        unstamped.completed_at = None;
        assert!(!covered(&unstamped, &peer, rest));
    }
}
