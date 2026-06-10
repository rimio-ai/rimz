use std::io::Write;

use anyhow::Result;
use rimz::run::{RunLiveStatus, RunRecord, RunStatus};

pub(super) fn print_run_output(record: &RunRecord) -> Result<()> {
    if let Some(message) = record
        .last_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        #[expect(clippy::print_stdout, reason = "command result is the run output")]
        {
            println!("{message}");
        }
    } else if record.status == RunStatus::Completed {
        writeln!(
            std::io::stderr().lock(),
            "rimz: run completed but no final assistant message was extracted"
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlockingRunOutput {
    Json,
    FinalMessage,
    StreamAlreadyEmitted,
}

pub(super) fn blocking_run_output(json: bool, stream: bool) -> BlockingRunOutput {
    if stream {
        BlockingRunOutput::StreamAlreadyEmitted
    } else if json {
        BlockingRunOutput::Json
    } else {
        BlockingRunOutput::FinalMessage
    }
}

pub(super) fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[derive(serde::Serialize)]
pub(super) struct RunStatusReport {
    #[serde(flatten)]
    pub(super) record: RunRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) live: Option<RunLiveStatus>,
}

pub(super) fn human_status_line(record: &RunRecord, live: Option<&RunLiveStatus>) -> String {
    let mut line = format!(
        "{} {} {}",
        record.run_id,
        status_label(record.status),
        record.kind
    );
    if let Some(live) = live {
        line.push_str(" (live: ");
        line.push_str(live_status_label(live).as_str());
        line.push(')');
    }
    line
}

fn live_status_label(live: &RunLiveStatus) -> String {
    if let Some(ask) = live.pending_ask.as_ref() {
        return format!(
            "{} - ask {} on {}",
            live.agent_status.as_str(),
            ask.request_id,
            ask.surface
        );
    }
    if live.phase != rimz::agents::TurnPhase::Idle {
        format!(
            "{} - {}",
            live.agent_status.as_str(),
            phase_label(live.phase)
        )
    } else {
        live.agent_status.as_str().to_owned()
    }
}

fn phase_label(phase: rimz::agents::TurnPhase) -> &'static str {
    match phase {
        rimz::agents::TurnPhase::Idle => "idle",
        rimz::agents::TurnPhase::Reasoning => "reasoning",
        rimz::agents::TurnPhase::Acting => "acting",
        rimz::agents::TurnPhase::Parked => "parked",
    }
}

pub(super) fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::TimedOut => "timed_out",
        RunStatus::Canceled => "canceled",
    }
}

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum RunStreamEvent {
    Message {
        text: String,
    },
    Status {
        #[serde(flatten)]
        live: RunLiveStatus,
    },
    End {
        status: RunStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_message: Option<String>,
    },
}
