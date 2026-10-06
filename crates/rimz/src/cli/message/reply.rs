//! Poll and present synchronous `rimz message --wait` replies.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::cli::render;
use crate::cli::render::prose::{Prose, prose_width};
use crate::cli::send::WaitSpec;
use crate::cli::spinner::Spinner;
use rimz::harness::run::report;
use rimz::ids::MessageId;
use rimz::message::reply::{ReplyFailure, ReplyProgress, ReplyResult, ReplyUpdate, ReplyWait};
use rimz::store::message::MessageStatus;
use rimz::store::run::RunStatus;

const POLL: Duration = Duration::from_millis(500);

pub(super) fn wait_for_replies(
    store: &rimz::Store,
    session_name: &str,
    mut wait_state: ReplyWait,
    wait: WaitSpec,
    deadline: Option<Instant>,
    caller: Option<&rimz::harness::ancestry::CallerIdentity>,
) -> Result<()> {
    let initial_progress = wait_state.progress();
    let total = progress_total(&initial_progress);
    let claim = |result: &ReplyResult| {
        let answered = result.failure.is_none()
            && result
                .final_message
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty());
        if !answered {
            return;
        }
        if let Err(err) =
            report::claim_reply(store, session_name, &result.message_id, caller, deadline)
        {
            tracing::warn!(error = %err, message_id = %result.message_id, "could not claim printed reply");
        }
    };
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
            &claim,
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
                    .collect::<Vec<_>>();
                print_timeout(total, &timed_out, wait)?;
                for result in update.settled {
                    gathered.insert(result.label.clone(), result);
                }
                if wait.json {
                    print_json_replies(&gathered, None)?;
                    for result in gathered.values() {
                        claim(result);
                    }
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
    claim: &impl Fn(&ReplyResult),
) -> Result<Option<RunStatus>> {
    let winner = update.join.as_ref().and_then(|join| join.winner.as_ref());
    for result in update.settled {
        if winner.is_some_and(|message_id| *message_id != result.message_id) {
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
            if print_reply_result(&result, total, printed_block, Prose::for_stdout())? {
                claim(&result);
            }
            spinner.resume();
        }
        gathered.insert(result.label.clone(), result);
    }
    let Some(join) = update.join else {
        return Ok(None);
    };
    spinner.pause();
    if wait.json {
        print_json_replies(gathered, join.winner.as_ref())?;
        for result in gathered.values() {
            if join
                .winner
                .as_ref()
                .is_none_or(|winner| *winner == result.message_id)
            {
                claim(result);
            }
        }
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

fn print_reply_result(
    result: &ReplyResult,
    total: usize,
    printed_block: &mut bool,
    prose: Prose,
) -> Result<bool> {
    if let Some(failure) = &result.failure {
        let mut err = render::err();
        writeln!(err, "rimz: {}", failure_message(result, failure))?;
        err.flush()?;
        return Ok(false);
    }
    let message = result
        .final_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let label = (total > 1).then_some(result.label.as_str());
    let printed = match (label, message, result.status) {
        (None, Some(message), _) => {
            let mut out = render::out();
            write_prose(&mut out, message, prose)?;
            out.flush()?;
            true
        }
        (Some(label), Some(message), RunStatus::Completed) => {
            let mut out = render::out();
            if *printed_block {
                writeln!(out)?;
            }
            writeln!(out, "{label}:")?;
            write_prose(&mut out, message, prose)?;
            out.flush()?;
            *printed_block = true;
            true
        }
        (None, None, RunStatus::Completed) => {
            let mut err = render::err();
            writeln!(
                err,
                "rimz: turn completed but no final assistant message was extracted"
            )?;
            err.flush()?;
            false
        }
        (Some(label), None, RunStatus::Completed) => {
            let mut err = render::err();
            writeln!(
                err,
                "rimz: {label} turn completed but no final assistant message was extracted"
            )?;
            err.flush()?;
            false
        }
        (Some(_), Some(_), _) | (_, None, _) => false,
    };
    if result.status != RunStatus::Completed {
        print_turn_failure(label, result.status, result.transcript_path.as_deref())?;
    }
    Ok(printed)
}

fn write_prose(out: &mut impl Write, message: &str, prose: Prose) -> Result<()> {
    for line in prose.lines(message, prose_width(0)) {
        writeln!(out, "{line}")?;
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
            status.label(),
            status.exit_code()
        )?;
    } else {
        writeln!(
            err,
            "rimz: turn {} (exit {})",
            status.label(),
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
    let mut out = render::out();
    serde_json::to_writer(&mut out, &replies)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn print_timeout(total: usize, timed_out: &[&ReplyResult], wait: WaitSpec) -> Result<()> {
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
            timed_out
                .iter()
                .map(|result| result.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    for result in timed_out {
        if let Some(hint) = timeout_hint(result.message_status, &result.label, &result.message_id) {
            writeln!(err, "  {hint}")?;
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

fn timeout_hint(status: MessageStatus, label: &str, id: &MessageId) -> Option<String> {
    Some(match status {
        MessageStatus::Queued | MessageStatus::Claimed => format!(
            "{label}: {id} is still queued and will deliver. withdraw it: rimz message cancel {id}   read the reply later: rimz agents logs {label}"
        ),
        MessageStatus::Sent => format!(
            "{label}: {id} was typed into the pane and not acknowledged; do not resend. check: rimz agents logs {label}"
        ),
        MessageStatus::Delivered => format!(
            "{label}: {id} was delivered; {label} is still working on it. read the reply later: rimz agents logs {label}"
        ),
        _ => return None,
    })
}

fn return_or_exit(status: RunStatus) -> Result<()> {
    if status == RunStatus::Completed {
        return Ok(());
    }
    std::process::exit(status.exit_code());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_hints_distinguish_queued_sent_and_delivered() {
        let id = "msg_0000000000000001".parse().unwrap();
        let actual = [
            MessageStatus::Queued,
            MessageStatus::Claimed,
            MessageStatus::Sent,
            MessageStatus::Delivered,
        ]
        .map(|status| timeout_hint(status, "@coder", &id));
        let queued = "@coder: msg_0000000000000001 is still queued and will deliver. withdraw it: rimz message cancel msg_0000000000000001   read the reply later: rimz agents logs @coder";
        assert_eq!(actual, [
            Some(queued.to_owned()), Some(queued.to_owned()),
            Some("@coder: msg_0000000000000001 was typed into the pane and not acknowledged; do not resend. check: rimz agents logs @coder".to_owned()),
            Some("@coder: msg_0000000000000001 was delivered; @coder is still working on it. read the reply later: rimz agents logs @coder".to_owned()),
        ]);
    }
}
