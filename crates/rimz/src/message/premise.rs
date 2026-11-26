//! Whether a queued message's premises still hold: the record's row premises, then the
//! subjects only the harness or the queue can read (a linked run, a fleet digest's or team
//! result's joins, a team stage visit, a provider-limit park, the original of a "still queued"
//! warning, the compaction command a continue follows). The write check calls it on every
//! batch member under the pane lock; a delivery attempt calls it once on its head before
//! candidacy, since the write path wakes a hibernated receiver before it reaches the write
//! check. The turn-start clear and each subject's owner
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
    let holders = crate::agents::state::LaunchHolders::new(rows.iter());
    // With its context, as the write check reads it: the hand-off premise reads occupancy.
    let row = store
        .rows_with_live_context(rows.iter().find(|agent| {
            message.same_agent_card(agent) && agent.ended_at.is_none() && !holders.replaced(agent)
        }))
        .into_iter()
        .next();
    let Some(reason) = broken(store, message, row.as_deref(), Timestamp::now())? else {
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
    let cache_lifetime = |row: &AgentState| {
        crate::config::MachineConfig::load_lenient()
            .harness
            .prompt_cache_lifetime(row)
    };
    if let Some(reason) = row.and_then(|row| message.lapsed_hand_off(row, cache_lifetime, now)) {
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
    if let Some(reason) = unsent_command(store, message)? {
        return Ok(Some(reason));
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

/// The withdrawal reason of a continue whose compaction command ended without reaching the
/// pane, looked up in the whole queue and history. A command still in the queue, or one no
/// record holds, keeps it; one that was typed keeps it whether or not a compaction hook confirmed
/// it.
pub(super) fn unsent_command(
    store: &Store,
    message: &MessageRecord,
) -> crate::store::Result<Option<String>> {
    let Some(command_id) = &message.follows_command else {
        return Ok(None);
    };
    Ok(
        crate::store::message::read_record(store.paths(), command_id)?
            .filter(|command| command.status.is_terminal())
            .filter(|command| command.last_sent_at.is_none() && command.delivered_at.is_none())
            .map(|command| {
                format!(
                    "compaction {command_id} {} before it was typed",
                    command.status
                )
            }),
    )
}

/// The withdrawal reason of a continue whose provider-limit park closed or was replaced.
fn ended_park(store: &Store, message: &MessageRecord, row: Option<&AgentState>) -> Option<String> {
    let parked_since = message.premises.iter().find_map(|premise| match premise {
        Premise::ProviderPark { parked_since } => Some(*parked_since),
        _ => None,
    })?;
    let (kind, agent_id) = row.map_or((&message.kind, &message.agent_id), |row| {
        (&row.kind, &row.agent_id)
    });
    let open = crate::harness::auto_continue::park_episode_open(
        store.runtime_paths(),
        kind,
        agent_id,
        parked_since,
    );
    (!open).then(|| "provider-limit park closed before delivery".to_owned())
}

/// The withdrawal reason of a notice about a run that ended or is gone, of a deadline stop whose
/// open run no longer holds the claimed stop rung (`harness::deadline::stop_claimed`), or of a
/// result-file run's reminder notice that its run no longer waits on.
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
        Ok(run)
            if run.result_file.is_some()
                && message.sender == MessageSender::System
                && run.pending_result_reminder() != Some(&message.message_id) =>
        {
            Some("result reminder spent before delivery".to_owned())
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

    /// A continue finds its untyped command in history however many records ended after it.
    #[test]
    fn a_continue_finds_its_untyped_command_past_the_newest_history() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::parse("ws_000000000000000000000000").unwrap();
        let store = Store::open(
            crate::StatePaths::under(workspace_id.clone(), dir.path()).unwrap(),
            crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap(),
        )
        .unwrap();
        let receiver = AgentState::stub("claude", "receiver", AgentStatus::Idle);
        let record = |text: &str| {
            MessageRecord::new(
                workspace_id.clone(),
                &receiver,
                text.to_owned(),
                DeliveryGate::Done,
            )
        };
        let command = record("/compact");
        store.queue_message(&command, "room").unwrap();
        let later: Vec<_> = (0..500).map(|_| record("later")).collect();
        for message in &later {
            store.queue_message(message, "room").unwrap();
        }
        store
            .withdraw_messages("room", |live| {
                (live.message_id == command.message_id).then(|| "canceled".to_owned())
            })
            .unwrap();
        store
            .withdraw_messages("room", |_| Some("canceled".to_owned()))
            .unwrap();
        let mut follow = record("continue");
        follow.follows_command = Some(command.message_id.clone());

        assert_eq!(
            unsent_command(&store, &follow).unwrap(),
            Some(format!(
                "compaction {} canceled before it was typed",
                command.message_id
            ))
        );
    }

    /// A result-file run's reminder notice holds only while that run's reminder is unspent and
    /// still names it.
    #[test]
    fn a_result_reminder_notice_is_withdrawn_once_its_reminder_is_spent_or_replaced() {
        use crate::store::run::{ParkCause, ResultReminder, RunRecord, RunStatus};

        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::parse("ws_000000000000000000000000").unwrap();
        let store = Store::open(
            crate::StatePaths::under(workspace_id.clone(), dir.path()).unwrap(),
            crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap(),
        )
        .unwrap();
        let receiver = AgentState::stub("claude", "child", AgentStatus::Idle);
        let mut run = RunRecord::new(
            workspace_id.clone(),
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            "task".to_owned(),
            dir.path().to_path_buf(),
        );
        run.subagent = true;
        run.status = RunStatus::Running;
        run.parked_at = Some(Timestamp::now());
        run.parked_on = Some(ParkCause::Message);
        run.result_file = Some(dir.path().join("child.output"));
        let notice = MessageRecord::new(
            workspace_id,
            &receiver,
            "remind".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::System)
        .with_about_run(Some(run.run_id.clone()));
        let judge = |reminder: Option<ResultReminder>| {
            let mut run = run.clone();
            run.result_reminder = reminder;
            crate::harness::run::create(store.paths(), &run).unwrap();
            ended_run(&store, &notice)
        };
        let reminder = |message_id: &crate::ids::MessageId, spent| ResultReminder {
            message_id: message_id.clone(),
            spent,
        };
        assert_eq!(judge(Some(reminder(&notice.message_id, false))), None);
        let spent = Some("result reminder spent before delivery".to_owned());
        assert_eq!(judge(Some(reminder(&notice.message_id, true))), spent);
        assert_eq!(
            judge(Some(reminder(&crate::ids::MessageId::new(), false))),
            spent
        );
        assert_eq!(judge(None), spent);
    }

    /// A continue parked on the agent's provider limit reads the park under its holder's own kind
    /// and session, which a molt to another kind moved.
    #[test]
    fn a_continue_reads_the_park_of_its_holder_across_a_molt() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::parse("ws_000000000000000000000000").unwrap();
        let store = Store::open(
            crate::StatePaths::under(workspace_id.clone(), dir.path()).unwrap(),
            crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap(),
        )
        .unwrap();
        let parked_since = Timestamp::now();
        let old = AgentState::stub("claude", "sess-old", AgentStatus::Idle);
        let holder = AgentState::stub("codex", "sess-new", AgentStatus::Idle);
        crate::harness::auto_continue::arm_test_park(
            store.runtime_paths(),
            &holder.kind,
            &holder.agent_id,
            parked_since,
        );
        let mut resume = MessageRecord::new(
            workspace_id,
            &old,
            "continue".to_owned(),
            DeliveryGate::Done,
        );
        resume.premises.push(Premise::ProviderPark { parked_since });
        assert_eq!(ended_park(&store, &resume, Some(&holder)), None);
        assert!(ended_park(&store, &resume, None).is_some());
    }

    /// The head check judges a premised record on its agent's holder, never on the session a
    /// swap replaced.
    #[test]
    fn head_check_judges_a_premise_on_the_holder_not_the_replaced_session() {
        use crate::store::writer::{AgentLaunchName, AgentLaunchRequest, AgentLaunchScope};
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
        let kind = crate::ids::AgentKind::new_unchecked("claude");
        let request = |id: &str, name| AgentLaunchRequest {
            login: crate::store::writer::LaunchLogin::RoomDefault,
            kind: kind.clone(),
            agent_id: crate::ids::AgentSessionId::from(id),
            name,
            launch: Default::default(),
            run_id: None,
            prompt: None,
        };
        let scope = AgentLaunchScope {
            session_name: "room".into(),
            cwd: dir.path().into(),
            branch: None,
            description: None,
        };
        let pane = |raw| crate::ids::PaneId::parse(raw).unwrap();
        let row = |key: &str| {
            store
                .audit_rows()
                .unwrap()
                .iter()
                .find(|agent| agent.agent_id == key)
                .cloned()
                .unwrap()
        };
        let first = store
            .begin_agent_launch_batch(
                &[request(
                    "launch_a",
                    AgentLaunchName::Explicit("coder".into()),
                )],
                scope.clone(),
            )
            .unwrap();
        store
            .bind_agent_launch(
                first.single_identity().unwrap(),
                "room",
                dir.path(),
                &pane("tmux:%1"),
                None,
            )
            .unwrap();
        let queued_at = Timestamp::now() - std::time::Duration::from_secs(60);
        let mut premised = MessageRecord::new(
            workspace_id,
            &row("launch_a"),
            "still relevant".to_owned(),
            DeliveryGate::Done,
        );
        premised.premises.push(Premise::NotPromptedSince {
            last_prompt_at: Some(queued_at),
        });
        store.queue_message(&premised, "room").unwrap();
        store
            .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
                session_name: "room",
                agent_kind: kind.clone(),
                event_name: "fixture",
                observation: &crate::agents::AgentLifecycleObservation::new(
                    Some("launch_a".into()),
                    crate::agents::LifecycleSignal::TurnStarted { turn_id: None },
                ),
                spawned_subagents: &[],
            })
            .unwrap();
        assert!(row("launch_a").last_prompt_at > Some(queued_at));
        let swap = store
            .begin_agent_swap(
                &row("launch_a"),
                &request("launch_b", AgentLaunchName::Mint),
                scope,
            )
            .unwrap();
        store
            .bind_agent_launch(
                swap.single_identity().unwrap(),
                "room",
                dir.path(),
                &pane("tmux:%2"),
                None,
            )
            .unwrap();

        assert!(!withdraw_before_candidacy(&workspace, &store, &premised).unwrap());
    }
}
