use super::*;

/// The suite's fixed "now" (matches the snapshot testkit epoch), so the
/// trailing-window bucketing is exact on any wall-clock day.
const NOW_SECS: u64 = 1_750_000_000;
use std::io::Write as _;
use tempfile::TempDir;

fn claude_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::ClaudeAdapter
}

fn codex_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::CodexAdapter
}

/// Claude tests don't need pricing — tag the files Claude, sum with an empty
/// book, and take the fleet total, matching the pre-per-provider assertions.
fn compute_total(files: &[PathBuf], cache: &mut SpendingDiskCache) -> SpendTally {
    let tagged: Vec<(&'static dyn AgentAdapter, PathBuf)> = files
        .iter()
        .map(|file| (claude_adapter(), file.clone()))
        .collect();
    compute_spending(&tagged, cache, &PriceBook::default(), NOW_SECS).total
}

/// ISO-8601 UTC timestamp for a Unix-seconds instant — round-trips through
/// [`iso_to_unix_secs`] back to that same whole second.
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

// Full Claude-format line including "usage":{ to pass the fast pre-filter.
fn claude_line_ts(ts: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
    )
}

fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    claude_line_ts(&format!("{date}T10:00:00.000Z"), cost, msg_id, req_id)
}

/// A Claude line stamped `secs_ago` before now — for trailing-window tests.
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

#[test]
fn utc_date_known_epoch() {
    assert_eq!(utc_date(0), "1970-01-01");
    // 2000-01-01 00:00:00 UTC = 946684800
    assert_eq!(utc_date(946_684_800), "2000-01-01");
    // 2025-06-01 00:00:00 UTC = 1748736000
    assert_eq!(utc_date(1_748_736_000), "2025-06-01");
    assert_eq!(utc_date(1_748_822_399), "2025-06-01");
}

#[test]
fn iso_to_unix_secs_parses_known_instants() {
    assert_eq!(
        iso_to_unix_secs("2000-01-01T00:00:00.000Z"),
        Some(946_684_800)
    );
    // 2025-06-01T00:00:00Z = 1748736000; + 12h = +43200.
    assert_eq!(
        iso_to_unix_secs("2025-06-01T12:00:00Z"),
        Some(1_748_779_200)
    );
    // A bare date parses to midnight UTC.
    assert_eq!(iso_to_unix_secs("1970-01-02"), Some(86_400));
    // Round-trips with the test formatter.
    assert_eq!(
        iso_to_unix_secs(&iso_at(1_700_000_123)),
        Some(1_700_000_123)
    );
    // Malformed prefixes are rejected.
    assert_eq!(iso_to_unix_secs("not-a-date"), None);
    assert_eq!(iso_to_unix_secs(""), None);
}

#[test]
fn mtime_cache_hit_skips_io() {
    let dir = TempDir::new().unwrap();
    let now_secs = NOW_SECS;
    let today = utc_date(now_secs);

    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );

    let mut cache = SpendingDiskCache::default();
    let t1 = compute_total(std::slice::from_ref(&file), &mut cache);
    assert!((t1.today.usd - 0.5).abs() < 1e-9);
    assert_eq!(t1.today.tokens, 15, "input 10 + output 5");
    assert!(cache.dirty);

    cache.dirty = false;
    let t2 = compute_total(&[file], &mut cache);
    assert_eq!(t2.today.usd, t1.today.usd);
    assert_eq!(t2.today.tokens, t1.today.tokens);
    assert!(
        !cache.dirty,
        "cache should not be marked dirty on a cache hit"
    );
}

#[test]
fn stale_version_cache_is_discarded_so_files_reparse() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("spending.json");

    // A cache from an older parse shape: a file entry whose `tokens` predate
    // the field, under the implicit pre-versioning `version: 0`.
    let stale = SpendingDiskCache {
        version: 0,
        files: HashMap::from([(
            "/old/chat.jsonl".to_string(),
            FileCacheEntry {
                mtime_secs: 123,
                len: 0,
                cursor: SpendCursor::default(),
                entries: vec![CachedEntry {
                    ts_secs: 1_767_225_600,
                    cost_usd: 9.0,
                    input: 0,
                    output: 0,
                    cache_write: 0,
                    cache_read: 0,
                    message_id: Some("msg-old".to_string()),
                    request_id: Some("req-old".to_string()),
                    is_sidechain: false,
                }],
            },
        )]),
        dirty: false,
    };
    write_spending_cache(&path, &stale);

    // Read drops the stale-shape cache entirely and stamps the current
    // version, so the finalized session re-parses instead of serving `0`
    // tokens from a mtime that will never change again.
    let healed = read_spending_cache(&path);
    assert_eq!(healed.version, SPENDING_CACHE_VERSION);
    assert!(
        healed.files.is_empty(),
        "a stale-version cache is discarded, not served"
    );

    // A current-version cache round-trips with its files intact — only a
    // version mismatch discards.
    let mut current = healed;
    current.files.insert(
        "/new/chat.jsonl".to_string(),
        FileCacheEntry {
            mtime_secs: 456,
            len: 0,
            cursor: SpendCursor::default(),
            entries: vec![CachedEntry {
                ts_secs: 1_770_000_000,
                cost_usd: 1.0,
                input: 30,
                output: 12,
                cache_write: 0,
                cache_read: 0,
                message_id: None,
                request_id: None,
                is_sidechain: false,
            }],
        },
    );
    write_spending_cache(&path, &current);
    let kept = read_spending_cache(&path);
    assert_eq!(kept.version, SPENDING_CACHE_VERSION);
    assert_eq!(
        kept.files["/new/chat.jsonl"].entries[0].input, 30,
        "a same-version cache keeps its entries"
    );
}

#[test]
fn token_split_and_session_counts_populate_windows() {
    let dir = TempDir::new().unwrap();
    let now_secs = NOW_SECS;
    let today = utc_date(now_secs);
    // One Claude thread spread across its `session_id` dir: a main chat file
    // plus a subagent file. Both fold under the one thread for session counts.
    let session = dir.path().join("sess-1");
    std::fs::create_dir_all(session.join("subagents")).unwrap();
    let main_line = format!(
        r#"{{"timestamp":"{today}T10:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":12000,"output_tokens":64000,"cache_creation_input_tokens":12000,"cache_read_input_tokens":68000}}}}}}"#
    );
    let main = write_jsonl(&session, "chat.jsonl", &[&main_line]);
    let sub_line = format!(
        r#"{{"timestamp":"{today}T10:01:00.000Z","costUSD":0.1,"requestId":"req-2","isSidechain":true,"message":{{"id":"msg-2","usage":{{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}}}}"#
    );
    let subfile = write_jsonl(&session.join("subagents"), "worker.jsonl", &[&sub_line]);

    let mut cache = SpendingDiskCache::default();
    let total = compute_total(&[main, subfile], &mut cache);

    // `◇` is input + output only; the cache split rides its own fields.
    assert_eq!(total.today.input, 13_000, "12000 + 1000");
    assert_eq!(total.today.output, 64_500, "64000 + 500");
    assert_eq!(total.today.tokens, 77_500, "◇ = input + output");
    assert_eq!(total.today.cache_write, 12_000);
    assert_eq!(total.today.cache_read, 70_000, "68000 + 2000");
    // The main + subagent files fold under one `session_id` directory, so the
    // thread counts once across every window its activity falls within.
    assert_eq!(total.today.sessions, 1, "main + subagent = one thread");
    assert_eq!(total.year.sessions, 1);
}

#[test]
fn trailing_windows_bucket_by_age() {
    let dir = TempDir::new().unwrap();
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;

    // One entry seated inside each successive window, plus one past the year.
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            &claude_line_ago(2 * HOUR, 1.0, "msg-1", "req-1"), // within 24h
            &claude_line_ago(3 * DAY, 0.5, "msg-2", "req-2"),  // within 7d, not 24h
            &claude_line_ago(20 * DAY, 0.25, "msg-3", "req-3"), // within 30d, not 7d
            &claude_line_ago(100 * DAY, 0.1, "msg-4", "req-4"), // within 365d, not 30d
            &claude_line_ago(400 * DAY, 9.0, "msg-5", "req-5"), // older than a year — dropped
        ],
    );

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[file], &mut cache);

    // The windows nest, so each wider one adds the next entry.
    assert!(
        (totals.today.usd - 1.0).abs() < 1e-9,
        "today (24h) = {}",
        totals.today.usd
    );
    assert_eq!(totals.today.tokens, 15, "one entry inside 24h");
    assert!(
        (totals.week.usd - 1.5).abs() < 1e-9,
        "week (7d) = {}",
        totals.week.usd
    );
    assert_eq!(totals.week.tokens, 30);
    assert!(
        (totals.month.usd - 1.75).abs() < 1e-9,
        "month (30d) = {}",
        totals.month.usd
    );
    assert_eq!(totals.month.tokens, 45);
    // year (365d) adds the 100-day entry; the 400-day entry falls out entirely.
    assert!(
        (totals.year.usd - 1.85).abs() < 1e-9,
        "year (365d) = {}",
        totals.year.usd
    );
    assert_eq!(
        totals.year.tokens, 60,
        "four entries inside the year; the 400-day one is dropped"
    );
}

#[test]
fn empty_file_list_returns_zero() {
    let mut cache = SpendingDiskCache::default();
    assert!(compute_total(&[], &mut cache).is_zero());
}

#[test]
fn zero_and_negative_costs_ignored() {
    let dir = TempDir::new().unwrap();
    let now_secs = NOW_SECS;
    let today = utc_date(now_secs);

    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            &format!(
                r#"{{"timestamp":"{today}T10:00:00.000Z","costUSD":0.0,"message":{{"usage":{{"input_tokens":1}}}}}}"#
            ),
            &format!(
                r#"{{"timestamp":"{today}T11:00:00.000Z","costUSD":-1.0,"message":{{"usage":{{"input_tokens":1}}}}}}"#
            ),
            &claude_line(&today, 0.3, "msg-1", "req-1"),
        ],
    );

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[file], &mut cache);
    assert!((totals.today.usd - 0.3).abs() < 1e-9);
    assert_eq!(
        totals.today.tokens, 15,
        "only the kept entry: input 10 + output 5"
    );
}

#[test]
fn claude_exact_dedup_drops_repeated_message_request_pair() {
    let dir = TempDir::new().unwrap();
    let now_secs = NOW_SECS;
    let today = utc_date(now_secs);
    let line = claude_line(&today, 1.0, "msg-a", "req-a");

    // Same (message_id, request_id) twice within one file (the parser
    // returns raw entries — this pass owns all dedup) and again in a
    // second file.
    let file1 = write_jsonl(dir.path(), "session1.jsonl", &[&line, &line]);
    let file2 = write_jsonl(dir.path(), "session2.jsonl", &[&line]);

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[file1, file2], &mut cache);
    assert!(
        (totals.today.usd - 1.0).abs() < 1e-9,
        "got {}",
        totals.today.usd
    );
    assert_eq!(totals.today.tokens, 15, "the duplicate pair counts once");
}

#[test]
fn sidechain_replay_does_not_double_count() {
    let dir = TempDir::new().unwrap();
    let now_secs = NOW_SECS;
    let today = utc_date(now_secs);

    // Main-chain entry for msg-parent in session file.
    let main_file = write_jsonl(
        dir.path(),
        "session.jsonl",
        &[&claude_line(&today, 0.05, "msg-parent", "req-parent")],
    );
    // Sidechain replay of the same message in subagent file — inflated cost.
    let side_file = write_jsonl(
        dir.path(),
        "subagent.jsonl",
        &[&claude_sidechain_line(
            &today,
            5.00,
            "msg-parent",
            "req-sidechain",
        )],
    );

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[main_file, side_file], &mut cache);
    assert!(
        (totals.today.usd - 0.05).abs() < 1e-9,
        "today.usd = {} (expected 0.05)",
        totals.today.usd
    );
    assert_eq!(
        totals.today.tokens, 15,
        "main-chain tokens kept, the 50k sidechain replay suppressed"
    );
}

#[test]
fn sidechain_only_kept_when_no_main_chain_exists() {
    let dir = TempDir::new().unwrap();
    let now_secs = NOW_SECS;
    let today = utc_date(now_secs);

    let file = write_jsonl(
        dir.path(),
        "sidechain.jsonl",
        &[&claude_sidechain_line(&today, 0.20, "msg-x", "req-x")],
    );

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[file], &mut cache);
    assert!(
        (totals.today.usd - 0.20).abs() < 1e-9,
        "got {}",
        totals.today.usd
    );
    assert_eq!(
        totals.today.tokens, 50_005,
        "a lone sidechain keeps its tokens: input 50000 + output 5"
    );
}

fn append_line(path: &Path, line: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

#[test]
fn grown_file_parses_only_the_appended_suffix() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 1.0, "msg-1", "req-1")],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((first.total.today.usd - 1.0).abs() < 1e-9);

    // Corrupt the already-parsed prefix in place (length unchanged, the
    // trailing newline kept), then append a second line. The incremental
    // pass must read only past its cursor, so the corruption is invisible
    // and the cached first entry still counts.
    let prefix_len = std::fs::metadata(&file).unwrap().len() as usize;
    {
        use std::io::{Seek as _, SeekFrom};
        let mut f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![b'x'; prefix_len - 1]).unwrap();
    }
    append_line(&file, &claude_line(&today, 0.25, "msg-2", "req-2"));

    let second = compute_spending(
        &[(claude_adapter(), file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!(
        (second.total.today.usd - 1.25).abs() < 1e-9,
        "suffix-only read: the cached prefix entry survives its corruption (got {})",
        second.total.today.usd
    );
}

#[test]
fn truncated_file_reparses_cold() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let line_a = claude_line(&today, 1.0, "msg-a", "req-a");
    let line_b = claude_line(&today, 0.5, "msg-b", "req-b");
    let file = write_jsonl(dir.path(), "chat.jsonl", &[&line_a, &line_b]);
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((first.total.today.usd - 1.5).abs() < 1e-9);

    // Rotation/truncation: the file shrinks. The stale tail entries must
    // drop with the cold re-parse, never lingering from the old cache.
    write_jsonl(dir.path(), "chat.jsonl", &[&line_a]);
    let second = compute_spending(
        &[(claude_adapter(), file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!(
        (second.total.today.usd - 1.0).abs() < 1e-9,
        "a shorter file re-parses cold (got {})",
        second.total.today.usd
    );
}

#[test]
fn same_length_rewrite_with_a_new_mtime_reparses_cold() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    // `1.0` and `3.0` format to the same byte length, so the rewrite
    // changes content but not size — only the mtime can reveal it.
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 1.0, "msg-a", "req-a")],
    );
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );

    write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 3.0, "msg-a", "req-a")],
    );
    let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();

    let second = compute_spending(
        &[(claude_adapter(), file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!(
        (second.total.today.usd - 3.0).abs() < 1e-9,
        "an in-place rewrite (same length, new mtime) re-parses cold (got {})",
        second.total.today.usd
    );
}

#[test]
fn codex_resume_state_survives_the_suffix_parse() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    // Cumulative-only token counts plus a model declared once up front:
    // both halves of the resume state are exercised — the appended event
    // must subtract the stored totals AND price under the remembered
    // model (a fresh fold would record the full cumulative as one
    // inflated delta; a lost model would drop the entry as unpriced).
    let file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_total_line(&today, 1000, 500),
        ],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(codex_adapter(), file.clone())],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(first.total.today.input, 1000);
    assert_eq!(first.total.today.output, 500);

    append_line(&file, &codex_total_line(&today, 1600, 800));
    let second = compute_spending(
        &[(codex_adapter(), file)],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(
        second.total.today.input, 1600,
        "the resumed fold subtracts the stored cumulative totals"
    );
    assert_eq!(second.total.today.output, 800);
}

fn codex_total_line(date: &str, input: u64, output: u64) -> String {
    format!(
        r#"{{"type":"event_msg","timestamp":"{date}T10:00:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}}}"#
    )
}

/// A price book with a single non-builtin model so the asserted cost is
/// independent of the hardcoded builtin values.
fn gpt4o_book() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{"gpt-4o": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                           "cache_read_input_token_cost": 1e-7}}"#,
    )
}

/// Write a Codex session file. The path is irrelevant — the provider is
/// tagged explicitly at the `compute_spending` call.
fn write_codex(dir: &Path, lines: &[&str]) -> PathBuf {
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    write_jsonl(&sessions, "sess.jsonl", lines)
}

fn codex_token_line(date: &str, input: u64, cached: u64, output: u64) -> String {
    format!(
        r#"{{"type":"event_msg","timestamp":"{date}T10:00:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
    )
}

#[test]
fn codex_tokens_priced_through_book() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_token_line(&today, 1000, 400, 500),
        ],
    );

    let mut cache = SpendingDiskCache::default();
    let spending = compute_spending(
        &[(codex_adapter(), file)],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );

    // uncached 600 * 1e-6 + cached 400 * 1e-7 + output 500 * 2e-6
    //   = 0.0006 + 0.00004 + 0.001 = 0.00164
    let codex = &spending.by_provider["codex"];
    assert!(
        (codex.today.usd - 0.00164).abs() < 1e-9,
        "got {}",
        codex.today.usd
    );
    // `◇` is fresh input + output: Codex's `input_tokens` includes the cached
    // slice, so the uncached 600 + output 500 = 1100, with the 400 cached
    // riding `cache_read` (never the total).
    assert_eq!(codex.today.tokens, 1100, "uncached input 600 + output 500");
    assert_eq!(codex.today.input, 600);
    assert_eq!(codex.today.output, 500);
    assert_eq!(codex.today.cache_read, 400);
    assert_eq!(codex.today.cache_write, 0, "Codex has no cache-creation");
    assert!((spending.total.today.usd - 0.00164).abs() < 1e-9);
}

#[test]
fn unpriced_codex_model_contributes_nothing() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"some-unknown-model-xyz"}}"#,
            &codex_token_line(&today, 1000, 0, 500),
        ],
    );

    let mut cache = SpendingDiskCache::default();
    // Empty json → only builtins; the unknown model has no price.
    let spending = compute_spending(
        &[(codex_adapter(), file)],
        &mut cache,
        &PriceBook::from_litellm_json("{}"),
        NOW_SECS,
    );
    assert!(spending.total.is_zero());
}

#[test]
fn per_provider_breakdown_splits_claude_and_codex() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);

    let claude_file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );
    let codex_file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_token_line(&today, 1000, 0, 0),
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

    assert!((spending.by_provider["claude"].today.usd - 0.5).abs() < 1e-9);
    assert!((spending.by_provider["codex"].today.usd - 0.001).abs() < 1e-9);
    assert!((spending.total.today.usd - 0.501).abs() < 1e-9);
}

// ── Provider-spending cache (the SPENDING_TTL gate's stamp) ────────────────────

/// A `Spending` with distinguishable values, so round-trip assertions can tell
/// a preserved payload from a defaulted one.
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
fn provider_cache_round_trips_with_stamp() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("provider-spending.json");
    let spending = sample_spending();

    write_provider_spending_cache(&path, 12_345, &spending);
    let cache = read_provider_spending_cache(&path);

    assert_eq!(cache.refreshed_at_ms, 12_345);
    assert_eq!(cache.spending, spending);
}

#[test]
fn pre_stamp_provider_cache_reads_values_as_stale() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("provider-spending.json");
    // The pre-stamp on-disk shape: a bare `Spending` with no `refreshed_at_ms`.
    let spending = sample_spending();
    std::fs::write(&path, serde_json::to_vec(&spending).unwrap()).unwrap();

    let cache = read_provider_spending_cache(&path);

    // Flatten tolerance: the values survive the upgrade; the missing stamp
    // defaults to 0, which any real wall clock reads as stale, so the gate
    // refreshes once instead of serving the old shape forever.
    assert_eq!(cache.refreshed_at_ms, 0);
    assert_eq!(cache.spending, spending);
    assert!(!cache.is_fresh(NOW_SECS * 1_000));
}

#[test]
fn provider_cache_missing_or_corrupt_reads_default() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("provider-spending.json");
    assert_eq!(read_provider_spending_cache(&path).refreshed_at_ms, 0);

    std::fs::write(&path, b"not json").unwrap();
    let cache = read_provider_spending_cache(&path);
    assert_eq!(cache.refreshed_at_ms, 0);
    assert_eq!(cache.spending, Spending::default());
}

#[test]
fn provider_cache_expires_after_spending_ttl() {
    let cache = ProviderSpendingCache {
        refreshed_at_ms: 1_000,
        spending: Spending::default(),
    };
    let ttl_ms = SPENDING_TTL.as_millis() as u64;
    // Boundary-exact: fresh at exactly the TTL, stale one ms past it.
    assert!(cache.is_fresh(1_000 + ttl_ms));
    assert!(!cache.is_fresh(1_001 + ttl_ms));
    // A clock that ran backwards reads fresh (saturating), never a walk storm.
    assert!(cache.is_fresh(500));
}
