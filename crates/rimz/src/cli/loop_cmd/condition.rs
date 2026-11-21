//! Condition admission and room-aware presentation.

use super::*;
use crate::cli::wake_view::{ConditionReading, read_condition, reset_minute};
use jiff::tz::TimeZone;
use rimz::harness::when::{self, CiSource, WhenExpr, WhenTerm};

pub(super) fn validate(clauses: &[String], root: &Path) -> Result<()> {
    if clauses.is_empty() {
        return Ok(());
    }
    let stages = rimz::harness::when::condition::condition_stages(root)?;
    WhenExpr::parse_with_stages(clauses, Some(&stages))?;
    Ok(())
}

/// `expr` read for `entry` in its own room.
fn read(entry: &TaskEntry, expr: &WhenExpr, fired_at: Option<Timestamp>) -> ConditionReading {
    read_condition(
        &crate::cli::wake_view::room_source(&entry.resolved_root()),
        entry,
        expr,
        fired_at,
    )
}

pub(super) fn reading(entry: &TaskEntry, expr: &WhenExpr) -> ConditionReading {
    read(entry, expr, None)
}

pub(super) fn write_json(entry: &TaskEntry, expr: &WhenExpr) -> Result<bool> {
    let room = crate::cli::wake_view::room_source(&entry.resolved_root());
    let reading = read_condition(&room, entry, expr, None);
    let scope = entry.run_dir();
    let checked_at = if expr.reads_forge() {
        room.source
            .as_ref()
            .and_then(|source| source.checked_at(&scope))
    } else {
        None
    };
    let terms = expr
        .leaves()
        .map(|(term, negated)| {
            let value = reading
                .verdict
                .readings
                .get(&term.key)
                .and_then(|value| value.as_deref());
            let unread = value
                .is_none()
                .then(|| term_unread_reason(term, &reading.unread));
            serde_json::json!({
                "term": term.to_string(),
                "key": term.key,
                "negated": negated,
                "reading": value,
                "met": term.truth(value).map(|truth| truth != negated),
                "unread": unread,
            })
        })
        .collect::<Vec<_>>();
    ui::json_pretty(&serde_json::json!({
        "when": expr.to_string(),
        "scope": scope,
        "room_open": room.room_open,
        "now": reading.verdict.truth,
        "checked_at": checked_at,
        "terms": terms,
    }))?;
    Ok(reading.verdict.truth == Some(true))
}

/// Task `name`'s timing, its record bound to `key`, with a condition trigger's reading folded in.
pub(super) fn observe(
    (key, name): (&str, &str),
    entry: &TaskEntry,
    timing: schedule::TaskTiming,
    now: Timestamp,
) -> schedule::TaskTiming {
    if !timing
        .parsed()
        .is_ok_and(|parsed| matches!(parsed.trigger, schedule::Trigger::Condition { .. }))
    {
        return timing;
    }
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let open = runtime.as_ref().is_some_and(fresh_sidebar_present);
    schedule::attention::RoomReadings::new(runtime.as_ref(), open, Timestamp::now()).observe(
        (key, name),
        entry,
        &entry.run_dir(),
        timing,
        now,
    )
}

pub(super) fn write_receipt(
    out: &mut impl Write,
    entry: &TaskEntry,
    parsed: &schedule::ParsedTrigger,
) -> Result<()> {
    let schedule::Trigger::Condition { expr, hold } = &parsed.trigger else {
        return Ok(());
    };
    writeln!(out, "scope: {}", entry.run_dir().display())?;
    writeln!(
        out,
        "now: {}",
        crate::cli::wake_view::condition_now(entry, expr, *hold)
    )?;
    Ok(())
}

pub(super) fn write_no_room_hint(
    out: &mut impl Write,
    parsed: &schedule::ParsedTrigger,
) -> Result<()> {
    if let schedule::Trigger::Condition { expr, .. } = &parsed.trigger
        && expr.reads_forge()
    {
        writeln!(
            out,
            "ci and pr readings come from the room's sidebar; the loop timer alone never sees them"
        )?;
    }
    Ok(())
}

pub(super) fn write_show(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    timing: &schedule::TaskTiming,
    zone: &TimeZone,
) -> Result<()> {
    let Ok(schedule::ParsedTrigger {
        trigger: schedule::Trigger::Condition { expr, .. },
        ..
    }) = timing.parsed()
    else {
        return Ok(());
    };
    let fired_at = runtime_for_root(&entry.resolved_root())
        .and_then(|runtime| schedule::fire::last_stamps(&runtime).get(name).copied());
    write_terms(out, expr, &read(entry, expr, fired_at), zone)?;
    if expr.reads_forge()
        && let Some(checked) = runtime_for_root(&entry.resolved_root())
            .map(|runtime| CiSource::read(&runtime))
            .and_then(|source| source.checked_at(&entry.run_dir()))
    {
        writeln!(
            out,
            "  forge checked {}",
            ui::rel_age(checked, Timestamp::now())
        )?;
    }
    Ok(())
}

pub(super) fn write_terms(
    out: &mut impl Write,
    expr: &WhenExpr,
    reading: &ConditionReading,
    zone: &TimeZone,
) -> Result<()> {
    for (term, negated) in expr.leaves() {
        let value = reading
            .verdict
            .readings
            .get(&term.key)
            .and_then(|value| value.as_deref());
        let role = match term.truth(value).map(|truth| truth != negated) {
            Some(true) => ui::status::StateRole::Success,
            Some(false) => ui::status::StateRole::Failed,
            None => ui::status::StateRole::Unknown,
        };
        let (glyph, style) = ui::verdict(role);
        let reset = reading
            .resets
            .get(&term.key)
            .filter(|_| role == ui::status::StateRole::Failed);
        let shown = match (value, reset) {
            (None, _) => term_unread_reason(term, &reading.unread),
            (Some(value), Some(at)) => format!("{value} (resets {})", reset_minute(*at, zone)),
            (Some(value), _) => value.to_owned(),
        };
        writeln!(
            out,
            "  {}{term}   {} {shown}",
            if negated { "!" } else { "" },
            ui::paint(style, glyph),
        )?;
    }
    Ok(())
}

/// A term's reason from [`Verdict::unread_reasons`]; a key that reads no forge is not read.
fn term_unread_reason(term: &WhenTerm, unread: &BTreeMap<String, String>) -> String {
    unread
        .get(&term.key)
        .cloned()
        .unwrap_or_else(|| "not read".to_owned())
}

pub(super) fn unread_reason(reason: Option<when::UnreadReason>) -> String {
    rimz::harness::when::condition::unread_reason(reason, Some(Timestamp::now()))
}

/// A condition's three-valued truth as `rimz loop explain` prints it.
pub(super) fn truth_label(truth: Option<bool>) -> &'static str {
    match truth {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}
