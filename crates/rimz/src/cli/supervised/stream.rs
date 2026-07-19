//! Supervised and attached run streaming effects.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::output::StreamSink;
use rimz::agents::AgentDefinition;
use rimz::agents::transcript::TranscriptCursor;
use rimz::harness::run::RunRecord;
use rimz::harness::run_wake::RunWaiter;

pub(crate) fn stream_blocking_run(
    waiter: &RunWaiter,
    store: &rimz::Store,
    adapter: &AgentDefinition,
    timeout: Option<Duration>,
    output: (&mut TranscriptCursor, &mut StreamSink<'_>),
) -> Result<RunRecord> {
    let (cursor, sink) = output;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run stream runtime")?;
    runtime.block_on(async {
        let mut observer =
            |record: &RunRecord| emit_stream_updates(store, adapter, cursor, sink, record);
        let record = waiter
            .wait_terminal(store, timeout, Some(&mut observer))
            .await?;
        sink.end_status(record.status, record.last_message.as_deref())?;
        Ok(record)
    })
}

pub(crate) fn stream_attached_run(
    store: &rimz::Store,
    run_id: &rimz::RunId,
    adapter: &AgentDefinition,
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

fn emit_stream_updates(
    store: &rimz::Store,
    adapter: &AgentDefinition,
    cursor: &mut TranscriptCursor,
    sink: &mut StreamSink<'_>,
    record: &RunRecord,
) -> Result<()> {
    for text in cursor.messages(
        record.transcript_path.as_deref(),
        record.agent_id.as_ref(),
        adapter,
    ) {
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
