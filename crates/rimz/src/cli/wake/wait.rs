use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use jiff::Timestamp;

use super::*;
use rimz::harness::schedule::run_log::{self, LoopRunRecord, LoopRunResult};

const POLL: Duration = Duration::from_millis(500);

pub(super) fn for_record(
    name: &str,
    created: Timestamp,
    timeout: Option<Duration>,
) -> Result<LoopRunRecord> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        if let Some(record) = run_log::task_records(&rimz::disk::paths::state_home(), name)
            .into_iter()
            .find(|record| record.at >= created)
        {
            return Ok(record);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("timed out waiting for {name}; the wake remains armed");
        }
        let sleep = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .map_or(POLL, |remaining| POLL.min(remaining));
        std::thread::sleep(sleep);
    }
}

pub(super) fn print_and_settle(ctx: &Ctx, record: &LoopRunRecord, json: bool) -> Result<()> {
    if let Some(message_id) = &record.message_id {
        ctx.store
            .cancel_message(message_id, &ctx.workspace.session_name, "joined inline")?;
    }
    if json {
        super::super::render::json(record)?;
    } else {
        write_record(&mut super::super::render::out(), record)?;
    }
    if !success_shaped(record) {
        std::process::exit(1);
    }
    Ok(())
}

fn write_record(out: &mut impl Write, record: &LoopRunRecord) -> std::io::Result<()> {
    write!(out, "{}: {}", record.task, record.result.label())?;
    if let Some(verdict) = &record.watch {
        write!(out, " · {}", verdict.label())?;
    } else if let Some(check) = &record.check {
        if check.timed_out {
            write!(out, " · timed out")?;
        } else if let Some(code) = check.code {
            write!(out, " · exit {code}")?;
        }
    }
    writeln!(out)?;
    if let Some(path) = record
        .check
        .as_ref()
        .and_then(|check| check.output_path.as_ref())
    {
        writeln!(out, "output: {}", path.display())?;
    }
    let output = record
        .check
        .as_ref()
        .map_or("", |check| check.output.as_str());
    if record.watch.is_some() && output.is_empty() {
        writeln!(out, "(no output)")?;
    } else if !output.is_empty() {
        writeln!(out, "{output}")?;
    }
    if let Some(error) = &record.error {
        writeln!(out, "{error}")?;
    }
    Ok(())
}

fn success_shaped(record: &LoopRunRecord) -> bool {
    if !matches!(
        record.result,
        LoopRunResult::Delivered | LoopRunResult::CheckSkipped
    ) {
        return false;
    }
    record
        .check
        .as_ref()
        .is_none_or(|check| !check.timed_out && check.code == Some(0))
}

#[cfg(test)]
mod tests {
    use rimz::agents::{AgentState, AgentStatus};
    use rimz::ids::WorkspaceId;
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageStatus};
    use rimz::{RuntimePaths, StatePaths, Store};

    #[test]
    fn inline_watch_result_prints_verdict_path_tail_and_error() {
        use super::*;
        use rimz::harness::schedule::run_log::{CheckRecord, LoopRunMode};
        use rimz::harness::schedule::signal::WatchVerdict;

        for output in ["", "last line"] {
            let mut record = LoopRunRecord::new(
                "wake-test",
                LoopRunResult::Delivered,
                LoopRunMode::Scheduled,
                0,
            );
            record.watch = Some(WatchVerdict::Exited {
                code: Some(3),
                elapsed_ms: 3_000,
            });
            record.check = Some(CheckRecord {
                code: Some(3),
                timed_out: false,
                output: output.to_owned(),
                output_path: Some("/tmp/wake.log".into()),
            });
            record.error = Some("delivery error".to_owned());
            let mut out = Vec::new();
            write_record(&mut out, &record).unwrap();
            let tail = if output.is_empty() {
                "(no output)"
            } else {
                output
            };
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!(
                    "wake-test: delivered · exit 3 after 3s\noutput: /tmp/wake.log\n{tail}\ndelivery error\n"
                )
            );
            assert!(!success_shaped(&record));
        }
    }

    #[test]
    fn inline_wait_cancels_open_message_but_preserves_sent_message() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        let store = Store::open(state, runtime).unwrap();
        let agent = AgentState::stub("claude", "provider-session", AgentStatus::Idle);
        let queued = MessageRecord::new(
            workspace_id.clone(),
            &agent,
            "queued".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let sent = MessageRecord::new(
            workspace_id,
            &agent,
            "sent".to_owned(),
            true,
            DeliveryGate::Done,
        );
        store.queue_message(&queued, "session").unwrap();
        store.queue_message(&sent, "session").unwrap();
        store
            .record_sent_batch(std::slice::from_ref(&sent), "session")
            .unwrap();

        store
            .cancel_message(&queued.message_id, "session", "joined inline")
            .unwrap();
        store
            .cancel_message(&sent.message_id, "session", "joined inline")
            .unwrap();

        let live = store.list_messages().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].message_id, sent.message_id);
        assert_eq!(live[0].status, MessageStatus::Sent);
        let history = store.list_message_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].message_id, queued.message_id);
        assert_eq!(history[0].status, MessageStatus::Canceled);
    }
}
