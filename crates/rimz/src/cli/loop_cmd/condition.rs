//! Condition admission and room-aware presentation.

use super::*;
use crate::cli::wake_view::{ConditionReading, RoomSource, read_condition};
use rimz::harness::when::{self, CiSource, Verdict, WhenExpr, WhenTerm};

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
        &RoomSource::read(&entry.resolved_root()),
        entry,
        expr,
        fired_at,
    )
}

pub(super) fn reading(entry: &TaskEntry, expr: &WhenExpr) -> Verdict {
    read(entry, expr, None).verdict
}

pub(super) fn write_json(entry: &TaskEntry, expr: &WhenExpr) -> Result<bool> {
    let room = RoomSource::read(&entry.resolved_root());
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

pub(super) fn observe(
    name: &str,
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
        name,
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
    let verdict = read(entry, expr, fired_at).verdict;
    write_terms(out, entry, expr, &verdict)?;
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
    entry: &TaskEntry,
    expr: &WhenExpr,
    verdict: &Verdict,
) -> Result<()> {
    let room = RoomSource::read(&entry.resolved_root());
    let unread = verdict.unread_reasons(
        expr,
        &entry.run_dir(),
        room.source.as_ref(),
        Some(Timestamp::now()),
    );
    for (term, negated) in expr.leaves() {
        let value = verdict
            .readings
            .get(&term.key)
            .and_then(|value| value.as_deref());
        let role = match term.truth(value).map(|truth| truth != negated) {
            Some(true) => ui::status::StateRole::Success,
            Some(false) => ui::status::StateRole::Failed,
            None => ui::status::StateRole::Unknown,
        };
        let (glyph, style) = ui::verdict(role);
        let reason = if value.is_none() {
            term_unread_reason(term, &unread)
        } else {
            String::new()
        };
        writeln!(
            out,
            "  {}{term}   {} {}",
            if negated { "!" } else { "" },
            ui::paint(style, glyph),
            value.unwrap_or(&reason)
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
