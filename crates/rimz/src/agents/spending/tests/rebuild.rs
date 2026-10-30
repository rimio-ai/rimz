use super::closed_days::Fixture;
use super::*;
use crate::agents::spending::rebuild::RebuildRun;
use jiff::civil::Date;
use serde_json::{Value, json};

const SESSION: &str = "12345678-1234-4234-8234-123456789abc";
const SECOND: &str = "22345678-1234-4234-8234-123456789abc";

fn seconds(at: &str) -> u64 {
    at.parse::<jiff::Timestamp>().unwrap().as_second() as u64
}

fn run_rebuild(
    fixture: &mut Fixture,
    now: u64,
    files: &[SpendingFile],
    progress: &mut dyn FnMut(SpendProgress),
) -> (SpendingWalkResult, Result<RebuildReport, RebuildErr>) {
    run_scoped_rebuild(fixture, now, files, RebuildScope::All, progress)
}

fn run_scoped_rebuild(
    fixture: &mut Fixture,
    now: u64,
    files: &[SpendingFile],
    scope: RebuildScope,
    progress: &mut dyn FnMut(SpendProgress),
) -> (SpendingWalkResult, Result<RebuildReport, RebuildErr>) {
    struct Progress<'a>(&'a mut dyn FnMut(SpendProgress));
    impl WalkObserver for Progress<'_> {
        fn on_file(&mut self, progress: SpendProgress) {
            (self.0)(progress);
        }
    }
    let mut run = RebuildRun::new(scope);
    let result = fixture.walker.walk_inner(
        &fixture.runtime.shared_spending_cursor_path(),
        true,
        &WalkRequest {
            history: &fixture.runtime.spending_history,
            files,
            prices: &fixture.prices,
            now_secs: now,
            origin_overrides: &HashMap::new(),
            user_inputs: &[],
            scope: None,
            spec: &HeadlineSpec::default(),
        },
        &mut Progress(progress),
        Some(&mut run),
    );
    (result, run.report)
}

#[test]
fn requested_rebuild_lowers_partial_days_and_keeps_days_without_transcripts() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let first = a.path(SESSION);
    let second = a.path(SECOND);
    std::fs::write(&first, claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    std::fs::write(
        &second,
        format!("{}\n", claude_line("2026-06-10", 3.0, "b", "b")),
    )
    .unwrap();
    append_line(&second, &claude_line("2026-06-11", 7.0, "c", "c"));
    assert_eq!(a.walk(now, true).spending.total.month.usd, 12.0);
    std::fs::remove_file(&second).unwrap();
    let files = a.discover(now);
    let (result, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report.files, 1,
        "only present target-day files are cold-parsed"
    );
    assert_eq!(report.rebuilt.len(), 1);
    assert_eq!(report.rebuilt[0].before_usd, Some(5.0));
    assert_eq!(report.rebuilt[0].after_usd, 2.0);
    assert_eq!(report.kept.len(), 1);
    assert_eq!(report.kept[0].day, "2026-06-11".parse::<Date>().unwrap());
    assert_eq!(result.spending.total.month.usd, 9.0);
    assert_eq!(a.lines().len(), 3, "rebuilds append rather than rewrite");
}

#[test]
fn rebuild_reads_a_copy_whose_rows_compaction_dropped_once_its_winner_is_gone() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let original = a.path(SESSION);
    let other = a.path(SECOND);
    std::fs::write(&original, claude_line("2026-06-10", 2.0, "same", "same")).unwrap();
    std::fs::write(&other, claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    let copy = a
        .home
        .join("projects/synced")
        .join(original.file_name().unwrap());
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&original, &copy).unwrap();
    assert_eq!(a.walk(now, true).spending.total.month.usd, 5.0);
    let holds_rows = |path: &std::path::Path| {
        !a.walker.cache.files[path.to_str().unwrap()]
            .entries
            .is_empty()
    };
    let (winner, survivor) = if holds_rows(&original) {
        (&original, &copy)
    } else {
        (&copy, &original)
    };
    assert!(
        !holds_rows(survivor),
        "compaction keeps no row of the copy that lost dedup"
    );
    std::fs::remove_file(winner).unwrap();
    let files = a.discover(now);
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report.unchanged, 1,
        "the surviving copy still holds the deleted winner's rows"
    );
    assert!(report.rebuilt.is_empty() && report.kept.is_empty());
    std::fs::remove_file(&other).unwrap();
    let files = a.discover(now);
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert!(
        report.kept.is_empty(),
        "the copy is a transcript of the day"
    );
    assert_eq!(report.rebuilt.len(), 1);
    assert_eq!(report.rebuilt[0].before_usd, Some(5.0));
    assert_eq!(report.rebuilt[0].after_usd, 2.0);
}

#[test]
fn rebuild_cold_dedups_synced_forks_and_ignores_dollar_roundoff() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path(SESSION);
    std::fs::write(
        &path,
        claude_line_ts_in("2026-06-10T15:00:00Z", 3.640361, "same", "same", &a.home),
    )
    .unwrap();
    a.walk(now, true);
    assert!(a.walker.cache.files[path.to_str().unwrap()].entries[0].rolled);
    let history = a.runtime.spending_history.spend(2026);
    let mut saved = a.lines()[0].clone();
    saved["rows"][0]["usd"] = json!(3.6403610000000004_f64);
    std::fs::write(&history, format!("{saved}\n")).unwrap();
    let copy = a
        .home
        .join("projects/synced")
        .join(path.file_name().unwrap());
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&path, &copy).unwrap();
    let files = a.discover(now);
    let (result, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report.unchanged, 1,
        "raw ids must dedup against the present original"
    );
    assert_eq!(report.files, 2);
    assert!(report.rebuilt.is_empty());
    assert_eq!(a.lines().len(), 1);
    assert!((result.spending.total.month.usd - 3.640361).abs() < 1e-12);
}

#[test]
fn rebuild_matches_close_dollar_rows_bijectively_in_any_order() {
    for (before, after) in [
        (vec![1.0, 1.0 + 1.5e-9], vec![1.0 + 0.75e-9, 1.0]),
        (
            vec![1.0, 1.0 + 0.9e-9, 1.0 + 1.5e-9],
            vec![1.0, 1.0 + 0.75e-9, 1.0 + 2.25e-9],
        ),
    ] {
        let dir = TempDir::new().unwrap();
        let mut a = Fixture::new(dir.path(), "a");
        let now = seconds("2026-06-30T12:00:00Z");
        let home = a.home.clone();
        let write = |index, usd| {
            let path = home.join(format!("projects/{index}/{SESSION}.jsonl"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let id = format!("turn-{index}");
            let mut line: Value =
                serde_json::from_str(&claude_line("2026-06-10", usd, &id, &id)).unwrap();
            line["cwd"] = json!(&home);
            std::fs::write(path, line.to_string()).unwrap();
        };
        for (index, usd) in before.iter().enumerate() {
            write(index, *usd);
        }
        a.walk(now, true);
        let mut rows = a.lines()[0]["rows"].as_array().unwrap().clone();
        assert_eq!(
            rows.iter()
                .map(|row| row["usd"].as_f64().unwrap())
                .collect::<Vec<_>>(),
            before
        );
        for row in &mut rows {
            row["usd"] = json!(0.0);
        }
        assert!(
            rows.windows(2).all(|rows| rows[0] == rows[1]),
            "only dollars may differ"
        );
        for (index, usd) in after.iter().enumerate() {
            write(index, *usd);
        }
        let mut files = a.discover(now);
        files.sort_by(|left, right| left.path.cmp(&right.path));
        for files in [files.clone(), files.into_iter().rev().collect()] {
            let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
            let report = report.unwrap();
            assert_eq!(
                report.unchanged, 1,
                "a one-to-one tolerance match must not append"
            );
            assert!(report.rebuilt.is_empty());
            assert_eq!(a.lines().len(), 1);
        }
    }
}

#[test]
fn rebuild_reprices_codex_and_preserves_stamped_origins() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    a.adapter = codex_adapter();
    a.prices = gpt4o_book();
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a
        .home
        .join("sessions")
        .join(format!("rollout-{SESSION}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            json!({"type":"turn_context", "payload":{"model":"gpt-4o"}}),
            codex_token_line("2026-06-10", 2_000_000, 0, 0)
        ),
    )
    .unwrap();
    a.walk(now, true);
    let origin = dir.path().join("stamped-origin");
    a.walker
        .cache
        .files
        .get_mut(path.to_str().unwrap())
        .unwrap()
        .origin_path = Some(origin.clone());
    let mut saved = a.lines()[0].clone();
    saved["rows"][0]["origin"] = json!(origin);
    std::fs::write(a.runtime.spending_history.spend(2026), format!("{saved}\n")).unwrap();
    let files = a.discover(now);
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    assert_eq!(
        report.unwrap().unchanged,
        1,
        "a cold parse must retain the cursor's origin"
    );
    a.prices = PriceBook::from_litellm_json(
        r#"{"gpt-4o":{"input_cost_per_token":2e-6,"output_cost_per_token":4e-6}}"#,
    );
    let (result, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(report.rebuilt.len(), 1);
    assert_eq!(report.rebuilt[0].before_usd, Some(2.0));
    assert_eq!(report.rebuilt[0].after_usd, 4.0);
    assert_eq!(
        a.lines().last().unwrap()["rows"][0]["origin"],
        json!(origin)
    );
    assert_eq!(result.spending.total.month.usd, 4.0);
}

#[test]
fn rebuild_skips_cold_parsing_when_maintenance_is_unavailable() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let files = a.discover(now);
    let hold = history::try_maintain(&a.runtime.spending_history)
        .unwrap()
        .unwrap();
    let mut progress_ticks = 0;
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| progress_ticks += 1);
    assert!(report.as_ref().unwrap_err().to_string().contains("retry"));
    assert!(matches!(report, Err(RebuildErr::MaintenanceUnavailable)));
    assert_eq!(
        progress_ticks, 1,
        "only the warm cursor hit may report progress"
    );
    drop(hold);
    assert_eq!(a.lines().len(), 1);
}

#[test]
fn rebuild_reports_unusable_format_marks_without_cold_parsing() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let files = a.discover(now);
    let format = a.runtime.spending_history.format();
    let saved = std::fs::read(a.runtime.spending_history.spend(2026)).unwrap();
    for mark in [Some("2\n"), Some("broken\n"), None] {
        std::fs::remove_file(&format).unwrap();
        if let Some(mark) = mark {
            std::fs::write(&format, mark).unwrap();
        } else {
            std::fs::create_dir(&format).unwrap();
        }
        let mut warm_ticks = 0;
        let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| warm_ticks += 1);
        let error = report.unwrap_err();
        assert!(
            matches!(&error, RebuildErr::FormatUnusable(path) if path == &format),
            "an unusable mark is not transient lock contention: {error:?}"
        );
        let message = error.to_string();
        assert!(message.contains(format.to_str().unwrap()), "{message}");
        assert!(!message.contains("retry"), "{message}");
        assert_eq!(warm_ticks, 1, "an unusable mark must skip the cold parse");
        let mut ticks = 0;
        let report = rebuild_spending_history(
            &a.runtime,
            &HeadlineSpec::default(),
            RebuildScope::All,
            &mut |_| ticks += 1,
        );
        assert!(matches!(report, Err(RebuildErr::FormatUnusable(_))));
        assert_eq!(ticks, 0, "the public entry must not start a walk");
        assert_eq!(
            std::fs::read(a.runtime.spending_history.spend(2026)).unwrap(),
            saved
        );
        if mark.is_none() {
            std::fs::remove_dir(&format).unwrap();
            std::fs::write(&format, "1\n").unwrap();
        }
    }
}

#[test]
fn rebuild_defers_an_account_with_an_unread_discovered_file() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path(SESSION);
    std::fs::write(
        &path,
        format!("{}\n", claude_line("2026-06-10", 2.0, "a", "a")),
    )
    .unwrap();
    a.walk(now, true);
    append_line(&path, &claude_line("2026-06-10", 3.0, "b", "b"));
    let unread = a.path(SECOND);
    std::fs::write(&unread, "").unwrap();
    let files = a.discover(now);
    std::fs::remove_file(unread).unwrap();
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report.deferred_days.len(),
        1,
        "an unread login must remain eligible for retry"
    );
    assert!(report.rebuilt.is_empty());
    assert!(report.kept.is_empty());
    assert_eq!(report.unchanged, 0);
    assert_eq!(a.lines().len(), 1);
}

#[test]
#[cfg(unix)]
fn spend_parsers_distinguish_read_failures_from_empty_input() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        return; // Root bypasses mode 000, so the fixture cannot deny the read.
    }
    let dir = TempDir::new().unwrap();
    for (kind, name) in [
        ("claude", format!("{SESSION}.jsonl")),
        ("codex", format!("rollout-{SESSION}.jsonl")),
        ("copilot", format!("{SESSION}/events.jsonl")),
        ("droid", format!("{SESSION}.settings.json")),
        ("grok", format!("{SESSION}/updates.jsonl")),
        ("kimi", format!("{SESSION}/wire.jsonl")),
        ("omp", format!("session_{SESSION}.jsonl")),
        ("pi", format!("{SESSION}.jsonl")),
        ("qwen", format!("{SESSION}.jsonl")),
    ] {
        let path = dir.path().join(kind).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        let adapter = crate::agents::definition_by_kind(kind).unwrap();
        let empty = adapter.parse_spend(&path, None, &PriceBook::default());
        assert!(
            empty.read_error.is_none(),
            "{kind}: readable empty input is not a read failure"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        let unread = adapter.parse_spend(&path, None, &PriceBook::default());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            unread.read_error,
            Some(std::io::ErrorKind::PermissionDenied),
            "{kind}"
        );
    }
}

#[test]
#[cfg(unix)]
fn opencode_spend_distinguishes_an_unread_database_from_an_empty_store() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        return; // Root bypasses mode 000, so the fixture cannot deny the read.
    }
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opencode.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session_v2 (id TEXT, directory TEXT);
         CREATE TABLE session_message (session_id TEXT, data TEXT, type TEXT, time_created INTEGER);",
    ).unwrap();
    drop(conn);
    let adapter = opencode_adapter();
    assert!(
        adapter
            .parse_spend(&path, None, &PriceBook::default())
            .read_error
            .is_none()
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
    let parsed = adapter.parse_spend(&path, None, &PriceBook::default());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        parsed.read_error.is_some(),
        "an unread SQLite store is not empty spend"
    );
}

#[test]
#[cfg(unix)]
fn rebuild_defers_an_account_when_a_candidate_read_fails_after_stat() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        return; // Root bypasses mode 000, so the fixture cannot deny the read.
    }
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let readable = a.path(SESSION);
    let unreadable = a.path(SECOND);
    std::fs::write(&readable, claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    std::fs::write(&unreadable, claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    a.walk(now, true);
    let before = a.lines();
    let files = a.discover(now);
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o0)).unwrap();
    assert!(a.adapter.transcript_stat(&unreadable).is_some());
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600)).unwrap();
    let report = report.unwrap();
    assert!(
        report.rebuilt.is_empty(),
        "unread input must not lower a saved day"
    );
    assert!(report.kept.is_empty());
    assert_eq!(report.unchanged, 0);
    assert_eq!(report.deferred_days.len(), 1);
    assert_eq!(a.lines(), before);
}

#[test]
fn rebuild_rereads_shared_history_after_parsing_without_holding_maintenance() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let files = a.discover(now);
    let paths = a.runtime.spending_history.clone();
    let mut changed: Value = a.lines()[0].clone();
    changed["rows"][0]["usd"] = json!(7.0);
    let mut intervened = false;
    let mut completed_passes = 0;
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |progress| {
        if intervened || progress.finished_files != progress.total_files {
            return;
        }
        completed_passes += 1;
        if completed_passes == 1 {
            return;
        }
        let hold = history::try_maintain(&paths).unwrap();
        assert!(
            hold.is_some(),
            "parsing must not hold the shared maintenance lock"
        );
        history::append(
            &paths,
            &paths.spend(2026),
            format!("\n{changed}\n").as_bytes(),
        )
        .unwrap();
        intervened = true;
    });
    let report = report.unwrap();
    assert_eq!(report.rebuilt.len(), 1);
    assert_eq!(report.rebuilt[0].before_usd, Some(7.0));
    assert_eq!(report.rebuilt[0].after_usd, 2.0);
    assert!(intervened);
    assert_eq!(a.lines().len(), 3);
}

#[test]
fn rebuild_dedups_readable_files_from_an_incomplete_account() {
    use crate::agents::ProviderLogin;
    use crate::ids::{AgentKind, LoginName};

    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let original = a.path(SESSION);
    std::fs::write(&original, claude_line("2026-06-10", 2.0, "same", "same")).unwrap();
    let other_home = dir.path().join("accounts/claude/zcopy");
    let copy = other_home
        .join("projects/project")
        .join(original.file_name().unwrap());
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&original, &copy).unwrap();
    let missing = a.path(SECOND);
    std::fs::write(&missing, "").unwrap();
    let kind = AgentKind::new_unchecked("claude");
    let mut files = a.walker.discovery.discover(
        [
            (ProviderLogin::default_for(kind.clone()), &a.home),
            (
                ProviderLogin::named(
                    kind,
                    "copy".parse::<LoginName>().unwrap(),
                    other_home.clone(),
                )
                .unwrap(),
                &other_home,
            ),
        ]
        .into_iter()
        .map(|(login, home)| {
            (
                login,
                a.adapter,
                BTreeMap::from([(
                    "CLAUDE_CONFIG_DIR".into(),
                    home.to_string_lossy().into_owned(),
                )]),
            )
        }),
        now,
    );
    files.sort_by(|left, right| left.path.cmp(&right.path));
    a.walk_files(&files, now, true);
    let before = a.lines();
    assert_eq!(before.len(), 2);
    assert!(
        before
            .iter()
            .any(|line| line["account"] == "claude/zcopy" && line["rows"][0]["usd"].is_null())
    );
    let cache =
        refresh::reparse_spending_files(&files, &a.walker.cache, &a.prices, now, &mut |_| {});
    std::fs::remove_file(missing).unwrap();
    let report = a
        .walker
        .closed_days
        .rebuild(
            &cache,
            &WalkRequest {
                history: &a.runtime.spending_history,
                files: &files,
                prices: &a.prices,
                now_secs: now,
                origin_overrides: &HashMap::new(),
                user_inputs: &[],
                scope: None,
                spec: &HeadlineSpec::default(),
            },
            &a.walker.discovery.history_accounts,
            RebuildScope::All,
            &BTreeSet::from(["claude@default".parse().unwrap()]),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        report.unchanged, 1,
        "the readable original must still win dedup"
    );
    assert!(report.rebuilt.is_empty());
    assert_eq!(a.lines(), before);
}

#[test]
fn interrupted_cold_fill_suppresses_growth_until_a_complete_walk() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    a.walk(now, true);
    let cursor = a.runtime.shared_spending_cursor_path();
    std::fs::remove_file(&cursor).unwrap();
    a.walker = SpendingWalker::new();
    let files = a.discover(now);
    struct Interrupt;
    impl WalkObserver for Interrupt {
        fn on_interval(&mut self, _: &SpendingDiskCache) {
            panic!("interrupt after the first cursor checkpoint");
        }
    }
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        a.walker.walk(
            &cursor,
            &WalkRequest {
                history: &a.runtime.spending_history,
                files: &files,
                prices: &a.prices,
                now_secs: now,
                origin_overrides: &HashMap::new(),
                user_inputs: &[],
                scope: None,
                spec: &HeadlineSpec::default(),
            },
            &mut Interrupt,
        );
    }));
    assert!(interrupted.is_err());
    let partial = read_spending_cache(&cursor);
    assert!(!partial.files.is_empty() && partial.files.len() < files.len());
    a.walker = SpendingWalker::new();
    a.walk(now, true);
    assert!(
        a.walker.cache.rebuild_days.is_empty(),
        "resuming a cold fill is not a restore"
    );
    assert!(read_spending_cache(&cursor).rebuild_days.is_empty());
    std::fs::write(
        a.path("32345678-1234-4234-8234-123456789abc"),
        claude_line("2026-06-10", 4.0, "c", "c"),
    )
    .unwrap();
    a.walk(now + 1, true);
    assert_eq!(
        a.walker.cache.rebuild_days,
        BTreeSet::from(["2026-06-10".parse::<Date>().unwrap()])
    );
}

#[test]
#[cfg(unix)]
fn ledger_read_failure_preserves_closed_growth_but_not_open_days() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        return; // Root bypasses mode 000, so the fixture cannot deny the read.
    }
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let history = a.runtime.spending_history.spend(2026);
    std::fs::set_permissions(&history, std::fs::Permissions::from_mode(0o0)).unwrap();
    std::fs::write(
        a.path(SECOND),
        format!(
            "{}\n{}\n",
            claude_line("2026-06-10", 3.0, "b", "b"),
            claude_line("2026-06-30", 4.0, "today", "today")
        ),
    )
    .unwrap();
    a.walker = SpendingWalker::new();
    a.walk(now, true);
    std::fs::set_permissions(&history, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path()).rebuild_days,
        BTreeSet::from(["2026-06-10".parse::<Date>().unwrap()]),
        "only closed growth survives a ledger read error"
    );
}

#[test]
fn rebuild_never_appends_an_open_day_in_either_scope() {
    for scope in [RebuildScope::All, RebuildScope::Pending] {
        let dir = TempDir::new().unwrap();
        let mut a = Fixture::new(dir.path(), "a");
        let now = seconds("2026-06-30T12:00:00Z");
        std::fs::write(
            a.path(SESSION),
            format!(
                "{}\n{}\n",
                claude_line("2026-06-10", 2.0, "a", "a"),
                claude_line("2026-06-30", 4.0, "today", "today")
            ),
        )
        .unwrap();
        a.walk(now, true);
        let today = "2026-06-30".parse::<Date>().unwrap();
        let mut open = a.lines()[0].clone();
        open["day"] = json!(today);
        let paths = &a.runtime.spending_history;
        history::append(paths, &paths.spend(2026), format!("\n{open}\n").as_bytes()).unwrap();
        a.walker.cache.rebuild_days.insert(today);
        let before = a.lines();
        let files = a.discover(now);
        let (_, report) = run_scoped_rebuild(&mut a, now, &files, scope, &mut |_| {});
        assert!(
            report.unwrap().rebuilt.is_empty(),
            "an open day must never be frozen by rebuild"
        );
        assert_eq!(a.lines(), before);
    }
}

#[test]
fn pending_rebuild_skips_files_last_written_before_the_earliest_pending_day() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let old = a.path(SECOND);
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    std::fs::write(&old, claude_line("2026-05-01", 3.0, "b", "b")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds("2026-05-01T13:00:00Z")),
        )
        .unwrap();
    a.walk(now, true);
    assert_eq!(a.lines().len(), 2);
    a.walker
        .cache
        .rebuild_days
        .insert("2026-06-10".parse::<Date>().unwrap());
    let files = a.discover(now);
    let (_, report) = run_scoped_rebuild(&mut a, now, &files, RebuildScope::Pending, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report.files, 1,
        "a file last written before the pending day cannot hold it"
    );
    assert_eq!(report.unchanged, 1);
}

#[test]
fn growth_in_one_file_is_not_canceled_by_another_files_shrink() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path(SESSION);
    let mut original: Value =
        serde_json::from_str(&claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    original["message"]["usage"]["input_tokens"] = json!(1000);
    std::fs::write(&path, original.to_string()).unwrap();
    a.walk(now, true);
    original["message"]["usage"]["input_tokens"] = json!(1);
    std::fs::write(&path, original.to_string()).unwrap();
    let cached = a
        .walker
        .cache
        .files
        .get_mut(path.to_str().unwrap())
        .unwrap();
    cached.cursor = SpendCursor::default();
    cached.stat.len = u64::MAX;
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    a.walk(now, true);
    assert_eq!(
        a.walker.cache.rebuild_days,
        BTreeSet::from(["2026-06-10".parse::<Date>().unwrap()]),
        "each file grows independently"
    );
}

#[test]
fn restored_cost_only_usage_marks_a_saved_day_pending() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    a.adapter = crate::agents::definition_by_kind("pi").unwrap();
    let sessions = a.home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{SESSION}.jsonl")),
        json!({
            "type": "usage", "timestamp": "2026-06-10T15:00:00Z",
            "usage": {"input": 10, "cost": {"total": 2.0}}
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(a.walk(now, true).spending.total.month.usd, 2.0);
    let restored = sessions.join(format!("{SECOND}.jsonl"));
    std::fs::write(
        &restored,
        json!({
            "type": "usage", "timestamp": "2026-06-10T15:00:00Z",
            "usage": {"cost": {"total": 3.0}}
        })
        .to_string(),
    )
    .unwrap();
    let entries = a.adapter.parse_spend(&restored, None, &a.prices).entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost_usd, 3.0);
    assert_eq!(
        entries[0].input + entries[0].output + entries[0].cache_write + entries[0].cache_read,
        0
    );
    a.walk(now, true);
    assert_eq!(
        a.walker.cache.rebuild_days,
        BTreeSet::from(["2026-06-10".parse::<Date>().unwrap()]),
        "logged dollars without tokens are still new usage"
    );
}

#[test]
fn rebuild_leaves_undiscovered_accounts_unreported() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path(SESSION);
    std::fs::write(&path, claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let mut foreign = a.lines()[0].clone();
    foreign["account"] = json!("claude/another-home");
    foreign["day"] = json!("2026-06-11");
    let paths = &a.runtime.spending_history;
    history::append(
        paths,
        &paths.spend(2026),
        format!("\n{foreign}\n").as_bytes(),
    )
    .unwrap();
    std::fs::remove_file(path).unwrap();
    let files = a.discover(now);
    let (_, report) = run_rebuild(&mut a, now, &files, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report.kept.len(),
        1,
        "only this home's discovered account is kept"
    );
    assert_eq!(report.days, ["2026-06-10".parse::<Date>().unwrap()]);
    assert_eq!(a.lines().len(), 2);
}

#[test]
fn requested_rebuild_reports_a_busy_producer_without_parsing() {
    let dir = TempDir::new().unwrap();
    let a = Fixture::new(dir.path(), "a");
    a.runtime.ensure_shared_dirs().unwrap();
    let _hold =
        crate::disk::lock::WorkspaceLock::acquire(&a.runtime.shared_spending_lock()).unwrap();
    let mut parsed = 0;
    let report = rebuild_spending_history(
        &a.runtime,
        &HeadlineSpec::default(),
        RebuildScope::All,
        &mut |_| parsed += 1,
    );
    assert!(
        matches!(report, Err(RebuildErr::ProducerBusy)),
        "a requested rebuild must win the producer lock"
    );
    assert_eq!(parsed, 0);
}

#[test]
fn token_growth_survives_a_local_walk_and_requests_one_helper_per_reconciliation() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    let files = a.discover(now);
    let local = a.walk_files(&files, now, false);
    let day = "2026-06-10".parse::<Date>().unwrap();
    assert_eq!(a.walker.cache.rebuild_days, BTreeSet::from([day]));
    assert!(!local.stats.rebuild_requested);
    assert!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path())
            .rebuild_days
            .is_empty()
    );
    let published = a.walk_files(&files, now, true);
    assert!(
        published.stats.rebuild_requested,
        "the unconsumed reconciliation must request the helper"
    );
    assert_eq!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path()).rebuild_days,
        BTreeSet::from([day])
    );
    assert!(!a.walk_files(&files, now + 1, true).stats.rebuild_requested);
    let files = a.discover(now + 900);
    assert!(
        a.walk_files(&files, now + 900, true)
            .stats
            .rebuild_requested
    );
}

#[test]
fn unusable_format_marks_consume_reconciliation_without_requesting_a_helper() {
    for mark in [Some("2\n"), Some("broken\n"), None] {
        let dir = TempDir::new().unwrap();
        let mut a = Fixture::new(dir.path(), "a");
        let now = seconds("2026-06-30T12:00:00Z");
        std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
        a.walk(now, true);
        std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
        let format = a.runtime.spending_history.format();
        std::fs::remove_file(&format).unwrap();
        if let Some(mark) = mark {
            std::fs::write(&format, mark).unwrap();
        } else {
            std::fs::create_dir(&format).unwrap();
        }
        let files = a.discover(now);
        let result = a.walk_files(&files, now, true);
        assert!(
            !result.stats.rebuild_requested,
            "an unusable format mark must suppress the helper: {mark:?}"
        );
        assert_eq!(
            a.walker.rebuild_checked_at,
            a.walker.discovery.last_complete()
        );
        let day = "2026-06-10".parse::<Date>().unwrap();
        assert_eq!(
            read_spending_cache(&a.runtime.shared_spending_cursor_path()).rebuild_days,
            BTreeSet::from([day])
        );
        if mark.is_none() {
            std::fs::remove_dir(&format).unwrap();
        }
        std::fs::write(&format, "1\n").unwrap();
        assert!(
            !a.walk_files(&files, now + 1, true).stats.rebuild_requested,
            "repairing the mark must not reuse a consumed reconciliation"
        );
        let result = a.walk(now + 900, true);
        assert!(result.stats.rebuild_requested);
        assert_eq!(a.walker.cache.rebuild_days, BTreeSet::from([day]));
    }
}

#[test]
fn a_missing_format_mark_allows_the_helper_without_creating_the_mark() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    let hold = history::try_maintain(&a.runtime.spending_history)
        .unwrap()
        .unwrap();
    std::fs::remove_file(a.runtime.spending_history.format()).unwrap();
    let result = a.walk(now, true);
    assert!(result.stats.rebuild_requested);
    assert!(!a.runtime.spending_history.format().exists());
    assert_eq!(a.walker.cache.rebuild_days.len(), 1);
    drop(hold);
}

#[test]
fn recent_appends_heals_shrinks_and_deletions_do_not_trigger_but_later_growth_does() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path(SESSION);
    std::fs::write(
        &path,
        format!("{}\n", claude_line("2026-06-10", 2.0, "old", "old")),
    )
    .unwrap();
    a.walk(now, true);
    let mut negatives = Vec::new();
    append_line(
        &path,
        &claude_line_ts("2026-06-30T11:00:00Z", 3.0, "recent", "recent"),
    );
    a.walk(now, true);
    negatives.push(a.walker.cache.rebuild_days.is_empty());
    let cached = a
        .walker
        .cache
        .files
        .get_mut(path.to_str().unwrap())
        .unwrap();
    cached.cursor = SpendCursor::default();
    cached.stat.len = u64::MAX;
    let healed = a.walk(now, true);
    assert_eq!(
        healed.stats.parse_jobs, 1,
        "the heal must really reparse the file"
    );
    negatives.push(a.walker.cache.rebuild_days.is_empty());
    std::fs::write(
        &path,
        claude_line_ts("2026-06-30T11:00:00Z", 3.0, "recent", "recent"),
    )
    .unwrap();
    a.walk(now, true);
    negatives.push(a.walker.cache.rebuild_days.is_empty());
    std::fs::remove_file(path).unwrap();
    a.walk(now, true);
    negatives.push(a.walker.cache.rebuild_days.is_empty());
    std::fs::write(
        a.path(SECOND),
        claude_line("2026-06-10", 4.0, "late", "late"),
    )
    .unwrap();
    a.walk(now, true);
    assert_eq!(
        a.walker.cache.rebuild_days.len(),
        1,
        "only growth on the saved day triggers"
    );
    assert_eq!(negatives, [true; 4]);
}

#[test]
fn local_growth_persists_below_the_floor_after_the_reconciliation_was_consumed() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let checked = a.walker.rebuild_checked_at;
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    let files = a.walker.discovery.discover(
        std::iter::once((
            crate::agents::ProviderLogin::default_for(crate::ids::AgentKind::new_unchecked(
                "claude",
            )),
            claude_adapter(),
            BTreeMap::from([(
                "CLAUDE_CONFIG_DIR".into(),
                a.home.to_string_lossy().into_owned(),
            )]),
        )),
        now,
    );
    assert_eq!(a.walker.discovery.last_complete(), checked);
    a.walk_files(&files, now, false);
    let result = a.walk_files(&files, now + 1, true);
    assert!(
        !result.stats.rebuild_requested,
        "the already-consumed cadence must not fire again"
    );
    assert_eq!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path()).rebuild_days,
        BTreeSet::from(["2026-06-10".parse::<Date>().unwrap()]),
        "publishing must persist local growth even below the ordinary write floor"
    );
    let cursor = a.runtime.shared_spending_cursor_path();
    let saved = std::fs::read(&cursor).unwrap();
    std::fs::write(
        a.path(SECOND),
        format!(
            "{}\n{}\n",
            claude_line("2026-06-10", 3.0, "b", "b"),
            claude_line("2026-06-30", 4.0, "recent", "recent"),
        ),
    )
    .unwrap();
    let result = a.walk_files(&files, now + 2, true);
    assert_eq!(result.stats.parse_jobs, 1, "the cursor must become dirty");
    assert!(
        !result.stats.cache_written,
        "an unchanged pending set must not bypass the persist floor"
    );
    assert_eq!(std::fs::read(&cursor).unwrap(), saved);
    assert!(
        a.walk_files(&files, now + 1 + SPENDING_PERSIST_MIN_INTERVAL, true)
            .stats
            .cache_written,
        "ordinary dirty cursor changes persist once the floor expires"
    );
}

#[test]
fn newer_cursor_does_not_request_a_helper_without_a_persisted_pending_day() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let cursor = a.runtime.shared_spending_cursor_path();
    let mut newer = read_spending_cache(&cursor);
    newer.version += 1;
    std::fs::write(&cursor, serde_json::to_vec(&newer).unwrap()).unwrap();
    assert_eq!(
        cache::peek_cache_version(&cursor),
        Some(SPENDING_CACHE_VERSION + 1)
    );
    a.walker = SpendingWalker::new();
    let reset = a.walk(now, true);
    assert!(!reset.stats.rebuild_requested);
    assert!(a.walker.cache.rebuild_days.is_empty());
    assert_eq!(a.walker.cache.files.len(), 1);
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    let result = a.walk(now, true);
    assert!(
        !result.stats.rebuild_requested,
        "a successful downgrade skip is not a persisted pending cursor"
    );
    assert_eq!(
        a.walker.cache.rebuild_days,
        BTreeSet::from(["2026-06-10".parse::<Date>().unwrap()])
    );
    assert_eq!(
        cache::peek_cache_version(&cursor),
        Some(SPENDING_CACHE_VERSION + 1)
    );
}

#[test]
fn lost_and_old_version_cursors_suppress_the_first_walk_not_subsequent_growth() {
    let dir = TempDir::new().unwrap();
    let now = seconds("2026-06-30T12:00:00Z");
    let mut observations = Vec::new();
    for missing in [false, true] {
        let mut a = Fixture::new(dir.path(), if missing { "missing" } else { "old" });
        let path = a.path(SESSION);
        std::fs::write(
            &path,
            format!("{}\n", claude_line("2026-06-10", 2.0, "old", "old")),
        )
        .unwrap();
        a.walk(now, true);
        let cursor = a.runtime.shared_spending_cursor_path();
        if missing {
            std::fs::remove_file(&cursor).unwrap();
        } else {
            let mut cache = read_spending_cache(&cursor);
            cache.version = SPENDING_CACHE_VERSION - 1;
            std::fs::write(&cursor, serde_json::to_vec(&cache).unwrap()).unwrap();
            let wire: Value = serde_json::from_slice(&std::fs::read(&cursor).unwrap()).unwrap();
            assert_eq!(wire["version"], SPENDING_CACHE_VERSION - 1);
        }
        std::fs::write(
            a.path(SECOND),
            claude_line("2026-06-10", 3.0, "restored", "restored"),
        )
        .unwrap();
        a.walker = SpendingWalker::new();
        a.walk(now, true);
        let suppressed = a.walker.cache.rebuild_days.is_empty();
        append_line(&path, &claude_line("2026-06-10", 4.0, "later", "later"));
        a.walk(now, true);
        observations.push((suppressed, a.walker.cache.rebuild_days.len()));
    }
    assert_eq!(observations, [(true, 1), (true, 1)]);
}

#[test]
fn pending_rebuild_publishes_restored_dollars_clears_pending_and_leaves_other_days_byte_exact() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let original = a.path(SESSION);
    std::fs::write(
        &original,
        format!("{}\n", claude_line("2026-06-10", 2.0, "a", "a")),
    )
    .unwrap();
    append_line(&original, &claude_line("2026-06-09", 7.0, "other", "other"));
    a.walk(now, true);
    let history_path = a.runtime.spending_history.spend(2026);
    let untouched = std::fs::read_to_string(&history_path)
        .unwrap()
        .lines()
        .find(|line| line.contains("\"day\":\"2026-06-09\""))
        .unwrap()
        .to_owned();
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    a.walk(now, true);
    let files = a.discover(now);
    let (result, report) =
        run_scoped_rebuild(&mut a, now, &files, RebuildScope::Pending, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(report.rebuilt.len(), 1);
    assert_eq!(report.days, ["2026-06-10".parse::<Date>().unwrap()]);
    assert_eq!(report.files, 2);
    assert_eq!(report.rebuilt[0].before_usd, Some(2.0));
    assert_eq!(report.rebuilt[0].after_usd, 5.0);
    assert_eq!(result.spending.total.month.usd, 12.0);
    assert!(
        !result.stats.rebuild_requested,
        "a rebuild walk must not request another helper"
    );
    assert!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path())
            .rebuild_days
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(history_path)
            .unwrap()
            .lines()
            .find(|line| line.contains("\"day\":\"2026-06-09\"")),
        Some(untouched.as_str())
    );
    assert_eq!(a.lines().len(), 3);
}

#[test]
fn checkpoints_never_persist_new_usage_without_its_pending_day() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let restored = a.path(SECOND);
    std::fs::write(&restored, claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    let files = a
        .discover(now)
        .into_iter()
        .filter(|file| file.path == restored)
        .collect::<Vec<_>>();
    a.walker.last_persisted_now_secs = None;
    let cursor = a.runtime.shared_spending_cursor_path();
    let day = "2026-06-10".parse::<Date>().unwrap();
    struct Checkpoint<'a> {
        cursor: &'a Path,
        restored: &'a Path,
        day: Date,
        intervals: usize,
    }
    impl WalkObserver for Checkpoint<'_> {
        fn on_interval(&mut self, _: &SpendingDiskCache) {
            self.intervals += 1;
            let disk = read_spending_cache(self.cursor);
            assert!(
                !disk.files.contains_key(self.restored.to_str().unwrap())
                    || disk.rebuild_days.contains(&self.day),
                "a crash after a checkpoint must not lose the rebuild trigger"
            );
        }
    }
    let mut observer = Checkpoint {
        cursor: &cursor,
        restored: &restored,
        day,
        intervals: 0,
    };
    a.walker.walk(
        &cursor,
        &WalkRequest {
            history: &a.runtime.spending_history,
            files: &files,
            prices: &a.prices,
            now_secs: now,
            origin_overrides: &HashMap::new(),
            user_inputs: &[],
            scope: None,
            spec: &HeadlineSpec::default(),
        },
        &mut observer,
    );
    assert!(observer.intervals > 0);
    let disk = read_spending_cache(&cursor);
    assert!(disk.files.contains_key(restored.to_str().unwrap()));
    assert_eq!(disk.rebuild_days, BTreeSet::from([day]));
}

#[test]
fn pending_days_expire_with_the_spend_window_even_when_maintenance_is_busy() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    let day = "2026-06-10".parse::<Date>().unwrap();
    a.walker.cache.rebuild_days = BTreeSet::from([day, "2024-06-10".parse().unwrap()]);
    a.walker.cache.mark_changed();
    let hold = history::try_maintain(&a.runtime.spending_history)
        .unwrap()
        .unwrap();
    a.walk(now, true);
    assert_eq!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path()).rebuild_days,
        BTreeSet::from([day])
    );
    drop(hold);
}

#[test]
fn pending_days_survive_busy_history_and_unread_accounts_until_a_successful_retry() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    std::fs::write(a.path(SESSION), claude_line("2026-06-10", 2.0, "a", "a")).unwrap();
    a.walk(now, true);
    std::fs::write(a.path(SECOND), claude_line("2026-06-10", 3.0, "b", "b")).unwrap();
    a.walk(now, true);
    let day = "2026-06-10".parse::<Date>().unwrap();
    a.walker.cache.rebuild_days.insert(day);
    a.walker.cache.mark_changed();
    let files = a.discover(now);
    let hold = history::try_maintain(&a.runtime.spending_history)
        .unwrap()
        .unwrap();
    let mut ticks = 0;
    let (_, report) = run_scoped_rebuild(&mut a, now, &files, RebuildScope::Pending, &mut |_| {
        ticks += 1
    });
    assert!(matches!(report, Err(RebuildErr::MaintenanceUnavailable)));
    assert_eq!(ticks, files.len(), "busy history must skip the cold parse");
    assert_eq!(a.walker.cache.rebuild_days, BTreeSet::from([day]));
    drop(hold);
    for mark in ["2\n", "broken\n"] {
        std::fs::write(a.runtime.spending_history.format(), mark).unwrap();
        let (_, report) =
            run_scoped_rebuild(&mut a, now, &files, RebuildScope::Pending, &mut |_| {});
        assert!(matches!(report, Err(RebuildErr::FormatUnusable(_))));
        assert_eq!(a.walker.cache.rebuild_days, BTreeSet::from([day]));
    }
    std::fs::write(a.runtime.spending_history.format(), "1\n").unwrap();
    let unread = a.path("32345678-1234-4234-8234-123456789abc");
    std::fs::write(&unread, "").unwrap();
    let files = a.discover(now);
    std::fs::remove_file(unread).unwrap();
    let (_, report) = run_scoped_rebuild(&mut a, now, &files, RebuildScope::Pending, &mut |_| {});
    let report = report.unwrap();
    assert_eq!(
        report
            .deferred_days
            .iter()
            .map(|deferred| deferred.day)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([day])
    );
    assert!(report.rebuilt.is_empty() && report.kept.is_empty());
    assert_eq!(a.walker.cache.rebuild_days, BTreeSet::from([day]));
    let files = a.discover(now);
    let (_, report) = run_scoped_rebuild(&mut a, now, &files, RebuildScope::Pending, &mut |_| {});
    assert_eq!(report.unwrap().rebuilt.len(), 1);
    assert!(
        read_spending_cache(&a.runtime.shared_spending_cursor_path())
            .rebuild_days
            .is_empty()
    );
}
