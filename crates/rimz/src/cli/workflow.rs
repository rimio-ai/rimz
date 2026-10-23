use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::config::definitions::{self, SkillCheck};
use rimz::disk::paths;
use rimz::harness::schedule::signal::Signal;
use rimz::harness::workflow::{self, Validation};
use rimz::ids::WorkflowRunId;
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
        name: PathBuf,
        #[arg(short = 'w', long)]
        worktree: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        prompt: Option<String>,
    },
    /// List active and parked workflow runs across rooms.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a run's work directory, current position, and history.
    Show {
        id: WorkflowRunId,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run and interrupt its live script.
    Cancel { id: WorkflowRunId },
    /// Re-enter a parked node or extend a run's bounds.
    Resume {
        id: WorkflowRunId,
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
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
    },
    /// Validate one workflow directory, or every machine workflow.
    Validate {
        dir: Option<PathBuf>,
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
            let changed = workflow::cancel(&state, &id, &Host::new(&state, globals))?;
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
        Command::Advance { id: Some(id), .. } => {
            let (state, _) = locate(&id)?;
            Ok(workflow::advance(&state, &id, &Host::new(&state, globals))?)
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
    record.wait.as_ref().map(|w| {
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
        format!(
            "waiting: {}, {detail}, since {}, ×{}{recheck}",
            w.node,
            w.first.to_zoned(zone).strftime("%H:%M"),
            w.count,
        )
    })
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
            let detail = waiting(&r)
                .or_else(|| r.park.as_ref().map(records::Park::label))
                .unwrap_or_default();
            rows.push(serde_json::json!({"id":r.id,"workflow":r.workflow,"scope":r.scope.path,"state":r.state,"node":r.node,"since":r.entered_at,"detail":detail}));
            table.row([
                render::cell(r.id.as_str()),
                render::cell(&r.workflow),
                render::cell(r.scope.path.display().to_string()),
                render::cell(r.state.as_str()).fg(render::status::workflow(r.state)),
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
    let state = if let Some(park) = &record.park {
        format!("{} ({})", record.state.as_str(), park.label())
    } else {
        record.state.as_str().into()
    };
    header.push(
        "state",
        render::cell(state).fg(render::status::workflow(record.state)),
    );
    header.push(
        "current node",
        render::cell(format!("{}, since {}", record.node, record.entered_at)),
    );
    if let Some(attempt) = &record.attempt
        && let Some(run) = &attempt.run
    {
        header.push(
            "attempt",
            render::cell(format!("{} #{}", run.handle, attempt.seq)),
        );
    }
    if record
        .park
        .as_ref()
        .is_some_and(|park| park.reason == records::ParkReason::Asked)
        && let Some(asked) = record
            .prev_attempt
            .as_ref()
            .and_then(|previous| previous.asked.as_ref())
    {
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
    if let Some(waiting) = waiting(&record) {
        writeln!(out, "{waiting}")?;
    }
    if record
        .wait
        .as_ref()
        .is_some_and(|wait| wait.outcome == "takeover")
    {
        match workflow::AttemptHost::occupancy(
            &Host::new(&state_paths, globals),
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
    writeln!(out, "history:")?;
    for entry in journal {
        let value = serde_json::to_value(&entry)?;
        let event = value["event"].as_str().unwrap_or_default();
        let outcome = ["outcome", "state", "reason", "signal"]
            .iter()
            .find_map(|field| value.get(*field).and_then(|v| v.as_str()))
            .map_or_else(String::new, |word| format!(" {word}"));
        let count = value
            .get("count")
            .map_or_else(String::new, |count| format!(" ×{count}"));
        let payload = value
            .get("object")
            .or_else(|| value.get("input"))
            .map_or_else(String::new, |v| format!(" {v}"));
        writeln!(
            out,
            "  {} #{} {} {event}{outcome}{count}{payload}",
            entry.at,
            entry.seq.map_or_else(|| "—".into(), |seq| seq.to_string()),
            entry.node
        )?;
        if let records::JournalEvent::Attempt {
            handle,
            head_start,
            head_end,
            cost_usd,
            children,
            limited,
            asked,
            digests,
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
            writeln!(
                out,
                "    {} HEAD {} → {} cost {}",
                handle.as_deref().unwrap_or("—"),
                short(head_start),
                short(head_end),
                cost_usd.map_or_else(|| "unknown".into(), rimz::theme::fmt::dollars2)
            )?;
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
