use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::output;
use rimz::agents::AgentAdapter;
use rimz::harness::run::{RunLiveStatus, RunRecord};
use rimz::harness::run_wake::{self, ExpectedRunFrame, RunWakeOutcome};

pub(crate) fn stream_blocking_run(
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    store: &rimz::Store,
    run_id: &rimz::RunId,
    adapter: &dyn AgentAdapter,
    timeout: Option<Duration>,
) -> Result<RunRecord> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run stream runtime")?;
    runtime.block_on(async {
        let sock = run_wake::adopt(sock).context("adopting run socket")?;
        let deadline = timeout.map(|duration| Instant::now() + duration);
        let mut cursor = TranscriptCursor::new(true);
        let mut last_live = None;
        loop {
            let record = rimz::harness::run::load(store.paths(), run_id)?;
            emit_stream_updates(store, adapter, &mut cursor, &mut last_live, &record)?;
            if record.status.is_terminal() {
                emit_stream_end(&record)?;
                return Ok(record);
            }
            let Some(wait) = next_stream_wait(deadline) else {
                let timed_out = rimz::harness::run::timeout(store.paths(), run_id)?;
                emit_stream_updates(store, adapter, &mut cursor, &mut last_live, &timed_out)?;
                emit_stream_end(&timed_out)?;
                return Ok(timed_out);
            };
            match run_wake::wait_for_run_completion(&sock, &expected, Some(wait))
                .await
                .context("waiting for run stream tick")?
            {
                RunWakeOutcome::Completed(_status) => {
                    let record = rimz::harness::run::load(store.paths(), run_id)?;
                    emit_stream_updates(store, adapter, &mut cursor, &mut last_live, &record)?;
                    emit_stream_end(&record)?;
                    return Ok(record);
                }
                RunWakeOutcome::Neutral => {}
            }
        }
    })
}

pub(crate) fn stream_attached_run(
    store: &rimz::Store,
    run_id: &rimz::RunId,
    adapter: &dyn AgentAdapter,
    from_start: bool,
    timeout: Option<Duration>,
) -> Result<Option<RunRecord>> {
    let mut cursor = TranscriptCursor::new(from_start);
    let mut last_live = None;
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let record = rimz::harness::run::load(store.paths(), run_id)?;
        emit_stream_updates(store, adapter, &mut cursor, &mut last_live, &record)?;
        if record.status.is_terminal() {
            emit_stream_end(&record)?;
            return Ok(Some(record));
        }
        if reached_deadline(deadline) {
            return Ok(None);
        }
        std::thread::sleep(next_attached_stream_sleep(deadline));
    }
}

fn reached_deadline(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn next_attached_stream_sleep(deadline: Option<Instant>) -> Duration {
    const ATTACHED_STREAM_TICK: Duration = Duration::from_millis(500);
    let Some(deadline) = deadline else {
        return ATTACHED_STREAM_TICK;
    };
    let now = Instant::now();
    if now >= deadline {
        Duration::ZERO
    } else {
        (deadline - now).min(ATTACHED_STREAM_TICK)
    }
}

fn next_stream_wait(deadline: Option<Instant>) -> Option<Duration> {
    const STREAM_TICK: Duration = Duration::from_secs(1);
    let deadline = match deadline {
        Some(deadline) => deadline,
        None => return Some(STREAM_TICK),
    };
    let now = Instant::now();
    if now >= deadline {
        return None;
    }
    Some((deadline - now).min(STREAM_TICK))
}

#[derive(Debug)]
pub(super) struct TranscriptCursor {
    path: Option<String>,
    offset: u64,
    skip_existing_on_first_path: bool,
}

impl TranscriptCursor {
    pub(super) fn new(from_start: bool) -> Self {
        Self {
            path: None,
            offset: 0,
            skip_existing_on_first_path: !from_start,
        }
    }

    pub(super) fn messages(
        &mut self,
        record: &RunRecord,
        adapter: &dyn AgentAdapter,
    ) -> Vec<String> {
        let Some(path) = record.transcript_path.as_deref() else {
            return Vec::new();
        };
        if self.path.as_deref() != Some(path) {
            self.offset = if self.skip_existing_on_first_path {
                std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
            } else {
                0
            };
            self.path = Some(path.to_owned());
            self.skip_existing_on_first_path = false;
        }
        if std::fs::metadata(path)
            .map(|meta| meta.len() < self.offset)
            .unwrap_or(false)
        {
            self.offset = 0;
        }
        let Some((bytes, next)) = rimz::agents::read_transcript_lines(Path::new(path), self.offset)
        else {
            return Vec::new();
        };
        self.offset = next;
        let text = String::from_utf8_lossy(&bytes);
        adapter.stream_assistant_messages(&text)
    }
}

fn emit_stream_updates(
    store: &rimz::Store,
    adapter: &dyn AgentAdapter,
    cursor: &mut TranscriptCursor,
    last_live: &mut Option<RunLiveStatus>,
    record: &RunRecord,
) -> Result<()> {
    for text in cursor.messages(record, adapter) {
        emit_ndjson(&output::RunStreamEvent::Message { text })?;
    }
    if let Some(live) = store
        .snapshot_cached()
        .ok()
        .and_then(|snapshot| rimz::harness::run::live_status(record, &snapshot))
        && last_live.as_ref() != Some(&live)
    {
        emit_ndjson(&output::RunStreamEvent::Status { live: live.clone() })?;
        *last_live = Some(live);
    }
    Ok(())
}

fn emit_stream_end(record: &RunRecord) -> Result<()> {
    emit_ndjson(&output::RunStreamEvent::End {
        status: record.status,
        last_message: record.last_message.clone(),
    })
}

fn emit_ndjson(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}
