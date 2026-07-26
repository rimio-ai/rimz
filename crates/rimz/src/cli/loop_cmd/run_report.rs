//! Shared summaries and stored-run forensics for loop executions.

use super::*;

const CHECK_SUMMARY_OUTPUT_CAP: usize = 4 * 1024;

pub(super) struct RunSummary<'a> {
    pub(super) record: &'a LoopRunRecord,
    pub(super) presentation: &'a LoopRunPresentation,
}

pub(super) fn write_manual_verdict(
    out: &mut impl Write,
    result: LoopRunResult,
    label: &str,
) -> std::io::Result<()> {
    let mark = render::loop_result_mark(result);
    writeln!(
        out,
        "{}",
        ui::paint(mark.style, &format!("{} {label}", mark.glyph))
    )
}

pub(super) fn write_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    mode: LoopRunMode,
    keep: bool,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let action_kind = action.kind();
    match mode {
        LoopRunMode::Manual => {
            write_manual_run_summary(out, name, entry, action, action_kind, keep, summary)
        }
        LoopRunMode::Scheduled => write_scheduled_run_summary(out, name, entry, action, summary),
    }
}

fn write_manual_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    action_kind: TaskActionKind,
    keep: bool,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let record = summary.record;
    let duration_ms = record.duration_ms.unwrap_or_default();
    if record.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            action,
            duration_ms,
            LoopRunMode::Manual,
            summary,
        );
    }
    if let Some((result, label)) = manual_early_verdict(summary) {
        return write_manual_verdict(out, result, &label);
    }

    let result_mark = render::loop_result_mark(record.result);
    let result_label = manual_result_label(action_kind, summary);
    write!(
        out,
        "{}",
        ui::paint(
            result_mark.style,
            &format!("{} {result_label}", result_mark.glyph)
        )
    )?;
    write!(out, " in {}", render::format_duration_ms(duration_ms))?;
    if let Some(spend) =
        render::spend_segments(record.cost_usd, record.input_tokens, record.output_tokens)
    {
        write!(out, " · {spend}")?;
    }
    writeln!(out)?;

    if is_spawn_failure(record.result) && !action_kind.is_check_only() {
        write_failure_forensics(out, name, summary)?;
    } else if record.result == LoopRunResult::Completed && record.run_id.is_some() {
        write_completion_detail(out, name, summary)?;
    }
    if !is_spawn_failure(record.result) && !keep && record.run_id.is_some() {
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::muted(),
                "  pane closed; rerun with --keep to watch"
            )
        )?;
    }
    Ok(())
}

fn manual_early_verdict(summary: &RunSummary<'_>) -> Option<(LoopRunResult, String)> {
    let label = match summary.record.result {
        LoopRunResult::Expired => "deadline expired — task left in place".to_owned(),
        LoopRunResult::TargetGone => format!(
            "{} not alive — schedule left in place",
            summary.record.target.as_deref().unwrap_or("target")
        ),
        _ => return None,
    };
    Some((summary.record.result, label))
}

fn is_spawn_failure(result: LoopRunResult) -> bool {
    matches!(
        result,
        LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
    )
}

fn write_scheduled_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let record = summary.record;
    let duration_ms = record.duration_ms.unwrap_or_default();
    if record.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            action,
            duration_ms,
            LoopRunMode::Scheduled,
            summary,
        );
    }
    let result_mark = render::loop_result_mark(record.result);
    let exit_label = outcome_exit_label(summary);
    if is_spawn_failure(record.result) {
        let mut label = record.result.label().to_owned();
        if let Some(exit_label) = exit_label.as_deref() {
            label.push(' ');
            label.push_str(exit_label);
        }
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_mark.style.bold(), &label)
        )?;
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(spend) =
            render::spend_segments(record.cost_usd, record.input_tokens, record.output_tokens)
        {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        write_failure_forensics(out, name, summary)?;
    } else {
        let result_label = success_result_label(record);
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_mark.style, &result_label)
        )?;
        if let Some(exit_label) = exit_label.as_deref() {
            write!(out, " {exit_label}")?;
        }
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(spend) =
            render::spend_segments(record.cost_usd, record.input_tokens, record.output_tokens)
        {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        if record.result == LoopRunResult::Completed && record.run_id.is_some() {
            write_completion_detail(out, name, summary)?;
        }
    }
    Ok(())
}

fn success_result_label(record: &LoopRunRecord) -> String {
    match (record.result, record.target.as_deref()) {
        (LoopRunResult::Delivered, Some(target)) => format!("delivered to {target}"),
        _ => record.result.label().to_owned(),
    }
}

fn manual_result_label(action_kind: TaskActionKind, summary: &RunSummary<'_>) -> String {
    if action_kind.is_check_only()
        && let Some(check) = &summary.record.check
    {
        return check_result_label(check);
    }
    let mut label = success_result_label(summary.record);
    if let Some(exit_label) = outcome_exit_label(summary) {
        label.push(' ');
        label.push_str(&exit_label);
    }
    label
}

fn check_result_label(check: &CheckRecord) -> String {
    if check.timed_out {
        "check timed out".to_owned()
    } else if check.code == Some(0) {
        "check passed (exit 0)".to_owned()
    } else {
        match check.code {
            Some(code) => format!("check failed (exit {code})"),
            None => "check failed (signal)".to_owned(),
        }
    }
}

pub(super) fn write_check_trip_line(
    out: &mut impl Write,
    action: &TaskAction,
    check: &CheckRecord,
    duration_ms: u64,
) -> std::io::Result<()> {
    let (glyph, style) = if check.timed_out || check.code != Some(0) {
        ("✗", ui::palette::alarm())
    } else {
        ("✓", ui::palette::good())
    };
    write!(
        out,
        "  {}",
        ui::paint(
            style,
            &format!(
                "{glyph} {} in {}",
                check_result_label(check),
                render::format_duration_ms(duration_ms)
            )
        )
    )?;
    writeln!(
        out,
        " {}",
        ui::paint(
            ui::palette::accent(),
            &format!("→ {}", render::action_progressive_phrase(action))
        )
    )
}

fn write_check_skipped_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    duration_ms: u64,
    mode: LoopRunMode,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let label = summary
        .record
        .check
        .as_ref()
        .map(check_result_label)
        .unwrap_or_else(|| "check skipped".to_owned());
    let check_duration_ms = summary
        .presentation
        .check_duration_ms
        .unwrap_or(duration_ms);
    let duration = render::format_duration_ms(check_duration_ms);
    let (glyph, style) = render::check_skip_display(summary.record.check.as_ref());
    if mode == LoopRunMode::Manual {
        write!(
            out,
            "{}",
            ui::paint(style, &format!("{glyph} {label} in {duration}"))
        )?;
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::muted(),
                &format!(" — {}", render::check_skip_decision(entry, action))
            )
        )
    } else {
        write!(out, "loop `{name}`: {}", ui::paint(style, &label))?;
        writeln!(
            out,
            " in {duration} — {}",
            render::check_skip_decision(entry, action)
        )
    }
}

fn write_failure_forensics(
    out: &mut impl Write,
    name: &str,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    if let Some(tail) = outcome_failure_tail(summary) {
        write_gutter_block(out, Some(ui::palette::alarm()), &tail)?;
    }
    write_run_links(out, summary.record, RunLinkDetail::Summary)?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::muted(),
            &format!("  see: rimz loop show {name}")
        )
    )
}

fn write_completion_detail(
    out: &mut impl Write,
    name: &str,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    if !summary.presentation.streamed {
        if let Some(message) = summary
            .record
            .last_message
            .as_deref()
            .filter(|msg| !msg.trim().is_empty())
        {
            write_gutter_block(out, None, message)?;
        } else {
            writeln!(
                out,
                "{}",
                ui::paint(
                    ui::palette::muted(),
                    &format!("  no final message; see: rimz loop show {name}")
                )
            )?;
        }
    }
    write_run_links(out, summary.record, RunLinkDetail::Summary)
}

fn outcome_exit_label(summary: &RunSummary<'_>) -> Option<String> {
    if let Some(exit) = summary.presentation.exit_code {
        if exit == 0 {
            return None;
        }
        Some(format!("(exit {exit})"))
    } else if let Some(exit) = summary.record.check.as_ref().and_then(|check| check.code) {
        Some(format!("(exit {exit})"))
    } else if summary
        .record
        .check
        .as_ref()
        .is_some_and(|check| check.timed_out)
    {
        Some("(timeout)".to_owned())
    } else {
        None
    }
}

fn outcome_failure_tail(summary: &RunSummary<'_>) -> Option<String> {
    if let Some(tail) = summary
        .presentation
        .failure_tail
        .as_deref()
        .filter(|tail| !tail.trim().is_empty())
    {
        return Some(tail.trim_end().to_owned());
    }
    let check = summary.record.check.as_ref()?;
    if !check.timed_out && check.code == Some(0) {
        return None;
    }
    let tail = rimz::proc::tail_output(check.output.as_bytes(), CHECK_SUMMARY_OUTPUT_CAP);
    let tail = tail.trim_end();
    (!tail.trim().is_empty()).then(|| tail.to_owned())
}

pub(super) fn render_record_detail(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
    title: &str,
    now: Timestamp,
) -> std::io::Result<()> {
    write!(out, "{} — ", ui::paint(anstyle::Style::new().bold(), title))?;
    let status = render::run_status(record);
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
    write_record_forensics(out, entry, record)
}

pub(super) fn write_failure_pointer(
    out: &mut impl Write,
    name: &str,
    record: &LoopRunRecord,
    now: Timestamp,
) -> std::io::Result<()> {
    write!(
        out,
        "{}",
        ui::paint(ui::palette::muted(), "  last failure — ")
    )?;
    let status = render::run_status(record);
    write!(
        out,
        "{}",
        ui::paint(status.style, &format!("{} {}", status.glyph, status.label))
    )?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::muted(),
            &format!(
                " · {} · {} · dig in: rimz loop logs {name} --failed",
                ui::rel_age(record.at, now),
                record.mode.map_or("legacy", LoopRunMode::label)
            )
        )
    )
}

pub(super) fn write_record_forensics(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
) -> std::io::Result<()> {
    let run_record = record
        .run_id
        .as_deref()
        .and_then(|run_id| run_record_for(entry, run_id));
    write_check_section(out, record, run_record.as_ref())?;
    write_verify_section(out, run_record.as_ref())?;
    if let Some(spend) = record_spend_label(record) {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  cost: {spend}"))
        )?;
    }
    write_run_links(out, record, RunLinkDetail::Stored(run_record.as_ref()))
}

fn write_check_section(
    out: &mut impl Write,
    record: &LoopRunRecord,
    run_record: Option<&rimz::harness::run::RunRecord>,
) -> std::io::Result<()> {
    if let Some(check) = &record.check {
        let first_style = if check.timed_out || check.code != Some(0) {
            Some(ui::palette::alarm())
        } else {
            None
        };
        write_gutter_block(out, first_style, &check.output)?;
    }
    if let Some(error) = &record.error {
        write_detail_label(out, "error")?;
        write_gutter_block(out, None, error)?;
    }
    if let Some(last_message) = record
        .last_message
        .as_ref()
        .or_else(|| run_record.and_then(|record| record.last_message.as_ref()))
    {
        write_detail_label(out, "last message")?;
        write_gutter_block(out, None, last_message)?;
    }
    Ok(())
}

fn write_verify_section(
    out: &mut impl Write,
    run_record: Option<&rimz::harness::run::RunRecord>,
) -> std::io::Result<()> {
    if let Some(verify) = run_record
        .and_then(|record| record.verify.as_ref())
        .filter(|verify| !verify.passed)
    {
        crate::cli::supervised::output::write_verify_failure(
            out,
            verify,
            "  ",
            Some(ui::palette::muted()),
        )?;
        write_gutter_block(out, Some(ui::palette::alarm()), &verify.output)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum RunLinkDetail<'a> {
    Summary,
    Stored(Option<&'a rimz::harness::run::RunRecord>),
}

fn write_run_links(
    out: &mut impl Write,
    record: &LoopRunRecord,
    detail: RunLinkDetail<'_>,
) -> std::io::Result<()> {
    if let Some(run_id) = &record.run_id {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  run: {run_id}"))
        )?;
        if let RunLinkDetail::Stored(run_record) = detail {
            if let Some(tail) = run_record
                .and_then(|record| record.failure_tail.as_deref())
                .filter(|tail| !tail.trim().is_empty())
            {
                write_detail_label(out, "output tail")?;
                write_gutter_block(out, None, tail)?;
            }
            if let Some(transcript) =
                run_record.and_then(|record| record.transcript_path.as_deref())
            {
                writeln!(
                    out,
                    "{}",
                    ui::paint(ui::palette::muted(), &format!("  transcript: {transcript}"))
                )?;
            }
        }
    }
    if matches!(detail, RunLinkDetail::Summary)
        && let Some(transcript) = &record.transcript_path
    {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  transcript: {transcript}"))
        )?;
    }
    Ok(())
}

fn record_spend_label(record: &LoopRunRecord) -> Option<String> {
    render::spend_segments(
        record
            .cost_usd
            .filter(|cost| cost.is_finite() && *cost >= 0.0),
        record.input_tokens,
        record.output_tokens,
    )
}

pub(super) fn detail_exit_segment(record: &LoopRunRecord) -> Option<String> {
    if matches!(
        record.result,
        LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
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
        ui::paint(ui::palette::muted(), &format!("  {label}:"))
    )
}

fn write_gutter_block(
    out: &mut impl Write,
    first_style: Option<anstyle::Style>,
    body: &str,
) -> std::io::Result<()> {
    let body = body.trim_end();
    if body.trim().is_empty() {
        return write_gutter_line(out, Some(ui::palette::faint()), "-");
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
    write!(out, "  {}", ui::paint(ui::palette::faint(), "│ "))?;
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
