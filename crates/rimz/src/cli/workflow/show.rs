use jiff::Timestamp;
use rimz::config::GlyphRole;
use rimz::harness::workflow::{HopKind, WorkflowSpec};
use rimz::store::workflow::{JournalEntry, JournalEvent, RunState};

use super::*;

mod history;
mod park;

struct View<'a> {
    record: &'a RunRecord,
    paths: &'a paths::WorkflowPaths,
    spec: &'a WorkflowSpec,
    journal: &'a [JournalEntry],
    now: Timestamp,
    commands: resolve::RunCommands<'a>,
}

pub(super) fn render(
    run: &resolve::Located,
    json: bool,
    history: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let journal = records::journal(&run.paths)?;
    if json {
        return render::json_pretty(&serde_json::json!({
            "address":run.record.address().to_string(),
            "dir":run.paths.work,"record":run.record,"journal":journal,
        }));
    }
    let spec = workflow::load(&run.paths.home)?;
    let view = View {
        record: &run.record,
        paths: &run.paths,
        spec: &spec,
        journal: &journal,
        now: Timestamp::now(),
        commands: resolve::RunCommands::new(&run.record, globals),
    };
    let mut out = render::out();
    view.headline(&mut out, history)?;
    if history {
        return view.history(&mut out);
    }
    view.origin(&mut out)?;
    if run.record.state == RunState::Active {
        view.current(&mut out)?;
        takeover(&mut out, run, globals)?;
    }
    if run.record.state == RunState::Parked {
        view.park(&mut out)?;
    }
    view.visits(&mut out)?;
    view.trail(&mut out)?;
    writeln!(
        out,
        "work dir {}",
        render::home_relative_path(&run.paths.work)
    )?;
    writeln!(
        out,
        "all of it: {}",
        view.command("show", None, &[], &["--history"])
    )?;
    Ok(())
}

impl View<'_> {
    fn clock(&self, at: Timestamp) -> String {
        render::workflow_clock(at, self.now)
    }

    fn command(&self, verb: &str, at: Option<&str>, extend: &[String], flags: &[&str]) -> String {
        self.commands.command(verb, at, extend, flags)
    }

    fn headline(&self, out: &mut impl Write, history: bool) -> Result<()> {
        let record = self.record;
        let asking = workflow_view::asking(record);
        let (role, phrase) = if record.state.is_terminal() {
            let role = match record.state {
                RunState::Done => GlyphRole::StatusDone,
                RunState::Failed => GlyphRole::StatusAttention,
                _ => GlyphRole::StatusIdle,
            };
            (
                role,
                format!(
                    "{} {}",
                    record.state.as_str(),
                    render::age_short(record.terminal_at.unwrap_or(record.entered_at), self.now)
                ),
            )
        } else if record.state == RunState::Parked {
            (
                GlyphRole::StatusWaiting,
                format!(
                    "parked at {} {} · {}",
                    record.node,
                    render::age_short(workflow_view::since(record), self.now),
                    workflow_view::park(record, Some(self.spec), self.now).unwrap_or_default()
                ),
            )
        } else if let Some(asking) = asking {
            let step =
                workflow_view::session_attempt(record).map_or(record.node.as_str(), |a| &a.node);
            (
                GlyphRole::StatusWaiting,
                format!(
                    "{step} is asking you · {} ({}): {}",
                    asking.ask.handle,
                    asking.ask.kind,
                    render::one_line(&asking.ask.text)
                ),
            )
        } else if workflow_view::with_you(record) {
            let attempt = workflow_view::session_attempt(record);
            (
                GlyphRole::StatusWorking,
                format!(
                    "{} is with you · {}",
                    attempt.map_or(record.node.as_str(), |a| &a.node),
                    attempt
                        .and_then(|a| a.run.as_ref())
                        .map_or("—", |run| &run.handle)
                ),
            )
        } else if let Some(hold) = self.hold()? {
            (GlyphRole::StatusSleeping, hold)
        } else if let Some(wait) = if history {
            history::waiting_line(record, Some(self.spec), self.journal, self.now)
        } else {
            workflow_view::wait(record, Some(self.spec), self.now)
        } {
            let phrase = if self.spec.checks.contains_key(&record.node) {
                let mut phrase = format!(
                    "waiting at {} {}: {wait}",
                    record.node,
                    render::age_short(record.entered_at, self.now)
                );
                if let Some((at, target)) = workflow::next_bound(self.spec, record, None, self.now)?
                {
                    phrase.push_str(&format!(" · gives up {}, then {target}", self.clock(at)));
                }
                phrase
            } else {
                format!("{} {wait}", record.node)
            };
            (GlyphRole::StatusSleeping, phrase)
        } else {
            (GlyphRole::StatusWorking, self.running())
        };
        let glyph = rimz::theme::theme_glyphs(&super::super::machine_config().theme)(role);
        writeln!(
            out,
            "{}  {}",
            record.address(),
            render::paint(
                render::status::workflow(record.state, asking.is_some()),
                &format!("{glyph} {phrase}")
            )
        )?;
        if asking.is_some() {
            let timeout = workflow_view::session_attempt(record)
                .and_then(|a| self.spec.steps.get(&a.node))
                .and_then(|step| step.timeout.as_ref());
            writeln!(
                out,
                "answer in its pane or with rimz answer · {}",
                if timeout.is_some() {
                    "the step timeout is paused"
                } else {
                    "no step timeout is set"
                }
            )?;
        }
        Ok(())
    }

    fn running(&self) -> String {
        let record = self.record;
        if !self.spec.steps.contains_key(&record.node) {
            return format!(
                "checking {} {}",
                record.node,
                render::age_short(record.entered_at, self.now)
            );
        }
        let started = record
            .attempt
            .as_ref()
            .map_or(record.entered_at, |a| a.started_at);
        let mut phrase = format!(
            "{} running {}{}",
            record.node,
            render::age_short(started, self.now),
            self.visit()
        );
        if let Some(run) = workflow_view::session_attempt(record).and_then(|a| a.run.as_ref()) {
            phrase.push_str(&format!(" · {}", run.handle));
        }
        phrase
    }

    fn visit(&self) -> String {
        self.spec
            .steps
            .get(&self.record.node)
            .and_then(|step| {
                step.visit_bounds(
                    self.record.visits.get(&self.record.node),
                    self.record.key.as_ref(),
                    self.record.visit_grant,
                )
                .next()
            })
            .map_or_else(String::new, |(_, used, max)| {
                format!(" · visit {used} of {max}")
            })
    }

    fn hold(&self) -> Result<Option<String>> {
        let Some(wait) = self
            .record
            .wait
            .as_ref()
            .filter(|wait| wait.outcome == "hold")
        else {
            return Ok(None);
        };
        let Some(index) = wait.case else {
            return Ok(None);
        };
        let Some(hold_at) = workflow::hold_until(self.spec, self.record, index, self.now)? else {
            return Ok(None);
        };
        let Some(case) = self
            .spec
            .checks
            .get(&wait.node)
            .and_then(|check| check.cases.get(index as usize))
        else {
            return Ok(None);
        };
        let Some(raw) = case.hold.as_deref() else {
            return Ok(None);
        };
        let Some((at, target)) =
            workflow::next_bound(self.spec, self.record, Some(index), self.now)?
        else {
            return Ok(None);
        };
        Ok(Some(format!(
            "holding {} at {} {} of {raw}, then {target} at {}",
            case.outcome,
            wait.node,
            render::age_short(wait.first, self.now.min(hold_at)),
            self.clock(at)
        )))
    }

    fn origin(&self, out: &mut impl Write) -> Result<()> {
        let record = self.record;
        let origin = record.task.as_ref().map_or_else(
            || "hand".into(),
            |task| {
                format!(
                    "loop task {task}{}",
                    if record.manual { ", fired by hand" } else { "" }
                )
            },
        );
        let attempts = self
            .journal
            .iter()
            .filter(|entry| matches!(entry.event, JournalEvent::Attempt { .. }))
            .count();
        let mut costs = self
            .journal
            .iter()
            .filter_map(|entry| match entry.event {
                JournalEvent::Attempt { cost_usd, .. }
                | JournalEvent::Evaluation { cost_usd, .. } => cost_usd,
                _ => None,
            })
            .peekable();
        let cost = if costs.peek().is_some() {
            format!(", {}", rimz::theme::fmt::dollars2(costs.sum()))
        } else {
            String::new()
        };
        writeln!(
            out,
            "started {} by {origin} · {attempts} attempt{}{cost}",
            self.clock(record.created_at),
            if attempts == 1 { "" } else { "s" }
        )?;
        Ok(())
    }

    fn current(&self, out: &mut impl Write) -> Result<()> {
        if let Some(asking) = workflow_view::asking(self.record) {
            return prose(out, &asking.ask.text, "  ");
        }
        if self.record.held.is_some() {
            writeln!(out, "checking {}", self.record.node)?;
        }
        if let Some(hold) = self.hold()? {
            writeln!(out, "  {hold}")?;
            return Ok(());
        }
        if let Some((_, object)) = self.evaluation()
            && let Some(readings) = object
                .get("readings")
                .and_then(serde_json::Value::as_object)
        {
            for (key, value) in readings {
                writeln!(out, "  {key}: {}", history::value(value))?;
            }
        }
        if let Some(wait) = workflow_view::wait(self.record, Some(self.spec), self.now) {
            writeln!(out, "  {wait}")?;
        } else if self.spec.steps.contains_key(&self.record.node) {
            writeln!(out, "  {}", self.running())?;
        }
        Ok(())
    }

    fn evaluation(&self) -> Option<(&JournalEntry, &serde_json::Value)> {
        self.journal
            .iter()
            .rev()
            .find_map(|entry| match &entry.event {
                JournalEvent::Evaluation { object, .. }
                    if entry.node == self.record.node && entry.at >= self.record.entered_at =>
                {
                    Some((entry, object))
                }
                _ => None,
            })
    }

    fn visits(&self, out: &mut impl Write) -> Result<()> {
        for (name, visits) in &self.record.visits {
            let Some(step) = self.spec.steps.get(name).filter(|_| visits.total > 0) else {
                continue;
            };
            let total = step
                .visit_bounds(Some(visits), None, self.record.visit_grant)
                .find(|(bound, _, _)| *bound == "max-visits");
            if let Some((_, used, max)) = total {
                writeln!(out, "visits  {name} {used} of {max}")?;
            }
            for (index, key) in visits.keys.iter().enumerate() {
                let Some((_, used, max)) = step
                    .visit_bounds(Some(visits), key.key.as_ref(), self.record.visit_grant)
                    .find(|(bound, _, _)| *bound == "max-visits-per-key")
                else {
                    break;
                };
                let label = if total.is_none() && index == 0 {
                    "visits"
                } else {
                    ""
                };
                writeln!(
                    out,
                    "{label}  {name} {used} of {max} for key {}",
                    serde_json::to_string(&key.key)?
                )?;
            }
        }
        Ok(())
    }

    fn trail(&self, out: &mut impl Write) -> Result<()> {
        let hops = workflow::hops(self.spec, self.journal);
        let earlier = hops.len().saturating_sub(8);
        let mut trail = Vec::new();
        if earlier > 0 {
            trail.push(format!("+{earlier} earlier"));
        }
        for hop in &hops[earlier..] {
            let fact = match hop.kind {
                HopKind::Resume => format!("resumed → {}", hop.node),
                HopKind::Park { reason } => format!("parked {}", reason.as_str()),
                HopKind::Terminal { state } => state.as_str().into(),
                HopKind::Route => format!(
                    "{} {}{} → {}",
                    hop.node,
                    hop.outcome.as_deref().unwrap_or_default(),
                    if hop.count > 1 {
                        format!(" ×{}", hop.count)
                    } else {
                        String::new()
                    },
                    hop.target.as_deref().unwrap_or_default()
                ),
            };
            trail.push(format!("{} {fact}", self.clock(hop.at)));
        }
        if !self.record.state.is_terminal() {
            trail.push("now".into());
        }
        writeln!(
            out,
            "{}  {}",
            render::paint(render::palette::muted(), "trail"),
            trail.join(" · ")
        )?;
        Ok(())
    }
}

fn prose(out: &mut impl Write, text: &str, indent: &str) -> Result<()> {
    for line in
        render::prose::Prose::for_stdout().lines(text, render::prose::prose_width(indent.len()))
    {
        writeln!(out, "{indent}{line}")?;
    }
    Ok(())
}

fn takeover(
    out: &mut impl Write,
    run: &resolve::Located,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    if !run
        .record
        .wait
        .as_ref()
        .is_some_and(|wait| wait.outcome == "takeover")
    {
        return Ok(());
    }
    match workflow::AttemptHost::occupancy(
        &Host::new(&run.state, globals, None),
        &run.record.scope.path,
        &run.record.project_root,
    ) {
        Err(_) => writeln!(out, "takeover: blockers unavailable")?,
        Ok(workflow::Occupancy::Blocked(blockers)) => {
            use rimz::harness::schedule::takeover::OpenTurn;
            for (handle, turn) in blockers {
                let phrase = match turn {
                    OpenTurn::Working => "is working",
                    OpenTurn::WaitingOnUser => "is waiting on you",
                    OpenTurn::Paused => "is paused on a limit",
                    OpenTurn::Compacting => "is compacting",
                };
                writeln!(out, "takeover: {handle} {phrase}")?;
            }
        }
        Ok(_) => {}
    }
    Ok(())
}
