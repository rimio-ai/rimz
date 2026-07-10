use std::io::Write;

use anyhow::Result;
use rimz::harness::run::{RunLiveStatus, RunRecord, RunStatus};

use crate::cli::render;

pub(crate) fn print_run_output(
    record: &RunRecord,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    if let Some(message) = trimmed_message(record.last_message.as_deref()) {
        writeln!(out, "{message}")?;
    } else if record.status == RunStatus::Completed {
        writeln!(
            err,
            "rimz: run completed but no final assistant message was extracted"
        )?;
    }
    print_run_forensics(record, err)
}

pub(crate) fn print_run_forensics<W: Write + ?Sized>(
    record: &RunRecord,
    err: &mut W,
) -> Result<()> {
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
        if let Some(verify) = record.verify.as_ref().filter(|verify| !verify.passed) {
            let status = if verify.timed_out {
                "timeout".to_owned()
            } else {
                verify
                    .code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_owned())
            };
            writeln!(
                err,
                "verify `{}` exited {status} (attempt {})",
                verify.cmd, verify.attempts
            )?;
            if !verify.output.trim().is_empty() {
                writeln!(
                    err,
                    "{}",
                    render::paint(render::palette::FAINT, &verify.output)
                )?;
            }
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
        RunStatus::VerifyFailed => "verify_failed",
        RunStatus::TimedOut => "timed_out",
        RunStatus::BudgetExceeded => "budget_exceeded",
        RunStatus::Canceled => "canceled",
    }
}

pub(crate) enum StreamSink<'a> {
    Text {
        out: &'a mut dyn Write,
        err: &'a mut dyn Write,
        last_emitted: Option<String>,
    },
    Ndjson {
        out: &'a mut dyn Write,
        last_live: Option<RunLiveStatus>,
    },
}

impl<'a> StreamSink<'a> {
    pub(crate) fn text(out: &'a mut dyn Write, err: &'a mut dyn Write) -> Self {
        Self::Text {
            out,
            err,
            last_emitted: None,
        }
    }

    pub(crate) fn ndjson(out: &'a mut dyn Write) -> Self {
        Self::Ndjson {
            out,
            last_live: None,
        }
    }

    pub(crate) fn is_text(&self) -> bool {
        matches!(self, Self::Text { .. })
    }

    pub(crate) fn message(&mut self, text: String) -> Result<()> {
        match self {
            Self::Text {
                out, last_emitted, ..
            } => emit_text_message(&mut **out, last_emitted, &text),
            Self::Ndjson { out, .. } => emit_ndjson(&mut **out, &RunStreamEvent::Message { text }),
        }
    }

    pub(crate) fn status(&mut self, live: RunLiveStatus) -> Result<()> {
        match self {
            Self::Text { .. } => Ok(()),
            Self::Ndjson { out, last_live } => {
                if last_live.as_ref() == Some(&live) {
                    return Ok(());
                }
                emit_ndjson(&mut **out, &RunStreamEvent::Status { live: live.clone() })?;
                *last_live = Some(live);
                Ok(())
            }
        }
    }

    pub(crate) fn end_record(&mut self, record: &RunRecord) -> Result<()> {
        self.end_status(record.status, record.last_message.as_deref())?;
        if let Self::Text { err, .. } = self {
            print_run_forensics(record, &mut **err)?;
        }
        Ok(())
    }

    pub(crate) fn end_status(
        &mut self,
        status: RunStatus,
        last_message: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Text {
                out, last_emitted, ..
            } => {
                if let Some(message) = trimmed_message(last_message)
                    && last_emitted.as_deref() != Some(message)
                {
                    emit_text_message(&mut **out, last_emitted, message)?;
                }
                Ok(())
            }
            Self::Ndjson { out, .. } => emit_ndjson(
                &mut **out,
                &RunStreamEvent::End {
                    status,
                    last_message: last_message.map(str::to_owned),
                },
            ),
        }
    }

    pub(crate) fn timeout(&mut self) -> Result<()> {
        if let Self::Text { err, .. } = self {
            writeln!(&mut **err, "rimz: wait timed out")?;
        }
        Ok(())
    }
}

fn emit_text_message(
    out: &mut dyn Write,
    last_emitted: &mut Option<String>,
    text: &str,
) -> Result<()> {
    let Some(message) = trimmed_message(Some(text)) else {
        return Ok(());
    };
    if last_emitted.is_some() {
        writeln!(out)?;
    }
    writeln!(out, "{message}")?;
    out.flush()?;
    *last_emitted = Some(message.to_owned());
    Ok(())
}

fn emit_ndjson(out: &mut dyn Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn trimmed_message(message: Option<&str>) -> Option<&str> {
    message.map(str::trim).filter(|message| !message.is_empty())
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

    #[test]
    fn verify_failed_run_prints_answer_and_verify_forensics() {
        let mut record = record(RunStatus::VerifyFailed);
        record.last_message = Some("claimed done".to_owned());
        record.verify = Some(rimz::harness::run::RunVerify {
            cmd: "cargo xtask test auth".to_owned(),
            attempts: 3,
            passed: false,
            code: Some(7),
            timed_out: false,
            output: "still broken".to_owned(),
        });
        let mut out = Vec::new();
        let mut err = Vec::new();

        print_run_output(&record, &mut out, &mut err).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "claimed done\n");
        let err = anstream::adapter::strip_str(&String::from_utf8(err).unwrap()).to_string();
        assert!(err.contains("rimz: run verify_failed (exit 123)"));
        assert!(err.contains("verify `cargo xtask test auth` exited 7 (attempt 3)"));
        assert!(err.contains("still broken"));
    }

    #[test]
    fn text_stream_dedupes_already_streamed_final_message() {
        let mut record = record(RunStatus::Completed);
        record.last_message = Some("done".to_owned());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut sink = StreamSink::text(&mut out, &mut err);

        sink.message("done".to_owned()).unwrap();
        sink.end_record(&record).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "done\n");
        assert!(err.is_empty());
    }

    #[test]
    fn text_stream_prints_unstreamed_final_message() {
        let mut record = record(RunStatus::Completed);
        record.last_message = Some("done".to_owned());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut sink = StreamSink::text(&mut out, &mut err);

        sink.end_record(&record).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "done\n");
        assert!(err.is_empty());
    }
}
