//! History-independence of the spending pass over a long transcript.
//!
//! A long-horizon agent session leaves a transcript far bigger than any one
//! turn, and the spending walk runs on every producer data tick. The contract
//! (docs/internals/performance.md): per-file IO is O(delta) — an unchanged
//! file is one stat, a grown file reads only its appended suffix from the
//! stored cursor — so a turn's cost never scales with the session's history.
//!
//! Proven two ways: the warm pass lands within a small multiple of an
//! append-only floor (wall-clock, generous margin), and the refreshed cache
//! grows by exactly the appended entry instead of a duplicated re-parse.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use rimz::agents::spending::{SpendingDiskCache, compute_spending};
use rimz::agents::{AgentAdapter, ClaudeAdapter, PriceBook};

const HISTORY_LINES: usize = 30_000;

fn claude_adapter() -> &'static dyn AgentAdapter {
    &ClaudeAdapter
}

fn claude_line(i: usize) -> String {
    format!(
        r#"{{"timestamp":"2026-06-01T10:00:00.000Z","costUSD":0.001,"requestId":"req-{i}","message":{{"id":"msg-{i}","usage":{{"input_tokens":1200,"output_tokens":80,"cache_read_input_tokens":800}}}}}}"#
    )
}

fn seed_history(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("chat.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&path).expect("create transcript"));
    for i in 0..HISTORY_LINES {
        writeln!(f, "{}", claude_line(i)).expect("seed line");
    }
    path
}

#[test]
fn spending_recompute_is_history_independent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = seed_history(dir.path());
    let files = [(claude_adapter(), file.clone())];
    let prices = PriceBook::default();

    let mut cache = SpendingDiskCache::default();
    let cold_start = Instant::now();
    let cold = compute_spending(&files, &mut cache, &prices);
    let cold_elapsed = cold_start.elapsed();
    let baseline_entries = cache
        .files
        .values()
        .next()
        .expect("cached file")
        .entries
        .len();
    assert_eq!(
        baseline_entries, HISTORY_LINES,
        "cold parse covers the history"
    );

    // One turn lands: a single appended line.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .expect("append");
    writeln!(f, "{}", claude_line(HISTORY_LINES)).expect("append line");
    drop(f);

    let warm_start = Instant::now();
    let warm = compute_spending(&files, &mut cache, &prices);
    let warm_elapsed = warm_start.elapsed();

    // Work proxy: the cache grew by exactly the appended entry — the history
    // was never re-read, so nothing duplicated.
    let refreshed = cache.files.values().next().expect("cached file");
    assert_eq!(
        refreshed.entries.len(),
        HISTORY_LINES + 1,
        "suffix parse appends exactly the new entry"
    );
    assert!(
        warm.total.year.usd > cold.total.year.usd,
        "the appended turn is counted"
    );

    // Wall-clock: the warm pass skips the parse entirely (one stat + a
    // one-line read + the in-memory fold), so even a generous bound on the
    // cold parse holds with margin. 3x guards against a re-parse regression
    // (which would land at ~1x) without flaking on slow CI.
    assert!(
        warm_elapsed * 3 < cold_elapsed,
        "warm recompute {warm_elapsed:?} must stay well under cold parse {cold_elapsed:?}"
    );
}
