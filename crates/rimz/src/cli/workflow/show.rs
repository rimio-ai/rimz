use jiff::Timestamp;
use rimz::config::GlyphRole;
use rimz::harness::workflow::{Definition, HopKind, Kind, Target};
use rimz::store::workflow::{JournalEntry, JournalEvent, RunState};

use super::*;

mod history;
mod park;
mod rail;

struct View<'a> {
    record: &'a RunRecord,
    paths: &'a paths::WorkflowPaths,
    spec: &'a Definition,
    journal: &'a [JournalEntry],
    now: Timestamp,
    commands: resolve::RunCommands<'a>,
}

struct Presentation {
    state: String,
    context: String,
    forecast: String,
}

pub(super) fn render(
    run: &resolve::Located,
    json: bool,
    history: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let journal = records::journal(&run.paths)?;
    if let Some(finished) = moved_to_finished(run)? {
        return render_other(&finished, json, history);
    }
    let spec = workflow::load(&run.record.definition_dir(&run.paths));
    let notes = run
        .record
        .notes
        .as_ref()
        .map(|relative| run.state.notes_run(paths::Class::AutomationNotes, relative));
    if json {
        let mut value = serde_json::json!({
            "address":run.record.address().to_string(),
            "dir":run.paths.work,"record":run.record,"journal":journal,
        });
        if let Some(notes) = notes {
            value["notes"] = serde_json::json!(notes);
        }
        return render::json_pretty(&value);
    }
    let refusal = (!run.record.state.is_terminal())
        .then(|| {
            let dir = run.record.definition_dir(&run.paths);
            workflow_view::definition_refusal(&run.record, &dir, &workflow_view::definitions())
                .map(|error| (dir, error))
        })
        .flatten();
    let spec = match (spec, &refusal) {
        (Ok(spec), None) => spec,
        (Err(error), None) => return Err(error.into()),
        (spec, Some((dir, error))) => {
            let heading = match spec {
                Ok(_) => "its definition is refused",
                Err(workflow::SpecErr::Io { .. }) => "its definition cannot be read",
                Err(_) => "its definition does not parse",
            };
            return unreadable(run, heading, dir, error, globals);
        }
    };
    let room = RoomView::new(&run.record.project_root);
    if history && run.record.state.is_terminal() {
        return workflow_view::history::terminal(
            &run.record,
            &spec,
            &journal,
            render::prose::Prose::for_stdout(),
            &room.attempt_agents(&run.state, &run.record, &journal),
            &mut render::out(),
        );
    }
    let view = View {
        record: &run.record,
        paths: &run.paths,
        spec: &spec,
        journal: &journal,
        now: Timestamp::now(),
        commands: resolve::RunCommands::new(&run.record, globals),
    };
    let agent = (run.record.state == RunState::Active
        && workflow_view::asking(&run.record).is_none())
    .then(|| room.agent(&run.state, &run.record, true))
    .flatten();
    let pr_number = (!run.record.state.is_terminal())
        .then(|| room.pr_number(&run.record.scope.path))
        .flatten();
    let mut out = render::out();
    view.headline(&mut out, pr_number, agent.as_deref())?;
    if history {
        return view.history(
            &room.attempt_agents(&run.state, &run.record, &journal),
            &mut out,
        );
    }
    if run.record.state == RunState::Active {
        view.readings(&mut out)?;
    }
    view.now_line(&run.state, &mut out)?;
    view.origin(&mut out)?;
    if let Some(text) = run
        .record
        .session_attempt()
        .and_then(|attempt| attempt.started_past_limit.as_deref())
    {
        writeln!(out, "{text}; started anyway: the run skips capacity")?;
    }
    if run.record.state == RunState::Active {
        if let Some(asking) = workflow_view::asking(&run.record) {
            prose(&mut out, &asking.ask.text, "  ")?;
        }
        takeover(&mut out, run, globals)?;
    }
    if run.record.state == RunState::Parked {
        view.park(&mut out)?;
    }
    view.rail(&mut out)?;
    view.visits(&mut out)?;
    view.trail(&mut out)?;
    writeln!(
        out,
        "work dir {}",
        render::home_relative_path(&run.paths.work)
    )?;
    if let Some(notes) = notes {
        writeln!(out, "notes {}", notes.display())?;
    }
    writeln!(
        out,
        "all of it: {}",
        view.command("show", None, &[], &["--history"])
    )?;
    Ok(())
}

/// `workflow show` of a live run whose definition admission refuses: the run from its record
/// under `heading`, why it waits, and the command that checks the definition.
fn unreadable(
    run: &resolve::Located,
    heading: &str,
    dir: &std::path::Path,
    error: &str,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let mut out = render::out();
    out.write_all(&workflow_view::history::header_only(
        &run.record.id,
        Some(&run.record.header()),
        heading,
    ))?;
    waits_on_definition(&mut out, dir, error)?;
    writeln!(
        out,
        "or end it: {}",
        resolve::RunCommands::new(&run.record, globals).command("cancel", None, &[], &[])
    )?;
    Ok(())
}

/// The first error admission reports for a live run's definition, which the run waits on until it
/// is fixed, and the command that checks the definition.
fn waits_on_definition(out: &mut impl Write, dir: &std::path::Path, error: &str) -> Result<()> {
    writeln!(out, "waits on its definition: {error}")?;
    let target = dir.to_string_lossy();
    writeln!(
        out,
        "check with {}",
        resolve::command("validate", &[&target], None, None, &[], &[])
    )?;
    Ok(())
}

/// The run as a finished run when the live home its record was read from moved to the finished
/// directory before its journal was read: a moved home is a finished run, so `show` prints it as
/// it prints a finished run found by id, whatever its record now classifies as.
fn moved_to_finished(run: &resolve::Located) -> Result<Option<resolve::Other>> {
    if run.paths.home.exists() {
        return Ok(None);
    }
    let id = &run.record.id;
    let found = records::find(&run.state, id)?.with_context(|| {
        format!("workflow run {id} is gone: neither its live nor its finished home holds it")
    })?;
    Ok(Some(resolve::Other {
        state: run.state.clone(),
        id: id.clone(),
        found,
    }))
}

/// `workflow show` of a run this RimZ does not run: a finished one, from its record's header and
/// its saved history, never its definition; one it cannot continue, from the header; a newer
/// RimZ's, refused.
pub(super) fn render_other(other: &resolve::Other, json: bool, history: bool) -> Result<()> {
    let paths = &other.found.paths;
    let header = other.found.class.header();
    if let records::RunClass::Newer { .. } = other.found.class
        && let Some(refusal) = other.refusal()
    {
        bail!("{refusal}");
    }
    let saved = match std::fs::read_to_string(paths.history()) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if json {
        // A record this RimZ decodes names its logs where they are now; any other, as written.
        let record = match &other.found.class {
            records::RunClass::Current(record) => serde_json::to_value(record).ok(),
            _ => std::fs::read(&paths.record)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()),
        };
        let mut value = serde_json::json!({
            "address": header
                .as_deref()
                .and_then(workflow_view::history::address)
                .unwrap_or_else(|| other.id.to_string()),
            "dir": paths.work,
            "record": record,
            "journal": records::journal(paths).unwrap_or_default(),
            "history": saved,
        });
        if let Some(notes) = header.as_ref().and_then(|header| header.notes.as_ref()) {
            value["notes"] =
                serde_json::json!(other.state.notes_run(paths::Class::AutomationNotes, notes));
        }
        return render::json_pretty(&value);
    }
    let mut out = render::out();
    if !(history && other.found.finished) {
        let why = match &other.found.class {
            records::RunClass::Stranded { cause, .. } => format!(
                "this RimZ cannot continue the run: {cause}; it reads only record version {}. \
                 It will end the run as failed",
                records::VERSION
            ),
            records::RunClass::NotObject => "this RimZ cannot read the run: its record holds no \
                JSON object. It will set the run aside"
                .into(),
            _ if other.found.finished => "the run is finished".into(),
            _ => "the run has ended".into(),
        };
        let text = workflow_view::history::header_only(&other.id, header.as_deref(), &why);
        out.write_all(&text)?;
    }
    match saved {
        Some(saved) => {
            if history {
                out.write_all(saved.as_bytes())?;
            } else {
                writeln!(out)?;
                // Both saved formats start with the name and ending, then an optional reason.
                // Those facts have already been printed from the record's header.
                let mut rows = saved.split_once('\n').map_or("", |(_, rows)| rows);
                if header.is_some() {
                    rows = rows.split_once('\n').map_or("", |(_, rows)| rows);
                }
                if rows.starts_with("reason: ") {
                    rows = rows.split_once('\n').map_or("", |(_, rows)| rows);
                }
                out.write_all(rows.as_bytes())?;
            }
        }
        None if other.found.finished => writeln!(out, "no saved history")?,
        None => {}
    }
    Ok(())
}

impl View<'_> {
    fn clock(&self, at: Timestamp) -> String {
        render::workflow_clock(at, self.now)
    }

    fn command(&self, verb: &str, at: Option<&str>, extend: &[String], flags: &[&str]) -> String {
        self.commands.command(verb, at, extend, flags)
    }

    fn presentation(&self, agent: Option<&str>) -> Result<Presentation> {
        let record = self.record;
        let agent = agent.or_else(|| {
            record
                .session_attempt()
                .and_then(|attempt| attempt.run.as_ref())
                .map(|run| run.handle.as_str())
        });
        let mut context = String::new();
        let mut forecast = String::new();
        let state = if record.state.is_terminal() {
            format!(
                "{} {}",
                workflow_view::state_word(
                    record.state,
                    record.outcome.as_ref().map(|outcome| &outcome.ending)
                ),
                render::age_short(record.terminal_at.unwrap_or(record.entered_at), self.now)
            )
        } else if record.state == RunState::Parked {
            format!(
                "parked at {} {} · {}",
                record.node,
                render::age_short(workflow_view::since(record), self.now),
                record.park.as_ref().map_or("", |park| park.reason.as_str())
            )
        } else if let Some((attempt, asking)) = workflow_view::asker(record) {
            context = format!(" · {} ({})", asking.ask.handle, asking.ask.kind);
            format!("{} is asking you", attempt.node)
        } else if let Some((attempt, hold)) = workflow_view::holder(record) {
            context = format!(
                " · {}: {}",
                workflow_view::hold_handle(attempt, hold)
                    .or(agent)
                    .unwrap_or("—"),
                render::one_line(&hold.label())
            );
            if attempt.kind == rimz::store::workflow::AttemptKind::Judge
                && self
                    .spec
                    .node(&attempt.node)
                    .and_then(|node| node.timeout())
                    .is_some()
            {
                forecast = " · the check timeout keeps running".to_owned();
            }
            format!(
                "{} is held {}",
                attempt.node,
                render::age_short(hold.since, self.now)
            )
        } else if workflow_view::with_you(record) {
            let step = record
                .session_attempt()
                .map_or(record.node.as_str(), |a| &a.node);
            context = format!(
                " · {}{}",
                agent.unwrap_or("—"),
                workflow_view::with_you_suffix(record, Some(self.spec), self.now)
            );
            format!("{step} is with you")
        } else if let Some((state, bound)) = self.hold()? {
            forecast = bound;
            state
        } else if let Some(wait) = workflow_view::wait(record, Some(self.spec), self.now) {
            if let Some(check) = self.spec.check(&record.node) {
                if let Some((at, target)) = workflow::next_bound(self.spec, record, None, self.now)?
                {
                    forecast = format!(" · gives up {}, then {target}", self.clock(at));
                }
                if check.is_sleep() {
                    wait
                } else {
                    format!(
                        "waiting at {} {}: {wait}",
                        record.node,
                        render::age_short(record.entered_at, self.now)
                    )
                }
            } else {
                format!("{} {wait}", record.node)
            }
        } else {
            if let Some(agent) = agent {
                context = format!(" · {agent}");
            }
            self.running()
        };
        Ok(Presentation {
            state,
            context,
            forecast,
        })
    }

    fn headline(
        &self,
        out: &mut impl Write,
        pr_number: Option<u64>,
        agent: Option<&str>,
    ) -> Result<()> {
        let record = self.record;
        let asking = workflow_view::asking(record);
        let phrase = self.presentation(agent)?;
        let role = workflow_view::glyph_role(record);
        let glyph = rimz::theme::theme_glyphs(&super::super::machine_config().theme)(role);
        write!(out, "{}", record.address())?;
        if record.once && !record.state.is_terminal() {
            write!(out, " · once")?;
        }
        if let Some(number) = pr_number {
            write!(out, "  PR #{number}")?;
        }
        writeln!(out)?;
        writeln!(
            out,
            "{}{}{}",
            render::paint(
                render::status::workflow(record.state, workflow_view::calls_you(record)).bold(),
                &format!("{glyph} {}", phrase.state)
            ),
            render::paint(render::palette::body(), &phrase.context),
            render::paint(render::palette::muted(), &phrase.forecast),
        )?;
        if asking.is_some() {
            // The attempt asking: the current one, a judge's included, else a held session.
            let node = record
                .attempt
                .iter()
                .chain(record.held.as_ref().map(|held| &held.attempt))
                .find(|attempt| attempt.asking.is_some())
                .map(|attempt| attempt.node.as_str());
            let kind = if node.is_some_and(|node| self.spec.step(node).is_some()) {
                "step"
            } else {
                "check"
            };
            let timeout = node
                .and_then(|node| self.spec.node(node))
                .and_then(|node| node.timeout());
            writeln!(
                out,
                "answer in its pane or with rimz answer · {}",
                if timeout.is_none() {
                    format!("no {kind} timeout is set")
                } else if kind == "check" {
                    // Only a judge check asks, and nothing it is stopped on pauses its timeout.
                    "the check timeout keeps running".to_owned()
                } else {
                    "the step timeout is paused".to_owned()
                }
            )?;
        }
        Ok(())
    }

    fn running(&self) -> String {
        let record = self.record;
        if !self.spec.step(&record.node).is_some() {
            let judge = workflow_view::judge(record)
                .map_or_else(String::new, |judge| format!(" in {judge}"));
            return format!(
                "checking {}{judge} {}",
                record.node,
                render::age_short(record.entered_at, self.now)
            );
        }
        let started = record
            .attempt
            .as_ref()
            .map_or(record.entered_at, |a| a.started_at);
        format!(
            "{} running {}{}",
            record.node,
            render::age_short(started, self.now),
            self.visit()
        )
    }

    fn visit(&self) -> String {
        self.spec
            .step(&self.record.node)
            .and_then(|step| step.visit_cap(self.record.visit_grant))
            .map_or_else(String::new, |max| {
                let used = self.record.visits_to(&self.record.node);
                format!(" · visit {used} of {max}")
            })
    }

    fn hold(&self) -> Result<Option<(String, String)>> {
        if self
            .spec
            .check(&self.record.node)
            .is_some_and(|check| check.is_sleep())
        {
            return Ok(None);
        }
        let Some(wait) = self
            .record
            .wait
            .as_ref()
            .filter(|wait| wait.outcome == "hold" && wait.unknown.is_none())
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
            .check(&wait.node)
            .and_then(|check| check.cases().get(index as usize))
        else {
            return Ok(None);
        };
        let Some(raw) = case.hold().map(|hold| hold.text()) else {
            return Ok(None);
        };
        let Some((at, target)) =
            workflow::next_bound(self.spec, self.record, Some(index), self.now)?
        else {
            return Ok(None);
        };
        Ok(Some((
            format!(
                "holding {} at {} {} of {raw}",
                case.outcome,
                wait.node,
                render::age_short(wait.first, self.now.min(hold_at)),
            ),
            format!(", then {target} at {}", self.clock(at)),
        )))
    }

    fn origin(&self, out: &mut impl Write) -> Result<()> {
        let record = self.record;
        let origin = run_origin(record);
        let settled = self
            .journal
            .iter()
            .filter(|entry| matches!(entry.event, JournalEvent::Attempt { .. }));
        // A step's attempt counts from its start; it reaches the journal only once it settles.
        let running = record
            .held
            .as_ref()
            .map(|held| &held.attempt)
            .or(record.attempt.as_ref())
            .filter(|attempt| {
                attempt.visit.is_some()
                    && !settled.clone().any(|entry| entry.seq == Some(attempt.seq))
            });
        let attempts = settled.count() + usize::from(running.is_some());
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

    fn readings(&self, out: &mut impl Write) -> Result<()> {
        if !self.spec.check(&self.record.node).is_some() {
            return Ok(());
        }
        let Some((entry, object)) = self.evaluation() else {
            return Ok(());
        };
        let Some(readings) = object
            .get("readings")
            .and_then(serde_json::Value::as_object)
            .filter(|readings| !readings.is_empty())
        else {
            return Ok(());
        };
        let values = readings
            .iter()
            .map(|(key, value)| format!("{key}={}", history::value(value)))
            .collect::<Vec<_>>();
        writeln!(
            out,
            "  {} · read {}",
            values.join("  "),
            self.clock(entry.at)
        )?;
        Ok(())
    }

    fn now_line(&self, state: &paths::StatePaths, out: &mut impl Write) -> Result<()> {
        if !matches!(self.record.state, RunState::Active | RunState::Parked) {
            return Ok(());
        }
        let name = match self.spec.step(&self.record.node) {
            Some(step) if matches!(step.kind(), Some(Kind::Agent(_))) => step
                .rest_check()
                .first()
                .or(step.then().first())
                .map(Target::as_str),
            Some(_) => None,
            None => Some(self.record.node.as_str()),
        };
        let Some((name, node)) =
            name.and_then(|name| self.spec.check(name).map(|check| (name, check)))
        else {
            return Ok(());
        };
        if !matches!(node.kind(), Some(Kind::Condition(_)))
            && !(node.is_sleep() && node.when().is_some())
        {
            return Ok(());
        }
        let mut preview = self.record.clone();
        preview.node = name.to_owned();
        preview.wait = preview.wait.filter(|wait| wait.node == name);
        let slept_from = if name == self.record.node {
            self.record.entered_at
        } else {
            self.now
        };
        let (object, probed_at) = workflow::condition_now(state, node, &preview, slept_from)
            .map_err(anyhow::Error::msg)?;
        let hold = object["case"]
            .as_u64()
            .and_then(|case| u32::try_from(case).ok())
            .map(|case| workflow::hold_until(self.spec, &preview, case, self.now))
            .transpose()?
            .flatten();
        let outcome = object["outcome"].as_str().unwrap_or("unknown");
        write!(out, "now {name} · ")?;
        if hold.is_some_and(|at| at > self.now) {
            write!(out, "hold ")?;
        }
        write!(out, "{outcome}")?;
        if let Some(at) = hold {
            write!(
                out,
                " · {} remaining",
                render::age_short(self.now, at.max(self.now))
            )?;
        }
        if let Some(readings) = object
            .get("readings")
            .and_then(serde_json::Value::as_object)
            .filter(|readings| !readings.is_empty())
        {
            let values = readings
                .iter()
                .map(|(key, value)| format!("{key}={}", history::value(value)))
                .collect::<Vec<_>>();
            write!(out, " · {}", values.join("  "))?;
        }
        if let Some(reason) = object["reason"].as_str() {
            write!(out, " · {reason}")?;
        }
        if let Some(at) = probed_at {
            write!(out, " · read {}", self.clock(at))?;
        }
        writeln!(out)?;
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
        for (name, used) in self.record.visits.iter().filter(|(_, used)| **used > 0) {
            let max = self
                .spec
                .step(name)
                .and_then(|step| step.visit_cap(self.record.visit_grant));
            let share = self.record.key.as_ref().and_then(|key| {
                self.record
                    .key_visits
                    .get(name)?
                    .iter()
                    .find(|share| &share.key == key)
            });
            if max.is_none() && share.is_none() {
                continue;
            }
            write!(out, "visits  {name} {used}")?;
            if let Some(max) = max {
                write!(out, " of {max}")?;
            }
            if let Some(share) = share {
                let key = workflow_view::history::value(&serde_json::to_value(&share.key)?);
                write!(out, " · key {key}: {}", share.visits)?;
            }
            writeln!(out)?;
        }
        Ok(())
    }

    fn trail(&self, out: &mut impl Write) -> Result<()> {
        let hops = workflow::hops(self.spec, self.journal, self.record.held.as_ref());
        let earlier = hops.len().saturating_sub(8);
        let mut trail = Vec::new();
        if earlier > 0 {
            trail.push(format!("+{earlier} earlier"));
        }
        for hop in &hops[earlier..] {
            let fact = match hop.kind {
                HopKind::Start => format!("started → {}", hop.node),
                HopKind::Resume => format!("resumed → {}", hop.node),
                HopKind::Restart => format!("restarted → {}", hop.node),
                HopKind::VisitsReset => format!(
                    "{} visits reset on {}",
                    hop.node,
                    hop.outcome.as_deref().unwrap_or_default()
                ),
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
                    hop.target.as_ref().map_or("", |target| target.as_str())
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
            use rimz::harness::takeover::OpenTurn;
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

#[cfg(test)]
mod tests;
