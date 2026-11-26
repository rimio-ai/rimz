//! Standalone native compaction through the durable, boundary-only command queue, and the
//! continue a busy agent's own compaction request owes it afterward.

use jiff::Timestamp;

use crate::Store;
use crate::agents::AgentState;
use crate::ids::{MessageId, PaneId};
use crate::store::message::{
    DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, SoftCompactStep,
    idle_compaction_edge,
};
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver::{self, DeliveryPolicy};
use super::synthetic::{self, SyntheticMessage};

type Result<T> = std::result::Result<T, CompactErr>;

#[derive(Debug, thiserror::Error)]
pub enum CompactErr {
    #[error("already compacting; wait for compaction to finish and take another turn")]
    Compacting,
    #[error("compaction {message_id} is still {status}; a compaction never follows a compaction")]
    Pending {
        message_id: MessageId,
        status: MessageStatus,
    },
    #[error(
        "already compacted and has not taken a turn since; a compaction never follows a compaction"
    )]
    Repeated { at: Timestamp },
    #[error("compaction {message_id}: {reason}")]
    Settled {
        message_id: MessageId,
        reason: String,
    },
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverErr),
}

impl CompactErr {
    pub fn live_message(&self, now: Timestamp) -> String {
        match self {
            Self::Repeated { at } => {
                let elapsed = crate::store::message::format_dwell(
                    now.duration_since(*at).as_secs().max(0) as u64,
                );
                format!(
                    "already compacted {elapsed} ago and has not taken a turn since; a compaction never follows a compaction"
                )
            }
            _ => self.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CompactOutcome {
    /// The command was typed at once; `continues` when its continue stays queued behind it.
    Sent {
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        continues: bool,
    },
    /// The command waits for a turn boundary; `continues` when a continue is queued behind it.
    Queued { continues: bool },
}

pub struct CompactRequest<'a> {
    pub message_id: MessageId,
    pub agent: &'a AgentState,
    pub pane_id: PaneId,
    pub command: String,
    pub sender: MessageSender,
    pub automated: bool,
    pub idle: bool,
    /// What must still hold for the compaction to be worth sending, beside the idle tag.
    pub premises: Vec<crate::store::message::Premise>,
    /// The continue owed once the compaction ends, queued behind the command when `agent`
    /// holds an open turn and the command queues; `None` owes none.
    pub continue_text: Option<String>,
    /// End `agent`'s open turn now for the compaction instead of waiting for its end: the
    /// continue is owed past the stop and stays queued when the command is typed at once.
    pub hard: bool,
}

pub fn refuse_repeat(store: &Store, agent: &AgentState, now: Timestamp) -> Result<()> {
    if agent.is_compacting(now) {
        return Err(CompactErr::Compacting);
    }
    if let Some(message) = store.list_messages()?.into_iter().find(|message| {
        message.body == MessageBody::Command
            && !message.status.is_terminal()
            && message.same_agent_card(agent)
            && !(message.status == MessageStatus::Queued && message.idle_compaction_lapsed(agent))
    }) {
        return Err(CompactErr::Pending {
            message_id: message.message_id,
            status: message.status,
        });
    }
    if let Some(at) = agent
        .compacted_awaiting_prompt
        .filter(|_| agent.compaction_unprompted(now))
    {
        return Err(CompactErr::Repeated { at });
    }
    Ok(())
}

/// What compacting an agent on request did: the command's record and whether it was sent now,
/// waits for a turn boundary, or was already waiting there and the request joined it, and why a
/// request to end the turn now waits for its boundary instead.
#[derive(Debug, serde::Serialize)]
pub struct CompactReceipt {
    pub message_id: MessageId,
    #[serde(flatten)]
    pub outcome: ReceiptOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_because: Option<SoftBecause>,
}

/// What a request to compact an agent asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactAsk {
    /// The agent asked for its own compaction.
    pub own: bool,
    /// Owe no continue after the agent's own compaction.
    pub no_continue: bool,
    /// End a busy agent's turn now for the compaction rather than at the turn's end.
    pub hard: bool,
}

/// Why a request to end a busy agent's turn now compacts it at the turn's end instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SoftBecause {
    /// The agent asked for itself while it holds an open turn, and its own request never ends it.
    OwnRequest,
    /// The agent waits on a person's answer.
    AwaitingAnswer,
    /// No continue text is configured, and a compaction that ends the turn must owe one.
    NoContinueText,
}

/// A request's [`CompactOutcome`], or `Joined` when the agent's command was already queued:
/// `continues` when a continue follows that command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ReceiptOutcome {
    Sent {
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        continues: bool,
    },
    Queued {
        continues: bool,
    },
    Joined {
        continues: bool,
    },
}

impl From<CompactOutcome> for ReceiptOutcome {
    fn from(outcome: CompactOutcome) -> Self {
        match outcome {
            CompactOutcome::Sent { continues } => Self::Sent { continues },
            CompactOutcome::Queued { continues } => Self::Queued { continues },
        }
    }
}

/// Send `agent` its native compaction command, with `instruction` in place of the configured
/// brief, from `sender`, as `ask` asks. When `agent` holds an open turn and the continue is
/// owed (its own request, unless `no_continue`, or an effective hard one), the configured continue
/// is queued behind the command (or added when joining a queued command that has none yet). A
/// hard attempt that does not type the command leaves what the request without `hard` leaves.
/// `handle` names the agent in a delivery refusal.
pub fn compact_agent(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentState,
    handle: &str,
    instruction: Option<&str>,
    ask: CompactAsk,
    sender: MessageSender,
) -> anyhow::Result<CompactReceipt> {
    use anyhow::{Context, bail};

    let pane_id = agent
        .pane
        .as_ref()
        .context("agent has no bound pane; nothing to compact")?
        .pane_id
        .clone();
    let spec = crate::agents::spec_by_kind(agent.kind.as_str())
        .with_context(|| format!("{} has no native compaction command", agent.kind))?;
    let compact = spec
        .launch
        .compact_command
        .with_context(|| format!("{} has no native compaction command", agent.kind))?;
    if !spec.lifecycle_hooks.turn_started.is_native() {
        bail!(
            "{} reports no durable turn starts, so RimZ cannot guarantee a compaction never follows a compaction; compact it in its own pane",
            agent.kind
        );
    }
    if instruction.is_some()
        && compact.instruction != crate::agents::definition::CompactInstruction::Trailing
    {
        bail!(
            "{} does not accept a compaction instruction; it receives `{}` bare — rerun without the instruction",
            agent.kind,
            compact.command,
        );
    }
    let config = crate::config::MachineConfig::load_lenient();
    let command = match instruction {
        Some(instruction) => spec.launch.compact_command(instruction),
        None => crate::agents::compact_command(agent, &config.harness),
    }
    .context("adapter has no native compaction command")?;
    let configured_continue = config.resume.auto_continue_text.trim();
    let soft_because = if !ask.hard {
        None
    } else if ask.own && agent.holds_open_turn() {
        Some(SoftBecause::OwnRequest)
    } else if agent.is_awaiting_input() {
        Some(SoftBecause::AwaitingAnswer)
    } else if agent.holds_open_turn() && configured_continue.is_empty() {
        Some(SoftBecause::NoContinueText)
    } else {
        None
    };
    let hard = ask.hard && soft_because.is_none() && agent.holds_open_turn();
    let continue_text = (hard || ask.own && !ask.no_continue)
        .then_some(configured_continue)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let refused =
        |err: CompactErr| anyhow::anyhow!("{handle}: {}", err.live_message(Timestamp::now()));
    // One compaction waits per agent: a request joins a queued command, and one that the join
    // finds no longer queued goes on to `send_compact`, which refuses the state it finds now.
    match refuse_repeat(store, agent, Timestamp::now()) {
        Ok(()) => {}
        Err(CompactErr::Pending {
            message_id,
            status: MessageStatus::Queued,
        }) => {
            if instruction.is_some() {
                bail!(
                    "{handle}: compaction {message_id} is already queued and has not been typed; an instruction cannot change its text — rerun without the instruction to join it"
                );
            }
            if let Some((continues, replaced)) = join_waiting(
                workspace,
                store,
                agent,
                &message_id,
                continue_text.clone(),
                hard,
            )
            .map_err(refused)?
            {
                let outcome = if hard {
                    // A miss leaves what a join without `--hard` leaves: the continue it replaced.
                    let restore = replaced.map(|follower| {
                        continue_record(workspace, agent, &message_id, follower.text)
                    });
                    match attempt(workspace, store, agent, &message_id, &pane_id, hard)
                        .map_err(refused)?
                    {
                        true => ReceiptOutcome::Sent { continues },
                        false => match untyped_fate(
                            workspace,
                            store,
                            &message_id,
                            hard,
                            restore.as_ref(),
                        )
                        .map_err(refused)?
                        {
                            Untyped::Waiting => ReceiptOutcome::Joined {
                                continues: restore.is_some(),
                            },
                            Untyped::Delivered => ReceiptOutcome::Sent { continues },
                        },
                    }
                } else {
                    ReceiptOutcome::Joined { continues }
                };
                return Ok(CompactReceipt {
                    message_id,
                    outcome,
                    soft_because,
                });
            }
        }
        Err(err) => return Err(refused(err)),
    }
    let message_id = MessageId::new();
    let outcome = send_compact(
        workspace,
        store,
        CompactRequest {
            premises: crate::store::message::Premise::NONE,
            idle: false,
            message_id: message_id.clone(),
            agent,
            pane_id,
            command,
            sender,
            automated: false,
            continue_text,
            hard,
        },
    )
    .map_err(refused)?;
    Ok(CompactReceipt {
        message_id,
        outcome: outcome.into(),
        soft_because,
    })
}

/// Join `agent`'s compaction `command_id`, waiting `Queued`: queue `continue_text` behind it
/// when the agent holds an open turn and nothing follows the command yet, or, owed past the stop
/// when `hard`, in place of a follower that is not. `None` when the command left `Queued`
/// before the join's commit, else whether a continue follows it and the follower it replaced.
fn join_waiting(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentState,
    command_id: &MessageId,
    continue_text: Option<String>,
    hard: bool,
) -> Result<Option<(bool, Option<MessageRecord>)>> {
    let tail = continue_text
        .filter(|_| agent.holds_open_turn())
        .map(|text| MessageRecord {
            past_stop: hard,
            ..continue_record(workspace, agent, command_id, text)
        });
    Ok(store.join_queued_command(command_id, tail.as_ref(), &workspace.session_name)?)
}

pub fn send_compact(
    workspace: &ResolvedWorkspace,
    store: &Store,
    request: CompactRequest<'_>,
) -> Result<CompactOutcome> {
    refuse_repeat(store, request.agent, Timestamp::now())?;
    let mut message = SyntheticMessage {
        agent: request.agent,
        text: request.command,
        sender: request.sender,
        gate: DeliveryGate::Done,
        premises: request.premises,
    }
    .record(workspace)
    .with_automated(request.automated)
    .with_body(MessageBody::Command);
    message.message_id = request.message_id;
    message.compacted_context_tokens = request.agent.occupied_context_tokens();
    if request.idle {
        message.idle_since = idle_compaction_edge(request.agent);
    }
    let continuation = request
        .continue_text
        .filter(|_| request.agent.holds_open_turn())
        .map(|text| MessageRecord {
            past_stop: request.hard,
            ..continue_record(workspace, request.agent, &message.message_id, text)
        });
    let continuation = match continuation {
        Some(tail) => {
            Some(store.queue_message_with_tail(&message, &tail, &workspace.session_name)?)
        }
        None => {
            store.queue_message(&message, &workspace.session_name)?;
            None
        }
    };
    let withdraw_continue = |reason: String| -> Result<()> {
        if let Some(tail) = &continuation {
            store.withdraw_messages(&workspace.session_name, |live| {
                (live.message_id == tail.message_id).then(|| reason.clone())
            })?;
        }
        Ok(())
    };
    // A command typed at once ran at a boundary, unless it ended the turn to run, when the
    // continue stays owed past that stop.
    let sent = || -> Result<CompactOutcome> {
        if !request.hard {
            withdraw_continue(format!(
                "compaction {} was typed at once",
                message.message_id
            ))?;
        }
        Ok(CompactOutcome::Sent {
            continues: request.hard && continuation.is_some(),
        })
    };
    if attempt(
        workspace,
        store,
        request.agent,
        &message.message_id,
        &request.pane_id,
        request.hard,
    )? {
        return sent();
    }
    match untyped_fate(workspace, store, &message.message_id, request.hard, None)? {
        // A hard request owes no continue unless it typed the command: it is never the agent's
        // own, so the same request without `--hard` queues none.
        Untyped::Waiting => Ok(CompactOutcome::Queued {
            continues: !request.hard && continuation.is_some(),
        }),
        Untyped::Delivered => sent(),
    }
}

/// Where a compaction command stands after an attempt that did not type it.
enum Untyped {
    /// It waits for the turn boundary.
    Waiting,
    /// It was delivered after all.
    Delivered,
}

/// Read what became of the compaction `command_id` after an attempt that did not type it. One
/// still waiting is left, when `hard`, as the same request without `--hard` leaves it
/// (`Store::soften_untyped_command`, queuing `restore` again); one that ended untyped withdraws
/// the continue behind it and is the error.
fn untyped_fate(
    workspace: &ResolvedWorkspace,
    store: &Store,
    command_id: &MessageId,
    hard: bool,
    restore: Option<&MessageRecord>,
) -> Result<Untyped> {
    if hard && store.soften_untyped_command(command_id, restore, &workspace.session_name)? {
        return Ok(Untyped::Waiting);
    }
    let live = store.list_messages()?;
    if live.iter().any(|queued| queued.message_id == *command_id) {
        return Ok(Untyped::Waiting);
    }
    let settled = store
        .list_message_history()?
        .into_iter()
        .find(|record| record.message_id == *command_id);
    if settled
        .as_ref()
        .is_some_and(|record| record.status == MessageStatus::Delivered)
    {
        return Ok(Untyped::Delivered);
    }
    if let Some(tail) = live
        .iter()
        .find(|live| live.follows_command.as_ref() == Some(command_id))
        && let Some(reason) = super::premise::unsent_command(store, tail)?
    {
        store.withdraw_messages(&workspace.session_name, |live| {
            (live.message_id == tail.message_id).then(|| reason.clone())
        })?;
    }
    let reason = settled.map_or_else(
        || "outcome is no longer in the message history".to_owned(),
        |record| {
            record
                .last_error
                .unwrap_or_else(|| format!("settled as {}", record.status))
        },
    );
    Err(CompactErr::Settled {
        message_id: command_id.clone(),
        reason,
    })
}

/// Attempt the queued compaction `command_id` now: at the boundary, or, when `hard`, into
/// `agent`'s open turn, ending it, as a steer where the kind's command steers
/// (`CompactCommand::steer_checked`) and an interrupt elsewhere. Whether it was typed; a miss
/// leaves it queued.
fn attempt(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentState,
    command_id: &MessageId,
    pane_id: &PaneId,
    hard: bool,
) -> Result<bool> {
    let steers = crate::agents::spec_by_kind(agent.kind.as_str())
        .and_then(|spec| spec.launch.compact_command)
        .is_some_and(|compact| compact.steer_checked);
    let policy = match (hard, steers) {
        (false, _) => DeliveryPolicy::Boundary,
        (true, true) => DeliveryPolicy::Steer { force: false },
        (true, false) => DeliveryPolicy::Interrupt { force: false },
    };
    Ok(synthetic::attempt_now(
        workspace,
        store,
        command_id,
        Some(pane_id),
        policy,
        DeliveryFailureDisposition::Retry,
        "compaction delivery gate closed",
    )?)
}

/// The continue for the compaction command `command_id` on `agent`'s card: a system prompt at
/// the `Done` gate, which the fold marks owed (`SoftCompactMark::continue_owed`) once queued and
/// delivery withdraws once that mark clears or the command ends unsent.
pub(super) fn continue_record(
    workspace: &ResolvedWorkspace,
    agent: &AgentState,
    command_id: &MessageId,
    text: String,
) -> MessageRecord {
    let mut message = SyntheticMessage {
        agent,
        text,
        sender: MessageSender::System,
        gate: DeliveryGate::Done,
        premises: crate::store::message::Premise::NONE,
    }
    .record(workspace);
    message.soft_compact = Some(SoftCompactStep::Continue);
    message.follows_command = Some(command_id.clone());
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::store::message::MessageRecord;
    use crate::{RuntimePaths, StatePaths};

    #[test]
    fn repeated_compaction_display_is_age_free() {
        let now = Timestamp::now();
        let texts = [now - std::time::Duration::from_secs(3600), now]
            .map(|at| CompactErr::Repeated { at }.to_string());
        assert_eq!(
            texts,
            ["already compacted and has not taken a turn since; a compaction never follows a compaction";
                2]
        );
    }

    #[test]
    fn repeated_compaction_live_message_uses_the_callers_time() {
        let now: Timestamp = "2026-10-31T12:00:00Z".parse().unwrap();
        let error = CompactErr::Repeated {
            at: now - std::time::Duration::from_secs(300),
        };
        assert_eq!(
            error.live_message(now),
            "already compacted 5m ago and has not taken a turn since; a compaction never follows a compaction"
        );
    }

    #[test]
    fn standalone_compaction_refuses_brackets_pending_commands_and_completed_compactions() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let store = Store::open(
            StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
            RuntimePaths::under(id.clone(), &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let now = Timestamp::now();
        let mut agent = AgentState::stub("claude", "session-1", AgentStatus::Idle);
        assert!(refuse_repeat(&store, &agent, now).is_ok());
        agent.compacting_since = Some(now);
        assert!(matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Compacting)
        ));
        agent.compacting_since = None;
        agent.compacted_awaiting_prompt = Some(now);
        assert!(matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Repeated { .. })
        ));
        agent.compacted_awaiting_prompt = None;
        let command = MessageRecord::new(id, &agent, "/compact".to_owned(), DeliveryGate::Done)
            .with_body(MessageBody::Command);
        store.queue_message(&command, "session").unwrap();
        assert!(
            matches!(refuse_repeat(&store, &agent, now), Err(CompactErr::Pending { message_id, .. }) if message_id == command.message_id)
        );
        let other = AgentState::stub("claude", "session-2", AgentStatus::Idle);
        assert!(refuse_repeat(&store, &other, now).is_ok());

        store
            .cancel_message(&command.message_id, "session", "test")
            .unwrap();
        let mut tagged = MessageRecord::requeue_from(&command);
        tagged.idle_since = Some(now);
        store.queue_message(&tagged, "session").unwrap();
        agent.turn_started_at = Some(now);
        let tied = matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Pending { .. })
        );
        agent.turn_started_at = Some(now + std::time::Duration::from_secs(1));
        let lapsed = refuse_repeat(&store, &agent, now).is_ok();
        store
            .claim_message_for_steer(&tagged.message_id, now)
            .unwrap()
            .unwrap();
        let claimed = matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Pending { .. })
        );
        assert_eq!((tied, lapsed, claimed), (true, true, true));
    }

    fn room(dir: &std::path::Path) -> (ResolvedWorkspace, Store) {
        let workspace = ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(dir),
            project_root: dir.into(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: dir.into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        };
        let store = Store::open(
            StatePaths::under(workspace.workspace_id.clone(), &dir.join("state")).unwrap(),
            RuntimePaths::under(workspace.workspace_id.clone(), &dir.join("runtime")).unwrap(),
        )
        .unwrap();
        (workspace, store)
    }

    #[test]
    fn a_join_adds_one_continue_behind_a_queued_command_and_nothing_once_it_left_queued() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, store) = room(dir.path());
        let queue = |agent: &AgentState, text: &str, body: MessageBody| {
            let record = MessageRecord::new(
                workspace.workspace_id.clone(),
                agent,
                text.to_owned(),
                DeliveryGate::Done,
            )
            .with_body(body);
            store.queue_message(&record, "room").unwrap();
            record.message_id
        };
        let live = |agent: &AgentState| -> Vec<MessageRecord> {
            store
                .list_messages()
                .unwrap()
                .into_iter()
                .filter(|record| record.same_agent_card(agent))
                .collect()
        };
        let join = |agent: &AgentState, command: &MessageId, text: Option<&str>| {
            join_waiting(
                &workspace,
                &store,
                agent,
                command,
                text.map(str::to_owned),
                false,
            )
            .unwrap()
            .map(|(follows, _)| follows)
        };

        let busy = AgentState::stub("claude", "session-1", AgentStatus::Running);
        assert!(busy.holds_open_turn());
        let command = queue(&busy, "/compact", MessageBody::Command);
        let between = queue(&busy, "between", MessageBody::Prompt);
        assert_eq!(join(&busy, &command, Some("keep going")), Some(true));
        assert_eq!(
            join(&busy, &command, Some("keep going")),
            Some(true),
            "a continue already follows the command, so a second join adds none"
        );
        let records = live(&busy);
        let continues: Vec<_> = records
            .iter()
            .filter(|record| record.soft_compact == Some(SoftCompactStep::Continue))
            .collect();
        assert_eq!((records.len(), continues.len()), (3, 1), "{records:?}");
        assert_eq!(continues[0].follows_command.as_ref(), Some(&command));
        assert_eq!(continues[0].text, "keep going");
        assert!(
            continues[0].message_id > between,
            "the continue queues behind every live record on the card"
        );

        let bare = AgentState::stub("claude", "session-2", AgentStatus::Running);
        let command = queue(&bare, "/compact", MessageBody::Command);
        assert_eq!(join(&bare, &command, None), Some(false));
        let resting = AgentState::stub("claude", "session-3", AgentStatus::Idle);
        let resting_command = queue(&resting, "/compact", MessageBody::Command);
        assert_eq!(
            join(&resting, &resting_command, Some("keep going")),
            Some(false),
            "an agent holding no open turn is owed no continue"
        );
        store
            .claim_message_for_steer(&command, Timestamp::now())
            .unwrap()
            .unwrap();
        assert_eq!(
            join(&bare, &command, Some("keep going")),
            None,
            "a claimed command is no longer joined"
        );
        assert_eq!((live(&bare).len(), live(&resting).len()), (1, 1));
    }

    /// What a hard attempt that typed nothing leaves: a command still queued keeps no continue owed
    /// past its stop and gets back the one owed before the request; one that ended untyped
    /// withdraws the continue behind it and is the error.
    #[test]
    fn an_untyped_hard_command_leaves_what_the_soft_request_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, store) = room(dir.path());
        let busy = AgentState::stub("claude", "session-1", AgentStatus::Running);
        let queue = || {
            let command = MessageRecord::new(
                workspace.workspace_id.clone(),
                &busy,
                "/compact".to_owned(),
                DeliveryGate::Done,
            )
            .with_body(MessageBody::Command);
            let tail = MessageRecord {
                past_stop: true,
                ..continue_record(&workspace, &busy, &command.message_id, "now".to_owned())
            };
            let tail = store
                .queue_message_with_tail(&command, &tail, "room")
                .unwrap();
            (command.message_id, tail.message_id)
        };
        let follower = |command: &MessageId| -> Vec<(MessageStatus, bool, String)> {
            let (live, history) = store.list_messages_and_history().unwrap();
            live.into_iter()
                .chain(history)
                .filter(|record| record.follows_command.as_ref() == Some(command))
                .map(|record| (record.status, record.past_stop, record.text))
                .collect()
        };

        let (waiting, _) = queue();
        let restore = continue_record(&workspace, &busy, &waiting, "owed before".to_owned());
        assert!(matches!(
            untyped_fate(&workspace, &store, &waiting, true, Some(&restore)),
            Ok(Untyped::Waiting)
        ));
        let mut followers = follower(&waiting);
        followers.sort_by_key(|(_, past_stop, _)| *past_stop);
        assert_eq!(
            followers,
            [
                (MessageStatus::Queued, false, "owed before".to_owned()),
                (MessageStatus::Canceled, true, "now".to_owned()),
            ]
        );

        let (ended, tail) = queue();
        assert!(store.cancel_message(&ended, "room", "withdrawn").unwrap());
        let Err(CompactErr::Settled { message_id, .. }) =
            untyped_fate(&workspace, &store, &ended, true, None)
        else {
            panic!("an ended command is the error");
        };
        assert_eq!(message_id, ended);
        assert_eq!(
            follower(&ended),
            [(MessageStatus::Canceled, true, "now".to_owned())],
            "{tail} is withdrawn with its command"
        );
    }

    #[test]
    fn standalone_compaction_persists_the_idle_decision_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, store) = room(dir.path());
        let stamp = Timestamp::from_second(1_000).unwrap();
        let earlier = stamp - std::time::Duration::from_secs(1);
        let cases = [
            (false, Some(stamp), Some(earlier), None),
            (true, Some(stamp), Some(earlier), Some(stamp)),
            (true, Some(earlier), Some(stamp), Some(stamp)),
            (true, None, Some(stamp), Some(stamp)),
            (true, None, None, None),
        ];
        let actual: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(index, &(idle, started, ended, _))| {
                let mut agent =
                    AgentState::stub("claude", &format!("session-{index}"), AgentStatus::Running);
                agent.turn_started_at = started;
                agent.turn_ended_at = ended;
                let id = MessageId::new();
                let outcome = send_compact(
                    &workspace,
                    &store,
                    CompactRequest {
                        premises: crate::store::message::Premise::NONE,
                        message_id: id.clone(),
                        agent: &agent,
                        pane_id: PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_3"),
                        command: "/compact".to_owned(),
                        sender: MessageSender::System,
                        automated: true,
                        idle,
                        continue_text: None,
                        hard: false,
                    },
                )
                .unwrap();
                assert_eq!(outcome, CompactOutcome::Queued { continues: false });
                let queued = store
                    .list_messages()
                    .unwrap()
                    .into_iter()
                    .find(|message| message.message_id == id)
                    .unwrap();
                serde_json::to_value(queued)
                    .unwrap()
                    .get("idle_since")
                    .cloned()
            })
            .collect();
        assert_eq!(
            actual,
            cases.map(|(_, _, _, expected)| expected.map(|stamp| serde_json::json!(stamp)))
        );
    }

    #[test]
    fn compact_agent_refuses_instruction_on_next_prompt_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, store) = room(dir.path());
        let mut agent = AgentState::stub("antigravity", "agy-1", AgentStatus::Idle);
        agent.pane = Some(crate::pane::PaneRef::from_id(PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            "terminal_4",
        )));
        let err = compact_agent(
            &workspace,
            &store,
            &agent,
            "@agy",
            Some("summarize carefully"),
            CompactAsk::default(),
            MessageSender::Human,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("antigravity does not accept a compaction instruction"),
            "unexpected error: {err:#}"
        );
    }
}
