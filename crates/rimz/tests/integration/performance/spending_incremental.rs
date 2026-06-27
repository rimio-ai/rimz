//! History-independence of the spending pass over a long transcript.
//!
//! A long-horizon agent session leaves a transcript far bigger than any one
//! turn, and the spending walk runs on every producer data tick. The contract
//! (docs/internals/health/performance.md): per-file IO is O(delta) — an unchanged
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

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use rimz::agents::spending::{
    CachedEntry, FileCacheEntry, HeadlineSpec, SpendCursor, SpendingWalker, read_spending_cache,
    write_spending_cache,
};
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

fn file_cache_entry(path: &std::path::Path, entries: Vec<CachedEntry>) -> FileCacheEntry {
    let metadata = std::fs::metadata(path).expect("transcript metadata");
    let mtime_secs = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    FileCacheEntry {
        mtime_secs,
        len: metadata.len(),
        cursor: SpendCursor::default(),
        origin_path: None,
        entries,
        unknown_models: BTreeMap::new(),
    }
}

fn cached_entries(count: usize) -> Vec<CachedEntry> {
    (0..count)
        .map(|index| CachedEntry {
            ts_secs: NOW_SECS - 86_400 + u64::try_from(index % 3_600).expect("index fits u64"),
            cost_usd: 0.001,
            input: 1200,
            output: 80,
            cache_write: 0,
            cache_read: 800,
            message_id: Some(format!("msg-{index}")),
            request_id: Some(format!("req-{index}")),
            thread_id: Some(format!("thread-{index}")),
            is_sidechain: false,
            model: Some("claude-opus-4-8".to_owned()),
            origin_path: None,
            rolled: false,
        })
        .collect()
}

fn seed_spending_cache(
    dir: &std::path::Path,
    cache_path: &std::path::Path,
    entries_per_file: usize,
) -> Vec<(&'static dyn AgentAdapter, PathBuf)> {
    let mut files = Vec::new();
    let mut cache = read_spending_cache(cache_path);
    cache.files = HashMap::new();
    for file_index in 0..3 {
        let transcript = dir.join(format!("cached-{file_index}.jsonl"));
        std::fs::write(&transcript, b"").expect("transcript");
        let start = file_index * entries_per_file;
        let entries = cached_entries(entries_per_file)
            .into_iter()
            .enumerate()
            .map(|(offset, mut entry)| {
                let index = start + offset;
                entry.message_id = Some(format!("msg-{index}"));
                entry.request_id = Some(format!("req-{index}"));
                entry.thread_id = Some(format!("thread-{index}"));
                entry
            })
            .collect();
        cache.files.insert(
            transcript.to_string_lossy().into_owned(),
            file_cache_entry(&transcript, entries),
        );
        files.push((claude_adapter(), transcript));
    }
    write_spending_cache(cache_path, &cache);
    files
}

fn modified(path: &std::path::Path) -> SystemTime {
    std::fs::metadata(path)
        .expect("metadata")
        .modified()
        .expect("mtime")
}

#[test]
fn spending_walk_io_is_history_independent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = seed_history(dir.path());
    let files = [(claude_adapter(), file.clone())];
    let prices = PriceBook::default();
    let cache_path = dir.path().join("spending.json");
    let mut walker = SpendingWalker::new();

    let cold_start = Instant::now();
    let cold = walker.walk(
        &cache_path,
        &files,
        &prices,
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
    );
    let cold_elapsed = cold_start.elapsed();
    let cache = read_spending_cache(&cache_path);
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
    let warm = walker.walk(
        &cache_path,
        &files,
        &prices,
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
    );
    let warm_elapsed = warm_start.elapsed();

    // Work proxy: the cache grew by exactly the appended entry — the history
    // was never re-read, so nothing duplicated.
    let cache = read_spending_cache(&cache_path);
    let refreshed = cache.files.values().next().expect("cached file");
    assert_eq!(
        refreshed.entries.len(),
        HISTORY_LINES + 1,
        "suffix parse appends exactly the new entry"
    );
    assert!(
        warm.spending.total.year.usd > cold.spending.total.year.usd,
        "the appended turn is counted"
    );

    let cache_mtime = modified(&cache_path);
    let steady = walker.walk(
        &cache_path,
        &files,
        &prices,
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
    );
    assert!(
        !steady.stats.cache_written,
        "unchanged walk does not rewrite"
    );
    assert_eq!(
        modified(&cache_path),
        cache_mtime,
        "unchanged walk leaves the spending cache mtime untouched"
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

#[test]
fn spending_walk_warm_skips_parse_dedup_and_write() {
    fn second_walk_stats(entries_per_file: usize) -> rimz::agents::spending::WalkStats {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_path = dir.path().join("spending.json");
        let files = seed_spending_cache(dir.path(), &cache_path, entries_per_file);
        let prices = PriceBook::default();
        let mut walker = SpendingWalker::new();

        let first = walker.walk(
            &cache_path,
            &files,
            &prices,
            NOW_SECS,
            &Default::default(),
            None,
            &HeadlineSpec::default(),
        );
        assert_eq!(first.stats.dedup_passes, 1);
        let cache_mtime = modified(&cache_path);

        let second = walker.walk(
            &cache_path,
            &files,
            &prices,
            NOW_SECS,
            &Default::default(),
            None,
            &HeadlineSpec::default(),
        );
        assert_eq!(
            modified(&cache_path),
            cache_mtime,
            "memo hit leaves the shared spending cache untouched"
        );
        second.stats
    }

    let baseline = second_walk_stats(1_000);
    assert_eq!(baseline.dedup_passes, 0);
    assert!(!baseline.cache_parsed);
    assert!(!baseline.cache_written);

    assert_eq!(
        second_walk_stats(10_000),
        baseline,
        "unchanged warm work is independent of retained-entry count"
    );
}

#[test]
fn spending_walk_warm_keeps_trailing_windows_fresh() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache_path = dir.path().join("spending.json");
    let transcript = dir.path().join("edge.jsonl");
    std::fs::write(&transcript, b"").expect("transcript");
    let mut cache = read_spending_cache(&cache_path);
    cache.files = HashMap::new();
    let cost_usd = 2.5;
    cache.files.insert(
        transcript.to_string_lossy().into_owned(),
        file_cache_entry(
            &transcript,
            vec![CachedEntry {
                ts_secs: NOW_SECS - (7 * 86_400) + 1,
                cost_usd,
                input: 1200,
                output: 80,
                cache_write: 0,
                cache_read: 800,
                message_id: Some("msg-edge".to_owned()),
                request_id: Some("req-edge".to_owned()),
                thread_id: Some("thread-edge".to_owned()),
                is_sidechain: false,
                model: Some("claude-opus-4-8".to_owned()),
                origin_path: None,
                rolled: false,
            }],
        ),
    );
    write_spending_cache(&cache_path, &cache);
    let files = vec![(claude_adapter(), transcript)];
    let prices = PriceBook::default();
    let mut walker = SpendingWalker::new();

    let first = walker.walk(
        &cache_path,
        &files,
        &prices,
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
    );
    let second = walker.walk(
        &cache_path,
        &files,
        &prices,
        NOW_SECS + 2,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
    );

    assert_eq!(first.stats.dedup_passes, 1);
    assert_eq!(second.stats.dedup_passes, 0);
    assert!((first.spending.total.week.usd - cost_usd).abs() < 1e-9);
    assert!(
        second.spending.total.week.usd.abs() < 1e-9,
        "warm memo must not freeze the rolling week window"
    );
    assert!(
        (second.spending.total.year.usd - cost_usd).abs() < 1e-9,
        "the entry still counts in wider windows"
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
