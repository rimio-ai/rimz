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
use super::workflow_view;

mod host;
mod list;
mod receipt;
mod resolve;
mod show;
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
    /// Show a run's current position, next moves, or full history.
    Show {
        /// Run address `<workflow>#<lane>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        /// Print the run address, work directory, record, and history as JSON.
        #[arg(long)]
        json: bool,
        /// Print detailed history instead of the compact run view.
        #[arg(long, conflicts_with = "json")]
        history: bool,
    },
    /// Cancel a run and interrupt its live script.
    Cancel {
        /// Run address `<workflow>#<lane>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
    },
    /// Re-enter a parked node or extend a run's bounds.
    Resume {
        /// Run address `<workflow>#<lane>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
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
        Command::List { json } => list::run(json, globals),
        Command::Show { id, json, history } => show(&id, json, history, globals),
        Command::Cancel { id } => receipt::cancel(&id, globals),
        Command::Resume { id, at, extend } => receipt::resume(&id, at.as_deref(), &extend, globals),
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
    writeln!(
        render::out(),
        "→ {}",
        resolve::run_command(&record, "show", globals, None, &[], &[])
    )?;
    Ok(())
}

fn locate(id: &WorkflowRunId) -> Result<(rimz::StatePaths, paths::WorkflowPaths)> {
    paths::workflow_runs_under(&paths::rimz_home())?
        .into_iter()
        .find(|(state, run)| state.workflow(id).home == run.home)
        .with_context(|| format!("unknown workflow run {id}"))
}

fn show(target: &str, json: bool, history: bool, globals: &super::GlobalFlags) -> Result<()> {
    if history && resolve::names_workflow(target) {
        return Err(super::usage::UsageError::new(
            "--history requires a run address or run id, not a workflow name".into(),
        )
        .into());
    }
    match resolve::select(target, "show", globals, None, &[])? {
        resolve::Selection::Workflow(runs) => list::render(&runs, Some(target), json, globals),
        resolve::Selection::Run(run) => show::render(&run, json, history, globals),
    }
}

fn validation_root(globals: &super::GlobalFlags) -> Option<PathBuf> {
    resolve::caller(globals).map(|workspace| workspace.project_root)
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
                        .map(|edge| {
                            let mark = edge
                                .mark()
                                .map_or(String::new(), |mark| format!(", {mark}"));
                            format!("{} → {} ({}{mark})", edge.outcome, edge.target, edge.source)
                        })
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
