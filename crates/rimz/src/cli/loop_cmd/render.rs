//! List and show loop tasks plus recorded run details.

use super::*;

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
        "NAME", "SOURCE", "SPEC", "SCHEDULE", "NEXT", "ROOM", "RUNS", "LAST RUN", "RESULT", "ROOT",
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
        let next = parsed
            .ok()
            .and_then(|parsed| next_fire_text(name, &parsed.schedule, &stamps, &now_zoned, now))
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
            .map(|stats| loop_result_cell(stats.last.result))
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
) -> Option<String> {
    let next = schedule.next_after(*stamps.get(name)?, now_zoned)?;
    Some(ui::rel_until(next, now))
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
    let next = parsed
        .as_ref()
        .ok()
        .and_then(|parsed| next_fire_text(&args.name, &parsed.schedule, &stamps, &now_zoned, now))
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
    let start = records.len().saturating_sub(args.runs);
    for record in &records[start..] {
        table.row([
            ui::cell(ui::rel_age(record.at, now)),
            ui::cell(record.mode.map_or("-", LoopRunMode::label)).dash(),
            loop_result_cell(record.result),
            ui::cell(
                record
                    .duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            ui::cell(record_exit(record).unwrap_or_else(|| "-".to_owned())).dash(),
            ui::cell(record_note(record).unwrap_or_else(|| "-".to_owned())).dash(),
        ]);
    }
    table.render(&mut out)?;

    if let Some(detail) = records
        .iter()
        .rev()
        .find(|record| record_has_detail(record))
    {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, detail)?;
    }
    Ok(())
}

fn loop_result_cell(result: LoopRunResult) -> ui::Cell {
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
    ui::cell(result.label()).fg(style)
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
    record
        .error
        .as_deref()
        .or(record.last_message.as_deref())
        .or(record.target.as_deref())
        .map(|note| truncate_note(first_line(note), 60))
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

fn render_record_detail(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
) -> std::io::Result<()> {
    writeln!(out, "last run detail")?;
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
    if let Some(error) = &record.error {
        writeln!(out, "error:")?;
        writeln!(out, "{error}")?;
    }
    if let Some(last_message) = &record.last_message {
        writeln!(out, "last message:")?;
        writeln!(out, "{last_message}")?;
    }
    if let Some(run_id) = &record.run_id {
        writeln!(out, "run: {run_id}")?;
        if let Some(transcript) = transcript_path_for_record(entry, run_id) {
            writeln!(out, "transcript: {transcript}")?;
        }
    }
    Ok(())
}

fn transcript_path_for_record(entry: &TaskEntry, run_id: &str) -> Option<String> {
    let run_id = rimz::RunId::parse(run_id).ok()?;
    let paths = StatePaths::under(
        WorkspaceId::from_project_root(&entry.resolved_root()),
        &state_home(),
    )
    .ok()?;
    rimz::harness::run::load(&paths, &run_id)
        .ok()
        .and_then(|record| record.transcript_path)
}
