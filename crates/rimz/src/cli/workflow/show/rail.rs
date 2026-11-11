use std::collections::BTreeSet;
use std::io::IsTerminal;

use rimz::harness::when::WhenExpr;
use rimz::harness::workflow::{CaseMark, Hop, reached};
use serde_json::Value;

use super::super::rail::{Rail, Row, horizontal};
use super::*;

impl View<'_> {
    pub(super) fn rail(&self, out: &mut impl Write) -> Result<()> {
        let hops = workflow::hops(self.spec, self.journal, self.record.held.as_ref());
        if self.record.state != RunState::Active {
            writeln!(out)?;
            return Ok(horizontal(
                out,
                self.spec,
                self.record,
                &hops,
                self.journal,
                std::io::stdout()
                    .is_terminal()
                    .then(|| render::terminal_columns(100)),
                0,
            )?);
        }
        let mut rail = Rail::definition(self.spec);
        let counts = reached(&hops, self.journal);
        let current = workflow_view::glyph_role(self.record);
        for row in rail.rows.iter_mut().chain(&mut rail.unreachable) {
            let count = counts.get(row.name.as_str()).copied().unwrap_or(0);
            let is_current = row.name == self.record.node
                || self
                    .record
                    .held
                    .as_ref()
                    .is_some_and(|held| held.attempt.node == row.name);
            let check = self.spec.checks.contains_key(&row.name);
            row.glyph = if is_current {
                current
            } else {
                match (check, count > 0) {
                    (true, true) => GlyphRole::PipelineCheckPassed,
                    (true, false) => GlyphRole::PipelineCheckFuture,
                    (false, true) => GlyphRole::PipelinePassed,
                    (false, false) => GlyphRole::PipelineFuture,
                }
            };
            row.style = if is_current {
                render::status::workflow(
                    self.record.state,
                    workflow_view::asking(self.record).is_some(),
                )
                .bold()
            } else if count > 0 {
                render::palette::body()
            } else {
                render::palette::muted()
            };
            row.glyph_style = if !is_current && count > 0 {
                render::palette::good()
            } else {
                row.style
            };
            let visits = self
                .spec
                .steps
                .get(&row.name)
                .and_then(|step| {
                    let bucket = if is_current
                        || self
                            .record
                            .session_attempt()
                            .is_some_and(|attempt| attempt.node == row.name)
                    {
                        self.record.visit_bucket()
                    } else {
                        self.journal
                            .iter()
                            .rev()
                            .find_map(|entry| match &entry.event {
                                JournalEvent::Attempt { routed_by, key, .. }
                                    if entry.node == row.name =>
                                {
                                    Some((routed_by.as_deref(), key.as_ref()))
                                }
                                _ => None,
                            })
                            .unwrap_or((None, None))
                    };
                    step.visit_bounds(
                        self.record.visits.get(&row.name),
                        bucket,
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
            if is_current && check {
                if let Some((entry, _)) = self.evaluation()
                    && let JournalEvent::Evaluation { outcome, .. } = &entry.event
                {
                    row.heading.push_str(&format!(" · {outcome}{repeated}"));
                }
            } else if let Some(summary) = self.last(&row.name, &hops) {
                row.heading.push_str(&format!(" · {summary}{repeated}"));
            }
            if let Some((_, used, max)) = visits.filter(|(_, used, _)| *used > 0) {
                row.heading.push_str(&format!(" · visit {used} of {max}"));
            }
            if is_current {
                let entered = self
                    .record
                    .held
                    .as_ref()
                    .filter(|held| held.attempt.node == row.name)
                    .map_or(self.record.entered_at, |held| held.attempt.started_at);
                row.heading
                    .push_str(&format!(" · since {}", self.clock(entered)));
                if check && let Some((entry, _)) = self.evaluation() {
                    row.heading
                        .push_str(&format!(" · last checked {}", self.clock(entry.at)));
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
                if let Some(step) = self.spec.steps.get(&row.name) {
                    let receives = step
                        .subscriptions()?
                        .into_iter()
                        .map(|(selector, _)| selector.to_string())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    if !receives.is_empty() {
                        row.heading
                            .push_str(&format!(" · receives {}", receives.join(" ")));
                    }
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
                "{} · {}",
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
                summary.push_str(&if head_start == head_end {
                    format!(" · HEAD {}, no new commits", short(head_start))
                } else {
                    format!(" · HEAD {} → {}", short(head_start), short(head_end))
                });
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
        if !matches!(check.kind(), Some(workflow::CheckKind::Condition)) {
            return Ok(());
        }
        let cases = check.condition_cases();
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
                _ => render::palette::cool().bold(),
            };
            let Some(index) = case.number.map(|number| number - 1) else {
                continue;
            };
            if mark == CaseMark::NotReached {
                continue;
            }
            let expression = WhenExpr::parse(&cases[index].when)?;
            for term in expression.terms() {
                if mark != CaseMark::Unread || !seen.insert(term.key.clone()) {
                    continue;
                }
                if let Some(value) = object
                    .get("readings")
                    .and_then(|readings| readings.get(&term.key))
                    && value.is_null()
                {
                    case.readings.push(format!(
                        "{} unread: {}",
                        term.key,
                        reasons.get(&term.key).map_or("not read", String::as_str)
                    ));
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
                    cases[index].hold.as_deref().unwrap_or_default()
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
