//! Condition admission and room-aware presentation.

use super::*;
use schedule::when::{self, CiSource, Verdict, WhenExpr, WhenTerm, WindowReadings};

struct Reading {
    verdict: Verdict,
    room_open: bool,
    source: Option<CiSource>,
}

pub(super) fn validate(clauses: &[String], root: &Path) -> Result<()> {
    if clauses.is_empty() {
        return Ok(());
    }
    let stages = schedule::condition_stages(root)?;
    WhenExpr::parse_with_stages(clauses, Some(&stages))?;
    Ok(())
}

pub(super) fn reading(entry: &TaskEntry, expr: &WhenExpr) -> Verdict {
    read(entry, expr).verdict
}

fn read(entry: &TaskEntry, expr: &WhenExpr) -> Reading {
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let room_open = render::room_open(&root);
    let source = runtime.as_ref().filter(|_| room_open).map(CiSource::read);
    let windows = WindowReadings::new(runtime.as_ref(), Timestamp::now());
    let verdict = when::evaluate(
        expr,
        &entry.run_dir(),
        source.as_ref(),
        entry.provider.as_ref(),
        entry.account.as_ref(),
        &windows,
    );
    Reading {
        verdict,
        room_open,
        source,
    }
}

pub(super) fn write_json(entry: &TaskEntry, expr: &WhenExpr) -> Result<bool> {
    let reading = read(entry, expr);
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
                term_unread_reason(term, &scope, reading.room_open, reading.source.as_ref())
            });
            serde_json::json!({
                "term": term.to_string(),
                "key": term.key,
                "negated": negated,
                "reading": value,
                "met": term.matches(value) != negated,
                "unread": unread,
            })
        })
        .collect::<Vec<_>>();
    ui::json_pretty(&serde_json::json!({
        "when": expr.to_string(),
        "scope": scope,
        "room_open": reading.room_open,
        "now": reading.verdict.ok,
        "checked_at": checked_at,
        "terms": terms,
    }))?;
    Ok(reading.verdict.ok)
}

pub(super) fn observe(
    name: &str,
    entry: &TaskEntry,
    timing: schedule::TaskTiming,
    now: Timestamp,
) -> schedule::TaskTiming {
    let Ok(schedule::ParsedTrigger {
        trigger: schedule::Trigger::Condition { expr, .. },
        ..
    }) = timing.parsed()
    else {
        return timing;
    };
    let verdict = reading(entry, expr);
    let states = runtime_for_root(&entry.resolved_root())
        .map(|runtime| schedule::fire::last_when_states(&runtime))
        .unwrap_or_default();
    timing.with_condition(&verdict, states.get(name), now)
}

pub(super) fn write_receipt(
    out: &mut impl Write,
    entry: &TaskEntry,
    parsed: &schedule::ParsedTrigger,
) -> Result<()> {
    let schedule::Trigger::Condition { expr, hold } = &parsed.trigger else {
        return Ok(());
    };
    let verdict = reading(entry, expr);
    let state = if verdict.ok {
        match hold {
            Some(hold) => schedule::TaskTimingState::Holding {
                elapsed: Duration::ZERO,
                hold: *hold,
            },
            None => schedule::TaskTimingState::Due(Timestamp::now()),
        }
    } else {
        schedule::TaskTimingState::Waiting {
            readings: expr
                .readings(&verdict)
                .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
                .collect(),
        }
    };
    writeln!(out, "scope: {}", entry.run_dir().display())?;
    writeln!(out, "now: {}", state.condition_label().unwrap_or_default())?;
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
    let verdict = reading(entry, expr);
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
        let role = if term.matches(value) != negated {
            ui::status::StateRole::Success
        } else if value.is_none() {
            ui::status::StateRole::Unknown
        } else {
            ui::status::StateRole::Failed
        };
        let (glyph, style) = ui::verdict(role);
        let reason = if value.is_none() {
            term_unread_reason(term, &entry.run_dir(), open, source.as_ref())
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
) -> String {
    if !term.reads_forge() {
        return "not read".to_owned();
    }
    if !open {
        return "readings paused (room closed)".to_owned();
    }
    unread_reason(source.and_then(|source| source.unread_reason(&term.key, scope)))
}

pub(super) fn unread_reason(reason: Option<when::UnreadReason>) -> String {
    schedule::unread_reason(reason, Timestamp::now())
}
