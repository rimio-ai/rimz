use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use tempfile::TempDir;

const NOW_SECS: u64 = 1_750_000_000;

fn claude_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::ClaudeAdapter
}

fn codex_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::CodexAdapter
}

fn compute_total(files: &[PathBuf], cache: &mut SpendingDiskCache) -> SpendTally {
    let tagged: Vec<(&'static dyn AgentAdapter, PathBuf)> = files
        .iter()
        .map(|file| (claude_adapter(), file.clone()))
        .collect();
    compute_spending(&tagged, cache, &PriceBook::default(), NOW_SECS).total
}

fn iso_at(secs: u64) -> String {
    let date = utc_date(secs);
    let tod = secs % 86_400;
    format!(
        "{date}T{:02}:{:02}:{:02}.000Z",
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60
    )
}

fn claude_line_ts(ts: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
    )
}

fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    claude_line_ts(&format!("{date}T10:00:00.000Z"), cost, msg_id, req_id)
}

fn claude_line_ago(secs_ago: u64, cost: f64, msg_id: &str, req_id: &str) -> String {
    claude_line_ts(
        &iso_at(NOW_SECS.saturating_sub(secs_ago)),
        cost,
        msg_id,
        req_id,
    )
}

fn claude_sidechain_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","isSidechain":true,"message":{{"id":"{msg_id}","usage":{{"input_tokens":50000,"output_tokens":5}}}}}}"#
    )
}

fn write_jsonl(dir: &Path, filename: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(filename);
    let mut f = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    path
}

fn append_line(path: &Path, line: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn codex_total_line(date: &str, input: u64, output: u64) -> String {
    format!(
        r#"{{"type":"event_msg","timestamp":"{date}T10:00:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}}}"#
    )
}

fn codex_token_line(date: &str, input: u64, cached: u64, output: u64) -> String {
    format!(
        r#"{{"type":"event_msg","timestamp":"{date}T10:00:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
    )
}

fn write_codex(dir: &Path, lines: &[&str]) -> PathBuf {
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    write_jsonl(&sessions, "sess.jsonl", lines)
}

fn gpt4o_book() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{"gpt-4o": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                           "cache_read_input_token_cost": 1e-7}}"#,
    )
}

#[test]
fn cache_hit_skips_io_and_version_gate_discards_old_entries() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );

    let mut cache = SpendingDiskCache::default();
    let first = compute_total(std::slice::from_ref(&file), &mut cache);
    assert_eq!(first.today.tokens, 15);
    assert!(cache.dirty);

    cache.dirty = false;
    let second = compute_total(&[file], &mut cache);
    assert_eq!(second.today.usd, first.today.usd);
    assert!(!cache.dirty, "unchanged files should be served from cache");

    let path = dir.path().join("spending.json");
    let stale = SpendingDiskCache {
        version: 0,
        files: HashMap::from([(
            "/old/chat.jsonl".to_string(),
            FileCacheEntry {
                mtime_secs: 123,
                len: 0,
                cursor: SpendCursor::default(),
                entries: vec![CachedEntry {
                    ts_secs: NOW_SECS,
                    cost_usd: 9.0,
                    input: 0,
                    output: 0,
                    cache_write: 0,
                    cache_read: 0,
                    message_id: Some("msg-old".to_string()),
                    request_id: Some("req-old".to_string()),
                    is_sidechain: false,
                }],
                unknown_models: BTreeMap::new(),
            },
        )]),
        dirty: false,
    };
    write_spending_cache(&path, &stale);

    let healed = read_spending_cache(&path);
    assert_eq!(healed.version, SPENDING_CACHE_VERSION);
    assert!(healed.files.is_empty());
}

#[test]
fn token_split_and_session_counts_populate_windows() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let session = dir.path().join("sess-1");
    std::fs::create_dir_all(session.join("subagents")).unwrap();
    let main_line = format!(
        r#"{{"timestamp":"{today}T10:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":12000,"output_tokens":64000,"cache_creation_input_tokens":12000,"cache_read_input_tokens":68000}}}}}}"#
    );
    let sub_line = format!(
        r#"{{"timestamp":"{today}T10:01:00.000Z","costUSD":0.1,"requestId":"req-2","isSidechain":true,"message":{{"id":"msg-2","usage":{{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}}}}"#
    );
    let main = write_jsonl(&session, "chat.jsonl", &[&main_line]);
    let subfile = write_jsonl(&session.join("subagents"), "worker.jsonl", &[&sub_line]);

    let mut cache = SpendingDiskCache::default();
    let total = compute_total(&[main, subfile], &mut cache);

    assert_eq!(total.today.input, 25_000);
    assert_eq!(total.today.output, 64_500);
    assert_eq!(total.today.tokens, 89_500);
    assert_eq!(total.today.cache_write, 12_000);
    assert_eq!(total.today.cache_read, 70_000);
    assert_eq!(total.today.sessions, 1);
    assert_eq!(total.year.sessions, 1);
}

#[test]
fn trailing_windows_bucket_by_age() {
    let dir = TempDir::new().unwrap();
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            &claude_line_ago(2 * HOUR, 1.0, "msg-1", "req-1"),
            &claude_line_ago(3 * DAY, 0.5, "msg-2", "req-2"),
            &claude_line_ago(20 * DAY, 0.25, "msg-3", "req-3"),
            &claude_line_ago(100 * DAY, 0.1, "msg-4", "req-4"),
            &claude_line_ago(400 * DAY, 9.0, "msg-5", "req-5"),
        ],
    );

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[file], &mut cache);

    assert_eq!(totals.today.tokens, 15);
    assert!((totals.today.usd - 1.0).abs() < 1e-9);
    assert_eq!(totals.week.tokens, 30);
    assert!((totals.week.usd - 1.5).abs() < 1e-9);
    assert_eq!(totals.month.tokens, 45);
    assert!((totals.month.usd - 1.75).abs() < 1e-9);
    assert_eq!(totals.year.tokens, 60);
    assert!((totals.year.usd - 1.85).abs() < 1e-9);
}

#[test]
fn claude_dedup_keeps_one_exact_entry_and_suppresses_sidechain_replays() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let line = claude_line(&today, 1.0, "msg-a", "req-a");
    let file1 = write_jsonl(dir.path(), "session1.jsonl", &[&line, &line]);
    let file2 = write_jsonl(dir.path(), "session2.jsonl", &[&line]);
    let mut cache = SpendingDiskCache::default();
    let exact = compute_total(&[file1, file2], &mut cache);
    assert!((exact.today.usd - 1.0).abs() < 1e-9);
    assert_eq!(exact.today.tokens, 15);

    let main = write_jsonl(
        dir.path(),
        "main.jsonl",
        &[&claude_line(&today, 0.05, "msg-parent", "req-parent")],
    );
    let replay = write_jsonl(
        dir.path(),
        "replay.jsonl",
        &[&claude_sidechain_line(
            &today,
            5.00,
            "msg-parent",
            "req-sidechain",
        )],
    );
    let mut cache = SpendingDiskCache::default();
    let deduped = compute_total(&[main, replay], &mut cache);
    assert!((deduped.today.usd - 0.05).abs() < 1e-9);
    assert_eq!(deduped.today.tokens, 15);

    let lone_sidechain = write_jsonl(
        dir.path(),
        "sidechain-only.jsonl",
        &[&claude_sidechain_line(&today, 0.20, "msg-x", "req-x")],
    );
    let mut cache = SpendingDiskCache::default();
    let kept = compute_total(&[lone_sidechain], &mut cache);
    assert!((kept.today.usd - 0.20).abs() < 1e-9);
    assert_eq!(kept.today.tokens, 50_005);
}

#[test]
fn file_change_cache_paths_parse_suffix_or_reparse_cold() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);

    let suffix_file = write_jsonl(
        dir.path(),
        "suffix.jsonl",
        &[&claude_line(&today, 1.0, "msg-1", "req-1")],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(claude_adapter(), suffix_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((first.total.today.usd - 1.0).abs() < 1e-9);
    let prefix_len = std::fs::metadata(&suffix_file).unwrap().len() as usize;
    {
        use std::io::{Seek as _, SeekFrom};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&suffix_file)
            .unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![b'x'; prefix_len - 1]).unwrap();
    }
    append_line(&suffix_file, &claude_line(&today, 0.25, "msg-2", "req-2"));
    let suffix = compute_spending(
        &[(claude_adapter(), suffix_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((suffix.total.today.usd - 1.25).abs() < 1e-9);

    let line_a = claude_line(&today, 1.0, "msg-a", "req-a");
    let line_b = claude_line(&today, 0.5, "msg-b", "req-b");
    let truncated_file = write_jsonl(dir.path(), "truncated.jsonl", &[&line_a, &line_b]);
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), truncated_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    write_jsonl(dir.path(), "truncated.jsonl", &[&line_a]);
    let truncated = compute_spending(
        &[(claude_adapter(), truncated_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((truncated.total.today.usd - 1.0).abs() < 1e-9);

    let rewrite_file = write_jsonl(
        dir.path(),
        "rewrite.jsonl",
        &[&claude_line(&today, 1.0, "msg-r", "req-r")],
    );
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), rewrite_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    write_jsonl(
        dir.path(),
        "rewrite.jsonl",
        &[&claude_line(&today, 3.0, "msg-r", "req-r")],
    );
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&rewrite_file)
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    let rewritten = compute_spending(
        &[(claude_adapter(), rewrite_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((rewritten.total.today.usd - 3.0).abs() < 1e-9);
}

#[test]
fn codex_pricing_resume_state_and_provider_breakdown_stay_intact() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let resumable = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_total_line(&today, 1000, 500),
        ],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(codex_adapter(), resumable.clone())],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(first.total.today.input, 1000);
    assert_eq!(first.total.today.output, 500);

    append_line(&resumable, &codex_total_line(&today, 1600, 800));
    let second = compute_spending(
        &[(codex_adapter(), resumable)],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(second.total.today.input, 1600);
    assert_eq!(second.total.today.output, 800);

    let claude_file = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );
    let codex_file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_token_line(&today, 1000, 400, 500),
        ],
    );
    let mut cache = SpendingDiskCache::default();
    let spending = compute_spending(
        &[
            (claude_adapter(), claude_file),
            (codex_adapter(), codex_file),
        ],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    let codex = &spending.by_provider["codex"];
    assert!((codex.today.usd - 0.00164).abs() < 1e-9);
    assert_eq!(codex.today.input, 600);
    assert_eq!(codex.today.output, 500);
    assert_eq!(codex.today.cache_read, 400);
    assert!((spending.by_provider["claude"].today.usd - 0.5).abs() < 1e-9);
    assert!((spending.total.today.usd - 0.50164).abs() < 1e-9);
}

#[test]
fn unknown_model_chase_records_and_heals_active_files() {
    assert!(!is_priceable_model_name("<synthetic>"));
    assert!(!is_priceable_model_name("   "));
    assert!(is_priceable_model_name("claude-new"));

    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let model = "new-claude-pricing-test-model";
    let line = format!(
        r#"{{"timestamp":"{today}T10:00:00.000Z","requestId":"req-1","message":{{"id":"msg-1","model":"{model}","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
    );
    let file = write_jsonl(dir.path(), "chat.jsonl", &[&line]);
    let mut cache = SpendingDiskCache::default();

    let first = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &PriceBook::from_litellm_json("{}"),
        NOW_SECS,
    );
    assert!(first.total.is_zero());
    assert_eq!(
        recorded_unknown_models(&[(claude_adapter(), file.clone())], &cache, NOW_SECS),
        BTreeSet::from([model.to_owned()])
    );

    let priced = PriceBook::from_litellm_json(&format!(
        r#"{{"{model}": {{"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}}}"#
    ));
    let cache_key = file.to_string_lossy().into_owned();
    cache.dirty = false;
    let healed = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &priced,
        NOW_SECS,
    );
    assert!((healed.total.today.usd - 0.0002).abs() < 1e-12);
    assert!(cache.files[&cache_key].unknown_models.is_empty());
    assert!(cache.dirty);

    let stale = PathBuf::from("/tmp/stale.jsonl");
    cache.files.insert(
        stale.to_string_lossy().into_owned(),
        FileCacheEntry {
            mtime_secs: 0,
            len: 0,
            cursor: SpendCursor::default(),
            entries: Vec::new(),
            unknown_models: BTreeMap::from([(
                "stale-model".to_owned(),
                NOW_SECS.saturating_sub(WIDEST_SPEND_WINDOW_SECS),
            )]),
        },
    );
    assert_eq!(
        recorded_unknown_models(
            &[(claude_adapter(), file), (claude_adapter(), stale)],
            &cache,
            NOW_SECS
        ),
        BTreeSet::new()
    );
}

fn sample_spending() -> Spending {
    let mut spending = Spending::default();
    spending.total.today.usd = 1.25;
    spending.total.today.tokens = 4_200;
    spending
        .by_provider
        .insert("claude".into(), spending.total.clone());
    spending
}

#[test]
fn provider_cache_staleness_and_error_cases_are_explicit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("provider-spending.json");
    let spending = sample_spending();

    write_provider_spending_cache(&path, 12_345, &spending);
    let cache = read_provider_spending_cache(&path);
    assert_eq!(cache.version, PROVIDER_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 12_345);
    assert_eq!(cache.spending, spending);
    assert!(cache.is_fresh(12_345));

    std::fs::write(&path, serde_json::to_vec(&spending).unwrap()).unwrap();
    let pre_stamp = read_provider_spending_cache(&path);
    assert_eq!(pre_stamp.refreshed_at_ms, 0);
    assert_eq!(pre_stamp.spending, spending);
    assert!(!pre_stamp.is_fresh(NOW_SECS * 1_000));

    let now_ms = NOW_SECS * 1_000;
    let stale_shape = ProviderSpendingCache {
        version: 0,
        refreshed_at_ms: now_ms,
        spending: spending.clone(),
    };
    std::fs::write(&path, serde_json::to_vec(&stale_shape).unwrap()).unwrap();
    let version_mismatch = read_provider_spending_cache(&path);
    assert_eq!(version_mismatch.spending, spending);
    assert!(!version_mismatch.is_fresh(now_ms));

    std::fs::write(&path, b"not json").unwrap();
    let corrupt = read_provider_spending_cache(&path);
    assert_eq!(corrupt.refreshed_at_ms, 0);
    assert_eq!(corrupt.spending, Spending::default());

    let ttl_ms = SPENDING_TTL.as_millis() as u64;
    let fresh = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: 1_000,
        ..ProviderSpendingCache::default()
    };
    assert!(fresh.is_fresh(1_000 + ttl_ms));
    assert!(!fresh.is_fresh(1_001 + ttl_ms));
    assert!(fresh.is_fresh(500));
}

#[test]
fn live_baselines_and_overlay_cases_stay_bounded() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("live-spend-baselines.json");
    let baselines = LiveSpendBaselines {
        observed_walk_ms: 12_345,
        baselines: BTreeMap::from([("claude-1".to_owned(), 1.05)]),
    };
    write_live_spend_baselines(&path, &baselines);
    assert_eq!(read_live_spend_baselines(&path), baselines);

    for (name, walked, live, baselines, published_at, expected) in [
        (
            "overshoot",
            10.0,
            vec![("a", 1.30, Some(5_000)), ("b", 2.50, Some(5_000))],
            BTreeMap::from([("a".to_owned(), 1.00), ("b".to_owned(), 2.50)]),
            9_000,
            10.30,
        ),
        (
            "new session",
            1.00,
            vec![("fresh", 0.40, Some(9_500))],
            BTreeMap::new(),
            9_000,
            1.40,
        ),
        (
            "unbaselined old sessions",
            5.00,
            vec![("old", 3.00, Some(8_000)), ("unstamped", 2.00, None)],
            BTreeMap::new(),
            9_000,
            5.00,
        ),
        (
            "negative delta",
            6.00,
            vec![("a", 3.20, Some(5_000))],
            BTreeMap::from([("a".to_owned(), 4.00)]),
            9_000,
            6.00,
        ),
    ] {
        let blended = today_spend_live_usd(walked, live.into_iter(), &baselines, published_at);
        assert!((blended - expected).abs() < 1e-9, "{name}");
    }
}
