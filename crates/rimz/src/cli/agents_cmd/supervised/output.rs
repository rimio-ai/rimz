use std::io::Write;

use anyhow::Result;
use rimz::harness::run::{RunLiveStatus, RunRecord, RunStatus};

pub(crate) fn print_run_output(record: &RunRecord) -> Result<()> {
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

pub(crate) fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

pub(crate) fn status_label(status: RunStatus) -> &'static str {
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
