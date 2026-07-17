//! Poll and present synchronous `rimz message --wait` replies.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::cli::render;
use crate::cli::send::WaitSpec;
use crate::cli::spinner::Spinner;
use rimz::harness::run::RunStatus;
use rimz::message::reply::{ReplyFailure, ReplyProgress, ReplyResult, ReplyUpdate, ReplyWait};

const POLL: Duration = Duration::from_millis(500);

pub(super) fn wait_for_replies(
    store: &rimz::Store,
    session_name: &str,
    mut wait_state: ReplyWait,
    wait: WaitSpec,
    deadline: Option<Instant>,
) -> Result<()> {
    let initial_progress = wait_state.progress();
    let total = progress_total(&initial_progress);
    let spinner = Spinner::delayed(
        progress_label(&initial_progress),
        Duration::from_millis(500),
    );
    let mut printed_block = false;
    let mut gathered = BTreeMap::new();
    let mut first_poll = true;
    loop {
        spinner.set(progress_label(&wait_state.progress()));
        let update = wait_state.poll(store)?;
        if let Some(status) = present_update(
            update,
            wait,
            total,
            &spinner,
            &mut printed_block,
            &mut gathered,
        )? {
            return return_or_exit(status);
        }
        if !first_poll {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                spinner.pause();
                let update = wait_state.timeout(store, session_name)?;
                let timed_out = update
                    .settled
                    .iter()
                    .filter(|result| result.status == RunStatus::TimedOut)
                    .map(|result| result.label.clone())
                    .collect::<Vec<_>>();
                for result in update.settled {
                    gathered.insert(result.label.clone(), result);
                }
                if wait.json {
                    print_json_replies(&gathered, None)?;
                } else {
                    print_timeout(total, &timed_out, wait)?;
                }
                std::process::exit(RunStatus::TimedOut.exit_code());
            }
            std::thread::sleep(next_sleep(deadline));
        }
        first_poll = false;
    }
}

fn present_update(
    update: ReplyUpdate,
    wait: WaitSpec,
    total: usize,
    spinner: &Spinner,
    printed_block: &mut bool,
    gathered: &mut BTreeMap<String, ReplyResult>,
) -> Result<Option<RunStatus>> {
    let winner = update.join.as_ref().and_then(|join| join.winner.as_ref());
    for result in update.settled {
        let is_winner = winner.is_none_or(|message_id| *message_id == result.message_id);
        if wait.any && !is_winner {
            continue;
        }
        if total == 1
            && let Some(failure) = &result.failure
            && matches!(failure, ReplyFailure::WaitingForInput)
        {
            bail!(failure_message(&result, failure));
        }
        if !wait.json {
            spinner.pause();
            print_reply_result(&result, total, printed_block)?;
            spinner.resume();
        }
        gathered.insert(result.label.clone(), result);
        if wait.any && is_winner {
            break;
        }
    }
    let Some(join) = update.join else {
        return Ok(None);
    };
    spinner.pause();
    if wait.json {
        print_json_replies(gathered, join.winner.as_ref())?;
    }
    Ok(Some(join.status))
}

fn progress_label(progress: &ReplyProgress) -> String {
    match progress {
        ReplyProgress::Target { label, parked } => {
            let phase = if *parked {
                "parked for next turn"
            } else {
                "turn running"
            };
            format!("waiting for {label} — {phase}")
        }
        ReplyProgress::Fanout { pending, total } => {
            format!("waiting for {pending}/{total} replies")
        }
    }
}

fn progress_total(progress: &ReplyProgress) -> usize {
    match progress {
        ReplyProgress::Target { .. } => 1,
        ReplyProgress::Fanout { total, .. } => *total,
    }
}

fn print_reply_result(result: &ReplyResult, total: usize, printed_block: &mut bool) -> Result<()> {
    if let Some(failure) = &result.failure {
        let mut err = render::err();
        writeln!(err, "rimz: {}", failure_message(result, failure))?;
        err.flush()?;
        return Ok(());
    }
    let message = result
        .final_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let label = (total > 1).then_some(result.label.as_str());
    match (label, message, result.status) {
        (None, Some(message), _) => {
            let mut out = render::out();
            writeln!(out, "{message}")?;
            out.flush()?;
        }
        (Some(label), Some(message), RunStatus::Completed) => {
            let mut out = render::out();
            if *printed_block {
                writeln!(out)?;
            }
            writeln!(out, "{label}:\n{message}")?;
            out.flush()?;
            *printed_block = true;
        }
        (None, None, RunStatus::Completed) => {
            let mut err = render::err();
            writeln!(
                err,
                "rimz: turn completed but no final assistant message was extracted"
            )?;
            err.flush()?;
        }
        (Some(label), None, RunStatus::Completed) => {
            let mut err = render::err();
            writeln!(
                err,
                "rimz: {label} turn completed but no final assistant message was extracted"
            )?;
            err.flush()?;
        }
        (Some(_), Some(_), _) | (_, None, _) => {}
    }
    if result.status != RunStatus::Completed {
        print_turn_failure(label, result.status, result.transcript_path.as_deref())?;
    }
    Ok(())
}

fn failure_message(result: &ReplyResult, failure: &ReplyFailure) -> String {
    match failure {
        ReplyFailure::WaitingForInput => format!(
            "{} ({}) is waiting on your input in its pane; answer it or pass --force",
            result.label, result.message_id
        ),
        ReplyFailure::DeliveryFailed { status } => format!(
            "message {} for {} ({})",
            status.as_str(),
            result.label,
            result.message_id
        ),
        ReplyFailure::AgentGone => {
            format!("{} stopped before its reply turn completed", result.label)
        }
        ReplyFailure::Deadlock {
            first_handle,
            first_message_id,
            chain,
        } => {
            let action =
                "aborted this wait — your message stays queued and delivers at the turn boundary";
            let (Some(handle), Some(message_id)) = (first_handle, first_message_id) else {
                return format!("deadlock: {} is your own agent; {action}", result.label);
            };
            let chain = chain
                .as_ref()
                .map(|chain| format!(" ({chain} reply-wait chain)"))
                .unwrap_or_default();
            format!("deadlock: {handle} ({message_id}) is waiting on your reply{chain}; {action}")
        }
    }
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
    error: Option<String>,
}

fn print_json_replies(
    gathered: &BTreeMap<String, ReplyResult>,
    winner: Option<&rimz::ids::MessageId>,
) -> Result<()> {
    let mut replies = BTreeMap::new();
    for (label, result) in gathered {
        if winner.is_some_and(|winner| *winner != result.message_id) {
            continue;
        }
        replies.insert(
            label.as_str(),
            ReplyJson {
                status: result.status,
                reply: result
                    .final_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|reply| !reply.is_empty()),
                message_id: result.message_id.as_str(),
                error: result
                    .failure
                    .as_ref()
                    .map(|failure| failure_message(result, failure)),
            },
        );
    }
    render::json(&replies)
}

fn print_timeout(total: usize, timed_out: &[String], wait: WaitSpec) -> Result<()> {
    let mut err = render::err();
    let hint = if wait.mode.uses_agent_default() {
        " (default 1h for agent callers; use --wait=<duration> to change)"
    } else {
        ""
    };
    if total == 1 {
        writeln!(err, "rimz: wait timed out{hint}")?;
    } else {
        writeln!(
            err,
            "rimz: wait timed out for {}{hint}",
            timed_out.join(", ")
        )?;
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

fn return_or_exit(status: RunStatus) -> Result<()> {
    if status == RunStatus::Completed {
        return Ok(());
    }
    std::process::exit(status.exit_code());
}
