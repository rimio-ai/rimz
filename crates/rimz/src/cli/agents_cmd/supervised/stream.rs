use std::time::{Duration, Instant};

use anyhow::Result;

use super::output::StreamSink;
use rimz::agents::AgentAdapter;
use rimz::agents::transcript::TranscriptCursor;
use rimz::harness::run::RunRecord;

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

pub(crate) fn emit_stream_updates(
    store: &rimz::Store,
    adapter: &dyn AgentAdapter,
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
