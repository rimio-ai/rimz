//! Whether a queued message's premises still hold: the record's row premises, then the
//! subjects only the harness or the queue can read (a linked run, a fleet digest's or team
//! result's joins, a team stage visit, a provider-limit park, the original of a "still queued"
//! warning). The write check calls it on every batch member under the pane lock; a delivery
//! attempt calls it once on its head before candidacy, since the write path wakes a hibernated
//! receiver before it reaches the write check. The turn-start clear and each subject's owner
//! withdraw through `Store::withdraw_messages` with the row-only half.

use jiff::Timestamp;

use crate::Store;
use crate::agents::AgentState;
use crate::store::message::{HarnessNotice, MessageRecord, MessageSender, MessageStatus, Premise};
use crate::store::run::RunStoreErr;
use crate::workspace::ResolvedWorkspace;

/// Withdraw `message` before a delivery attempt claims it when a premise already broke, judged
/// against its card's live row; whether it was withdrawn. A record without premises reads nothing.
pub(super) fn withdraw_before_candidacy(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
) -> crate::store::Result<bool> {
    if !message.carries_premise() {
        return Ok(false);
    }
    let rows = store.audit_rows()?;
    let row = rows
        .iter()
        .find(|agent| message.same_agent_card(agent) && agent.ended_at.is_none());
    let Some(reason) = broken(store, message, row, Timestamp::now())? else {
        return Ok(false);
    };
    let withdrawn = store.withdraw_messages(&workspace.session_name, |live| {
        (live.message_id == message.message_id).then(|| reason.clone())
    })?;
    Ok(!withdrawn.is_empty())
}

/// The first premise of `message` that no longer holds, as its withdrawal reason. `row` is the
/// receiver's live row as read under the pane lock. A subject that cannot be read holds.
pub(super) fn broken(
    store: &Store,
    message: &MessageRecord,
    row: Option<&AgentState>,
    now: Timestamp,
) -> crate::store::Result<Option<String>> {
    if let Some(reason) = message.broken_row_premise(row, now) {
        return Ok(Some(reason));
    }
    if matches!(&message.sender, MessageSender::Harness { notice } if notice.awaits_join())
        && crate::harness::run::report::digest_fully_joined(store.paths(), &message.message_id)?
    {
        return Ok(Some("joined before delivery".to_owned()));
    }
    if message
        .premises
        .iter()
        .any(|premise| matches!(premise, Premise::StillQueued { .. }))
    {
        let live = store.list_messages()?;
        let waiting = |original: &crate::ids::MessageId| {
            live.iter().any(|record| {
                record.message_id == *original
                    && matches!(
                        record.status,
                        MessageStatus::Queued | MessageStatus::Claimed
                    )
            })
        };
        if let Some(reason) = message.outlived_original(waiting) {
            return Ok(Some(reason));
        }
    }
    if let Some(reason) = ended_park(store, message, row) {
        return Ok(Some(reason));
    }
    if let Some(reason) = ended_run(store, message) {
        return Ok(Some(reason));
    }
    Ok(message.premises.iter().find_map(|premise| {
        let Premise::StageVisit { checkout, .. } = premise else {
            return None;
        };
        let zone = crate::config::MachineConfig::load_time_zone_lenient();
        let run = crate::harness::scratch::board_run(checkout, &zone)?;
        message.ended_stage_visit(checkout, &run.stage.name, run.stage_started_at)
    }))
}

/// The withdrawal reason of a continue whose provider-limit park closed or was replaced.
fn ended_park(store: &Store, message: &MessageRecord, row: Option<&AgentState>) -> Option<String> {
    let parked_since = message.premises.iter().find_map(|premise| match premise {
        Premise::ProviderPark { parked_since } => Some(*parked_since),
        _ => None,
    })?;
    let agent_id = row.map_or(&message.agent_id, |row| &row.agent_id);
    let open = crate::harness::auto_continue::park_episode_open(
        store.runtime_paths(),
        &message.kind,
        agent_id,
        parked_since,
    );
    (!open).then(|| "provider-limit park closed before delivery".to_owned())
}

/// The withdrawal reason of a notice about a run that ended or is gone, or of a deadline stop
/// whose open run no longer holds the claimed stop rung (`harness::deadline::stop_claimed`).
fn ended_run(store: &Store, message: &MessageRecord) -> Option<String> {
    let run_id = message.about_run.as_ref()?;
    match crate::harness::run::load(store.paths(), run_id) {
        Ok(run) if run.status.is_terminal() => {
            Some(format!("child run {} before delivery", run.status.as_str()))
        }
        Ok(run)
            if matches!(
                message.sender,
                MessageSender::Harness {
                    notice: HarnessNotice::Deadline
                }
            ) && !crate::harness::deadline::stop_claimed(&run) =>
        {
            Some("deadline stop superseded before delivery".to_owned())
        }
        Ok(_) => None,
        Err(RunStoreErr::NotFound(_)) => Some("child run gone before delivery".to_owned()),
        Err(error) => {
            tracing::warn!(
                message_id = %message.message_id,
                %run_id,
                %error,
                "delivering notice: cannot read linked run"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::store::message::DeliveryGate;

    /// The head check reads the card's row only for a record that carries a premise: with the
    /// event log unreadable, a plain record still passes and a premised one fails the read.
    #[test]
    fn head_check_reads_no_row_for_a_record_without_premises() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::parse("ws_000000000000000000000000").unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id: workspace_id.clone(),
            project_root: dir.path().into(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: dir.path().into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        };
        let store = Store::open(
            crate::StatePaths::under(workspace_id.clone(), dir.path()).unwrap(),
            crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(&store.paths().events_log).unwrap();
        let receiver = AgentState::stub("claude", "receiver", AgentStatus::Idle);
        let plain = MessageRecord::new(
            workspace_id,
            &receiver,
            "hello".to_owned(),
            DeliveryGate::Done,
        );
        let mut idle = plain.clone();
        idle.idle_since = Some(Timestamp::now());

        assert!(!withdraw_before_candidacy(&workspace, &store, &plain).unwrap());
        assert!(withdraw_before_candidacy(&workspace, &store, &idle).is_err());
    }
}
