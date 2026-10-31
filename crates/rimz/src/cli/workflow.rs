use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::definitions::{self, SkillCheck};
use rimz::disk::paths;
use rimz::harness::schedule::signal::Signal;
use rimz::harness::workflow::{self, Validation};
use rimz::ids::{EventId, WorkflowRunId};
use rimz::store::workflow::{self as records, RunRecord};

use super::render;

mod host;
use host::Host;

#[derive(Debug, Args)]
pub(super) struct WorkflowArgs {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a workflow in the current checkout or an existing owned worktree.
    Run {
        /// Workflow name under `~/.rimz/workflows/`, or a directory inside it.
        #[arg(value_name = "NAME|DIR")]
        name: PathBuf,
        /// Run in this existing RimZ-owned worktree instead of the current checkout; never creates
        /// one. A project-scoped workflow runs at the project root regardless.
        #[arg(short = 'w', long, value_name = "WORKTREE")]
        worktree: Option<String>,
        /// Text appended to every agent step's prompt for this run, and exposed as `prompt` in
        /// `context.json`.
        #[arg(long, allow_hyphen_values = true, value_name = "TEXT")]
        prompt: Option<String>,
    },
    /// List active and parked workflow runs across rooms.
    List {
        /// Print the active and parked runs as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show a run's work directory, current position, and history.
    Show {
        /// Run id (`wf_...`), as `workflow run` or `workflow list` prints it.
        #[arg(value_name = "RUN")]
        id: WorkflowRunId,
        /// Print the work directory, run record, and history as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run and interrupt its live script.
    Cancel {
        /// Run id to cancel.
        #[arg(value_name = "RUN")]
        id: WorkflowRunId,
    },
    /// Re-enter a parked node or extend a run's bounds.
    Resume {
        /// Run id to resume or extend.
        #[arg(value_name = "RUN")]
        id: WorkflowRunId,
        /// Re-enter this node instead of the parked one. Parked runs only.
        #[arg(long, value_name = "NODE")]
        at: Option<String>,
        /// Grant this run more bounds: `visits=+N` or `deadline=<WHEN>`. Repeatable; an active run
        /// accepts it alone.
        #[arg(long, value_name = "GRANT")]
        extend: Vec<String>,
    },
    #[command(hide = true)]
    Advance {
        #[arg(required_unless_present = "wake_signal")]
        id: Option<WorkflowRunId>,
        #[arg(
            long,
            conflicts_with = "id",
            value_parser = |raw: &str| serde_json::from_str::<Signal>(raw)
        )]
        wake_signal: Option<Signal>,
        #[arg(long, hide = true, requires = "id", conflicts_with = "wake_signal")]
        woken_by: Option<EventId>,
    },
    /// Validate one workflow directory, or every machine workflow.
    Validate {
        /// Workflow directory to check. Without it, checks every workflow under
        /// `~/.rimz/workflows/`.
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Print rows, warnings, and errors as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the workflow specification's JSON Schema.
    Schema,
}

pub(super) fn run(args: WorkflowArgs, globals: &super::GlobalFlags) -> Result<()> {
    match args.command {
        Command::Run {
            name,
            worktree,
            prompt,
        } => start(name, worktree, prompt, globals),
        Command::List { json } => list(json),
        Command::Show { id, json } => show(&id, json, globals),
        Command::Cancel { id } => {
            let (state, _) = locate(&id)?;
            let changed = workflow::cancel(&state, &id, &Host::new(&state, globals, None))?;
            writeln!(
                render::out(),
                "{id}: {}",
                if changed {
                    "canceled"
                } else {
                    "already terminal; unchanged"
                }
            )?;
            Ok(())
        }
        Command::Resume { id, at, extend } => {
            let (state, _) = locate(&id)?;
            workflow::resume(&state, &id, at.as_deref(), &extend)?;
            writeln!(render::out(), "{id}: resumed")?;
            Ok(())
        }
        Command::Advance {
            id: Some(id),
            woken_by,
            ..
        } => {
            let (state, _) = locate(&id)?;
            Ok(workflow::advance(
                &state,
                &id,
                &Host::new(&state, globals, woken_by),
            )?)
        }
        Command::Advance {
            wake_signal: Some(signal),
            ..
        } => {
            let ctx = super::ctx::Ctx::open(globals)?;
            Ok(workflow::wake(ctx.store.paths(), &signal)?)
        }
        Command::Advance { .. } => unreachable!("clap requires a run id or wake signal"),
        Command::Schema => render::json_pretty(&workflow::schema()),
        Command::Validate { dir, json } => validate(dir, json, globals),
    }
}

fn definitions() -> rimz::config::definitions::LoadedDefinitions {
    let machine = super::machine_config();
    definitions::load(
        &paths::agents_home(),
        SkillCheck::Skip,
        &machine.agents.commands,
        &rimz::config::MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact,
    )
}

fn start(
    name: PathBuf,
    worktree: Option<String>,
    prompt: Option<String>,
    globals: &super::GlobalFlags,
) -> Result<()> {
    let mut components = name.components();
    let dir = if matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
    {
        paths::workflows_dir().join(name)
    } else {
        name
    };
    let project_root = validation_root(globals);
    let report = workflow::validate(
        &dir,
        &paths::workflows_dir(),
        &definitions(),
        project_root.as_deref(),
    );
    if !report.errors.is_empty() {
        bail!(
            "{}",
            report
                .errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let ctx = super::ctx::Ctx::open(globals)?;
    let scope = if let Some(name) = &worktree {
        rimz::worktree::resolve_owned(
            &ctx.workspace.project_root,
            &super::machine_config().agents.worktree,
            name,
        )?
        .path
    } else {
        ctx.workspace.worktree_root.clone()
    };
    let record = workflow::create(
        ctx.store.paths(),
        &dir,
        workflow::ScopeRoots {
            checkout: scope.clone(),
            project: ctx.workspace.project_root.clone(),
        },
        || Ok(prompt),
        None,
        false,
    )?;
    if worktree.is_some() && record.scope.kind == records::ScopeKind::Project {
        writeln!(
            render::err(),
            "note: {} has scope = project; the run is at {}, not {}",
            record.workflow,
            record.scope.path.display(),
            scope.display()
        )?;
    }
    writeln!(
        render::out(),
        "{}\n{}",
        record.id,
        ctx.store.paths().workflow(&record.id).work.display()
    )?;
    Ok(())
}

fn locate(id: &WorkflowRunId) -> Result<(rimz::StatePaths, paths::WorkflowPaths)> {
    paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .find(|(state, run)| state.workflow(id).home == run.home)
        .with_context(|| format!("unknown workflow run {id}"))
}

pub(super) fn waiting(record: &RunRecord) -> Option<String> {
    waiting_line(record, None)
}

fn waiting_line(record: &RunRecord, history: Option<&[HistoryRow<'_>]>) -> Option<String> {
    record.wait.as_ref().map(|w| {
        if w.outcome == "capacity" {
            return format!(
                "{} · {}, {}, ×{}",
                rimz::theme::fmt::capacity_wait(&w.kinds, w.first, w.gives_up_at),
                w.node,
                w.detail.as_deref().unwrap_or("—"),
                w.count,
            );
        }
        let zone = jiff::tz::TimeZone::system();
        let detail = match w.outcome.as_str() {
            "hold" => {
                let hold = paths::StatePaths::for_project_root(&record.project_root)
                    .ok()
                    .and_then(|state| workflow::load(&state.workflow(&record.id).home).ok())
                    .and_then(|spec| {
                        spec.checks
                            .get(&w.node)
                            .and_then(|check| {
                                w.case.and_then(|index| check.cases.get(index as usize))
                            })
                            .and_then(|case| case.hold.clone())
                    })
                    .unwrap_or_else(|| "?".into());
                let elapsed = jiff::Timestamp::now()
                    .duration_since(w.first)
                    .as_secs()
                    .max(0) as u64;
                format!(
                    "holding {} {}/{hold}",
                    w.detail.as_deref().unwrap_or("—"),
                    render::age_label(elapsed)
                )
            }
            "unknown" => w
                .detail
                .as_ref()
                .map_or_else(|| "unknown".into(), |detail| format!("unknown: {detail}")),
            "window" => w
                .detail
                .as_ref()
                .map_or_else(|| "window".into(), |detail| format!("window {detail}")),
            "takeover" | "occupied" => w.outcome.clone(),
            _ => format!("said {}", w.outcome),
        };
        let recheck = w
            .recheck_at
            .map(|at| format!(", recheck {}", at.to_zoned(zone.clone()).strftime("%H:%M")))
            .unwrap_or_default();
        let show_count = history.is_none_or(|rows| {
            !wait_row_counts(rows, &w.node, &w.outcome, w.first, w.last).eq([w.count])
        });
        let count = if show_count {
            format!(", ×{}", w.count)
        } else {
            String::new()
        };
        format!(
            "waiting: {}, {detail}, since {}{count}{recheck}",
            w.node,
            w.first.to_zoned(zone).strftime("%H:%M"),
        )
    })
}

fn session_attempt(record: &records::RunRecord) -> Option<&records::LiveAttempt> {
    record
        .held
        .as_ref()
        .map(|held| &held.attempt)
        .or(record.attempt.as_ref())
        .filter(|attempt| attempt.kind == records::AttemptKind::Agent)
}

fn with_you(record: &records::RunRecord) -> bool {
    record.state == records::RunState::Active
        && session_attempt(record)
            .is_some_and(|attempt| !attempt.lifecycle.is_turn() && attempt.run.is_some())
}

fn asking(record: &records::RunRecord) -> Option<&records::Asked> {
    if record.state != records::RunState::Active {
        return None;
    }
    session_attempt(record)?
        .asking
        .as_ref()
        .map(|asking| &asking.ask)
}

fn list(json: bool) -> Result<()> {
    let mut rows = Vec::new();
    let mut table = render::Table::new([
        "RUN", "WORKFLOW", "CHECKOUT", "STATE", "NODE", "SINCE", "DETAIL",
    ]);
    let rooms: BTreeMap<_, _> = paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .map(|(state, _)| (state.root.clone(), state))
        .collect();
    for state in rooms.values() {
        for r in records::list(state)? {
            if r.state.is_terminal() {
                continue;
            }
            let asking = asking(&r);
            let asking_line = asking.map(|ask| {
                format!(
                    "asking you: {} ({}): {}",
                    ask.handle,
                    ask.kind,
                    render::one_line(&ask.text)
                )
            });
            let detail = if with_you(&r) {
                asking_line.unwrap_or_else(|| {
                    let mut detail = "with you".to_owned();
                    if r.held.is_some() {
                        detail.push_str(&format!("; checking {}", r.node));
                    }
                    if let Some(waiting) = waiting(&r) {
                        detail.push_str(&format!("; {waiting}"));
                    }
                    detail
                })
            } else {
                waiting(&r)
                    .or_else(|| r.park.as_ref().map(records::Park::label))
                    .or(asking_line)
                    .unwrap_or_default()
            };
            rows.push(serde_json::json!({"id":r.id,"workflow":r.workflow,"scope":r.scope.path,"state":r.state,"node":r.node,"since":r.entered_at,"detail":detail}));
            table.row([
                render::cell(r.id.as_str()),
                render::cell(&r.workflow),
                render::cell(r.scope.path.display().to_string()),
                render::cell(r.state.as_str())
                    .fg(render::status::workflow(r.state, asking.is_some())),
                render::cell(&r.node),
                render::cell(r.entered_at.to_string()),
                render::cell(detail),
            ]);
        }
    }
    if json {
        render::json_pretty(&rows)
    } else {
        Ok(table.render(&mut render::out())?)
    }
}

fn show(id: &WorkflowRunId, json: bool, globals: &super::GlobalFlags) -> Result<()> {
    let (state_paths, paths) = locate(id)?;
    let record = records::load(&paths)?;
    let journal = records::journal(&paths)?;
    if json {
        return render::json_pretty(
            &serde_json::json!({"dir":paths.work,"record":record,"journal":journal}),
        );
    }
    let spec = workflow::load(&paths.home)?;
    let history = collapsed_history(&journal);
    let mut out = render::out();
    writeln!(out, "{}", paths.work.display())?;
    let mut header = render::KeyVals::new();
    header.push("workflow", render::cell(&record.workflow));
    header.push(
        match record.scope.kind {
            records::ScopeKind::Worktree => "scope worktree",
            records::ScopeKind::Project => "scope project",
        },
        render::cell(record.scope.path.display().to_string()),
    );
    let asking = asking(&record);
    let state = if let Some(park) = &record.park {
        format!("{} ({})", record.state.as_str(), park.label())
    } else if asking.is_some() {
        "active (asking you)".into()
    } else if with_you(&record) {
        "active (with you)".into()
    } else {
        record.state.as_str().into()
    };
    header.push(
        "state",
        render::cell(state).fg(render::status::workflow(record.state, asking.is_some())),
    );
    header.push(
        "current node",
        render::cell(format!("{}, since {}", record.node, record.entered_at)),
    );
    if let Some(attempt) = session_attempt(&record)
        && let Some(run) = &attempt.run
    {
        header.push(
            "attempt",
            render::cell(format!("{} #{}", run.handle, attempt.seq)),
        );
    }
    if record.held.is_some() {
        header.push("check", render::cell(&record.node));
    }
    let asked = asking.or_else(|| {
        record
            .park
            .as_ref()
            .filter(|park| park.reason == records::ParkReason::Asked)
            .and_then(|_| record.prev_attempt.as_ref()?.asked.as_ref())
    });
    if let Some(asked) = asked {
        header.push(
            "ask",
            render::cell(format!("{} ({}): {}", asked.handle, asked.kind, asked.text)),
        );
    }
    header.push(
        "deadline",
        render::cell(record.deadline.map_or_else(
            || "none".into(),
            |at| {
                format!(
                    "{at} ({})",
                    if record.deadline_fired {
                        "fired"
                    } else {
                        "armed"
                    }
                )
            },
        )),
    );
    header.push(
        "prompt",
        render::cell(record.prompt.as_deref().unwrap_or("none")),
    );
    header.render(&mut out)?;
    if let Some(waiting) = waiting_line(&record, Some(&history)) {
        writeln!(out, "{waiting}")?;
    }
    if record
        .wait
        .as_ref()
        .is_some_and(|wait| wait.outcome == "takeover")
    {
        match workflow::AttemptHost::occupancy(
            &Host::new(&state_paths, globals, None),
            &record.scope.path,
            &record.project_root,
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
    }
    for (name, visits) in &record.visits {
        let max = spec.steps.get(name).and_then(|s| s.max_visits).map_or_else(
            || "unbounded".into(),
            |max| format!("{} + {} grant", max, record.visit_grant),
        );
        writeln!(out, "step {name}: {} visits of {max}", visits.total)?;
        if let Some(max) = spec.steps.get(name).and_then(|s| s.max_visits_per_key) {
            for key in &visits.keys {
                writeln!(
                    out,
                    "  key {}: {} visits of {max} + {} grant",
                    serde_json::to_string(&key.key)?,
                    key.visits,
                    record.visit_grant,
                )?;
            }
        }
    }
    render_history(&mut out, &history, &spec)
}

struct HistoryRow<'a> {
    first: &'a records::JournalEntry,
    latest: &'a records::JournalEntry,
    count: u64,
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
        let mut note = if count > 1 {
            let span = last.duration_since(first).as_secs().max(0) as u64;
            format!(" ×{count} over {}", render::age_label(span))
        } else {
            String::new()
        };
        if let Some(detail) = detail {
            note.push_str(&format!(" · {detail}"));
        }
        note
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
        _ => None,
    }
}

fn collapsed_history(journal: &[records::JournalEntry]) -> Vec<HistoryRow<'_>> {
    let mut rows = Vec::<HistoryRow<'_>>::new();
    for entry in journal {
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
            row.count += 1;
            continue;
        }
        rows.push(HistoryRow {
            first: entry,
            latest: entry,
            count: 1,
        });
    }
    rows
}

fn wait_row_counts<'a>(
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

fn render_history(
    out: &mut impl Write,
    rows: &[HistoryRow<'_>],
    spec: &workflow::WorkflowSpec,
) -> Result<()> {
    writeln!(out, "history:")?;
    for row in rows {
        let entry = row.latest;
        let value = serde_json::to_value(entry)?;
        let event = value["event"].as_str().unwrap_or_default();
        let action = value
            .get("action")
            .and_then(serde_json::Value::as_str)
            .map_or_else(String::new, |action| format!(" {action}"));
        let outcome = ["outcome", "state", "reason", "signal"]
            .iter()
            .find_map(|field| value.get(*field).and_then(|v| v.as_str()))
            .map_or_else(String::new, |word| format!(" {word}"));
        let note = row.note();
        let target = value
            .get("target")
            .and_then(serde_json::Value::as_str)
            .map_or_else(String::new, |target| format!(" → {target}"));
        let payload = value
            .get("object")
            .or_else(|| value.get("input"))
            .map_or_else(String::new, |v| format!(" {v}"));
        let seq = match (row.count, row.first.seq, entry.seq) {
            (1, _, Some(seq)) => seq.to_string(),
            (_, Some(first), Some(last)) => format!("{first}–{last}"),
            _ => "—".into(),
        };
        writeln!(
            out,
            "  {} #{seq} {} {event}{action}{outcome}{target}{note}{payload}",
            entry.at, entry.node
        )?;
        if let records::JournalEvent::Attempt {
            handle,
            head_start,
            head_end,
            code,
            log,
            cost_usd,
            children,
            limited,
            asked,
            digests,
            reason,
            ..
        } = &entry.event
        {
            let short = |head: &Option<String>| {
                head.as_deref()
                    .unwrap_or("—")
                    .chars()
                    .take(8)
                    .collect::<String>()
            };
            if spec
                .steps
                .get(&entry.node)
                .is_some_and(|step| step.agent.is_none())
            {
                writeln!(
                    out,
                    "    exit {} HEAD {} → {}",
                    code.map_or_else(|| "—".into(), |code| code.to_string()),
                    short(head_start),
                    short(head_end),
                )?;
            } else {
                writeln!(
                    out,
                    "    {} HEAD {} → {} cost {}",
                    handle.as_deref().unwrap_or("—"),
                    short(head_start),
                    short(head_end),
                    cost_usd.map_or_else(|| "unknown".into(), rimz::theme::fmt::dollars2)
                )?;
            }
            if let Some(log) = log {
                writeln!(out, "    log: {}", log.display())?;
            }
            if let Some(reason) = reason {
                writeln!(out, "    reason: {reason}")?;
            }
            if let Some(children) = children {
                for (label, children) in [("stopped", &children.stopped), ("live", &children.live)]
                {
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
            }
            if let Some(limited) = limited {
                writeln!(
                    out,
                    "    reset_at: {}",
                    limited
                        .reset_at
                        .map_or_else(|| "unknown".into(), |at| at.to_string())
                )?;
            }
            if let Some(asked) = asked {
                writeln!(out, "    {} ({}): {}", asked.handle, asked.kind, asked.text)?;
            }
            for (path, digest) in digests {
                writeln!(out, "    {path}: {digest}")?;
            }
        }
    }
    Ok(())
}

fn validation_root(globals: &super::GlobalFlags) -> Option<PathBuf> {
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok()?;
    let recorded = paths::StatePaths::for_project_root(&workspace.project_root)
        .ok()
        .is_some_and(|state| rimz::workspace::record::read(&state.workspace_record).is_ok());
    if globals.root.is_none()
        && workspace.root_class == rimz::workspace::RootClass::Directory
        && !recorded
    {
        return None;
    }
    Some(workspace.project_root)
}

fn validate(dir: Option<PathBuf>, json: bool, globals: &super::GlobalFlags) -> Result<()> {
    let home = paths::workflows_dir();
    let dirs = if let Some(dir) = dir {
        vec![dir]
    } else {
        let mut dirs = Vec::new();
        match std::fs::read_dir(&home) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry
                        .with_context(|| format!("reading {}", home.display()))?
                        .path();
                    if path.is_dir() {
                        dirs.push(path);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("reading {}", home.display())),
        }
        dirs.sort();
        dirs
    };
    let loaded = definitions();
    let project_root = validation_root(globals);
    let mut report = Validation::default();
    for dir in dirs {
        let result = workflow::validate(&dir, &home, &loaded, project_root.as_deref());
        report.rows.extend(result.rows);
        report.errors.extend(result.errors);
        report.warnings.extend(result.warnings);
    }
    if json {
        render::json_pretty(&report)?;
    } else {
        let mut table =
            render::Table::new(["WORKFLOW", "NODE", "KIND", "RUNNER", "BACKGROUND", "EDGES"]);
        for row in &report.rows {
            table.row([
                render::cell(&row.workflow),
                render::cell(&row.node),
                render::cell(&row.kind),
                render::cell(match row.runner.as_str() {
                    "condition" => "condition",
                    "run" => row.run.as_deref().unwrap_or("—"),
                    "shell" => row.shell.as_deref().unwrap_or("—"),
                    _ => row.agent.as_deref().unwrap_or("—"),
                }),
                render::cell(if row.agent.is_none() {
                    "—"
                } else if row.do_background {
                    "yes"
                } else {
                    "no"
                }),
                render::cell(
                    row.edges
                        .iter()
                        .map(|edge| format!("{} → {} ({})", edge.outcome, edge.target, edge.source))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ]);
        }
        let mut out = render::out();
        table.render(&mut out)?;
        for error in &report.errors {
            writeln!(out, "{}", error.message)?;
        }
        for warning in &report.warnings {
            writeln!(out, "warning: {}", warning.message)?;
        }
    }
    if !report.errors.is_empty() {
        bail!("{} workflow error(s)", report.errors.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use records::{EvaluationOrigin, JournalEntry, JournalEvent};
    use serde_json::json;

    fn at(minute: i64) -> Timestamp {
        "2026-10-30T19:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .checked_add(jiff::SignedDuration::from_secs(minute * 60))
            .unwrap()
    }

    fn evaluation(seq: u64, minute: i64, outcome: &str, case: Option<u32>) -> JournalEntry {
        let mut object = json!({"outcome":outcome, "reason":format!("answer {seq}")});
        if let Some(case) = case {
            object["case"] = json!(case);
        }
        JournalEntry {
            seq: Some(seq),
            at: at(minute),
            node: "pr".into(),
            event: JournalEvent::Evaluation {
                origin: EvaluationOrigin::Check,
                outcome: outcome.into(),
                object,
                digests: BTreeMap::new(),
                started_at: at(minute),
                ended_at: at(minute),
                stderr_tail: None,
                cost_usd: None,
                retry: None,
            },
        }
    }

    fn wake(minute: i64, signal: &str) -> JournalEntry {
        JournalEntry {
            seq: None,
            at: at(minute),
            node: "pr".into(),
            event: JournalEvent::Wake {
                signal: signal.into(),
            },
        }
    }

    fn wait(minute: i64, outcome: &str, first: i64, last: i64, count: u64) -> JournalEntry {
        JournalEntry {
            seq: None,
            at: at(minute),
            node: "pr".into(),
            event: JournalEvent::Wait {
                outcome: outcome.into(),
                detail: None,
                first: at(first),
                last: at(last),
                count,
                recheck_at: None,
            },
        }
    }

    fn history(journal: &[JournalEntry]) -> String {
        let spec = toml::from_str(
            "[workflow]\nscope='worktree'\nstart='pr'\n\
             [checks.pr]\nrun='sh checks/pr.sh'\non.done='@done'\n\
             [steps.build]\nagent='codex'\nprompt-file='prompt.md'\nthen='pr'\n",
        )
        .unwrap();
        let mut out = Vec::new();
        render_history(&mut out, &collapsed_history(journal), &spec).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn evaluations_fold_with_newest_answer_and_suppress_the_closed_wait() {
        let mut journal = (1..=5)
            .map(|seq| evaluation(seq, seq as i64 - 1, "pending", None))
            .collect::<Vec<_>>();
        journal.push(evaluation(6, 5, "done", None));
        journal.push(wait(5, "pending", 0, 4, 5));
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
        assert!(
            text.contains("2026-10-30T19:04:00Z #1–5 pr evaluation pending ×5 over 4m "),
            "{text}"
        );
        assert!(text.contains("\"reason\":\"answer 5\""), "{text}");
        assert!(!text.contains("answer 1"), "{text}");
        assert!(
            text.contains("2026-10-30T19:05:00Z #6 pr evaluation done {"),
            "{text}"
        );
        assert!(!text.contains("pr wait"), "{text}");
    }

    #[test]
    fn a_closed_hold_searches_past_a_newer_case_without_breaking_its_run() {
        let journal = [
            evaluation(1, 0, "hold", Some(0)),
            evaluation(2, 1, "hold", Some(0)),
            evaluation(3, 2, "hold", Some(1)),
            wait(2, "hold", 0, 1, 2),
            evaluation(4, 3, "hold", Some(1)),
            evaluation(5, 4, "hold", Some(1)),
        ];
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
        assert!(
            text.contains("#1–2 pr evaluation hold ×2 over 1m "),
            "{text}"
        );
        assert!(
            text.contains("#3–5 pr evaluation hold ×3 over 2m "),
            "{text}"
        );
        assert!(!text.contains("pr wait"), "{text}");
    }

    #[test]
    fn wakes_break_evaluation_runs_and_only_fold_with_the_same_signal() {
        let mut journal = vec![
            evaluation(1, 0, "pending", None),
            wake(1, "review.ready"),
            evaluation(2, 2, "pending", None),
        ];
        assert_eq!(collapsed_history(&journal).len(), 3);
        journal.extend([
            wake(3, "review.ready"),
            wake(12, "review.ready"),
            wake(13, "review.changed"),
        ]);
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 5, "{text}");
        assert!(
            text.contains("2026-10-30T19:12:00Z #— pr wake review.ready ×2 over 9m\n"),
            "{text}"
        );
        assert!(text.contains("pr wake review.changed\n"), "{text}");
    }

    #[test]
    fn unrepresented_waits_show_their_span_and_detail_but_no_singleton_count() {
        let mut window = wait(3, "window", 0, 2, 3);
        window.node = "build".into();
        if let JournalEvent::Wait { detail, .. } = &mut window.event {
            *detail = Some("5h.left=12<42".into());
        }
        let text = history(&[window, wait(4, "pending", 4, 4, 1)]);
        assert_eq!(
            text,
            concat!(
                "history:\n",
                "  2026-10-30T19:03:00Z #— build wait window ×3 over 2m · 5h.left=12<42\n",
                "  2026-10-30T19:04:00Z #— pr wait pending\n",
            )
        );
    }

    #[test]
    fn attempts_keep_individual_rows_and_their_detail_lines() {
        let journal = [1, 2].map(|seq| JournalEntry {
            seq: Some(seq),
            at: at(seq as i64),
            node: "build".into(),
            event: JournalEvent::Attempt {
                outcome: records::AttemptOutcome::Ended,
                visit: seq,
                key: None,
                digests: BTreeMap::from([("prompt.md".into(), format!("digest-{seq}"))]),
                started_at: at(seq as i64),
                ended_at: at(seq as i64),
                handle: Some(format!("@builder-{seq}")),
                run_id: None,
                head_start: Some("0123456789".into()),
                head_end: Some("abcdef0123".into()),
                code: None,
                log: Some(PathBuf::from(format!("work/{seq}.step.log"))),
                cost_usd: Some(0.25),
                children: None,
                limited: None,
                asked: None,
                reason: None,
            },
        });
        let text = history(&journal);
        assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
        for seq in [1, 2] {
            assert!(
                text.contains(&format!("#{seq} build attempt ended\n")),
                "{text}"
            );
            assert!(
                text.contains(&format!(
                    concat!(
                        "    @builder-{seq} HEAD 01234567 → abcdef01 cost $0.25\n",
                        "    log: work/{seq}.step.log\n    prompt.md: digest-{seq}\n",
                    ),
                    seq = seq,
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn evaluation_keys_keep_origins_nodes_and_outcomes_separate() {
        let check = evaluation(1, 0, "pending", None);
        let mut engine = evaluation(2, 1, "pending", None);
        if let JournalEvent::Evaluation { origin, .. } = &mut engine.event {
            *origin = EvaluationOrigin::Engine;
        }
        let mut other_node = engine.clone();
        other_node.node = "ci".into();
        let mut other_outcome = other_node.clone();
        if let JournalEvent::Evaluation {
            outcome, object, ..
        } = &mut other_outcome.event
        {
            *outcome = "done".into();
            object["outcome"] = json!("done");
        }
        let journal = [check, engine, other_node, other_outcome];
        assert_eq!(collapsed_history(&journal).len(), 4);
    }

    #[test]
    fn wait_rows_match_the_nodes_outcome_and_inclusive_span() {
        let journal = [evaluation(1, 1, "pending", None)];
        let rows = collapsed_history(&journal);
        assert!(wait_row_counts(&rows, "pr", "pending", at(1), at(1)).eq([1]));
        assert!(wait_row_counts(&rows, "pr", "pending", at(0), at(2)).eq([1]));
        for (node, outcome, first, last) in [
            ("pr", "window", 0, 2),
            ("ci", "pending", 0, 2),
            ("pr", "pending", 2, 3),
            ("pr", "pending", 0, 0),
        ] {
            assert!(
                wait_row_counts(&rows, node, outcome, at(first), at(last))
                    .next()
                    .is_none()
            );
        }
    }

    fn waiting_record(first: i64, last: i64, count: u64) -> RunRecord {
        serde_json::from_value(json!({
            "version":2, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "spec_digest":"digest",
            "scope":{"kind":"worktree", "path":"/checkout"}, "project_root":"/checkout",
            "created_at":at(0), "state":"active", "node":"build", "entered_at":at(0),
            "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":4,
            "wait":{"node":"pr", "outcome":"pending", "first":at(first), "last":at(last),
                    "count":count}
        }))
        .unwrap()
    }

    fn kept_turn(minute: i64) -> JournalEntry {
        JournalEntry {
            seq: Some(1),
            at: at(minute),
            node: "build".into(),
            event: JournalEvent::TurnEnd {
                action: records::TurnEndAction::Kept,
                turn: records::TurnEnd {
                    agent_id: "leader".into(),
                    started_at: at(minute - 1),
                    ended_at: at(minute),
                },
                check: "pr".into(),
                outcome: Some("pending".into()),
                target: Some("@wait".into()),
                reason: None,
            },
        }
    }

    #[test]
    fn one_kept_turn_states_the_open_wait_and_suppresses_its_closing_row() {
        let mut journal = vec![
            evaluation(2, 0, "pending", None),
            kept_turn(1),
            evaluation(3, 2, "done", None),
        ];
        let record = waiting_record(0, 0, 1);
        let line = waiting_line(&record, Some(&collapsed_history(&journal))).unwrap();
        assert!(!line.contains('×'), "{line}");
        assert!(waiting(&record).unwrap().contains("×1"));
        journal.push(wait(2, "pending", 0, 0, 1));
        assert_eq!(collapsed_history(&journal).len(), 3);
        assert!(!history(&journal).contains("pr wait"));
    }

    #[test]
    fn several_kept_turns_keep_the_header_total_but_not_a_closing_summary() {
        let mut journal = vec![
            evaluation(2, 0, "pending", None),
            kept_turn(1),
            evaluation(3, 2, "pending", None),
            kept_turn(3),
        ];
        let record = waiting_record(0, 2, 2);
        let line = waiting_line(&record, Some(&collapsed_history(&journal))).unwrap();
        assert!(line.contains("×2"), "{line}");
        journal.push(wait(4, "pending", 0, 2, 2));
        assert_eq!(collapsed_history(&journal).len(), 4);
        assert!(!history(&journal).contains("pr wait"));
    }

    #[test]
    fn wakes_keep_the_open_waits_total_across_rows() {
        let journal = [
            evaluation(1, 0, "pending", None),
            evaluation(2, 1, "pending", None),
            wake(2, "review.ready"),
            evaluation(3, 3, "pending", None),
        ];
        let record = waiting_record(0, 3, 3);
        let line = waiting_line(&record, Some(&collapsed_history(&journal))).unwrap();
        assert!(line.contains("×3"), "{line}");
    }

    #[test]
    fn early_advances_keep_a_count_no_single_evaluation_states() {
        let mut journal = vec![evaluation(1, 0, "pending", None)];
        let record = waiting_record(0, 2, 3);
        let line = waiting_line(&record, Some(&collapsed_history(&journal))).unwrap();
        assert!(line.contains("×3"), "{line}");
        journal.push(wait(3, "pending", 0, 2, 3));
        assert_eq!(collapsed_history(&journal).len(), 1);
        assert!(!history(&journal).contains("pr wait"));
    }
}
