//! Synchronous reply wait for `rimz message --wait`.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use super::DispatchOutcome;
use crate::cli::agents_cmd::TranscriptCursor;
use crate::cli::render;
use rimz::agents::{AgentAdapter, AgentState, AgentStatus};
use rimz::harness::run::RunStatus;
use rimz::ids::{AgentKind, AgentSessionId, MessageId};
use rimz::message::MessageStatus;

const POLL: Duration = Duration::from_millis(500);

pub(super) struct ReplyTarget {
    kind: AgentKind,
    agent_id: AgentSessionId,
    agent_name: Option<String>,
    label: String,
    transcript_path: Option<String>,
}

impl ReplyTarget {
    pub(super) fn new(agent: &AgentState, label: String) -> Self {
        Self {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            agent_name: agent.name.clone(),
            label,
            transcript_path: agent.transcript_path.clone(),
        }
    }

    fn matches(&self, agent: &AgentState) -> bool {
        rimz::message::card_matches(
            &self.kind,
            &self.agent_id,
            self.agent_name.as_deref(),
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitPhase {
    Delivery,
    Reply { turn_started_at: Option<Timestamp> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CardView {
    status: AgentStatus,
    turn_started_at: Option<Timestamp>,
}

impl From<&AgentState> for CardView {
    fn from(agent: &AgentState) -> Self {
        Self {
            status: agent.status,
            turn_started_at: agent.turn_started_at,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Wait(WaitPhase),
    Finish(RunStatus),
    DeliveryFailed(MessageStatus),
    AgentGone,
}

fn step(
    phase: WaitPhase,
    steer: bool,
    message_status: MessageStatus,
    card: Option<CardView>,
) -> Step {
    if phase == WaitPhase::Delivery
        && message_status.is_terminal()
        && message_status != MessageStatus::Delivered
    {
        return Step::DeliveryFailed(message_status);
    }
    let Some(card) = card else {
        return Step::AgentGone;
    };
    match phase {
        WaitPhase::Delivery => {
            let delivered = message_status == MessageStatus::Delivered
                || (steer
                    && message_status == MessageStatus::Sent
                    && card.status == AgentStatus::Running);
            if !delivered {
                return Step::Wait(WaitPhase::Delivery);
            }
            step_reply(None, card)
        }
        WaitPhase::Reply { turn_started_at } => step_reply(turn_started_at, card),
    }
}

fn step_reply(turn_started_at: Option<Timestamp>, card: CardView) -> Step {
    match card.status {
        AgentStatus::Idle | AgentStatus::Success => Step::Finish(RunStatus::Completed),
        AgentStatus::Failed => Step::Finish(RunStatus::Failed),
        AgentStatus::Running
            if turn_started_at.is_some()
                && card.turn_started_at.is_some()
                && turn_started_at != card.turn_started_at =>
        {
            Step::Finish(RunStatus::Completed)
        }
        AgentStatus::Running | AgentStatus::Waiting | AgentStatus::Paused => {
            Step::Wait(WaitPhase::Reply {
                turn_started_at: turn_started_at.or(card.turn_started_at),
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn wait_for_reply(
    store: &rimz::Store,
    session_name: &str,
    target: ReplyTarget,
    steer: bool,
    outcomes: &[DispatchOutcome],
    mut wait_base: u64,
    deadline: Option<Instant>,
) -> Result<()> {
    let (message_id, initial_status) = match outcomes {
        [DispatchOutcome::Sent { message_id, .. }] => (message_id, MessageStatus::Sent),
        [DispatchOutcome::Queued { message_id, .. }] => (message_id, MessageStatus::Queued),
        [DispatchOutcome::SkippedWaiting { label, message_id }] => {
            bail!(
                "{label} ({message_id}) is waiting on your input in its pane; answer it or pass --force"
            )
        }
        _ => bail!("--wait requires exactly one dispatched message"),
    };
    let adapter = rimz::agents::find_adapter(target.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", target.kind))?;
    let mut cursor = (initial_status == MessageStatus::Sent)
        .then(|| anchored_cursor(target.transcript_path.as_deref(), adapter));
    let mut phase = WaitPhase::Delivery;
    let mut message_status = initial_status;
    let mut last_message = None;

    loop {
        if let Some(status) = current_message_status(store, message_id, &mut wait_base)? {
            message_status = status;
        }
        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
        let agent = snapshot.agents.iter().find(|agent| target.matches(agent));

        if cursor.is_none()
            && matches!(
                message_status,
                MessageStatus::Sent | MessageStatus::Delivered
            )
        {
            cursor = Some(anchored_cursor(
                agent.and_then(|agent| agent.transcript_path.as_deref()),
                adapter,
            ));
        } else if let (Some(cursor), Some(agent)) = (&mut cursor, agent) {
            for message in cursor.messages(agent.transcript_path.as_deref(), adapter) {
                last_message = Some(message);
            }
        }

        match step(phase, steer, message_status, agent.map(CardView::from)) {
            Step::Wait(next) => phase = next,
            Step::Finish(status) => {
                print_reply_result(
                    status,
                    last_message.as_deref(),
                    agent.and_then(|agent| agent.transcript_path.as_deref()),
                )?;
                if status == RunStatus::Completed {
                    return Ok(());
                }
                std::process::exit(status.exit_code());
            }
            Step::DeliveryFailed(status) => {
                let mut err = render::err();
                writeln!(
                    err,
                    "rimz: message {} for {} ({message_id})",
                    status.as_str(),
                    target.label
                )?;
                err.flush()?;
                std::process::exit(RunStatus::Failed.exit_code());
            }
            Step::AgentGone => {
                let mut err = render::err();
                writeln!(
                    err,
                    "rimz: {} stopped before its reply turn completed",
                    target.label
                )?;
                err.flush()?;
                std::process::exit(RunStatus::Failed.exit_code());
            }
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if message_status == MessageStatus::Sent {
                let _ = store.mark_message_timed_out(message_id, session_name, Some("wait"))?;
            }
            let mut err = render::err();
            writeln!(err, "rimz: wait timed out")?;
            err.flush()?;
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(next_sleep(deadline));
    }
}

fn anchored_cursor(path: Option<&str>, adapter: &dyn AgentAdapter) -> TranscriptCursor {
    let mut cursor = TranscriptCursor::new(false);
    let _ = cursor.messages(path, adapter);
    cursor
}

fn current_message_status(
    store: &rimz::Store,
    message_id: &MessageId,
    wait_base: &mut u64,
) -> Result<Option<MessageStatus>> {
    if let Some(message) = store
        .list_messages()?
        .into_iter()
        .find(|message| message.message_id == *message_id)
    {
        return Ok(Some(message.status));
    }
    rimz::message::send::latest_terminal_message_status(store, message_id, wait_base)
        .map_err(Into::into)
}

fn print_reply_result(
    status: RunStatus,
    last_message: Option<&str>,
    transcript_path: Option<&str>,
) -> Result<()> {
    let mut out = render::out();
    let mut err = render::err();
    if let Some(message) = last_message
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        writeln!(out, "{message}")?;
        out.flush()?;
    } else if status == RunStatus::Completed {
        writeln!(
            err,
            "rimz: turn completed but no final assistant message was extracted"
        )?;
    }
    if status != RunStatus::Completed {
        writeln!(
            err,
            "rimz: turn {} (exit {})",
            run_status_label(status),
            status.exit_code()
        )?;
        if let Some(transcript_path) = transcript_path {
            writeln!(err, "transcript: {transcript_path}")?;
        }
    }
    err.flush()?;
    Ok(())
}

fn next_sleep(deadline: Option<Instant>) -> Duration {
    deadline.map_or(POLL, |deadline| {
        deadline.saturating_duration_since(Instant::now()).min(POLL)
    })
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::TimedOut => "timed out",
        RunStatus::Canceled => "canceled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(status: AgentStatus, started: i64) -> Option<CardView> {
        Some(CardView {
            status,
            turn_started_at: Some(Timestamp::from_second(started).unwrap()),
        })
    }

    #[test]
    fn parked_message_waits_for_delivery_then_success() {
        assert_eq!(
            step(
                WaitPhase::Delivery,
                false,
                MessageStatus::Queued,
                card(AgentStatus::Running, 1),
            ),
            Step::Wait(WaitPhase::Delivery)
        );
        let next = step(
            WaitPhase::Delivery,
            false,
            MessageStatus::Delivered,
            card(AgentStatus::Running, 2),
        );
        assert_eq!(
            next,
            Step::Wait(WaitPhase::Reply {
                turn_started_at: card(AgentStatus::Running, 2).unwrap().turn_started_at,
            })
        );
        assert_eq!(
            step(
                match next {
                    Step::Wait(phase) => phase,
                    _ => unreachable!(),
                },
                false,
                MessageStatus::Delivered,
                card(AgentStatus::Success, 2),
            ),
            Step::Finish(RunStatus::Completed)
        );
    }

    #[test]
    fn steer_into_running_turn_proceeds_without_delivered() {
        assert!(matches!(
            step(
                WaitPhase::Delivery,
                true,
                MessageStatus::Sent,
                card(AgentStatus::Running, 4),
            ),
            Step::Wait(WaitPhase::Reply { .. })
        ));
    }

    #[test]
    fn waiting_and_paused_keep_reply_open() {
        for status in [AgentStatus::Waiting, AgentStatus::Paused] {
            assert!(matches!(
                step(
                    WaitPhase::Reply {
                        turn_started_at: None,
                    },
                    false,
                    MessageStatus::Delivered,
                    card(status, 5),
                ),
                Step::Wait(WaitPhase::Reply { .. })
            ));
        }
    }

    #[test]
    fn changed_turn_start_finishes_missed_boundary() {
        assert_eq!(
            step(
                WaitPhase::Reply {
                    turn_started_at: Some(Timestamp::from_second(5).unwrap()),
                },
                false,
                MessageStatus::Delivered,
                card(AgentStatus::Running, 6),
            ),
            Step::Finish(RunStatus::Completed)
        );
    }

    #[test]
    fn delivery_terminal_failures_stop_wait() {
        for status in [
            MessageStatus::TimedOut,
            MessageStatus::Errored,
            MessageStatus::Removed,
            MessageStatus::Abandoned,
            MessageStatus::Archived,
        ] {
            assert_eq!(
                step(
                    WaitPhase::Delivery,
                    false,
                    status,
                    card(AgentStatus::Running, 1),
                ),
                Step::DeliveryFailed(status)
            );
            assert_eq!(
                step(WaitPhase::Delivery, false, status, None),
                Step::DeliveryFailed(status),
                "delivery failure wins when the card disappears too"
            );
        }
    }

    #[test]
    fn missing_agent_stops_the_wait() {
        assert_eq!(
            step(
                WaitPhase::Reply {
                    turn_started_at: None,
                },
                false,
                MessageStatus::Delivered,
                None,
            ),
            Step::AgentGone
        );
    }
}
