//! Condition admission and room-aware presentation.

use super::*;
use crate::cli::wake_view::read_condition as read;
use rimz::harness::when::{self, CiSource, Verdict, WhenExpr, WhenTerm};

pub(super) fn validate(clauses: &[String], root: &Path) -> Result<()> {
    if clauses.is_empty() {
        return Ok(());
    }
    let stages = rimz::harness::when::condition::condition_stages(root)?;
    WhenExpr::parse_with_stages(clauses, Some(&stages))?;
    Ok(())
}

pub(super) fn reading(entry: &TaskEntry, expr: &WhenExpr) -> Verdict {
    read(entry, expr, None).verdict
}

pub(super) fn write_json(entry: &TaskEntry, expr: &WhenExpr) -> Result<bool> {
    let reading = read(entry, expr, None);
    let scope = entry.run_dir();
    let checked_at = if expr.reads_forge() {
        reading
            .source
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
            let unread = value.is_none().then(|| {
                term_unread_reason(
                    term,
                    &scope,
                    reading.room_open,
                    reading.source.as_ref(),
                    &reading.verdict,
                )
            });
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
        "room_open": reading.room_open,
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
    let root = entry.resolved_root();
    let open = render::room_open(&root);
    let source = runtime_for_root(&root)
        .filter(|_| open)
        .map(|runtime| CiSource::read(&runtime));
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
            term_unread_reason(term, &entry.run_dir(), open, source.as_ref(), verdict)
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

fn term_unread_reason(
    term: &WhenTerm,
    scope: &Path,
    open: bool,
    source: Option<&CiSource>,
    verdict: &Verdict,
) -> String {
    if !term.reads_forge() {
        return "not read".to_owned();
    }
    if !open {
        return "readings paused (room closed)".to_owned();
    }
    if let Some(reason) = verdict.unread.get(&term.key) {
        return reason.clone();
    }
    unread_reason(source.and_then(|source| source.unread_reason(&term.key, scope)))
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
