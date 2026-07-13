//! Synchronous reply wait for `rimz message --wait`.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use super::DispatchOutcome;
use crate::cli::render;
use crate::cli::send::WaitSpec;
use crate::cli::spinner::Spinner;
use rimz::agents::transcript::TranscriptCursor;
use rimz::agents::{AgentAdapter, AgentState, AgentStatus};
use rimz::harness::run::RunStatus;
use rimz::ids::{AgentKind, AgentSessionId, MessageId};
use rimz::message::{MessageRecord, MessageStatus};

const POLL: Duration = Duration::from_millis(500);
const WAIT_GUARD_TICKS: u8 = 10;

pub(super) struct ReplyTarget {
    kind: AgentKind,
    agent_id: AgentSessionId,
    agent_name: Option<String>,
    label: String,
    cursor: Option<TranscriptCursor>,
    transcript_path: Option<String>,
}

impl ReplyTarget {
    pub(super) fn new(agent: &AgentState, label: String, adapter: &dyn AgentAdapter) -> Self {
        let transcript_path = agent.transcript_path.clone();
        Self {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            agent_name: agent.name.clone(),
            label,
            cursor: Some(anchored_cursor(
                transcript_path.as_deref(),
                Some(&agent.agent_id),
                adapter,
            )),
            transcript_path,
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

pub(super) struct Leg {
    target: ReplyTarget,
    message_id: MessageId,
    phase: WaitPhase,
    message_status: MessageStatus,
    wait_base: u64,
    cursor: Option<TranscriptCursor>,
    last_message: Option<String>,
    transcript_path: Option<String>,
    done: Option<RunStatus>,
    error: Option<String>,
}

impl Leg {
    pub(super) fn new(mut target: ReplyTarget, outcome: &DispatchOutcome, wait_base: u64) -> Self {
        let (message_id, message_status, done, error) = match outcome {
            DispatchOutcome::Sent { label, message_id } => {
                debug_assert_eq!(label, &target.label);
                (message_id.clone(), MessageStatus::Sent, None, None)
            }
            DispatchOutcome::Queued { label, message_id } => {
                debug_assert_eq!(label, &target.label);
                (message_id.clone(), MessageStatus::Queued, None, None)
            }
            DispatchOutcome::CompactionPending { label, message_id } => {
                debug_assert_eq!(label, &target.label);
                (message_id.clone(), MessageStatus::Queued, None, None)
            }
            DispatchOutcome::SkippedWaiting { label, message_id } => {
                debug_assert_eq!(label, &target.label);
                (
                    message_id.clone(),
                    MessageStatus::Errored,
                    Some(RunStatus::Failed),
                    Some(format!(
                        "{label} ({message_id}) is waiting on your input in its pane; answer it or pass --force"
                    )),
                )
            }
        };
        let cursor = (message_status == MessageStatus::Sent)
            .then(|| target.cursor.take())
            .flatten();
        let transcript_path = target.transcript_path.clone();
        Self {
            target,
            message_id,
            phase: WaitPhase::Delivery,
            message_status,
            wait_base,
            cursor,
            last_message: None,
            transcript_path,
            done,
            error,
        }
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

pub(super) fn wait_for_replies(
    store: &rimz::Store,
    session_name: &str,
    mut legs: Vec<Leg>,
    steer: bool,
    wait: WaitSpec,
    deadline: Option<Instant>,
    caller_identity: Option<(AgentKind, String)>,
) -> Result<()> {
    if legs.is_empty() {
        bail!("--wait requires at least one dispatched message");
    }
    let total = legs.len();
    if total == 1
        && legs[0].done.is_some()
        && let Some(error) = legs[0].error.take()
    {
        bail!(error);
    }
    let spinner = Spinner::delayed(wait_label(&legs), Duration::from_millis(500));
    let mut printed_block = false;
    let settled = legs
        .iter()
        .enumerate()
        .filter_map(|(index, leg)| leg.done.is_some().then_some(index))
        .collect::<Vec<_>>();
    if let Some(winner) = drain_settled(&legs, &settled, wait, total, &spinner, &mut printed_block)?
    {
        return finish_join(&legs, wait, Some(winner));
    }
    if settled_status(&legs.iter().map(|leg| leg.done).collect::<Vec<_>>(), None).is_some() {
        return finish_join(&legs, wait, None);
    }

    let mut tick = 0_u8;
    loop {
        spinner.set(wait_label(&legs));
        let messages = store.list_messages()?;
        let mut snapshot = store.snapshot_cached().context("reading agent snapshot")?;
        let mut settled = Vec::new();
        if tick == 0
            && let Some((self_kind, self_name)) = caller_identity.as_ref()
        {
            snapshot = snapshot
                .with_agent_context(rimz::store::agent_context::read_all(store.runtime_paths()));
            let history = store.list_message_history()?;
            let deadlocked =
                deadlocked_legs(&legs, &messages, &history, &snapshot, self_kind, self_name);
            for (index, cycle) in deadlocked {
                let leg = &mut legs[index];
                leg.done = Some(RunStatus::Failed);
                leg.error = Some(deadlock_error(leg, &cycle));
                settled.push(index);
            }
        }
        for (index, leg) in legs.iter_mut().enumerate() {
            if leg.done.is_some() {
                continue;
            }
            if advance_leg(leg, store, &messages, &snapshot, steer)? {
                settled.push(index);
            }
        }

        if let Some(winner) =
            drain_settled(&legs, &settled, wait, total, &spinner, &mut printed_block)?
        {
            return finish_join(&legs, wait, Some(winner));
        }
        if settled_status(&legs.iter().map(|leg| leg.done).collect::<Vec<_>>(), None).is_some() {
            spinner.pause();
            return finish_join(&legs, wait, None);
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let unfinished = legs
                .iter()
                .enumerate()
                .filter(|(_, leg)| leg.done.is_none())
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            for index in &unfinished {
                let leg = &mut legs[*index];
                if leg.message_status == MessageStatus::Sent {
                    let _ = store.mark_message_timed_out(
                        &leg.message_id,
                        session_name,
                        Some("wait"),
                    )?;
                }
                leg.done = Some(timeout_status(leg.done));
            }
            spinner.pause();
            if wait.json {
                print_json_replies(&legs, None)?;
            } else {
                print_timeout(&legs, &unfinished, wait)?;
            }
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        tick = (tick + 1) % WAIT_GUARD_TICKS;
        std::thread::sleep(next_sleep(deadline));
    }
}

/// Report each newly settled leg; returns the `--any` winner index when the join short-circuits.
fn drain_settled(
    legs: &[Leg],
    settled: &[usize],
    wait: WaitSpec,
    total: usize,
    spinner: &Spinner,
    printed_block: &mut bool,
) -> Result<Option<usize>> {
    for &index in settled {
        if !wait.json {
            spinner.pause();
            print_reply_result_for_leg(&legs[index], total, printed_block)?;
        }
        if wait.any {
            spinner.pause();
            return Ok(Some(index));
        }
        spinner.resume();
    }
    Ok(None)
}

fn wait_label(legs: &[Leg]) -> String {
    let pending = legs
        .iter()
        .filter(|leg| leg.done.is_none())
        .collect::<Vec<_>>();
    if let [leg] = pending.as_slice() {
        let phase = if matches!(
            leg.message_status,
            MessageStatus::Queued | MessageStatus::Claimed
        ) {
            "parked for next turn"
        } else {
            "turn running"
        };
        format!("waiting for {} — {phase}", leg.target.label)
    } else {
        format!("waiting for {}/{} replies", pending.len(), legs.len())
    }
}

fn deadlocked_legs(
    legs: &[Leg],
    live: &[MessageRecord],
    history: &[MessageRecord],
    snapshot: &rimz::SidebarSnapshot,
    self_kind: &AgentKind,
    self_name: &str,
) -> Vec<(usize, Vec<rimz::message::wait_guard::WaitCycleHop>)> {
    legs.iter()
        .enumerate()
        .filter(|(_, leg)| leg.done.is_none())
        .filter_map(|(index, leg)| {
            let target = snapshot
                .agents
                .iter()
                .find(|agent| leg.target.matches(agent))?;
            let cycle = rimz::message::wait_guard::wait_cycle(
                live,
                history,
                &snapshot.agents,
                self_kind,
                self_name,
                target,
            )?;
            (rimz::message::wait_guard::youngest_wait_message(&cycle, &leg.message_id)
                == leg.message_id)
                .then_some((index, cycle))
        })
        .collect()
}

fn deadlock_error(leg: &Leg, cycle: &[rimz::message::wait_guard::WaitCycleHop]) -> String {
    let action = "aborted this wait — your message stays queued and delivers at the turn boundary";
    let Some(first) = cycle.first() else {
        return format!("deadlock: {} is your own agent; {action}", leg.target.label);
    };
    let chain = (cycle.len() > 1).then(|| {
        let mut handles = cycle
            .iter()
            .map(|hop| hop.handle.as_str())
            .collect::<Vec<_>>();
        handles.push("you");
        format!(" ({} reply-wait chain)", handles.join(" → "))
    });
    format!(
        "deadlock: {} ({}) is waiting on your reply{}; {action}",
        first.handle,
        first.message_id,
        chain.as_deref().unwrap_or_default()
    )
}

fn advance_leg(
    leg: &mut Leg,
    store: &rimz::Store,
    messages: &[MessageRecord],
    snapshot: &rimz::SidebarSnapshot,
    steer: bool,
) -> Result<bool> {
    if let Some(status) =
        current_message_status(store, messages, &leg.message_id, &mut leg.wait_base)?
    {
        leg.message_status = status;
    }
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| leg.target.matches(agent));
    let adapter = rimz::agents::find_adapter(leg.target.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", leg.target.kind))?;
    if let Some(path) = agent.and_then(|agent| agent.transcript_path.as_deref()) {
        leg.transcript_path = Some(path.to_owned());
    }

    if leg.cursor.is_none()
        && matches!(
            leg.message_status,
            MessageStatus::Sent | MessageStatus::Delivered
        )
    {
        leg.cursor = Some(anchored_cursor(
            agent.and_then(|agent| agent.transcript_path.as_deref()),
            agent.map(|agent| &agent.agent_id),
            adapter,
        ));
    } else if let (Some(cursor), Some(agent)) = (&mut leg.cursor, agent) {
        for message in cursor.messages(
            agent.transcript_path.as_deref(),
            Some(&agent.agent_id),
            adapter,
        ) {
            leg.last_message = Some(message);
        }
    }

    match step(
        leg.phase,
        steer,
        leg.message_status,
        agent.map(CardView::from),
    ) {
        Step::Wait(next) => {
            leg.phase = next;
            Ok(false)
        }
        Step::Finish(status) => {
            leg.done = Some(status);
            Ok(true)
        }
        Step::DeliveryFailed(status) => {
            leg.done = Some(RunStatus::Failed);
            leg.error = Some(format!(
                "message {} for {} ({})",
                status.as_str(),
                leg.target.label,
                leg.message_id
            ));
            Ok(true)
        }
        Step::AgentGone => {
            leg.done = Some(RunStatus::Failed);
            leg.error = Some(format!(
                "{} stopped before its reply turn completed",
                leg.target.label
            ));
            Ok(true)
        }
    }
}

fn anchored_cursor(
    path: Option<&str>,
    session_id: Option<&AgentSessionId>,
    adapter: &dyn AgentAdapter,
) -> TranscriptCursor {
    let mut cursor = TranscriptCursor::new(false);
    let _ = cursor.messages(path, session_id, adapter);
    cursor
}

fn current_message_status(
    store: &rimz::Store,
    messages: &[MessageRecord],
    message_id: &MessageId,
    wait_base: &mut u64,
) -> Result<Option<MessageStatus>> {
    if let Some(message) = messages
        .iter()
        .find(|message| message.message_id == *message_id)
    {
        return Ok(Some(message.status));
    }
    rimz::message::send::latest_terminal_message_status(store, message_id, wait_base)
        .map_err(Into::into)
}

fn finish_join(legs: &[Leg], wait: WaitSpec, winner: Option<usize>) -> Result<()> {
    if wait.json {
        print_json_replies(legs, winner)?;
    }
    let statuses = legs.iter().map(|leg| leg.done).collect::<Vec<_>>();
    let status = settled_status(&statuses, winner)
        .context("reply join finished without a terminal status")?;
    return_or_exit(status)
}

fn settled_status(statuses: &[Option<RunStatus>], winner: Option<usize>) -> Option<RunStatus> {
    if let Some(winner) = winner {
        return statuses.get(winner).copied().flatten();
    }
    if statuses.iter().any(Option::is_none) {
        return None;
    }
    Some(
        statuses
            .iter()
            .flatten()
            .copied()
            .find(|status| *status != RunStatus::Completed)
            .unwrap_or(RunStatus::Completed),
    )
}

fn timeout_status(status: Option<RunStatus>) -> RunStatus {
    status.unwrap_or(RunStatus::TimedOut)
}

fn return_or_exit(status: RunStatus) -> Result<()> {
    if status == RunStatus::Completed {
        return Ok(());
    }
    std::process::exit(status.exit_code());
}

fn print_reply_result_for_leg(leg: &Leg, total: usize, printed_block: &mut bool) -> Result<()> {
    let status = leg.done.context("rendering an unfinished reply leg")?;
    if let Some(error) = &leg.error {
        let mut err = render::err();
        writeln!(err, "rimz: {error}")?;
        err.flush()?;
        return Ok(());
    }
    let message = leg
        .last_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty());
    if total == 1 {
        if let Some(message) = message {
            let mut out = render::out();
            writeln!(out, "{message}")?;
            out.flush()?;
        } else if status == RunStatus::Completed {
            let mut err = render::err();
            writeln!(
                err,
                "rimz: turn completed but no final assistant message was extracted"
            )?;
            err.flush()?;
        }
        if status != RunStatus::Completed {
            print_turn_failure(None, status, leg.transcript_path.as_deref())?;
        }
        return Ok(());
    }
    if status == RunStatus::Completed {
        if let Some(message) = message {
            let mut out = render::out();
            if *printed_block {
                writeln!(out)?;
            }
            writeln!(out, "{}:\n{message}", leg.target.label)?;
            out.flush()?;
            *printed_block = true;
        } else {
            let mut err = render::err();
            writeln!(
                err,
                "rimz: {} turn completed but no final assistant message was extracted",
                leg.target.label
            )?;
            err.flush()?;
        }
        return Ok(());
    }

    print_turn_failure(
        Some(&leg.target.label),
        status,
        leg.transcript_path.as_deref(),
    )
}

fn print_turn_failure(
    label: Option<&str>,
    status: RunStatus,
    transcript_path: Option<&str>,
) -> Result<()> {
    let mut err = render::err();
    if let Some(label) = label {
        writeln!(
            err,
            "rimz: {label} turn {} (exit {})",
            run_status_label(status),
            status.exit_code()
        )?;
    } else {
        writeln!(
            err,
            "rimz: turn {} (exit {})",
            run_status_label(status),
            status.exit_code()
        )?;
    }
    if let Some(transcript_path) = transcript_path {
        writeln!(err, "transcript: {transcript_path}")?;
    }
    err.flush()?;
    Ok(())
}

#[derive(Serialize)]
struct ReplyJson<'a> {
    status: RunStatus,
    reply: Option<&'a str>,
    message_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

fn print_json_replies(legs: &[Leg], winner: Option<usize>) -> Result<()> {
    let mut replies = BTreeMap::new();
    for (index, leg) in legs.iter().enumerate() {
        if winner.is_some_and(|winner| winner != index) {
            continue;
        }
        let status = leg.done.context("serializing an unfinished reply leg")?;
        replies.insert(
            leg.target.label.as_str(),
            ReplyJson {
                status,
                reply: leg
                    .last_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|reply| !reply.is_empty()),
                message_id: leg.message_id.as_str(),
                error: leg.error.as_deref(),
            },
        );
    }
    let mut out = render::out();
    serde_json::to_writer(&mut out, &replies)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn print_timeout(legs: &[Leg], unfinished: &[usize], wait: WaitSpec) -> Result<()> {
    let mut err = render::err();
    let hint = if wait.mode.uses_agent_default() {
        " (default 1h for agent callers; use --wait=<duration> to change)"
    } else {
        ""
    };
    if legs.len() == 1 {
        writeln!(err, "rimz: wait timed out{hint}")?;
    } else {
        let labels = unfinished
            .iter()
            .map(|index| legs[*index].target.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(err, "rimz: wait timed out for {labels}{hint}")?;
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
        RunStatus::VerifyFailed => "verify failed",
        RunStatus::TimedOut => "timed out",
        RunStatus::BudgetExceeded => "budget exceeded",
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

    fn leg(label: &str, status: MessageStatus, done: Option<RunStatus>) -> Leg {
        Leg {
            target: ReplyTarget {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session"),
                agent_name: None,
                label: label.to_owned(),
                cursor: None,
                transcript_path: None,
            },
            message_id: MessageId::new(),
            phase: WaitPhase::Delivery,
            message_status: status,
            wait_base: 0,
            cursor: None,
            last_message: None,
            transcript_path: None,
            done,
            error: None,
        }
    }

    #[test]
    fn wait_label_names_single_leg_phase_and_multi_leg_count() {
        assert_eq!(
            wait_label(&[leg("@planner", MessageStatus::Queued, None)]),
            "waiting for @planner — parked for next turn"
        );
        assert_eq!(
            wait_label(&[leg("@planner", MessageStatus::Claimed, None)]),
            "waiting for @planner — parked for next turn"
        );
        assert_eq!(
            wait_label(&[leg("@planner", MessageStatus::Sent, None)]),
            "waiting for @planner — turn running"
        );
        assert_eq!(
            wait_label(&[
                leg(
                    "@planner",
                    MessageStatus::Delivered,
                    Some(RunStatus::Completed)
                ),
                leg("@reviewer", MessageStatus::Delivered, None),
            ]),
            "waiting for @reviewer — turn running"
        );
        assert_eq!(
            wait_label(&[
                leg("@planner", MessageStatus::Queued, None),
                leg("@reviewer", MessageStatus::Sent, None),
            ]),
            "waiting for 2/2 replies"
        );
    }

    #[test]
    fn reply_target_anchors_transcript_before_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"old answer\"}]}}\n",
        )
        .unwrap();
        let mut agent = rimz::testkit::agent_state("claude", "sess-reply", Timestamp::UNIX_EPOCH);
        agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
        let adapter = rimz::agents::find_adapter("claude").unwrap();
        let mut target = ReplyTarget::new(&agent, "@claude".to_owned(), adapter);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(
            file,
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"fresh answer\"}}]}}}}"
        )
        .unwrap();

        let messages = target.cursor.as_mut().unwrap().messages(
            agent.transcript_path.as_deref(),
            Some(&agent.agent_id),
            adapter,
        );
        assert_eq!(messages, ["fresh answer"]);
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

    #[test]
    fn gather_uses_the_first_failure_in_target_order() {
        let statuses = [
            Some(RunStatus::Completed),
            Some(RunStatus::BudgetExceeded),
            Some(RunStatus::Failed),
        ];
        assert_eq!(
            settled_status(&statuses, None),
            Some(RunStatus::BudgetExceeded)
        );
        assert_eq!(
            settled_status(&[Some(RunStatus::Completed), None], None),
            None
        );
    }

    #[test]
    fn any_uses_the_observed_winner_status() {
        let statuses = [None, Some(RunStatus::Failed), Some(RunStatus::Completed)];
        assert_eq!(settled_status(&statuses, Some(1)), Some(RunStatus::Failed));
        assert_eq!(settled_status(&statuses, Some(0)), None);
    }

    #[test]
    fn deadline_only_reclassifies_unfinished_legs() {
        assert_eq!(timeout_status(None), RunStatus::TimedOut);
        assert_eq!(
            timeout_status(Some(RunStatus::Completed)),
            RunStatus::Completed
        );
    }
}
