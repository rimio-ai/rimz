use std::io::Write;

use anyhow::Result;
use rimz::harness::run::{RunLiveStatus, RunRecord, RunStatus};

use crate::cli::render;

pub(crate) fn print_run_output(
    record: &RunRecord,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    if let Some(message) = record
        .last_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        writeln!(out, "{message}")?;
    } else if record.status == RunStatus::Completed {
        writeln!(
            err,
            "rimz: run completed but no final assistant message was extracted"
        )?;
    }
    if record.status != RunStatus::Completed {
        writeln!(
            err,
            "rimz: run {} (exit {})",
            render::paint(
                render::status::run(record.status),
                status_label(record.status)
            ),
            record.status.exit_code()
        )?;
        if let Some(tail) = record
            .failure_tail
            .as_deref()
            .filter(|tail| !tail.trim().is_empty())
        {
            writeln!(err, "{}", render::paint(render::palette::FAINT, tail))?;
        }
        if let Some(transcript) = record.transcript_path.as_deref() {
            writeln!(
                err,
                "{}",
                render::paint(render::palette::MUTED, &format!("transcript: {transcript}"))
            )?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use rimz::harness::run::PermissionMode;
    use rimz::ids::{AgentKind, WorkspaceId};

    fn record(status: RunStatus) -> RunRecord {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = status;
        record
    }

    #[test]
    fn completed_run_prints_last_message_to_stdout_only() {
        let mut record = record(RunStatus::Completed);
        record.last_message = Some("done\n".to_owned());
        let mut out = Vec::new();
        let mut err = Vec::new();

        print_run_output(&record, &mut out, &mut err).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "done\n");
        assert!(err.is_empty());
    }

    #[test]
    fn failed_run_prints_forensics_to_stderr() {
        let mut record = record(RunStatus::Failed);
        record.failure_tail = Some("agent died\nfatal error".to_owned());
        record.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        let mut out = Vec::new();
        let mut err = Vec::new();

        print_run_output(&record, &mut out, &mut err).unwrap();

        assert!(out.is_empty());
        let raw = String::from_utf8(err).unwrap();
        assert!(raw.contains(&render::paint(
            render::status::run(RunStatus::Failed),
            "failed"
        )));
        let err = anstream::adapter::strip_str(&raw).to_string();
        assert!(err.contains("rimz: run failed (exit 1)"));
        assert!(err.contains("agent died\nfatal error"));
        assert!(err.contains("transcript: /tmp/transcript.jsonl"));
    }
}
