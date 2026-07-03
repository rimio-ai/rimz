//! List and show loop tasks plus recorded run details.

use super::*;

const NOTE_MAX: usize = 60;

// ---- list -------------------------------------------------------------------

pub(super) fn list() -> Result<()> {
    let tasks = instances::load_all();
    let mut out = ui::out();
    if tasks.is_empty() {
        writeln!(out, "no loop tasks; add one with `rimz loop add`")?;
        return Ok(());
    }
    let stats = run_log::stats(&state_home());
    let now = Timestamp::now();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let mut table = ui::Table::new([
        "NAME", "SOURCE", "SPEC", "SCHEDULE", "NEXT", "ROOM", "RUNS", "LAST RUN", "RESULT", "NOTE",
        "ROOT",
    ])
    .right(&[6]);
    for (name, (entry, source)) in &tasks {
        let parsed = schedule::parse_schedule(name, entry);
        let when = match &parsed {
            Ok(schedule) => schedule.describe(),
            Err(err) => format!("invalid: {err}"),
        };
        let root = entry.resolved_root();
        let runtime = runtime_for_root(&root);
        let state = if runtime.as_ref().is_some_and(fresh_sidebar_present) {
            "open"
        } else {
            "no room"
        };
        let stamps = runtime
            .as_ref()
            .map(rimz::harness::schedule::last_stamps)
            .unwrap_or_default();
        let window_reset = window_reset_for(entry);
        let next = parsed
            .ok()
            .and_then(|parsed| {
                next_fire_text(
                    name,
                    &parsed.schedule,
                    &stamps,
                    &now_zoned,
                    now,
                    window_reset,
                )
            })
            .map(ui::cell)
            .unwrap_or_else(|| ui::cell("-").dash());
        let task_stats = stats.get(name);
        let runs = task_stats.map_or(0, |stats| stats.runs);
        let last_run = if let Some(stats) = task_stats {
            ui::cell(ui::rel_age(stats.last.at, now))
        } else {
            ui::cell("-").dash()
        };
        let result = task_stats
            .map(|stats| loop_result_cell(stats.last.result, stats.streak))
            .unwrap_or_else(|| ui::cell("-").dash());
        let note = task_stats
            .map(|stats| {
                ui::cell(record_note(&stats.last).unwrap_or_else(|| "-".to_owned())).dash()
            })
            .unwrap_or_else(|| ui::cell("-").dash());
        let root_text = root.to_string_lossy();
        table.row([
            ui::cell(name.as_str()).fg(ui::palette::ACCENT),
            ui::cell(source.label()),
            ui::cell(task_subject(entry)),
            ui::cell(when),
            next,
            ui::cell(state),
            ui::cell(runs.to_string()),
            last_run,
            result,
            note,
            ui::cell(ui::home_relative(root_text.as_ref())),
        ]);
    }
    table.render(&mut out)?;
    Ok(())
}

pub(super) fn room_open(root: &Path) -> bool {
    runtime_for_root(root)
        .as_ref()
        .is_some_and(fresh_sidebar_present)
}

fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    RuntimePaths::for_workspace(WorkspaceId::from_project_root(root)).ok()
}

fn next_fire_text(
    name: &str,
    schedule: &schedule::Schedule,
    stamps: &BTreeMap<String, Timestamp>,
    now_zoned: &jiff::Zoned,
    now: Timestamp,
    window_reset: Option<Timestamp>,
) -> Option<String> {
    let next = schedule.next_after(*stamps.get(name)?, now_zoned, window_reset)?;
    Some(ui::rel_until(next, now))
}

fn window_reset_for(entry: &TaskEntry) -> Option<Timestamp> {
    if !entry.at_reset {
        return None;
    }
    let kind = entry
        .spec
        .as_deref()
        .and_then(rimz::harness::spec::ping_kind)?;
    window_reset_at(entry, kind).ok().flatten()
}

pub(super) fn show(args: ShowArgs) -> Result<()> {
    let (entry, source) = instances::load_entry(&args.name).ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let stamps = runtime
        .as_ref()
        .map(rimz::harness::schedule::last_stamps)
        .unwrap_or_default();
    let room = if runtime.as_ref().is_some_and(fresh_sidebar_present) {
        "open"
    } else {
        "no room"
    };
    let parsed = schedule::parse_schedule(&args.name, &entry);
    let schedule_text = match &parsed {
        Ok(parsed) => parsed.describe(),
        Err(err) => format!("invalid: {err}"),
    };
    let now = Timestamp::now();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let window_reset = window_reset_for(&entry);
    let next = parsed
        .as_ref()
        .ok()
        .and_then(|parsed| {
            next_fire_text(
                &args.name,
                &parsed.schedule,
                &stamps,
                &now_zoned,
                now,
                window_reset,
            )
        })
        .unwrap_or_else(|| "-".to_owned());

    let mut out = ui::out();
    writeln!(out, "loop `{}`", args.name)?;
    let mut kv = ui::KeyVals::new();
    kv.push("source", ui::cell(source.label()));
    kv.push("task", ui::cell(task_subject(&entry)));
    kv.push("schedule", ui::cell(schedule_text));
    kv.push(
        "root",
        ui::cell(ui::home_relative(root.to_string_lossy().as_ref())),
    );
    kv.push("room", ui::cell(room));
    kv.push("next", ui::cell(next));
    kv.render(&mut out)?;

    let records = run_log::task_records(&state_home(), &args.name);
    if records.is_empty() {
        writeln!(out, "no runs recorded; try `rimz loop fire {}`", args.name)?;
        return Ok(());
    }

    writeln!(out)?;
    let mut table = ui::Table::new(["WHEN", "MODE", "RESULT", "TIME", "EXIT", "NOTE"]);
    let rows = collapsed_run_rows(&records);
    let start = rows.len().saturating_sub(args.runs);
    for row in &rows[start..] {
        let record = row.latest;
        table.row([
            ui::cell(ui::rel_age(record.at, now)),
            ui::cell(row.key.mode.map_or("-", LoopRunMode::label)).dash(),
            loop_result_cell(row.key.result, row.count),
            ui::cell(
                record
                    .duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            ui::cell(row.key.exit.as_deref().unwrap_or("-")).dash(),
            ui::cell(row.key.note.as_deref().unwrap_or("-")).dash(),
        ]);
    }
    table.render(&mut out)?;

    let (detail_idx, failure_idx) = detail_indices(&records);
    if let Some(detail) = detail_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, detail, "last run detail", now)?;
    }
    if let Some(failure) = failure_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, failure, "last failure detail", now)?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunRowKey {
    mode: Option<LoopRunMode>,
    result: LoopRunResult,
    exit: Option<String>,
    note: Option<String>,
}

impl RunRowKey {
    fn new(record: &LoopRunRecord) -> Self {
        Self {
            mode: record.mode,
            result: record.result,
            exit: record_exit(record),
            note: record_note(record),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollapsedRunRow<'a> {
    key: RunRowKey,
    latest: &'a LoopRunRecord,
    count: usize,
}

fn collapsed_run_rows(records: &[LoopRunRecord]) -> Vec<CollapsedRunRow<'_>> {
    let mut rows = Vec::<CollapsedRunRow<'_>>::new();
    for record in records {
        let key = RunRowKey::new(record);
        if let Some(row) = rows.last_mut().filter(|row| row.key == key) {
            row.count += 1;
            if record.at >= row.latest.at {
                row.latest = record;
            }
        } else {
            rows.push(CollapsedRunRow {
                key,
                latest: record,
                count: 1,
            });
        }
    }
    rows
}

fn loop_result_cell(result: LoopRunResult, count: usize) -> ui::Cell {
    let style = match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => ui::palette::GOOD,
        LoopRunResult::Failed | LoopRunResult::TimedOut | LoopRunResult::Errored => {
            ui::palette::ALARM
        }
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::SkippedWindow => ui::palette::WARN,
        LoopRunResult::CheckSkipped => ui::palette::MUTED,
    };
    let label = if count > 1 {
        format!("{} x{count}", result.label())
    } else {
        result.label().to_owned()
    };
    ui::cell(label).fg(style)
}

pub(super) fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else if ms < 60_000 {
        format!("{}s", ms / 1_000)
    } else {
        format!("{}m", ms / 60_000)
    }
}

fn record_exit(record: &LoopRunRecord) -> Option<String> {
    if let Some(check) = &record.check {
        if check.timed_out {
            return Some("timeout".to_owned());
        }
        return check.code.map(|code| code.to_string());
    }
    record
        .run_id
        .as_ref()
        .and_then(|_| spawn_exit_code(record.result))
        .map(|code| code.to_string())
}

fn spawn_exit_code(result: LoopRunResult) -> Option<i32> {
    match result {
        LoopRunResult::Completed => Some(0),
        LoopRunResult::Failed => Some(1),
        LoopRunResult::TimedOut => Some(124),
        LoopRunResult::Canceled => Some(130),
        _ => None,
    }
}

fn record_note(record: &LoopRunRecord) -> Option<String> {
    let note = record
        .error
        .as_deref()
        .map(first_line)
        .or_else(|| check_failure_line(record))
        .or_else(|| record.last_message.as_deref().map(first_line))
        .or_else(|| record.target.as_deref().map(first_line))?;
    Some(truncate_note(note, NOTE_MAX))
}

fn check_failure_line(record: &LoopRunRecord) -> Option<&str> {
    let check = record.check.as_ref()?;
    if !check.timed_out && check.code == Some(0) {
        return None;
    }
    check
        .output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn truncate_note(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let clipped: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() && max >= 3 {
        format!("{}...", clipped.chars().take(max - 3).collect::<String>())
    } else {
        clipped
    }
}

fn record_has_detail(record: &LoopRunRecord) -> bool {
    record.check.is_some()
        || record.error.is_some()
        || record.last_message.is_some()
        || record.run_id.is_some()
}

fn record_is_failure(record: &LoopRunRecord) -> bool {
    matches!(
        record.result,
        LoopRunResult::Errored | LoopRunResult::Failed | LoopRunResult::TimedOut
    )
}

fn detail_indices(records: &[LoopRunRecord]) -> (Option<usize>, Option<usize>) {
    let detail_idx = records.iter().rposition(record_has_detail);
    let failure_idx = records
        .iter()
        .enumerate()
        .rev()
        .find(|(idx, record)| {
            Some(*idx) != detail_idx && record_is_failure(record) && record_has_detail(record)
        })
        .map(|(idx, _record)| idx);
    (detail_idx, failure_idx)
}

fn render_record_detail(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
    title: &str,
    now: Timestamp,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{title} ({}, {}, {})",
        record.result.label(),
        ui::rel_age(record.at, now),
        record.mode.map_or("legacy", LoopRunMode::label)
    )?;
    if let Some(check) = &record.check {
        let status = if check.timed_out {
            "timeout".to_owned()
        } else {
            check
                .code
                .map(|code| format!("exit {code}"))
                .unwrap_or_else(|| "signal".to_owned())
        };
        writeln!(out, "last run output ({status}):")?;
        if check.output.trim().is_empty() {
            writeln!(out, "-")?;
        } else {
            writeln!(out, "{}", check.output.trim_end())?;
        }
    }
    let run_record = record
        .run_id
        .as_deref()
        .and_then(|run_id| run_record_for(entry, run_id));
    if let Some(error) = &record.error {
        writeln!(out, "error:")?;
        writeln!(out, "{error}")?;
    }
    if let Some(last_message) = record.last_message.as_ref().or_else(|| {
        run_record
            .as_ref()
            .and_then(|record| record.last_message.as_ref())
    }) {
        writeln!(out, "last message:")?;
        writeln!(out, "{last_message}")?;
    }
    if let Some(run_id) = &record.run_id {
        writeln!(out, "run: {run_id}")?;
        if let Some(tail) = run_record
            .as_ref()
            .and_then(|record| record.failure_tail.as_deref())
            .filter(|tail| !tail.trim().is_empty())
        {
            writeln!(out, "output tail:")?;
            writeln!(out, "{tail}")?;
        }
        if let Some(transcript) = run_record
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref())
        {
            writeln!(out, "transcript: {transcript}")?;
        }
    }
    Ok(())
}

fn run_record_for(entry: &TaskEntry, run_id: &str) -> Option<rimz::harness::run::RunRecord> {
    let run_id = rimz::RunId::parse(run_id).ok()?;
    let paths = StatePaths::under(
        WorkspaceId::from_project_root(&entry.resolved_root()),
        &state_home(),
    )
    .ok()?;
    rimz::harness::run::load(&paths, &run_id).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(second: i64, result: LoopRunResult) -> LoopRunRecord {
        LoopRunRecord {
            task: "wake".to_owned(),
            at: Timestamp::from_second(second).expect("timestamp"),
            result,
            mode: None,
            duration_ms: None,
            error: None,
            check: None,
            run_id: None,
            last_message: None,
            target: None,
        }
    }

    #[test]
    fn check_failure_line_uses_last_non_empty_failed_check_line() {
        let mut failed = record(10, LoopRunResult::Failed);
        failed.check = Some(CheckRecord {
            code: Some(127),
            timed_out: false,
            output: "ignored\n\nmissing command\n".to_owned(),
        });
        assert_eq!(check_failure_line(&failed), Some("missing command"));

        let mut passed = record(11, LoopRunResult::Completed);
        passed.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        assert_eq!(check_failure_line(&passed), None);
    }

    #[test]
    fn record_note_prefers_error_then_failed_check_output() {
        let mut failed = record(10, LoopRunResult::Failed);
        failed.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "first\ncheck failed".to_owned(),
        });
        failed.last_message = Some("last message".to_owned());
        assert_eq!(record_note(&failed), Some("check failed".to_owned()));

        failed.error = Some("outer error\nignored detail".to_owned());
        assert_eq!(record_note(&failed), Some("outer error".to_owned()));
    }

    #[test]
    fn collapsed_run_rows_merge_adjacent_matching_render_columns() {
        let mut first = record(10, LoopRunResult::Failed);
        first.mode = Some(LoopRunMode::Scheduled);
        first.duration_ms = Some(10);
        first.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "boom".to_owned(),
        });
        let mut second = first.clone();
        second.at = Timestamp::from_second(20).expect("timestamp");
        second.duration_ms = Some(20);
        let mut third = second.clone();
        third.at = Timestamp::from_second(30).expect("timestamp");
        third.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "different".to_owned(),
        });
        let records = vec![first, second, third];

        let rows = collapsed_run_rows(&records);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].count, 2);
        assert_eq!(
            rows[0].latest.at,
            Timestamp::from_second(20).expect("timestamp")
        );
        assert_eq!(rows[0].latest.duration_ms, Some(20));
        assert_eq!(rows[0].key.note.as_deref(), Some("boom"));
        assert_eq!(rows[1].count, 1);
        assert_eq!(rows[1].key.note.as_deref(), Some("different"));
    }

    #[test]
    fn detail_indices_include_prior_failure_when_latest_detail_shadows_it() {
        let mut error = record(10, LoopRunResult::Errored);
        error.error = Some("reading prompt-file\nmissing".to_owned());
        let mut failed = record(20, LoopRunResult::Failed);
        failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
        let records = vec![error, failed];

        assert_eq!(detail_indices(&records), (Some(1), Some(0)));
    }

    #[test]
    fn render_record_detail_titles_status_age_and_mode() {
        let mut detail = record(20, LoopRunResult::Errored);
        detail.mode = Some(LoopRunMode::Manual);
        detail.error = Some("outer error\ninner detail".to_owned());
        let entry = TaskEntry {
            root: PathBuf::from("/tmp/rimz-run"),
            ..TaskEntry::default()
        };
        let mut out = Vec::new();

        render_record_detail(
            &mut out,
            &entry,
            &detail,
            "last failure detail",
            Timestamp::from_second(30).expect("timestamp"),
        )
        .unwrap();

        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("last failure detail (error, "));
        assert!(out.contains(", manual)"));
        assert!(out.contains("outer error\ninner detail"));
    }
}
