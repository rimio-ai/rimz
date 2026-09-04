use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use jiff::Timestamp;

use rimz::harness::schedule::run_log::{self, LoopRunRecord, LoopRunResult};

use super::*;

const POLL: Duration = Duration::from_millis(500);

pub(super) fn for_record(
    _ctx: &Ctx,
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
        let mut out = super::super::render::out();
        write!(out, "{}: {}", record.task, record.result.label())?;
        if let Some(check) = &record.check {
            if check.timed_out {
                write!(out, " · timed out")?;
            } else if let Some(code) = check.code {
                write!(out, " · exit {code}")?;
            }
        }
        writeln!(out)?;
        if let Some(check) = &record.check
            && !check.output.is_empty()
        {
            writeln!(out, "{}", check.output)?;
        }
        if let Some(error) = &record.error {
            writeln!(out, "{error}")?;
        }
    }
    if !success_shaped(record) {
        std::process::exit(1);
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
