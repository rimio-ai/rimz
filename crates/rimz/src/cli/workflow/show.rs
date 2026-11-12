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
    let spec = workflow::load(&run.paths.home);
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
        if let Ok(spec) = &spec {
            value["warnings"] = serde_json::json!(spec.ignored());
        }
        if let Some(notes) = notes {
            value["notes"] = serde_json::json!(notes);
        }
        return render::json_pretty(&value);
    }
    let spec = spec?;
    if history && run.record.state.is_terminal() {
        return workflow_view::history::terminal(
            &run.record,
            &spec,
            &journal,
            render::prose::Prose::for_stdout(),
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
    let room = RoomView::new(&run.record.project_root);
    let agent = (!history
        && run.record.state == RunState::Active
        && workflow_view::asking(&run.record).is_none())
    .then(|| room.agent(&run.state, &run.record, true))
    .flatten();
    let pr_number = (!run.record.state.is_terminal())
        .then(|| room.pr_number(&run.record.scope.path))
        .flatten();
    let mut out = render::out();
    view.headline(&mut out, pr_number, agent.as_deref())?;
    if history {
        for warning in spec.ignored() {
            writeln!(out, "warning: {}", warning.message)?;
        }
        return view.history(&mut out);
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
        writeln!(out, "{text}; started anyway: manual starts do not wait")?;
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
    for warning in spec.ignored() {
        writeln!(out, "warning: {}", warning.message)?;
    }
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

impl View<'_> {
    fn clock(&self, at: Timestamp) -> String {
        render::workflow_clock(at, self.now)
    }

    fn command(&self, verb: &str, at: Option<&str>, extend: &[String], flags: &[&str]) -> String {
        self.commands.command(verb, at, extend, flags)
    }

    fn presentation(&self, agent: Option<&str>) -> Result<Presentation> {
        let record = self.record;
        let asking = workflow_view::asking(record);
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
                record.state.as_str(),
                render::age_short(record.terminal_at.unwrap_or(record.entered_at), self.now)
            )
        } else if record.state == RunState::Parked {
            format!(
                "parked at {} {} · {}",
                record.node,
                render::age_short(workflow_view::since(record), self.now),
                record.park.as_ref().map_or("", |park| park.reason.as_str())
            )
        } else if let Some(asking) = asking {
            let step = record
                .session_attempt()
                .map_or(record.node.as_str(), |a| &a.node);
            context = format!(" · {} ({})", asking.ask.handle, asking.ask.kind);
            format!("{step} is asking you")
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
        if let Some(number) = pr_number {
            write!(out, "  PR #{number}")?;
        }
        writeln!(out)?;
        writeln!(
            out,
            "{}{}{}",
            render::paint(
                render::status::workflow(record.state, asking.is_some()).bold(),
                &format!("{glyph} {}", phrase.state)
            ),
            render::paint(render::palette::body(), &phrase.context),
            render::paint(render::palette::muted(), &phrase.forecast),
        )?;
        if asking.is_some() {
            let timeout = record
                .session_attempt()
                .and_then(|a| self.spec.step(&a.node))
                .and_then(|step| step.timeout());
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
        if !self.spec.step(&record.node).is_some() {
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
            .and_then(|step| {
                step.visit_bounds(
                    self.record.visits.get(&self.record.node),
                    self.record.visit_bucket(),
                    self.record.visit_grant,
                )
                .next()
            })
            .map_or_else(String::new, |(_, used, max)| {
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
            Some(step) if matches!(step.kind(), Some(Kind::Agent(_))) => {
                step.then().first().map(Target::as_str)
            }
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
        for (name, visits) in &self.record.visits {
            let Some(step) = self.spec.step(name).filter(|_| visits.total > 0) else {
                continue;
            };
            let total = step
                .visit_bounds(Some(visits), (None, None), self.record.visit_grant)
                .find(|(bound, _, _)| *bound == "max-visits");
            if let Some((_, used, max)) = total {
                writeln!(out, "visits  {name} {used} of {max}")?;
            }
            for (index, row) in visits.outcomes.iter().enumerate() {
                let Some((_, used, max)) = step
                    .visit_bounds(
                        Some(visits),
                        (row.outcome.as_deref(), row.key.as_ref()),
                        self.record.visit_grant,
                    )
                    .find(|(bound, _, _)| *bound == "max-visits-per-outcome")
                else {
                    break;
                };
                let label = if total.is_none() && index == 0 {
                    "visits"
                } else {
                    ""
                };
                let key = row
                    .key
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .map(|key| format!(" key {key}"))
                    .unwrap_or_default();
                writeln!(
                    out,
                    "{label}  {name} {used} of {max} for {}{key}",
                    row.outcome.as_deref().unwrap_or("null")
                )?;
            }
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
        &run.record.break_agents,
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
mod tests {
    use super::*;

    #[test]
    fn now_line_uses_the_open_case_hold_instead_of_restarting_it() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
            "spec_dir":dir.path(),"spec_digest":"digest","project_root":dir.path(),
            "scope":{"kind":"worktree","path":dir.path()},"state":"parked","node":"gate",
            "created_at":now,"entered_at":now,"visits":{},"visit_grant":0,
            "deadline_fired":false,"next_seq":1,
            "wait":{"node":"gate","outcome":"hold","case":0,
                "first":now-jiff::SignedDuration::from_mins(1),"last":now,"count":1},
        }))
        .unwrap();
        let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='gate'\n[checks.gate]\nelse='pending'\non={ready='@done',pending='@wait',unknown='@wait'}\ncases=[{when=['team.stage=Done'],outcome='ready',for='2m'}]\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
        std::fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
        let paths = state.workflow(&record.id);
        let globals = crate::cli::GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        let view = View {
            record: &record,
            paths: &paths,
            spec: &spec,
            journal: &[],
            now,
            commands: resolve::RunCommands::new(&record, &globals),
        };
        let mut output = Vec::new();
        view.now_line(&state, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("now gate")
                && output.contains("hold")
                && output.contains("1m remaining")
                && output.contains("team.stage=Done"),
            "{output}"
        );
    }

    #[test]
    fn a_hold_under_unknown_reads_unknown_without_a_forecast_until_true_again() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        let first = now - jiff::SignedDuration::from_mins(3);
        let mut record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
            "spec_dir":dir.path(),"spec_digest":"digest","project_root":dir.path(),
            "scope":{"kind":"worktree","path":dir.path()},"state":"active","node":"gate",
            "created_at":first,"entered_at":first,"visits":{},"visit_grant":0,
            "deadline_fired":false,"next_seq":1,
            "wait":{"node":"gate","outcome":"hold","case":0,"detail":"ready",
                "unknown":"team.stage (no board)","first":first,"last":now,"count":2},
        }))
        .unwrap();
        let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='gate'\n[checks.gate]\nelse='pending'\non={ready='@done',pending='@wait',unknown='@wait'}\ncases=[{when=['team.stage=Done'],outcome='ready',for='2m'}]\n").unwrap();
        let paths = state.workflow(&record.id);
        let globals = crate::cli::GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        let present = |record: &RunRecord| {
            let view = View {
                record,
                paths: &paths,
                spec: &spec,
                journal: &[],
                now,
                commands: resolve::RunCommands::new(record, &globals),
            };
            let phrase = view.presentation(None).unwrap();
            (phrase.state, phrase.forecast)
        };
        let (state_line, forecast) = present(&record);
        assert!(
            state_line.contains("unknown: team.stage (no board)")
                && !state_line.contains("holding")
                && !forecast.contains("then"),
            "{state_line}{forecast}"
        );
        record.wait.as_mut().unwrap().unknown = None;
        let (state_line, forecast) = present(&record);
        assert!(
            state_line.starts_with("holding ready at gate 2m of 2m")
                && forecast.starts_with(", then @done at "),
            "{state_line}{forecast}"
        );
    }

    #[test]
    fn now_line_reads_a_sleep_a_step_enters_next_instead_of_reporting_it_slept() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
            "spec_dir":dir.path(),"spec_digest":"digest","project_root":dir.path(),
            "scope":{"kind":"worktree","path":dir.path()},"state":"active","node":"build",
            "created_at":now,"entered_at":now-jiff::SignedDuration::from_mins(10),"visits":{},
            "visit_grant":0,"deadline_fired":false,"next_seq":1,
        }))
        .unwrap();
        let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='p.md'\nlifecycle='turn'\nthen='nap'\n[checks.nap]\nsleep='5m'\nwhen=['team.stage=Done']\nthen='@done'\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
        std::fs::write(
            dir.path().join(".rimz/team/blackboard.md"),
            "Stage: Review\n",
        )
        .unwrap();
        let paths = state.workflow(&record.id);
        let globals = crate::cli::GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        let view = View {
            record: &record,
            paths: &paths,
            spec: &spec,
            journal: &[],
            now,
            commands: resolve::RunCommands::new(&record, &globals),
        };
        let mut output = Vec::new();
        view.now_line(&state, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("now nap")
                && output.contains("team.stage=Review")
                && !output.contains("slept"),
            "{output}"
        );
    }

    const GATE: &str = "[workflow]\nscope='worktree'\nstart='gate'\n[checks.gate]\ntimeout='1h'\ncases=[{when=['ci=pending'],outcome='pending'}]\non={pending='@wait',pass='future',fail='build'}\n[checks.future]\nshell='true'\non={pass='@done',fail='@failed'}\n[steps.build]\nshell='true'\nthen='gate'\n";

    fn output(with_you: bool) -> (String, String) {
        output_of(GATE, with_you)
    }

    fn output_of(definition: &str, with_you: bool) -> (String, String) {
        let now = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2);
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":WorkflowRunId::new(),"workflow":"review",
            "spec_dir":"/spec","spec_digest":"digest","project_root":"/repo",
            "scope":{"kind":"worktree","path":"/repo/lane"},
            "created_at":Timestamp::UNIX_EPOCH,"entered_at":Timestamp::UNIX_EPOCH,
            "state":"active","node":"gate","visits":{"build":{"total":1,"outcomes":[]}},
            "visit_grant":0,"deadline_fired":false,"next_seq":3,
            "wait":{"node":"gate","outcome":"pending","first":Timestamp::UNIX_EPOCH,
                "last":Timestamp::UNIX_EPOCH,"count":2},
            "held":with_you.then(|| serde_json::json!({"entered_at":Timestamp::UNIX_EPOCH,
                "attempt":{"kind":"agent","lifecycle":"until","seq":2,"node":"build",
                    "started_at":Timestamp::UNIX_EPOCH,"host":{"pid":1,"start":null},
                    "run":{"run_id":rimz::ids::RunId::new(),"handle":"@pet"}}})),
        }))
        .unwrap();
        let spec = toml::from_str(definition).unwrap();
        let journal: Vec<JournalEntry> = serde_json::from_value(serde_json::json!([
            {"event":"attempt","node":"build","seq":1,"visit":1,"outcome":"ended",
                "started_at":Timestamp::UNIX_EPOCH,"ended_at":Timestamp::UNIX_EPOCH,
                "at":Timestamp::UNIX_EPOCH,"digests":{}},
            {"event":"evaluation","node":"gate","seq":2,"outcome":"pending",
                "object":{"outcome":"pending","case":0,"readings":{"ci":"pending"}},
                "started_at":now,"ended_at":now,"at":now,"digests":{}},
        ]))
        .unwrap();
        let paths = paths::StatePaths::for_project_root(&record.project_root)
            .unwrap()
            .workflow(&record.id);
        let globals = crate::cli::GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Always,
        };
        let view = View {
            record: &record,
            paths: &paths,
            spec: &spec,
            journal: &journal,
            now,
            commands: resolve::RunCommands::new(&record, &globals),
        };
        let mut headline = Vec::new();
        let mut rail = Vec::new();
        view.headline(&mut headline, None, None).unwrap();
        view.rail(&mut rail).unwrap();
        (
            String::from_utf8(headline).unwrap(),
            String::from_utf8(rail).unwrap(),
        )
    }

    #[test]
    fn headline_separates_state_context_and_forecast_tones() {
        let (waiting, _) = output(false);
        let (with_you, _) = output(true);
        let style = render::status::workflow(RunState::Active, false).bold();
        let clock = render::workflow_clock(
            Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(1),
            Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_mins(2),
        );
        assert_eq!(
            [
                waiting.contains(&render::paint(style, "☾ waiting at gate 2m: pending")),
                waiting.contains(&render::paint(
                    render::palette::muted(),
                    &format!(" · gives up {clock}, then @failed")
                )),
                with_you.contains(&render::paint(style, "⢿ build is with you")),
                with_you.contains(&render::paint(
                    render::palette::body(),
                    " · @pet · checking gate: pending"
                )),
            ],
            [true; 4],
            "{waiting}\n{with_you}"
        );
    }

    #[test]
    fn vertical_rail_distinguishes_reached_current_future_and_winning_case() {
        let (_, rail) = output(false);
        let (_, held) = output(true);
        let current = render::status::workflow(RunState::Active, false).bold();
        assert_eq!(
            [
                rail.contains(&format!(
                    "{} {}build",
                    render::paint(render::palette::good(), "●"),
                    render::palette::body().render()
                )),
                rail.contains(&format!(
                    "{} {}gate",
                    render::paint(current, "☾"),
                    current.render()
                )),
                rail.contains(&render::paint(render::palette::muted(), "◇")),
                rail.contains(&format!(
                    "│ {} ",
                    render::paint(render::palette::cool().bold(), "▸")
                )),
                held.contains(&format!(
                    "{} {}build",
                    render::paint(current, "⢿"),
                    current.render()
                )),
                held.contains(&format!(
                    "{} {}gate",
                    render::paint(current, "⢿"),
                    current.render()
                )),
            ],
            [true; 6],
            "{rail}\n{held}"
        );
    }

    #[test]
    fn a_delayed_terminal_case_draws_as_a_route_to_a_node() {
        let mut actual = Vec::new();
        for route in ["'@done'", "{to='@done',delay='30m'}"] {
            let definition = GATE
                .replace(
                    "outcome='pending'}]",
                    "outcome='pending'},{when=['ci=passed'],outcome='ready'}]",
                )
                .replace("pending='@wait'", &format!("pending={route},ready={route}"));
            let (_, rail) = output_of(&definition, false);
            let line = |outcome: &str| {
                rail.lines()
                    .find(|line| line.contains(&format!(" {outcome} @")))
                    .unwrap_or_default()
                    .to_owned()
            };
            actual.push((line("pending"), line("ready")));
        }
        let painted = |style: anstyle::Style, text: &str| render::paint(style, text);
        assert_eq!(
            actual,
            vec![
                (
                    format!(
                        "│ {} {}",
                        painted(render::palette::good().bold(), "▸"),
                        painted(
                            render::palette::good().bold(),
                            "1  ci=pending  pending @done"
                        )
                    ),
                    format!(
                        "│   {}",
                        painted(render::palette::good(), "2  ci=passed  ready @done")
                    ),
                ),
                (
                    format!(
                        "│ {} {}",
                        painted(render::palette::cool().bold(), "▸"),
                        painted(
                            render::palette::cool().bold(),
                            "1  ci=pending  pending @done after 30m"
                        )
                    ),
                    format!(
                        "│   {}",
                        painted(
                            render::palette::muted(),
                            "2  ci=passed  ready @done after 30m"
                        )
                    ),
                ),
            ],
        );
    }
}
