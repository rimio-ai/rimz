//! List and show loop tasks plus recorded run details.

use super::*;

const NOTE_MAX: usize = 60;

// ---- list -------------------------------------------------------------------

pub(super) fn list(globals: &GlobalFlags) -> Result<()> {
    let tasks = load_all_tasks(globals)?;
    let pause_entries = pauses::load();
    let mut out = ui::out();
    if tasks.is_empty() {
        writeln!(out, "no loop tasks; add one with `rimz loop add`")?;
        return Ok(());
    }
    let stats = run_log::stats(&state_home());
    let now = Timestamp::now();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let mut groups: BTreeMap<PathBuf, Vec<(&String, &TaskEntry, TaskSource)>> = BTreeMap::new();
    for (name, (entry, source)) in &tasks {
        let root = entry.resolved_root();
        groups.entry(root).or_default().push((name, entry, *source));
    }
    for (idx, (root, entries)) in groups.into_iter().enumerate() {
        let runtime = runtime_for_root(&root);
        let room_is_open = runtime.as_ref().is_some_and(fresh_sidebar_present);
        let stamps = runtime
            .as_ref()
            .map(rimz::harness::schedule::last_stamps)
            .unwrap_or_default();
        if idx > 0 {
            writeln!(out)?;
        }
        write_root_heading(&mut out, &root, room_is_open)?;
        let mut table = ui::Table::new([
            "NAME", "TASK", "SOURCE", "SCHEDULE", "LAST", "STATUS", "NEXT",
        ])
        .indent(2);
        for (name, entry, source) in entries {
            let parsed = schedule::parse_schedule(name, entry);
            let when = match &parsed {
                Ok(schedule) => schedule.describe(),
                Err(err) => format!("invalid: {err}"),
            };
            let window_reset = window_reset_for(entry);
            let pause = pause_entries.get(name);
            let next = match pause.filter(|entry| pauses::is_active(entry, now)) {
                Some(PauseEntry { until: None }) => ui::cell("paused").fg(ui::palette::MUTED),
                Some(PauseEntry { until: Some(until) }) => {
                    ui::cell(format!("paused · {}", ui::rel_until(*until, now)))
                        .fg(ui::palette::MUTED)
                }
                None => parsed
                    .ok()
                    .and_then(|parsed| {
                        next_fire_text(
                            name,
                            &parsed.schedule,
                            &stamps,
                            pause,
                            &now_zoned,
                            now,
                            window_reset,
                        )
                    })
                    .map(ui::cell)
                    .unwrap_or_else(|| ui::cell("-").dash()),
            };
            let (last, status) = stats
                .get(name)
                .map(|stats| last_run_cells(stats, now))
                .unwrap_or_else(|| (ui::cell("-").dash(), ui::cell("-").dash()));
            table.row([
                ui::cell(name.as_str()).fg(ui::palette::ACCENT),
                ui::cell(task_subject(entry)),
                ui::cell(source_label(source)),
                ui::cell(when),
                last,
                status,
                next,
            ]);
        }
        table.render(&mut out)?;
    }
    Ok(())
}

pub(super) fn room_open(root: &Path) -> bool {
    runtime_for_root(root)
        .as_ref()
        .is_some_and(fresh_sidebar_present)
}

fn write_root_heading(
    out: &mut impl Write,
    root: &Path,
    room_is_open: bool,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{} · {}",
        ui::paint(
            ui::palette::ACCENT.bold(),
            &ui::home_relative(root.to_string_lossy().as_ref())
        ),
        ui::paint(room_style(room_is_open), room_label(room_is_open))
    )
}

fn root_with_room(root: &Path, room_is_open: bool) -> String {
    format!(
        "{} · {}",
        ui::home_relative(root.to_string_lossy().as_ref()),
        room_label(room_is_open)
    )
}

fn room_label(room_is_open: bool) -> &'static str {
    if room_is_open { "room open" } else { "no room" }
}

fn room_style(room_is_open: bool) -> anstyle::Style {
    if room_is_open {
        ui::palette::GOOD
    } else {
        ui::palette::MUTED
    }
}

fn schedule_style<T, E>(parsed: std::result::Result<&T, &E>) -> anstyle::Style {
    if parsed.is_ok() {
        anstyle::Style::new()
    } else {
        ui::palette::ALARM
    }
}

fn check_summary(entry: &TaskEntry) -> Option<String> {
    let check = entry.check.as_ref()?;
    if entry.agent.is_some() || entry.wake.is_some() {
        Some(format!(
            "{check} ({} {} on {})",
            action_third_person_verb(entry),
            task_subject(entry),
            check_on_label(entry.on.unwrap_or_default())
        ))
    } else {
        Some(check.clone())
    }
}

pub(super) fn task_run_rule(entry: &TaskEntry) -> String {
    let action = if entry.agent.is_some() || entry.wake.is_some() {
        Some(format!(
            "{} {}",
            action_base_verb(entry),
            task_subject(entry)
        ))
    } else {
        None
    };
    match (entry.check.is_some(), action) {
        (true, Some(action)) => format!(
            "check, then {action} on {}",
            check_on_label(entry.on.unwrap_or_default())
        ),
        (true, None) => "check".to_owned(),
        (false, Some(action)) => action,
        (false, None) => "run".to_owned(),
    }
}

pub(super) fn action_progressive_verb(entry: &TaskEntry) -> &'static str {
    if entry.agent.is_some() {
        "starting"
    } else {
        "waking"
    }
}

pub(super) fn check_skip_decision(entry: &TaskEntry) -> String {
    let subject = task_subject(entry);
    let untouched = if entry.agent.is_some() {
        "not started"
    } else {
        "not woken"
    };
    let condition = match entry.on.unwrap_or_default() {
        CheckOn::Fail => "fails",
        CheckOn::Success => "passes",
    };
    format!("{subject} {untouched}; fires when the check {condition}")
}

fn action_base_verb(entry: &TaskEntry) -> &'static str {
    if entry.agent.is_some() {
        "start"
    } else {
        "wake"
    }
}

fn action_third_person_verb(entry: &TaskEntry) -> &'static str {
    if entry.agent.is_some() {
        "starts"
    } else {
        "wakes"
    }
}

fn check_on_label(on: CheckOn) -> &'static str {
    match on {
        CheckOn::Fail => "fail",
        CheckOn::Success => "success",
    }
}

fn source_label(source: TaskSource) -> String {
    match source {
        TaskSource::Project { state } if state != TrustState::Trusted => {
            format!("project · {}", state.as_str())
        }
        _ => source.label().to_owned(),
    }
}

fn source_detail(source: TaskSource, entry: &TaskEntry) -> String {
    format!(
        "{} — {}",
        source_label(source),
        display_path(&source_path(source, entry))
    )
}

fn source_path(source: TaskSource, entry: &TaskEntry) -> PathBuf {
    match source {
        TaskSource::Config => MachineConfig::loop_path(),
        TaskSource::Project { .. } => project_config_path(&entry.root),
        TaskSource::Instance => instances::path(&state_home()),
    }
}

fn display_path(path: &Path) -> String {
    ui::home_relative(path.to_string_lossy().as_ref())
}

fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    RuntimePaths::for_workspace(WorkspaceId::from_project_root(root)).ok()
}

fn next_fire_text(
    name: &str,
    schedule: &schedule::Schedule,
    stamps: &BTreeMap<String, Timestamp>,
    pause: Option<&PauseEntry>,
    now_zoned: &jiff::Zoned,
    now: Timestamp,
    window_reset: Option<Timestamp>,
) -> Option<String> {
    let stamp = pauses::effective_last_fire(*stamps.get(name)?, pause, now);
    let next = schedule.next_after(stamp, now_zoned, window_reset)?;
    Some(ui::rel_until(next, now))
}

pub(super) fn task_next_fire_text(
    name: &str,
    entry: &TaskEntry,
    pause: Option<&PauseEntry>,
    now: Timestamp,
) -> Option<String> {
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root)?;
    let stamps = rimz::harness::schedule::last_stamps(&runtime);
    let parsed = schedule::parse_schedule(name, entry).ok()?;
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    next_fire_text(
        name,
        &parsed.schedule,
        &stamps,
        pause,
        &now_zoned,
        now,
        window_reset_for(entry),
    )
}

fn window_reset_for(entry: &TaskEntry) -> Option<Timestamp> {
    if entry.every.as_deref() != Some("reset") {
        return None;
    }
    let kind = entry
        .agent
        .as_deref()
        .and_then(rimz::harness::spec::ping_kind)?;
    window_reset_at(entry, kind).ok().flatten()
}

pub(super) fn show(args: ShowArgs, globals: &GlobalFlags) -> Result<()> {
    let (entry, source) = load_task(&args.name, globals)?.ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let stamps = runtime
        .as_ref()
        .map(rimz::harness::schedule::last_stamps)
        .unwrap_or_default();
    let room_is_open = runtime.as_ref().is_some_and(fresh_sidebar_present);
    let parsed = schedule::parse_schedule(&args.name, &entry);
    let schedule_text = match &parsed {
        Ok(parsed) => parsed.describe(),
        Err(err) => format!("invalid: {err}"),
    };
    let now = Timestamp::now();
    let pause = pauses::load().remove(&args.name);
    let active_pause = pause.as_ref().filter(|entry| pauses::is_active(entry, now));
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let window_reset = window_reset_for(&entry);
    let next = parsed.as_ref().ok().and_then(|parsed| {
        next_fire_text(
            &args.name,
            &parsed.schedule,
            &stamps,
            pause.as_ref(),
            &now_zoned,
            now,
            window_reset,
        )
    });
    let records = run_log::task_records(&state_home(), &args.name);

    let mut out = ui::out();
    write!(
        out,
        "{} — {}",
        ui::paint(ui::palette::ACCENT.bold(), &args.name),
        ui::paint(schedule_style(parsed.as_ref()), &schedule_text)
    )?;
    match active_pause {
        Some(PauseEntry { until: None }) => write!(out, " · paused")?,
        Some(PauseEntry { until: Some(until) }) => {
            write!(out, " · paused, resumes {}", pause_until_text(*until, now))?
        }
        None => {
            if let Some(next) = next {
                write!(out, " · next {next}")?;
            }
        }
    }
    writeln!(out)?;
    if matches!(active_pause, Some(PauseEntry { until: None })) {
        writeln!(out, "  resume with `rimz loop resume {}`", args.name)?;
    }
    let mut kv = ui::KeyVals::new().indent(2);
    kv.push("task", ui::cell(task_subject(&entry)));
    if let Some(check) = check_summary(&entry) {
        kv.push("check", ui::cell(check));
    }
    kv.push("root", ui::cell(root_with_room(&root, room_is_open)));
    kv.push("source", ui::cell(source_detail(source, &entry)));
    if let Some(raw) = entry.budget_per_day.as_deref()
        && let Ok(spec) = raw.parse::<rimz::harness::budget::BudgetSpec>()
    {
        let spend = run_log::spend_on_local_day(&records, &now_zoned);
        kv.push(
            "budget_today",
            ui::cell(format!("${spend:.2} of ${:.2}", spec.cap_usd)),
        );
    }
    kv.render(&mut out)?;

    if records.is_empty() {
        writeln!(out)?;
        writeln!(out, "no runs recorded; try `rimz loop fire {}`", args.name)?;
        return Ok(());
    }

    writeln!(out)?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::HEADER,
            &format!("RECENT RUNS ({} recorded)", records.len())
        )
    )?;
    let rows = collapsed_run_rows(&records);
    let start = rows.len().saturating_sub(args.runs);
    let visible_rows = &rows[start..];
    let show_note = visible_rows.iter().any(|row| row.key.note.is_some());
    let headers = if show_note {
        vec!["WHEN", "MODE", "STATUS", "TOOK", "NOTE"]
    } else {
        vec!["WHEN", "MODE", "STATUS", "TOOK"]
    };
    let mut table = ui::Table::new(headers).indent(2);
    for row in visible_rows {
        let record = row.latest;
        let mut cells = vec![
            ui::cell(ui::rel_age(record.at, now)),
            ui::cell(row.key.mode.map_or("-", LoopRunMode::label)).dash(),
            run_status_cell(record, row.count),
            ui::cell(
                record
                    .duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
        ];
        if show_note {
            cells.push(ui::cell(row.key.note.as_deref().unwrap_or("-")).dash());
        }
        table.row(cells);
    }
    table.render(&mut out)?;

    let (detail_idx, failure_idx) = detail_indices(&records);
    if let Some(detail) = detail_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, detail, "LAST RUN", now)?;
    }
    if let Some(failure) = failure_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, failure, "LAST FAILURE", now)?;
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

#[derive(Clone, Debug, PartialEq)]
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

fn last_run_cells(stats: &run_log::LoopRunStats, now: Timestamp) -> (ui::Cell, ui::Cell) {
    let status = run_status(&stats.last);
    let mut label = format!("{} {}", status.glyph, status.label);
    if stats.streak > 1 {
        label.push_str(&format!(" ×{}", stats.streak));
    }
    if failure_note_visible(stats.last.result)
        && let Some(note) = record_note(&stats.last)
    {
        label.push_str(" · ");
        label.push_str(&note);
    }
    (
        ui::cell(ui::rel_age(stats.last.at, now)),
        ui::cell(label).fg(status.style),
    )
}

fn run_status_cell(record: &LoopRunRecord, count: usize) -> ui::Cell {
    let status = run_status(record);
    let mut label = format!("{} {}", status.glyph, status.label);
    if count > 1 {
        label.push_str(&format!(" ×{count}"));
    }
    ui::cell(label).fg(status.style)
}

struct RunStatusDisplay {
    glyph: &'static str,
    label: String,
    style: anstyle::Style,
}

fn run_status(record: &LoopRunRecord) -> RunStatusDisplay {
    let label = match record.result {
        LoopRunResult::Completed => "completed".to_owned(),
        LoopRunResult::Delivered => "delivered".to_owned(),
        LoopRunResult::Failed => {
            let mut label = "failed".to_owned();
            if let Some(exit) = failure_exit_label(record) {
                label.push_str(" (");
                label.push_str(&exit);
                label.push(')');
            }
            label
        }
        LoopRunResult::TimedOut => "timed out".to_owned(),
        LoopRunResult::BudgetExceeded => "budget exceeded".to_owned(),
        LoopRunResult::BudgetSkipped => "budget skipped".to_owned(),
        LoopRunResult::Errored => "error".to_owned(),
        LoopRunResult::SkippedWindow => "skipped".to_owned(),
        LoopRunResult::Expired => "expired".to_owned(),
        LoopRunResult::Canceled => "canceled".to_owned(),
        LoopRunResult::TargetGone => "target gone".to_owned(),
        LoopRunResult::Overlapped => "overlapped".to_owned(),
        LoopRunResult::CheckSkipped => check_skipped_label(record).to_owned(),
    };
    RunStatusDisplay {
        glyph: loop_result_glyph(record.result),
        label,
        style: loop_result_style(record.result),
    }
}

fn check_skipped_label(record: &LoopRunRecord) -> &'static str {
    match record.check.as_ref() {
        Some(check) if check.timed_out => "check timed out",
        Some(check) if check.code == Some(0) => "check passed",
        Some(_) => "check failed",
        None => "check failed",
    }
}

fn failure_exit_label(record: &LoopRunRecord) -> Option<String> {
    record_exit(record).map(|exit| match exit.as_str() {
        "timeout" | "signal" => exit,
        code => format!("exit {code}"),
    })
}

fn failure_note_visible(result: LoopRunResult) -> bool {
    matches!(
        result,
        LoopRunResult::Failed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
            | LoopRunResult::BudgetSkipped
            | LoopRunResult::Errored
    )
}

pub(super) fn loop_result_style(result: LoopRunResult) -> anstyle::Style {
    match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => ui::palette::GOOD,
        LoopRunResult::Failed
        | LoopRunResult::TimedOut
        | LoopRunResult::BudgetExceeded
        | LoopRunResult::Errored => ui::palette::ALARM,
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::SkippedWindow
        | LoopRunResult::BudgetSkipped => ui::palette::WARN,
        LoopRunResult::CheckSkipped => ui::palette::MUTED,
    }
}

pub(super) fn loop_result_glyph(result: LoopRunResult) -> &'static str {
    match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => "✓",
        LoopRunResult::Failed
        | LoopRunResult::TimedOut
        | LoopRunResult::BudgetExceeded
        | LoopRunResult::Errored => "✗",
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::SkippedWindow
        | LoopRunResult::BudgetSkipped
        | LoopRunResult::CheckSkipped => "○",
    }
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
        return Some(
            check
                .code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned()),
        );
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
        LoopRunResult::BudgetExceeded => Some(125),
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
        LoopRunResult::Errored
            | LoopRunResult::Failed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
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
    write!(out, "{} — ", ui::paint(anstyle::Style::new().bold(), title))?;
    let status = run_status(record);
    write!(
        out,
        "{}",
        ui::paint(status.style, &format!("{} {}", status.glyph, status.label))
    )?;
    write!(
        out,
        " · {} · {}",
        ui::rel_age(record.at, now),
        record.mode.map_or("legacy", LoopRunMode::label)
    )?;
    if let Some(exit) = detail_exit_segment(record) {
        write!(out, " · {exit}")?;
    }
    writeln!(out)?;
    if let Some(check) = &record.check {
        let first_style = if check.timed_out || check.code != Some(0) {
            Some(ui::palette::ALARM)
        } else {
            None
        };
        write_gutter_block(out, first_style, &check.output)?;
    }
    let run_record = record
        .run_id
        .as_deref()
        .and_then(|run_id| run_record_for(entry, run_id));
    if let Some(error) = &record.error {
        write_detail_label(out, "error")?;
        write_gutter_block(out, None, error)?;
    }
    if let Some(last_message) = record.last_message.as_ref().or_else(|| {
        run_record
            .as_ref()
            .and_then(|record| record.last_message.as_ref())
    }) {
        write_detail_label(out, "last message")?;
        write_gutter_block(out, None, last_message)?;
    }
    if let Some(run_id) = &record.run_id {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::MUTED, &format!("  run: {run_id}"))
        )?;
        if let Some(tail) = run_record
            .as_ref()
            .and_then(|record| record.failure_tail.as_deref())
            .filter(|tail| !tail.trim().is_empty())
        {
            write_detail_label(out, "output tail")?;
            write_gutter_block(out, None, tail)?;
        }
        if let Some(transcript) = run_record
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref())
        {
            writeln!(
                out,
                "{}",
                ui::paint(ui::palette::MUTED, &format!("  transcript: {transcript}"))
            )?;
        }
    }
    Ok(())
}

fn detail_exit_segment(record: &LoopRunRecord) -> Option<String> {
    if matches!(
        record.result,
        LoopRunResult::Failed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
            | LoopRunResult::Errored
    ) {
        return None;
    }
    let check = record.check.as_ref()?;
    if check.timed_out {
        return Some("timeout".to_owned());
    }
    Some(
        check
            .code
            .map(|code| format!("exit {code}"))
            .unwrap_or_else(|| "signal".to_owned()),
    )
}

fn write_detail_label(out: &mut impl Write, label: &str) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        ui::paint(ui::palette::MUTED, &format!("  {label}:"))
    )
}

pub(super) fn write_gutter_block(
    out: &mut impl Write,
    first_style: Option<anstyle::Style>,
    body: &str,
) -> std::io::Result<()> {
    let body = body.trim_end();
    if body.trim().is_empty() {
        return write_gutter_line(out, Some(ui::palette::FAINT), "-");
    }
    for (idx, line) in body.lines().enumerate() {
        let style = if idx == 0 { first_style } else { None };
        write_gutter_line(out, style, line)?;
    }
    Ok(())
}

fn write_gutter_line(
    out: &mut impl Write,
    style: Option<anstyle::Style>,
    line: &str,
) -> std::io::Result<()> {
    write!(out, "  {}", ui::paint(ui::palette::FAINT, "│ "))?;
    if let Some(style) = style {
        write!(out, "{}", ui::paint(style, line))?;
    } else {
        write!(out, "{line}")?;
    }
    writeln!(out)
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
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: None,
        }
    }

    #[test]
    fn task_rules_and_check_rows_use_action_specific_verbs() {
        let spawn = TaskEntry {
            agent: Some("codex".to_owned()),
            check: Some("cargo test".to_owned()),
            on: Some(CheckOn::Fail),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&spawn), "check, then start codex on fail");
        assert_eq!(
            check_summary(&spawn).as_deref(),
            Some("cargo test (starts codex on fail)")
        );

        let wake = TaskEntry {
            wake: Some(TaskTarget {
                kind: "claude".to_owned(),
                session: "sess-planner".to_owned(),
                handle: "@planner".to_owned(),
            }),
            check: Some("cargo test".to_owned()),
            on: Some(CheckOn::Success),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&wake), "check, then wake @planner on success");
        assert_eq!(
            check_summary(&wake).as_deref(),
            Some("cargo test (wakes @planner on success)")
        );

        let check = TaskEntry {
            check: Some("cargo test".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&check), "check");

        let spawn = TaskEntry {
            agent: Some("claude".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&spawn), "start claude");

        let mut wake_only = wake;
        wake_only.check = None;
        assert_eq!(task_run_rule(&wake_only), "wake @planner");
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
    fn run_status_names_check_skipped_outcomes() {
        let mut skipped = record(10, LoopRunResult::CheckSkipped);
        skipped.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        let status = run_status(&skipped);
        assert_eq!(status.glyph, "○");
        assert_eq!(status.label, "check passed");

        skipped.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "not yet".to_owned(),
        });
        assert_eq!(run_status(&skipped).label, "check failed");
    }

    #[test]
    fn source_detail_names_definition_path() {
        let entry = TaskEntry {
            root: PathBuf::from("/repo"),
            ..TaskEntry::default()
        };

        assert_eq!(
            source_detail(TaskSource::Config, &entry),
            format!(
                "machine — {}",
                ui::home_relative(MachineConfig::loop_path().to_string_lossy().as_ref())
            )
        );
        assert_eq!(
            source_detail(
                TaskSource::Project {
                    state: TrustState::Untrusted
                },
                &entry,
            ),
            "project · untrusted — /repo/.rimz/config.toml"
        );
        assert_eq!(
            source_detail(TaskSource::Instance, &entry),
            format!(
                "state — {}",
                ui::home_relative(instances::path(&state_home()).to_string_lossy().as_ref())
            )
        );
    }

    #[test]
    fn run_status_merges_failed_check_exit() {
        let mut failed = record(10, LoopRunResult::Failed);
        failed.check = Some(CheckRecord {
            code: Some(127),
            timed_out: false,
            output: "missing".to_owned(),
        });

        let status = run_status(&failed);

        assert_eq!(status.glyph, "✗");
        assert_eq!(status.label, "failed (exit 127)");
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
            "LAST FAILURE",
            Timestamp::from_second(30).expect("timestamp"),
        )
        .unwrap();

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&ui::paint(ui::palette::MUTED, "  error:")));
        let out = anstream::adapter::strip_str(&raw).to_string();
        assert!(out.contains("LAST FAILURE — ✗ error · "));
        assert!(out.contains(" · manual"));
        assert!(out.contains("  error:\n  │ outer error\n  │ inner detail"));
    }

    #[test]
    fn render_record_detail_marks_failed_check_output() {
        let mut detail = record(20, LoopRunResult::Failed);
        detail.check = Some(CheckRecord {
            code: Some(2),
            timed_out: false,
            output: "first line\nsecond line".to_owned(),
        });
        let entry = TaskEntry {
            root: PathBuf::from("/tmp/rimz-run"),
            ..TaskEntry::default()
        };
        let mut out = Vec::new();

        render_record_detail(
            &mut out,
            &entry,
            &detail,
            "LAST FAILURE",
            Timestamp::from_second(30).expect("timestamp"),
        )
        .unwrap();

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&ui::paint(ui::palette::ALARM, "first line")));
        let out = anstream::adapter::strip_str(&raw).to_string();
        assert!(out.contains("LAST FAILURE — ✗ failed (exit 2)"));
        assert!(out.contains("  │ first line\n  │ second line"));
    }
}
