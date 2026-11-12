use std::io::Write;

use anyhow::Result;
use jiff::Timestamp;
use rimz::harness::workflow::{self, WorkflowSpec};
use rimz::store::workflow::{
    self as records, Children, JournalEntry, JournalEvent, RunRecord, TurnEndAction,
};
use serde_json::Value;

use super::super::{render, workflow_view};

pub(in crate::cli) struct HistoryRow<'a> {
    first: &'a records::JournalEntry,
    latest: &'a records::JournalEntry,
    count: u64,
    index: usize,
    cost_usd: Option<f64>,
}

impl HistoryRow<'_> {
    fn note(&self) -> String {
        let (count, first, last, detail) = match &self.latest.event {
            records::JournalEvent::Wait {
                count,
                first,
                last,
                detail,
                ..
            } => (*count, *first, *last, detail.as_deref()),
            _ => (self.count, self.first.at, self.latest.at, None),
        };
        let mut note = fold_note(count, first, last);
        if let Some(detail) = detail {
            note.push_str(&format!(" · {detail}"));
        }
        note
    }

    fn target(
        &self,
        spec: &WorkflowSpec,
        journal: &[JournalEntry],
        record: &RunRecord,
    ) -> Option<String> {
        match &self.latest.event {
            JournalEvent::Evaluation { .. } | JournalEvent::Route { .. } => {
                workflow::evaluation_target(
                    spec,
                    self.latest,
                    &journal[self.index + 1..],
                    record.held.as_ref(),
                )
            }
            _ => None,
        }
    }
}

#[derive(PartialEq, Eq)]
enum HistoryKey<'a> {
    Evaluation {
        node: &'a str,
        origin: records::EvaluationOrigin,
        outcome: &'a str,
        case: Option<u64>,
    },
    Wake {
        node: &'a str,
        signal: &'a str,
    },
    Signal {
        node: &'a str,
        seq: Option<u64>,
        signal: &'a str,
    },
}

fn history_key(entry: &records::JournalEntry) -> Option<HistoryKey<'_>> {
    match &entry.event {
        records::JournalEvent::Evaluation {
            origin,
            outcome,
            object,
            ..
        } => Some(HistoryKey::Evaluation {
            node: &entry.node,
            origin: *origin,
            outcome,
            case: object.get("case").and_then(serde_json::Value::as_u64),
        }),
        records::JournalEvent::Wake { signal } => Some(HistoryKey::Wake {
            node: &entry.node,
            signal,
        }),
        records::JournalEvent::Signal { signal, .. } => Some(HistoryKey::Signal {
            node: &entry.node,
            seq: entry.seq,
            signal,
        }),
        _ => None,
    }
}

pub(in crate::cli) fn collapsed_history(journal: &[records::JournalEntry]) -> Vec<HistoryRow<'_>> {
    let mut rows = Vec::<HistoryRow<'_>>::new();
    for (index, entry) in journal.iter().enumerate() {
        if workflow::repeats_evaluation(entry, index.checked_sub(1).map(|i| &journal[i])) {
            continue;
        }
        if let records::JournalEvent::Wait {
            outcome,
            first,
            last,
            ..
        } = &entry.event
            && wait_row_counts(&rows, &entry.node, outcome, *first, *last)
                .next()
                .is_some()
        {
            continue;
        }
        if let Some(key) = history_key(entry)
            && let Some(row) = rows.last_mut()
            && history_key(row.latest).as_ref() == Some(&key)
        {
            row.latest = entry;
            row.index = index;
            row.count += 1;
            if let Some(cost) = evaluation_cost(entry) {
                *row.cost_usd.get_or_insert(0.0) += cost;
            }
            continue;
        }
        rows.push(HistoryRow {
            first: entry,
            latest: entry,
            count: 1,
            index,
            cost_usd: evaluation_cost(entry),
        });
    }
    rows
}

fn evaluation_cost(entry: &records::JournalEntry) -> Option<f64> {
    match &entry.event {
        records::JournalEvent::Evaluation { cost_usd, .. } => *cost_usd,
        _ => None,
    }
}

pub(in crate::cli) fn wait_row_counts<'a>(
    rows: &'a [HistoryRow<'_>],
    node: &'a str,
    outcome: &'a str,
    first: jiff::Timestamp,
    last: jiff::Timestamp,
) -> impl Iterator<Item = u64> + 'a {
    rows.iter()
        .rev()
        .filter(move |row| {
            row.latest.node == node
                && matches!(
                    &row.latest.event,
                    records::JournalEvent::Evaluation { outcome: answer, .. } if answer == outcome
                )
                && row.first.at <= last
                && first <= row.latest.at
        })
        .map(|row| row.count)
}

struct History<'a> {
    record: &'a RunRecord,
    spec: &'a WorkflowSpec,
    journal: &'a [JournalEntry],
    now: Timestamp,
    prose: render::prose::Prose,
}

pub(in crate::cli) fn render(
    record: &RunRecord,
    spec: &WorkflowSpec,
    journal: &[JournalEntry],
    now: Timestamp,
    prose: render::prose::Prose,
    out: &mut impl Write,
) -> Result<()> {
    History {
        record,
        spec,
        journal,
        now,
        prose,
    }
    .render(out)
}

pub(in crate::cli) fn terminal(
    record: &RunRecord,
    spec: &WorkflowSpec,
    journal: &[JournalEntry],
    prose: render::prose::Prose,
    out: &mut impl Write,
) -> Result<()> {
    let now = record.terminal_at.unwrap_or(record.entered_at);
    let glyph = rimz::theme::theme_glyphs(&super::super::machine_config().theme)(
        workflow_view::glyph_role(record),
    );
    writeln!(out, "{}", record.address())?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::status::workflow(record.state, false).bold(),
            &format!("{glyph} {} {}", record.state.as_str(), dated_clock(now))
        )
    )?;
    for warning in &spec.ignored {
        writeln!(out, "warning: {}", warning.message)?;
    }
    render(record, spec, journal, now, prose, out)
}

pub(in crate::cli) fn write_copy(state: &rimz::StatePaths, record: &RunRecord) -> Result<()> {
    let Some(relative) = record.notes.as_ref().filter(|_| record.state.is_terminal()) else {
        return Ok(());
    };
    let folder = state.notes_run(rimz::disk::paths::Class::AutomationNotes, relative);
    if !folder.try_exists()? {
        return Ok(());
    }
    let paths = state.workflow(&record.id);
    let spec = workflow::load(&paths.home)?;
    let journal = records::journal(&paths)?;
    let mut out = anstream::AutoStream::new(Vec::new(), anstream::ColorChoice::Never);
    terminal(record, &spec, &journal, render::prose::Prose::Raw, &mut out)?;
    rimz::disk::atomic::write_bytes_atomically(&folder.join("history.txt"), &out.into_inner())?;
    Ok(())
}

fn dated_clock(at: Timestamp) -> String {
    at.to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

impl History<'_> {
    fn clock(&self, at: Timestamp) -> String {
        if self.record.state.is_terminal() {
            dated_clock(at)
        } else {
            render::workflow_clock(at, self.now)
        }
    }

    fn prose(&self, out: &mut impl Write, text: &str, indent: &str) -> Result<()> {
        for line in self
            .prose
            .lines(text, render::prose::prose_width(indent.len()))
        {
            writeln!(out, "{indent}{line}")?;
        }
        Ok(())
    }

    /// The leader of the attempt `entry` belongs to: its journaled row once it ended,
    /// else the live session.
    fn leader(&self, entry: &records::JournalEntry) -> Option<&str> {
        let ended = self.journal.iter().find_map(|row| match &row.event {
            JournalEvent::Attempt { handle, .. }
                if row.seq == entry.seq && row.node == entry.node =>
            {
                handle.as_deref()
            }
            _ => None,
        });
        ended.or_else(|| {
            let attempt = self.record.session_attempt()?;
            (Some(attempt.seq) == entry.seq)
                .then(|| attempt.run.as_ref().map(|run| run.handle.as_str()))
                .flatten()
        })
    }

    fn holding(&self) -> Result<bool> {
        let Some(wait) = self
            .record
            .wait
            .as_ref()
            .filter(|wait| wait.outcome == "hold")
        else {
            return Ok(false);
        };
        let Some(index) = wait.case else {
            return Ok(false);
        };
        Ok(workflow::hold_until(self.spec, self.record, index, self.now)?.is_some())
    }

    fn render(&self, out: &mut impl Write) -> Result<()> {
        if let Some(asking) = workflow_view::asking(self.record) {
            self.prose(out, &asking.ask.text, "  ")?;
        }
        if let Some(wait) = &self.record.wait
            && (workflow_view::asking(self.record).is_some()
                || (!workflow_view::with_you(self.record) && self.holding()?))
            && let Some(line) = workflow_view::wait(self.record, Some(self.spec), self.now)
        {
            writeln!(out, "  waiting: {}, {line}", wait.node)?;
        }
        let rows = collapsed_history(self.journal);
        for (index, row) in rows.iter().enumerate() {
            let entry = row.latest;
            let seq = match (row.count, row.first.seq, entry.seq) {
                (1, _, Some(seq)) => seq.to_string(),
                (_, Some(first), Some(last)) => format!("{first}–{last}"),
                _ => "—".into(),
            };
            let note = row.note();
            write!(out, "  {} #{seq} {} ", self.clock(entry.at), entry.node)?;
            match &entry.event {
                JournalEvent::Evaluation {
                    outcome,
                    object,
                    stderr_tail,
                    digests,
                    ..
                } => {
                    let answer = if object.is_null() {
                        format!("said nothing · {outcome}")
                    } else {
                        format!("said {outcome}")
                    };
                    writeln!(
                        out,
                        "{answer}{}{note}{}",
                        row.target(self.spec, self.journal, self.record)
                            .map_or_else(String::new, |target| format!(" → {target}")),
                        cost(row.cost_usd)
                    )?;
                    self.answer(out, object)?;
                    if outcome == "invalid"
                        && let Some(stderr) = stderr_tail
                    {
                        tail(out, stderr)?;
                    }
                    // The check-retry context carries only the outcome and reason of the
                    // failed evaluation the engine journaled before this one, which its own
                    // row already prints, so it takes no line here; `--json` keeps it.
                    digests_out(out, digests)?;
                }
                JournalEvent::Attempt {
                    outcome,
                    visit,
                    started_past_limit,
                    started_at,
                    ended_at,
                    handle,
                    code,
                    head_start,
                    head_end,
                    cost_usd,
                    log,
                    reason,
                    children,
                    limited,
                    asked,
                    digests,
                    ..
                } => {
                    writeln!(
                        out,
                        "attempt {} · visit {visit} · {}",
                        outcome.as_str(),
                        render::format_compact_duration(
                            ended_at.duration_since(*started_at).as_secs().max(0) as u64
                        )
                    )?;
                    let step = self.spec.steps.get(&entry.node);
                    let message = step.is_some_and(|step| step.message.is_some());
                    if message {
                        match (handle, reason) {
                            (Some(handle), _) => {
                                writeln!(out, "    to {handle} · {}", outcome.as_str())?
                            }
                            (None, Some(reason)) => {
                                self.prose(out, &format!("to — · {reason}"), "    ")?
                            }
                            (None, None) => writeln!(out, "    to — · {}", outcome.as_str())?,
                        }
                    } else if step.is_some_and(|step| step.agent.is_some()) {
                        writeln!(
                            out,
                            "    {} HEAD {} → {} cost {}",
                            handle.as_deref().unwrap_or("—"),
                            short(head_start),
                            short(head_end),
                            cost_usd.map_or_else(|| "unknown".into(), rimz::theme::fmt::dollars2)
                        )?;
                    } else {
                        writeln!(
                            out,
                            "    exit {} HEAD {} → {}",
                            code.map_or_else(|| "—".into(), |code| code.to_string()),
                            short(head_start),
                            short(head_end)
                        )?;
                        if let Some(cost) = cost_usd {
                            writeln!(out, "    cost: {}", rimz::theme::fmt::dollars2(*cost))?;
                        }
                    }
                    if let Some(log) = log {
                        writeln!(out, "    log: {}", log.display())?;
                    }
                    if let Some(text) = started_past_limit {
                        writeln!(out, "    {text}; started anyway: manual starts do not wait")?;
                    }
                    if let Some(reason) = reason.as_ref().filter(|_| !message) {
                        self.prose(out, &format!("reason: {reason}"), "    ")?;
                    }
                    if let Some(children) = children {
                        children_out(out, children)?;
                    }
                    if let Some(limited) = limited {
                        writeln!(
                            out,
                            "    reset_at: {}",
                            limited
                                .reset_at
                                .map_or_else(|| "unknown".into(), |at| self.clock(at))
                        )?;
                    }
                    if let Some(asked) = asked {
                        self.prose(
                            out,
                            &format!("{} ({}): {}", asked.handle, asked.kind, asked.text),
                            "    ",
                        )?;
                    }
                    digests_out(out, digests)?;
                }
                JournalEvent::Route { outcome, input } => {
                    let mut input = input.clone();
                    if let Some(at) = input
                        .get("deadline")
                        .and_then(Value::as_str)
                        .and_then(|raw| raw.parse::<Timestamp>().ok())
                    {
                        input["deadline"] = Value::String(self.clock(at));
                    }
                    writeln!(
                        out,
                        "route {outcome}{} · {}",
                        row.target(self.spec, self.journal, self.record)
                            .map_or_else(String::new, |target| format!(" → {target}")),
                        value(&input)
                    )?;
                }
                JournalEvent::VisitsReset { step, outcome, all } => {
                    let scope = if *all { "all visits" } else { "visits" };
                    if step != &entry.node {
                        write!(out, "{step} ")?;
                    }
                    writeln!(out, "{scope} reset on {outcome}")?;
                }
                JournalEvent::Restart { continued, .. } => {
                    let mode = if *continued { "continued" } else { "fresh" };
                    writeln!(out, "restart · {mode} · visits restarted at 1")?;
                }
                JournalEvent::Wait {
                    outcome,
                    recheck_at,
                    ..
                } => {
                    writeln!(out, "wait {outcome}{note}")?;
                    if let Some(at) = recheck_at {
                        writeln!(out, "    recheck {}", self.clock(*at))?;
                    }
                }
                JournalEvent::TurnEnd {
                    action,
                    check,
                    outcome,
                    target,
                    reason,
                    visit,
                    message,
                    ..
                } => {
                    let action_name = match action {
                        TurnEndAction::Kept => "kept",
                        TurnEndAction::Pushed => "pushed",
                        TurnEndAction::Closed => "closed",
                        TurnEndAction::Discarded => "discarded",
                        TurnEndAction::Interrupted => "interrupted",
                    };
                    writeln!(
                        out,
                        "turn_end {action_name} · {check} {}{}",
                        outcome.as_deref().unwrap_or("—"),
                        target
                            .as_ref()
                            .filter(|_| matches!(
                                action,
                                TurnEndAction::Kept | TurnEndAction::Pushed | TurnEndAction::Closed
                            ))
                            .map_or_else(String::new, |target| format!(" → {target}"))
                    )?;
                    if let Some(visit) = visit {
                        writeln!(out, "    visit {visit}")?;
                    }
                    if let Some(message) = message {
                        writeln!(out, "    message: {message}")?;
                    }
                    if let Some(reason) = reason {
                        self.prose(out, reason, "    ")?;
                    }
                }
                JournalEvent::Park { reason, text } => {
                    writeln!(out, "park {}", reason.as_str())?;
                    let object = rows[..index]
                        .iter()
                        .rev()
                        .filter(|row| row.latest.node == entry.node)
                        .find_map(|row| match &row.latest.event {
                            JournalEvent::Evaluation { object, .. }
                            | JournalEvent::Route { input: object, .. } => Some(object),
                            _ => None,
                        });
                    let repeats_reason = object
                        .and_then(|object| object.get("reason"))
                        .and_then(Value::as_str)
                        == text.as_deref();
                    if let Some(text) = text
                        && !repeats_reason
                    {
                        self.prose(out, text, "    ")?;
                    }
                }
                JournalEvent::Resume {
                    visit_grant,
                    deadline,
                } => {
                    writeln!(
                        out,
                        "resume → {} · visit grant {visit_grant} · deadline {}",
                        entry.node,
                        deadline.map_or_else(|| "none".into(), |at| self.clock(at))
                    )?;
                }
                JournalEvent::Cancel { reason } => writeln!(
                    out,
                    "cancel canceled{}",
                    reason
                        .as_ref()
                        .map_or_else(String::new, |reason| format!(" · {reason}"))
                )?,
                JournalEvent::Terminal { state } => writeln!(out, "terminal {}", state.as_str())?,
                JournalEvent::Wake { signal } => writeln!(out, "wake {signal}{note}")?,
                JournalEvent::Signal { signal, .. } => writeln!(
                    out,
                    "signal {signal} → {}{note}",
                    self.leader(entry).unwrap_or("—")
                )?,
            }
        }
        Ok(())
    }

    fn answer(&self, out: &mut impl Write, object: &Value) -> Result<()> {
        if let Some(readings) = object.get("readings") {
            writeln!(out, "    {}", value(readings))?;
        }
        if let Some(reason) = object.get("reason").and_then(Value::as_str) {
            self.prose(
                out,
                &format!("reason: {}", compact_reason(object, reason)),
                "    ",
            )?;
        }
        if let Some(log) = object.get("log").and_then(Value::as_str) {
            writeln!(out, "    log: {log}")?;
        }
        Ok(())
    }
}

pub(in crate::cli) fn fold_note(count: u64, first: Timestamp, last: Timestamp) -> String {
    if count <= 1 {
        return String::new();
    }
    let mut note = format!(" ×{count}");
    if let Some(span) = render::folded_span(first, last) {
        note.push_str(&format!(" {span}"));
    }
    note
}

pub(in crate::cli) fn value(value: &Value) -> String {
    match value {
        Value::Null => "unread".into(),
        Value::String(text) => render::one_line(text),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, field)| format!("{key}={}", self::value(field)))
            .collect::<Vec<_>>()
            .join(" · "),
        Value::Array(items) => items.iter().map(self::value).collect::<Vec<_>>().join(", "),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

fn short(head: &Option<String>) -> String {
    head.as_deref().unwrap_or("—").chars().take(8).collect()
}

fn cost(cost: Option<f64>) -> String {
    cost.map_or_else(String::new, |cost| {
        format!(" · {}", rimz::theme::fmt::dollars2(cost))
    })
}

pub(in crate::cli) fn children_out(out: &mut impl Write, children: &Children) -> Result<()> {
    for (label, children) in [("stopped", &children.stopped), ("live", &children.live)] {
        if children.is_empty() {
            continue;
        }
        let handles = children
            .iter()
            .map(|child| {
                child.team.as_ref().map_or_else(
                    || child.handle.clone(),
                    |team| {
                        format!(
                            "{} ({}#{}, {})",
                            child.handle,
                            team.name,
                            team.channel,
                            team.stage.as_deref().unwrap_or("stage unknown")
                        )
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "    {label} ({}): {handles}", children.len())?;
    }
    Ok(())
}

pub(in crate::cli) fn digests_out(
    out: &mut impl Write,
    digests: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    for (path, digest) in digests {
        writeln!(
            out,
            "    {path}: {}",
            digest.chars().take(12).collect::<String>()
        )?;
    }
    Ok(())
}

pub(in crate::cli) fn tail(out: &mut impl Write, text: &str) -> Result<()> {
    let lines = text.lines().collect::<Vec<_>>();
    for line in &lines[lines.len().saturating_sub(8)..] {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

fn compact_reason(object: &Value, reason: &str) -> String {
    let reasons = workflow::unread_reasons(object);
    let mut causes = reasons.values();
    match causes.next() {
        Some(cause) if reasons.len() > 1 && causes.all(|other| other == cause) => {
            let fields: Vec<_> = reasons.keys().map(String::as_str).collect();
            format!("{} ({cause})", fields.join(", "))
        }
        _ => reason.into(),
    }
}
