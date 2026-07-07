use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::output::StreamSink;
use rimz::agents::AgentAdapter;
use rimz::harness::run::RunRecord;
use rimz::harness::run_wake::{self, ExpectedRunFrame, RunWakeOutcome};

pub(crate) fn stream_blocking_run(
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    store: &rimz::Store,
    run_id: &rimz::RunId,
    adapter: &dyn AgentAdapter,
    timeout: Option<Duration>,
    interrupt: &AtomicBool,
) -> Result<RunRecord> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run stream runtime")?;
    let mut stdout = std::io::stdout().lock();
    let mut sink = StreamSink::ndjson(&mut stdout);
    runtime.block_on(async {
        let sock = run_wake::adopt(sock).context("adopting run socket")?;
        let deadline = timeout.map(|duration| Instant::now() + duration);
        let mut cursor = TranscriptCursor::new(true);
        loop {
            let record = rimz::harness::run::load(store.paths(), run_id)?;
            emit_stream_updates(store, adapter, &mut cursor, &mut sink, &record)?;
            if record.status.is_terminal() {
                sink.end_record(&record)?;
                return Ok(record);
            }
            if interrupt.load(Ordering::SeqCst) {
                let (canceled, _wrote) = rimz::harness::run::cancel(store.paths(), run_id)?;
                sink.end_record(&canceled)?;
                return Ok(canceled);
            }
            let Some(wait) = next_stream_wait(deadline) else {
                let timed_out = rimz::harness::run::timeout(store.paths(), run_id)?;
                emit_stream_updates(store, adapter, &mut cursor, &mut sink, &timed_out)?;
                sink.end_record(&timed_out)?;
                return Ok(timed_out);
            };
            match run_wake::wait_for_run_completion(&sock, &expected, Some(wait))
                .await
                .context("waiting for run stream tick")?
            {
                RunWakeOutcome::Completed(_status) => {
                    let record = rimz::harness::run::load(store.paths(), run_id)?;
                    emit_stream_updates(store, adapter, &mut cursor, &mut sink, &record)?;
                    sink.end_record(&record)?;
                    return Ok(record);
                }
                RunWakeOutcome::Neutral => {
                    if interrupt.load(Ordering::SeqCst) {
                        let (canceled, _wrote) = rimz::harness::run::cancel(store.paths(), run_id)?;
                        sink.end_record(&canceled)?;
                        return Ok(canceled);
                    }
                }
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
    sink: &mut StreamSink<'_>,
) -> Result<Option<RunRecord>> {
    let mut cursor = TranscriptCursor::new(from_start);
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let record = rimz::harness::run::load(store.paths(), run_id)?;
        emit_stream_updates(store, adapter, &mut cursor, sink, &record)?;
        if record.status.is_terminal() {
            sink.end_record(&record)?;
            return Ok(Some(record));
        }
        if reached_deadline(deadline) {
            if sink.is_text() {
                sink.timeout()?;
            }
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
    const STREAM_TICK: Duration = Duration::from_millis(250);
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
pub(crate) struct TranscriptCursor {
    path: Option<String>,
    offset: u64,
    skip_existing_on_first_path: bool,
}

impl TranscriptCursor {
    pub(crate) fn new(from_start: bool) -> Self {
        Self {
            path: None,
            offset: 0,
            skip_existing_on_first_path: !from_start,
        }
    }

    pub(crate) fn messages(
        &mut self,
        transcript_path: Option<&str>,
        adapter: &dyn AgentAdapter,
    ) -> Vec<String> {
        let Some(path) = transcript_path else {
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
    sink: &mut StreamSink<'_>,
    record: &RunRecord,
) -> Result<()> {
    for text in cursor.messages(record.transcript_path.as_deref(), adapter) {
        sink.message(text)?;
    }
    if let Some(live) = store
        .snapshot_cached()
        .ok()
        .and_then(|snapshot| rimz::harness::run::live_status(record, &snapshot))
    {
        sink.status(live)?;
    }
    Ok(())
}
