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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::SystemTime;

use rimz::agents::spending::{
    CachedEntry, FileCacheEntry, HeadlineSpec, SilentWalk, SpendCursor, SpendingWalker,
    WalkRequest, read_spending_cache, write_spending_cache,
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
            dedup_key: None,
            thread_id: Some(format!("thread-{index}")),
            is_sidechain: false,
            has_speed: false,
            model: Some("claude-opus-4-8".to_owned()),
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

macro_rules! walk_spending {
    ($walker:expr, $method:ident, $cache_path:expr, $files:expr, $prices:expr, $now_secs:expr) => {{
        let origin_overrides = HashMap::new();
        let automation_files = HashSet::new();
        let live_excluded = BTreeSet::new();
        let spec = HeadlineSpec::default();
        let req = WalkRequest {
            files: $files,
            prices: $prices,
            now_secs: $now_secs,
            origin_overrides: &origin_overrides,
            automation_files: &automation_files,
            automation_signature: 0,
            scope: None,
            live_excluded: &live_excluded,
            spec: &spec,
        };
        $walker.$method($cache_path, &req, &mut SilentWalk)
    }};
}

#[test]
fn spending_walk_io_is_history_independent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = seed_history(dir.path());
    let files = [(claude_adapter(), file.clone())];
    let prices = PriceBook::default();
    let cache_path = dir.path().join("spending.json");
    let mut walker = SpendingWalker::new();

    let cold_len = std::fs::metadata(&file).expect("seed metadata").len();
    let cold = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS);
    assert_eq!(cold.stats.parse_jobs, 1, "cold walk parses the file once");
    assert_eq!(
        cold.stats.parse_bytes, cold_len,
        "cold walk parses the whole transcript"
    );
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
    let prior_len = std::fs::metadata(&file).expect("pre-append metadata").len();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .expect("append");
    writeln!(f, "{}", claude_line(HISTORY_LINES)).expect("append line");
    drop(f);
    let appended_len = std::fs::metadata(&file)
        .expect("post-append metadata")
        .len()
        - prior_len;

    let warm = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS);
    assert_eq!(warm.stats.parse_jobs, 1, "warm walk parses one suffix");
    assert_eq!(
        warm.stats.parse_bytes, appended_len,
        "warm walk parses only the appended turn"
    );

    assert!(
        !warm.stats.cache_written,
        "small warm suffixes stay in the in-memory walker until the persist gate"
    );
    assert!(
        warm.spending.total.year.usd > cold.spending.total.year.usd,
        "the appended turn is counted"
    );

    let cache_mtime = modified(&cache_path);
    let steady = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS);
    assert!(
        !steady.stats.cache_written,
        "unchanged walk does not rewrite"
    );
    assert_eq!(
        modified(&cache_path),
        cache_mtime,
        "unchanged walk leaves the spending cache mtime untouched"
    );
    let due = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS + 301);
    assert!(
        due.stats.cache_written,
        "post-interval walk persists the held suffix cursor"
    );

    // Work proxy: once the persist gate opens, the cache contains exactly the
    // appended entry — the history was never re-read, so nothing duplicated.
    let cache = read_spending_cache(&cache_path);
    let refreshed = cache.files.values().next().expect("cached file");
    assert_eq!(
        refreshed.entries.len(),
        HISTORY_LINES + 1,
        "suffix parse appends exactly the new entry"
    );

    // Resource shape: the warm pass keeps transcript IO to the appended turn.
    // The in-memory fold still walks the retained cache to publish trailing
    // totals, so wall-clock ratios are a noisy proxy for the invariant.
    assert!(
        warm.stats.parse_bytes < cold.stats.parse_bytes / 100,
        "warm transcript IO must stay history-independent"
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

        let first = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS);
        assert_eq!(first.stats.dedup_passes, 1);
        let cache_mtime = modified(&cache_path);

        let second = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS);
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
                dedup_key: None,
                thread_id: Some("thread-edge".to_owned()),
                is_sidechain: false,
                has_speed: false,
                model: Some("claude-opus-4-8".to_owned()),
                rolled: false,
            }],
        ),
    );
    write_spending_cache(&cache_path, &cache);
    let files = vec![(claude_adapter(), transcript)];
    let prices = PriceBook::default();
    let mut walker = SpendingWalker::new();

    let first = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS);
    let second = walk_spending!(walker, walk, &cache_path, &files, &prices, NOW_SECS + 2);

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

#[test]
fn spending_walk_local_seeds_from_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache_path = dir.path().join("spending.json");
    let transcript = dir.path().join("local.jsonl");
    std::fs::write(&transcript, claude_line(999)).expect("transcript");
    let cached_cost = 1.25;
    let actual_cost = 0.001;
    let mut cache = read_spending_cache(&cache_path);
    cache.files = HashMap::new();
    cache.files.insert(
        transcript.to_string_lossy().into_owned(),
        file_cache_entry(
            &transcript,
            vec![CachedEntry {
                cost_usd: cached_cost,
                ..cached_entries(1).remove(0)
            }],
        ),
    );
    write_spending_cache(&cache_path, &cache);
    let files = vec![(claude_adapter(), transcript.clone())];
    let prices = PriceBook::default();
    let mut local_walker = SpendingWalker::new();

    let local = walk_spending!(
        local_walker,
        walk_local,
        &cache_path,
        &files,
        &prices,
        NOW_SECS
    );

    assert!(local.stats.cache_parsed);
    assert!(
        !local.stats.cache_written,
        "local fallback reads the cursor cache but never writes it"
    );
    assert!(
        (local.spending.total.year.usd - cached_cost).abs() < 1e-9,
        "matching on-disk mtime/len must serve the seeded cached entry"
    );

    let cold_cache_path = dir.path().join("cold-spending.json");
    let mut cold_walker = SpendingWalker::new();
    let cold = walk_spending!(
        cold_walker,
        walk,
        &cold_cache_path,
        &files,
        &prices,
        NOW_SECS
    );
    assert!(
        (cold.spending.total.year.usd - actual_cost).abs() < 1e-9,
        "an unseeded cold walk parses the transcript content"
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
