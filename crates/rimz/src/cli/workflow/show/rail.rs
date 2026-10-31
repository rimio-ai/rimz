use std::collections::{BTreeMap, BTreeSet};

use rimz::harness::schedule::when::WhenExpr;
use rimz::harness::workflow::{CaseMark, Hop};
use serde_json::Value;

use super::super::rail::{Rail, Row};
use super::*;

impl View<'_> {
    pub(super) fn rail(&self, out: &mut impl Write) -> Result<()> {
        let mut rail = Rail::definition(self.spec);
        rail.compact = self.record.state != RunState::Active;
        let hops = workflow::hops(self.spec, self.journal);
        let counts = entries(&hops);
        let current = self.presentation(false)?.0;
        for row in rail.rows.iter_mut().chain(&mut rail.unreachable) {
            let count = counts.get(row.name.as_str()).copied().unwrap_or(0);
            let last = self.last(&row.name, &hops);
            let is_current = !self.record.state.is_terminal() && row.name == self.record.node;
            row.glyph = if is_current {
                current
            } else if count > 0 || last.is_some() {
                GlyphRole::PipelinePassed
            } else {
                GlyphRole::PipelineFuture
            };
            row.style = if is_current {
                render::status::workflow(
                    self.record.state,
                    workflow_view::asking(self.record).is_some(),
                )
            } else {
                render::palette::muted()
            };
            let visits = self
                .spec
                .steps
                .get(&row.name)
                .and_then(|step| {
                    let key = if is_current
                        || workflow_view::session_attempt(self.record)
                            .is_some_and(|attempt| attempt.node == row.name)
                    {
                        self.record.key.as_ref()
                    } else {
                        self.journal
                            .iter()
                            .rev()
                            .find_map(|entry| match &entry.event {
                                JournalEvent::Attempt { key, .. } if entry.node == row.name => {
                                    Some(key.as_ref())
                                }
                                _ => None,
                            })
                            .flatten()
                    };
                    step.visit_bounds(
                        self.record.visits.get(&row.name),
                        key,
                        self.record.visit_grant,
                    )
                    .next()
                })
                .filter(|(_, used, _)| *used > 0 || is_current);
            let count = self
                .record
                .visits
                .get(&row.name)
                .map_or(count, |visits| visits.total);
            let repeated = if count > 1 {
                format!(" ×{count}")
            } else {
                String::new()
            };
            if rail.compact {
                row.heading = visits.map_or_else(
                    || repeated.trim_start().into(),
                    |(_, used, max)| format!("{used}/{max}"),
                );
                continue;
            }
            if let Some(summary) = last {
                row.heading.push_str(&format!(" · {summary}{repeated}"));
            }
            if let Some((_, used, max)) = visits.filter(|(_, used, _)| *used > 0) {
                row.heading.push_str(&format!(" · visit {used} of {max}"));
            }
            if is_current {
                row.heading
                    .push_str(&format!(" · since {}", self.clock(self.record.entered_at)));
                if let Some(wait) = self
                    .record
                    .wait
                    .as_ref()
                    .filter(|wait| wait.node == row.name)
                {
                    row.heading.push_str(&format!(" · checked ×{}", wait.count));
                }
                if let Some(check) = self.spec.checks.get(&row.name) {
                    let wakes = check
                        .subscriptions()?
                        .into_iter()
                        .map(|(selector, _)| selector.to_string())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    row.heading.push_str(&format!(
                        " · wakes on {}the minute tick",
                        if wakes.is_empty() {
                            String::new()
                        } else {
                            format!("{}, ", wakes.join(" "))
                        }
                    ));
                    self.cases(row)?;
                }
            }
        }
        writeln!(out)?;
        rail.write(out, render::terminal_columns(100))?;
        Ok(())
    }

    fn last(&self, node: &str, hops: &[Hop]) -> Option<String> {
        if self.spec.steps.contains_key(node)
            && let Some(entry) = self.journal.iter().rev().find(|entry| {
                entry.node == node && matches!(entry.event, JournalEvent::Attempt { .. })
            })
            && let JournalEvent::Attempt {
                handle,
                outcome,
                started_at,
                ended_at,
                cost_usd,
                head_start,
                head_end,
                ..
            } = &entry.event
        {
            let mut summary = format!(
                "{}{} · {}",
                handle
                    .as_ref()
                    .map(|handle| format!("{handle} "))
                    .unwrap_or_default(),
                outcome.as_str(),
                render::format_compact_duration(
                    ended_at.duration_since(*started_at).as_secs().max(0) as u64
                )
            );
            if let Some(cost) = cost_usd {
                summary.push_str(&format!(" · {}", rimz::theme::fmt::dollars2(*cost)));
            }
            if head_start.is_some() || head_end.is_some() {
                let short = |head: &Option<String>| {
                    head.as_deref()
                        .unwrap_or("—")
                        .chars()
                        .take(8)
                        .collect::<String>()
                };
                summary.push_str(&format!(
                    " · HEAD {} → {}",
                    short(head_start),
                    short(head_end)
                ));
            }
            summary.push_str(&format!(" · {}", self.clock(entry.at)));
            return Some(summary);
        }
        if let Some(entry) = self.journal.iter().rev().find(|entry| {
            entry.node == node && matches!(entry.event, JournalEvent::Evaluation { .. })
        }) && let JournalEvent::Evaluation { outcome, .. } = &entry.event
        {
            return Some(format!("{outcome} · {}", self.clock(entry.at)));
        }
        let hop = hops
            .iter()
            .rev()
            .find(|hop| hop.node == node && hop.kind == HopKind::Route)?;
        Some(format!(
            "{} · {}",
            hop.outcome.as_deref().unwrap_or_default(),
            self.clock(hop.at)
        ))
    }

    fn cases(&self, row: &mut Row) -> Result<()> {
        let check = &self.spec.checks[&row.name];
        if check.cases.is_empty() {
            return Ok(());
        }
        let wait = self
            .record
            .wait
            .as_ref()
            .filter(|wait| wait.node == row.name);
        let object = self.evaluation().map(|(_, object)| object);
        if object.is_none() && !wait.is_some_and(|wait| wait.outcome == "hold") {
            return Ok(());
        }
        let object = object.unwrap_or(&Value::Null);
        let marks = workflow::case_marks(check, object, wait)?;
        let reasons = workflow::unread_reasons(object);
        let mut seen = BTreeSet::new();
        for case in &mut row.cases {
            let mark = case
                .number
                .map_or(marks.else_mark, |number| marks.cases[number - 1]);
            case.mark = match mark {
                CaseMark::Winner | CaseMark::Holding => "▸",
                CaseMark::Unread => "?",
                _ => "",
            }
            .into();
            case.style = match mark {
                CaseMark::False => render::palette::faint(),
                CaseMark::NotReached => match case.target.as_str() {
                    "@done" | "@failed" => case.style,
                    _ => render::palette::muted(),
                },
                CaseMark::Unread => render::palette::warn(),
                _ if case.target == "@done" => render::palette::good().bold(),
                _ if case.target == "@failed" => render::palette::alarm().bold(),
                _ => render::palette::body().bold(),
            };
            let Some(index) = case.number.map(|number| number - 1) else {
                continue;
            };
            if mark == CaseMark::NotReached {
                continue;
            }
            let expression = WhenExpr::parse(&check.cases[index].when)?;
            for term in expression.terms() {
                if !seen.insert(term.key.clone()) {
                    continue;
                }
                if let Some(value) = object
                    .get("readings")
                    .and_then(|readings| readings.get(&term.key))
                {
                    case.readings.push(if value.is_null() {
                        format!(
                            "{} unread: {}",
                            term.key,
                            reasons.get(&term.key).map_or("not read", String::as_str)
                        )
                    } else {
                        format!("{}: {}", term.key, history::value(value))
                    });
                }
            }
            if mark == CaseMark::Holding
                && let Some(wait) = wait
                && let Some(at) =
                    workflow::hold_until(self.spec, self.record, index as u32, self.now)?
            {
                case.when = format!(
                    "{expression} holding {} of {}",
                    render::age_short(wait.first, self.now.min(at)),
                    check.cases[index].hold.as_deref().unwrap_or_default()
                );
                if workflow::next_bound(self.spec, self.record, Some(index as u32), self.now)?
                    .is_some_and(|(next, target)| {
                        next == at
                            && self.spec.target(&row.name, &case.outcome).as_deref()
                                == Some(target.as_str())
                    })
                {
                    case.target.push_str(&format!(" at {}", self.clock(at)));
                }
            }
        }
        Ok(())
    }
}

fn entries(hops: &[Hop]) -> BTreeMap<&str, u64> {
    let mut counts = BTreeMap::new();
    let mut position = None;
    for hop in hops {
        let node = hop.node.as_str();
        if position != Some(node) || hop.kind == HopKind::Resume {
            *counts.entry(node).or_default() += 1;
        }
        position = Some(node);
        if hop.kind == HopKind::Route
            && let Some(target) = hop
                .target
                .as_deref()
                .filter(|target| !target.starts_with('@'))
        {
            *counts.entry(target).or_default() += hop.count;
            position = Some(target);
        }
    }
    counts
}
