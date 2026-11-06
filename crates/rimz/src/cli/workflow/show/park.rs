use rimz::store::workflow::ParkReason;

use super::*;

/// What last sent the run to its node: a routed outcome, manual re-entry, or neither.
enum Sender {
    Edge(String, String),
    Resume,
    Restart,
    Direct,
}

impl View<'_> {
    pub(super) fn park(&self, out: &mut impl Write) -> Result<()> {
        let Some(park) = &self.record.park else {
            return Ok(());
        };
        match park.reason {
            ParkReason::Cap => self.cap(out)?,
            ParkReason::Timeout => self.timeout(out)?,
            ParkReason::Invalid => {
                if let Some(text) = &park.text {
                    prose(out, text, "")?;
                }
                if let Some((entry, _)) = self.evaluation()
                    && let JournalEvent::Evaluation {
                        stderr_tail: Some(stderr),
                        ..
                    } = &entry.event
                {
                    history::tail(out, stderr)?;
                }
            }
            ParkReason::User => prose(
                out,
                &park.text.as_ref().map_or_else(
                    || format!("{} parked the run", self.record.node),
                    |text| format!("{} says: {text}", self.record.node),
                ),
                "",
            )?,
            ParkReason::Asked => {
                let ask = self
                    .record
                    .prev_attempt
                    .as_ref()
                    .and_then(|a| a.asked.as_ref())
                    .or_else(|| {
                        self.journal
                            .iter()
                            .rev()
                            .find_map(|entry| match &entry.event {
                                JournalEvent::Attempt { asked, .. }
                                    if entry.node == self.record.node =>
                                {
                                    asked.as_ref()
                                }
                                _ => None,
                            })
                    });
                if let Some(ask) = ask {
                    prose(
                        out,
                        &format!("{} ({}): {}", ask.handle, ask.kind, ask.text),
                        "",
                    )?;
                }
                writeln!(out, "the turn was stopped and its pane closed")?;
            }
            ParkReason::Deadline => {
                if let Some(at) = self.record.deadline {
                    writeln!(out, "the deadline {} passed", self.clock(at))?;
                }
            }
            ParkReason::Check => {
                writeln!(out, "the check failed to evaluate twice")?;
                if let Some(text) = &park.text {
                    prose(out, text, "")?;
                }
            }
        }
        writeln!(
            out,
            "{}",
            render::paint(render::palette::header(), "you can")
        )?;
        match park.reason {
            ParkReason::Cap => {
                self.option(
                    out,
                    None,
                    &["visits=+1".into()],
                    "run the step once more; every capped step gets one more visit",
                )?;
                if let Sender::Edge(node, _) = self.sender(self.journal)
                    && self.spec.checks.contains_key(&node)
                {
                    self.option(out, Some(&node), &[], "check again after a fix by hand")?;
                }
            }
            ParkReason::User => {
                self.option(out, None, &[], &format!("run {} again", self.record.node))?;
                if self.spec.checks.contains_key(&self.record.node) {
                    let nodes = self
                        .spec
                        .edges(&self.record.node)
                        .into_iter()
                        .filter(|edge| edge.outcome != "deadline")
                        .map(|edge| edge.target)
                        .filter(|target| !target.starts_with('@'))
                        .collect::<std::collections::BTreeSet<_>>();
                    for node in nodes {
                        self.option(out, Some(&node), &[], &format!("continue at {node}"))?;
                    }
                }
            }
            ParkReason::Deadline => {
                self.option(out, None, &[], "go on with no deadline; it has fired")?;
                self.option(out, None, &["deadline=<when>".into()], "set a new deadline")?;
            }
            ParkReason::Asked => {
                writeln!(
                    out,
                    "  write the answer in {}, then",
                    render::home_relative_path(&self.paths.signs(Some(&self.record.node)))
                )?;
                self.option(out, None, &[], "relaunch the step with that answer")?;
            }
            ParkReason::Timeout => {
                self.option(out, None, &[], "run the node again with a fresh timeout")?
            }
            ParkReason::Invalid => self.option(
                out,
                None,
                &[],
                if self.spec.steps.contains_key(&self.record.node) {
                    "retry after fixing the launch"
                } else {
                    "retry after fixing the check"
                },
            )?,
            ParkReason::Check => {
                self.option(out, None, &[], "run the check again; retry cleared")?
            }
        }
        writeln!(
            out,
            "  {}  end the run; commits and worktree stay",
            self.command("cancel", None, &[], &[])
        )?;
        Ok(())
    }

    fn option(
        &self,
        out: &mut impl Write,
        at: Option<&str>,
        extend: &[String],
        gloss: &str,
    ) -> Result<()> {
        writeln!(
            out,
            "  {}  {gloss}",
            self.command("resume", at, extend, &[])
        )?;
        Ok(())
    }

    fn cap(&self, out: &mut impl Write) -> Result<()> {
        let Some((bound, used, max)) = self
            .spec
            .steps
            .get(&self.record.node)
            .and_then(|step| step.tripped_visit_bound(self.record))
        else {
            return Ok(());
        };
        let per_outcome = bound == "max-visits-per-outcome";
        let bucket = self.record.visit_bucket();
        let key = bucket
            .1
            .map(serde_json::to_string)
            .transpose()?
            .map(|key| format!(" key {key}"))
            .unwrap_or_default();
        let bucket_label = if per_outcome {
            format!(" for {}{key}", bucket.0.unwrap_or("null"))
        } else {
            String::new()
        };
        writeln!(out, "{bound}{bucket_label}: {used} of {max} visits used")?;
        let since = self
            .journal
            .iter()
            .rposition(|entry| match &entry.event {
                JournalEvent::VisitsReset { step, all, .. } => {
                    step == &self.record.node && (per_outcome || *all)
                }
                _ => false,
            })
            .map_or(0, |index| index + 1);
        let mut total_visit = 0;
        for (index, entry) in self.journal.iter().enumerate().skip(since) {
            if let JournalEvent::TurnEnd {
                action: records::TurnEndAction::Pushed,
                check,
                outcome,
                visit: Some(visit),
                message,
                ..
            } = &entry.event
            {
                if entry.node != self.record.node {
                    continue;
                }
                let key = self.journal[..index]
                    .iter()
                    .rev()
                    .find_map(|answer| match &answer.event {
                        JournalEvent::Evaluation { object, .. } if &answer.node == check => {
                            Some(object.get("key").and_then(|key| {
                                serde_json::from_value::<records::Key>(key.clone()).ok()
                            }))
                        }
                        _ => None,
                    })
                    .flatten();
                if per_outcome && (outcome.as_deref(), key.as_ref()) != bucket {
                    continue;
                }
                total_visit += 1;
                let visit = if per_outcome { *visit } else { total_visit };
                writeln!(
                    out,
                    "  {} {check} {} · {} {visit} · push-back{}",
                    self.clock(entry.at),
                    outcome.as_deref().unwrap_or("—"),
                    entry.node,
                    message
                        .as_ref()
                        .map_or_else(String::new, |message| format!(" {message}"))
                )?;
                continue;
            }
            let JournalEvent::Attempt {
                visit,
                key,
                routed_by,
                handle,
                code,
                outcome,
                started_at,
                ended_at,
                cost_usd,
                ..
            } = &entry.event
            else {
                continue;
            };
            if entry.node != self.record.node
                || (per_outcome && (routed_by.as_deref(), key.as_ref()) != bucket)
            {
                continue;
            }
            total_visit += 1;
            let visit = if per_outcome { *visit } else { total_visit };
            let sender = match self.sender(&self.journal[..index]) {
                Sender::Edge(node, outcome) => format!("{node} {outcome}"),
                Sender::Resume => "resumed".into(),
                Sender::Restart => "restarted".into(),
                Sender::Direct => "direct entry".into(),
            };
            let runner = handle.clone().unwrap_or_else(|| {
                format!(
                    "exit {}",
                    code.map_or_else(|| "—".into(), |code| code.to_string())
                )
            });
            let cost = cost_usd.map_or_else(String::new, |cost| {
                format!(" · {}", rimz::theme::fmt::dollars2(cost))
            });
            writeln!(
                out,
                "  {} {sender} · {} {visit} · {runner} {} · {}{cost}",
                self.clock(*started_at),
                entry.node,
                outcome.as_str(),
                render::format_compact_duration(
                    ended_at.duration_since(*started_at).as_secs().max(0) as u64
                )
            )?;
        }
        let visit = used.saturating_add(1);
        writeln!(
            out,
            "  {} {} {visit} refused: {bound}{bucket_label} {max}",
            self.clock(
                self.record
                    .park
                    .as_ref()
                    .map_or(self.record.entered_at, |park| park.at)
            ),
            self.record.node
        )?;
        Ok(())
    }

    fn sender(&self, journal: &[JournalEntry]) -> Sender {
        match workflow::routing_hop(
            self.spec,
            journal,
            &self.record.node,
            self.record.held.as_ref(),
        ) {
            Some(hop) if hop.kind == HopKind::Resume => Sender::Resume,
            Some(hop) if hop.kind == HopKind::Restart => Sender::Restart,
            Some(hop) => Sender::Edge(hop.node, hop.outcome.unwrap_or_default()),
            None => Sender::Direct,
        }
    }

    fn timeout(&self, out: &mut impl Write) -> Result<()> {
        let timeout = self
            .spec
            .checks
            .get(&self.record.node)
            .and_then(|check| check.timeout.as_ref())
            .or_else(|| self.spec.steps.get(&self.record.node)?.timeout.as_ref());
        if let Some(timeout) = timeout {
            writeln!(out, "waited the full {timeout}")?;
        }
        if let Some(entry) = self.journal.iter().rev().find(|entry| {
            entry.node == self.record.node
                && entry.at >= self.record.entered_at
                && matches!(entry.event, JournalEvent::Wait { .. })
        }) && let JournalEvent::Wait {
            outcome,
            count,
            first,
            last,
            ..
        } = &entry.event
        {
            let note = history::fold_note(*count, *first, *last);
            writeln!(out, "{} said {outcome}{note}", entry.node)?;
        }
        if let Some(log) = self
            .journal
            .iter()
            .rev()
            .find_map(|entry| match &entry.event {
                JournalEvent::Evaluation { object, .. }
                    if entry.node == self.record.node && entry.at >= self.record.entered_at =>
                {
                    object.get("log").and_then(serde_json::Value::as_str)
                }
                _ => None,
            })
        {
            writeln!(out, "last log {log}")?;
        }
        Ok(())
    }
}
