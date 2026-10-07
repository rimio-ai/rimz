//! Shared summaries and stored-run forensics for loop executions.

use super::*;
use rimz::harness::schedule::signal::WatchVerdict;
use serde::ser::{SerializeMap, Serializer as _};

const CHECK_SUMMARY_OUTPUT_CAP: usize = 4 * 1024;

pub(super) struct RunSummary<'a> {
    pub(super) record: &'a LoopRunRecord,
    pub(super) presentation: &'a LoopRunPresentation,
    pub(super) prose: ui::prose::Prose,
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
    if record.watch.is_none() {
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
    }
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
        if record.watch.is_none() {
            write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        }
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
        if record.watch.is_none() {
            write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        }
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
        return check_result_label(check, summary.record.watch.as_ref());
    }
    let mut label = success_result_label(summary.record);
    if let Some(exit_label) = outcome_exit_label(summary) {
        label.push(' ');
        label.push_str(&exit_label);
    }
    label
}

fn check_result_label(check: &CheckRecord, watch: Option<&WatchVerdict>) -> String {
    if let Some(verdict) = watch {
        return verdict.label();
    }
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
    watch: Option<&WatchVerdict>,
    duration_ms: u64,
) -> std::io::Result<()> {
    let (glyph, style) = if check.timed_out || check.code != Some(0) {
        ("✗", ui::palette::alarm())
    } else {
        ("✓", ui::palette::good())
    };
    let mut label = check_result_label(check, watch);
    if watch.is_none() {
        label.push_str(&format!(" in {}", render::format_duration_ms(duration_ms)));
    }
    write!(out, "  {}", ui::paint(style, &format!("{glyph} {label}")))?;
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
        .map(|check| check_result_label(check, summary.record.watch.as_ref()))
        .unwrap_or_else(|| "check skipped".to_owned());
    let check_duration_ms = summary
        .presentation
        .check_duration_ms
        .unwrap_or(duration_ms);
    let duration = if summary.record.watch.is_some() {
        String::new()
    } else {
        format!(" in {}", render::format_duration_ms(check_duration_ms))
    };
    let (glyph, style) = render::check_skip_display(summary.record.check.as_ref());
    if mode == LoopRunMode::Manual {
        write!(
            out,
            "{}",
            ui::paint(style, &format!("{glyph} {label}{duration}"))
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
            "{duration} — {}",
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
    write_summary_run_links(out, summary.record)?;
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
            write_gutter_prose(out, message, summary.prose)?;
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
    write_summary_run_links(out, summary.record)
}

fn outcome_exit_label(summary: &RunSummary<'_>) -> Option<String> {
    if let Some(verdict) = &summary.record.watch {
        return Some(format!("· {}", verdict.label()));
    }
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

/// How much of a record `write_record_forensics` prints: `loop logs` prints it
/// whole, `loop show` a summary whose header already names the signal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Forensics {
    Full,
    Summary,
}

const SUMMARY_CHECK_LINES: usize = 5;

/// The tail a summary prints in place of a passing check's output, when that
/// output is longer. A failed check keeps every line.
fn summary_check_tail<'a>(record: &LoopRunRecord, check: &'a CheckRecord) -> Option<&'a str> {
    if render::record_is_failure(record) || check.timed_out || check.code != Some(0) {
        return None;
    }
    let output = check.output.trim_end();
    let (cut, _) = output.rmatch_indices('\n').nth(SUMMARY_CHECK_LINES - 1)?;
    Some(&output[cut + 1..])
}

/// `full_output` is the `loop logs` command whose first block is this record,
/// printed when the summary cut its check output.
pub(super) fn render_record_detail(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
    title: &str,
    full_output: &str,
    now: Timestamp,
    prose: ui::prose::Prose,
) -> std::io::Result<()> {
    write!(out, "{} — ", ui::paint(anstyle::Style::new().bold(), title))?;
    let status = render::run_status(record);
    write!(
        out,
        "{}",
        ui::paint(status.style, &format!("{} {}", status.glyph, status.label))
    )?;
    write!(out, " · {}", ui::rel_age(record.at, now))?;
    if let Some(took) = render::run_duration_label(record) {
        write!(out, " · {took}")?;
    }
    if let Some(exit) = detail_exit_segment(record) {
        write!(out, " · {exit}")?;
    }
    if record.mode != Some(LoopRunMode::Scheduled) {
        write!(
            out,
            " · {}",
            record.mode.map_or("legacy", LoopRunMode::label)
        )?;
    }
    if let Some(signal) = &record.signal {
        write!(out, " · signal {}", signal.name.as_str())?;
    }
    writeln!(out)?;
    write_record_forensics(out, Some(entry), record, prose, Forensics::Summary)?;
    let is_cut = record
        .check
        .as_ref()
        .is_some_and(|check| summary_check_tail(record, check).is_some());
    if is_cut {
        write_detail_link(out, "full output", full_output)?;
    }
    Ok(())
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
    entry: Option<&TaskEntry>,
    record: &LoopRunRecord,
    prose: ui::prose::Prose,
    detail: Forensics,
) -> std::io::Result<()> {
    let run_record = record
        .run_id
        .as_deref()
        .and_then(|run_id| entry.and_then(|entry| run_record_for(entry, run_id)));
    if let Some(checkout) = &record.checkout {
        write_detail_link(out, "checkout", &checkout.display().to_string())?;
    }
    if record.result == LoopRunResult::Launched
        && let Some(leader) = &record.target
    {
        write_detail_link(out, "leader", leader)?;
    }
    write_check_section(out, record, run_record.as_ref(), prose, detail)?;
    write_verify_section(out, run_record.as_ref())?;
    if let Some(spend) = record_spend_label(record) {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  cost: {spend}"))
        )?;
    }
    if detail == Forensics::Full
        && let Some(signal) = &record.signal
    {
        write_detail_link(out, "signal", signal.name.as_str())?;
        if !signal.payload.is_empty() {
            writeln!(out, "  {}", serde_json::to_string(&signal.payload)?)?;
        }
    }
    if let Some(condition) = &record.condition {
        let held = condition
            .hold
            .as_ref()
            .map_or_else(String::new, |hold| format!(" · held {hold}"));
        write_detail_link(out, "when", &format!("{}{held}", condition.when))?;
        write!(out, "  ")?;
        let mut serializer = serde_json::Serializer::new(&mut *out);
        let mut readings = serializer.serialize_map(None)?;
        if let Ok(expr) = schedule::when::WhenExpr::parse(std::slice::from_ref(&condition.when)) {
            let verdict = schedule::when::Verdict {
                ok: false,
                readings: condition.readings.clone(),
            };
            for (key, value) in expr.readings(&verdict) {
                readings.serialize_entry(key, &value)?;
            }
        } else {
            for (key, value) in &condition.readings {
                readings.serialize_entry(key, value)?;
            }
        }
        readings.end()?;
        writeln!(out)?;
    }
    if let Some(message_id) = &record.message_id {
        write_detail_link(out, "message", message_id.as_str())?;
    }
    write_stored_run_links(out, record, run_record.as_ref())
}

fn write_check_section(
    out: &mut impl Write,
    record: &LoopRunRecord,
    run_record: Option<&rimz::store::run::RunRecord>,
    prose: ui::prose::Prose,
    detail: Forensics,
) -> std::io::Result<()> {
    if let Some(check) = &record.check {
        if let Some(path) = &check.output_path {
            write_detail_link(out, "output", &path.display().to_string())?;
        }
        let first_style = if check.timed_out || check.code != Some(0) {
            Some(ui::palette::alarm())
        } else {
            None
        };
        let tail = match detail {
            Forensics::Full => None,
            Forensics::Summary => summary_check_tail(record, check),
        };
        write_gutter_block(out, first_style, tail.unwrap_or(&check.output))?;
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
        write_gutter_prose(out, last_message, prose)?;
    }
    Ok(())
}

fn write_verify_section(
    out: &mut impl Write,
    run_record: Option<&rimz::store::run::RunRecord>,
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

fn write_summary_run_links(out: &mut impl Write, record: &LoopRunRecord) -> std::io::Result<()> {
    if let Some(run_id) = &record.run_id {
        write_detail_link(out, "run", run_id)?;
    }
    if let Some(transcript) = &record.transcript_path {
        write_detail_link(out, "transcript", transcript)?;
    }
    Ok(())
}

fn write_stored_run_links(
    out: &mut impl Write,
    record: &LoopRunRecord,
    run_record: Option<&rimz::store::run::RunRecord>,
) -> std::io::Result<()> {
    if let Some(run_id) = &record.run_id {
        write_detail_link(out, "run", run_id)?;
        if let Some(tail) = run_record
            .and_then(|record| record.failure_tail.as_deref())
            .filter(|tail| !tail.trim().is_empty())
        {
            write_detail_label(out, "output tail")?;
            write_gutter_block(out, None, tail)?;
        }
        if let Some(transcript) = run_record.and_then(|record| record.transcript_path.as_deref()) {
            write_detail_link(out, "transcript", transcript)?;
        }
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
            | LoopRunResult::StartFailed
    ) {
        return None;
    }
    if let Some(verdict) = &record.watch {
        return Some(verdict.label());
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

fn write_detail_link(out: &mut impl Write, label: &str, value: &str) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        ui::paint(ui::palette::muted(), &format!("  {label}: {value}"))
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

fn write_gutter_prose(
    out: &mut impl Write,
    body: &str,
    prose: ui::prose::Prose,
) -> std::io::Result<()> {
    let body = body.trim_end();
    if body.trim().is_empty() {
        return write_gutter_line(out, Some(ui::palette::faint()), "-");
    }
    for line in prose.lines(body, ui::prose::prose_width(4)) {
        write_gutter_line(out, None, &line)?;
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

fn run_record_for(entry: &TaskEntry, run_id: &str) -> Option<rimz::store::run::RunRecord> {
    let run_id = rimz::RunId::parse(run_id).ok()?;
    let paths = StatePaths::for_project_root(&entry.resolved_root()).ok()?;
    rimz::harness::run::load(&paths, &run_id).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condition_readings_follow_expression_order() {
        let mut record =
            LoopRunRecord::new("sweep", LoopRunResult::Launched, LoopRunMode::Scheduled, 0);
        record.condition = Some(run_log::ConditionRecord {
            when: "pr=open && ci=passed && pr=open".into(),
            hold: None,
            held_ms: 0,
            readings: BTreeMap::from([
                ("ci".into(), Some("passed".into())),
                ("pr".into(), Some("open".into())),
            ]),
        });
        let mut out = Vec::new();
        write_record_forensics(
            &mut out,
            None,
            &record,
            ui::prose::Prose::Raw,
            Forensics::Full,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("  {\"pr\":\"open\",\"ci\":\"passed\"}\n"),
            "{text}"
        );
    }
}
