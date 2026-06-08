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
//!
//! The second guard pins the cadence gate above the walk: a produce within
//! `SPENDING_TTL` of the published stamp serves the cache verbatim and runs
//! zero transcript IO — no discovery, no stat, no parse.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use rimz::agents::spending::{SpendingDiskCache, compute_spending};
use rimz::agents::{AgentAdapter, ClaudeAdapter, PriceBook};

use crate::common::Env;

const HISTORY_LINES: usize = 30_000;

/// 2026-06-02T10:00:00Z — one day past the fixture stamp, so the trailing
/// windows hold the same verdict on any wall-clock day the suite runs.
const NOW_SECS: u64 = 1_780_394_400;

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
fn spending_walk_io_is_history_independent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = seed_history(dir.path());
    let files = [(claude_adapter(), file.clone())];
    let prices = PriceBook::default();

    let mut cache = SpendingDiskCache::default();
    let cold_start = Instant::now();
    let cold = compute_spending(&files, &mut cache, &prices, NOW_SECS);
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
    let warm = compute_spending(&files, &mut cache, &prices, NOW_SECS);
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

/// Within `SPENDING_TTL`, a produce serves the published `provider-spending.json`
/// verbatim and never touches the transcripts. Witnessed behaviorally through
/// the real CLI: the fixture transcripts are deleted between two produces — a
/// re-walk would find nothing and publish a re-stamped empty cache, so an
/// identical `value_tally` and a byte-identical cache file prove the walk was
/// skipped entirely (discovery, stats, cursor parse, and price book included).
#[test]
fn spending_walk_skips_entirely_within_ttl() {
    use rimz::agents::spending::{unix_secs_now, utc_date};

    let env = Env::new();

    // A Claude transcript fixture under an explicit CLAUDE_CONFIG_DIR, stamped
    // one hour ago so the trailing windows count it on any wall-clock day.
    let config_dir = env.project_root.join("claude-config");
    let proj_dir = config_dir.join("projects").join("p");
    std::fs::create_dir_all(&proj_dir).expect("mkdir projects");
    let secs = unix_secs_now() - 3_600;
    let tod = secs % 86_400;
    let iso = format!(
        "{}T{:02}:{:02}:{:02}.000Z",
        utc_date(secs),
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60
    );
    let transcript = proj_dir.join("chat.jsonl");
    std::fs::write(
        &transcript,
        format!(
            r#"{{"timestamp":"{iso}","costUSD":0.25,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":1200,"output_tokens":80}}}}}}"#,
        ) + "\n",
    )
    .expect("write transcript fixture");

    // An empty pane fixture bypasses `list-panes` (no mux in CI) while leaving
    // the spending walk — the path under test — fully live.
    let panes_path = env.project_root.join("panes.json");
    std::fs::write(&panes_path, b"[]").expect("write panes fixture");

    let snapshot = |label: &str| -> serde_json::Value {
        let output = env
            .rimz()
            .args([
                "sidebar",
                "snapshot",
                "--json",
                "--workspace-id",
                env.workspace_id.as_str(),
                "--session-name",
                "rimz-spend-ttl",
            ])
            .env("RIMZ_TEST_PANE_LIST", &panes_path)
            .env("CLAUDE_CONFIG_DIR", &config_dir)
            .output()
            .expect("spawn rimz sidebar snapshot");
        assert!(
            output.status.success(),
            "{label} snapshot failed:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
        serde_json::from_slice(&output.stdout).expect("snapshot json")
    };

    let cold = snapshot("cold");
    assert!(
        cold["value_tally"].is_object(),
        "the cold produce walks the fixture and counts its spend:\n{cold:#}"
    );
    let cache_path = env.runtime_paths().shared_provider_spending_path();
    let published = std::fs::read(&cache_path).expect("published provider-spending cache");

    // Remove the transcripts: from here, only the published cache can supply
    // the figure.
    std::fs::remove_dir_all(&config_dir).expect("delete transcript fixture");

    let warm = snapshot("warm");
    assert_eq!(
        warm["value_tally"], cold["value_tally"],
        "a produce within SPENDING_TTL serves the published walk verbatim"
    );
    assert_eq!(
        std::fs::read(&cache_path).expect("published provider-spending cache"),
        published,
        "the fresh path must not re-stamp or rewrite the published cache"
    );
}
