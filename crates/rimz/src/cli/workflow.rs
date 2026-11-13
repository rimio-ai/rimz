use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::disk::paths;
use rimz::harness::schedule::run_log;
use rimz::harness::signal::Signal;
use rimz::harness::workflow::{self, Validation};
use rimz::ids::{EventId, WorkflowRunId};
use rimz::store::workflow::{self as records, RunRecord};

use super::render;
use super::workflow_view::{self, resolve};

mod definition;
mod host;
mod list;
mod node;
mod rail;
mod receipt;
mod show;

use host::Host;
pub(super) use receipt::restart;

fn workflow_agent(
    record: &RunRecord,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    include_channel: bool,
) -> Option<String> {
    let peers = rimz::address::addressable_agents(snapshot);
    let agent = record.step_agent(&peers)?;
    Some(rimz::address::agent_handle(agent, &peers, include_channel))
}

struct RoomView {
    runtime: Option<rimz::RuntimePaths>,
    ci: Option<rimz::harness::when::CiSource>,
    snapshot: std::cell::OnceCell<Option<rimz::store::snapshot::SidebarSnapshot>>,
}

impl RoomView {
    fn new(root: &std::path::Path) -> Self {
        let runtime = rimz::RuntimePaths::for_project_root(root).ok();
        let ci = runtime.as_ref().map(rimz::harness::when::CiSource::read);
        Self {
            runtime,
            ci,
            snapshot: std::cell::OnceCell::new(),
        }
    }

    fn pr_number(&self, scope: &std::path::Path) -> Option<u64> {
        self.ci.as_ref()?.pr_number(scope)
    }

    fn agent(
        &self,
        state: &rimz::StatePaths,
        record: &RunRecord,
        include_channel: bool,
    ) -> Option<String> {
        let run = record.session_attempt()?.run.as_ref()?;
        let snapshot = self.snapshot.get_or_init(|| {
            let runtime = self.runtime.as_ref()?;
            let workspace = rimz::workspace::record::read(&state.workspace_record).ok()?;
            let store = rimz::Store::open_existing(state.clone(), runtime.clone())?;
            let mut snapshot = rimz::sidebar::consumer::cached_alive_snapshot(
                store.snapshot_with_context(|_, _| false).ok()?,
                runtime,
                &workspace.session_name,
            );
            if let Some(frame) = rimz::sidebar::cache::read_snapshot_cache(
                &runtime.pane_frame_path(),
                &workspace.session_name,
            ) {
                snapshot = snapshot.with_live_panes(frame.to_pane_refs(), None);
            }
            Some(snapshot)
        });
        Some(
            snapshot
                .as_ref()
                .and_then(|snapshot| workflow_agent(record, snapshot, include_channel))
                .unwrap_or_else(|| run.handle.clone()),
        )
    }
}

fn run_origin(record: &RunRecord) -> String {
    record.task.as_ref().map_or_else(
        || "hand".into(),
        |task| {
            format!(
                "loop task {task}{}",
                if record.manual { ", fired by hand" } else { "" }
            )
        },
    )
}

fn limit_text(limit: &rimz::harness::workflow::Limit) -> String {
    limit
        .max_parallel
        .map(|cap| format!("max-parallel={cap}"))
        .into_iter()
        .chain(
            limit
                .stagger
                .as_ref()
                .map(|stagger| format!("stagger={}", stagger.text())),
        )
        .collect::<Vec<_>>()
        .join(" · ")
}

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
    /// Show a definition, one node, or a run's progress and history.
    Show {
        /// Run address `<workflow>#<lane>`, workflow name, or run id; defaults to the caller's run.
        #[arg(value_name = "RUN")]
        id: Option<String>,
        /// Show this node in full; requires a workflow name, not a run.
        #[arg(value_name = "NODE", requires = "id", conflicts_with_all = ["json", "history"])]
        node: Option<String>,
        /// Print the run address, work directory, notes folder, record, and history as JSON.
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
        #[arg(long, hide = true, allow_hyphen_values = true, value_name = "TEXT")]
        reason: Option<String>,
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
    /// Replace the current agent step, resetting its visits.
    Restart {
        /// Run address `<workflow>#<lane>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        /// Resume the current conversation instead of starting fresh; refuses if unavailable.
        #[arg(long = "continue")]
        continued: bool,
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
        Command::Show {
            id,
            node,
            json,
            history,
        } => show(id.as_deref(), node.as_deref(), json, history, globals),
        Command::Cancel { id, reason } => receipt::cancel(&id, reason.as_deref(), globals),
        Command::Resume { id, at, extend } => receipt::resume(&id, at.as_deref(), &extend, globals),
        Command::Restart { id, continued } => restart(&id, continued, globals),
        Command::Advance {
            id: Some(id),
            woken_by,
            ..
        } => {
            let (state, _) = resolve::locate(&id)?;
            // A terminal write can precede a later error, so the run is collected either way.
            let advanced = workflow::advance(&state, &id, &Host::new(&state, globals, woken_by));
            run_log::collect_run(&state, &id);
            advanced?;
            let record = records::load(&state.workflow(&id))?;
            write_history_copy(&state, &record)
        }
        Command::Advance {
            wake_signal: Some(signal),
            ..
        } => {
            let ctx = super::ctx::Ctx::open(globals)?;
            let state = ctx.store.paths();
            Ok(workflow::wake(
                state,
                &signal,
                &Host::new(state, globals, None),
            )?)
        }
        Command::Advance { .. } => unreachable!("clap requires a run id or wake signal"),
        Command::Schema => render::json_pretty(&workflow::schema()),
        Command::Validate { dir, json } => validate(dir, json, globals),
    }
}

fn write_history_copy(state: &rimz::StatePaths, record: &records::RunRecord) -> Result<()> {
    if let Err(error) = workflow_view::history::write_copy(state, record) {
        writeln!(
            render::err(),
            "warning: could not write history for run `{}`: {error}; GC will retry",
            record.id
        )?;
    }
    Ok(())
}

/// Refuses before `action` changes anything when the detached engine could not open the
/// home-wide start lock its first launch takes. It checks RimZ's own needs only.
fn refuse_unwritable_home(action: &str) -> Result<()> {
    rimz::harness::throttle::probe_queue_lock().map_err(|error| unwritable_home(error, action))
}

fn unwritable_home(error: rimz::disk::lock::LockErr, action: &str) -> anyhow::Error {
    let error = anyhow::Error::from(error);
    if super::home_write::is_read_only(&error) {
        return super::home_write::hint(error, "rimz workflow", Some(action));
    }
    error.context(format!(
        "nothing was {action}: the detached workflow engine needs this lock"
    ))
}

fn start(
    name: PathBuf,
    worktree: Option<String>,
    prompt: Option<String>,
    globals: &super::GlobalFlags,
) -> Result<()> {
    let dir = workflow_view::deployed_definition(&name, validation_root(globals).as_deref())?;
    let ctx = super::ctx::Ctx::open(globals)?;
    if worktree.is_some() && workflow::load(&dir)?.scope() == workflow::ScopeKind::Project {
        bail!(
            "{} has scope = project, so it runs at {}; start it without -w",
            dir.file_name().unwrap_or(dir.as_os_str()).to_string_lossy(),
            ctx.workspace.project_root.display()
        );
    }
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
    refuse_unwritable_home("started")?;
    let record = workflow::create(
        ctx.store.paths(),
        workflow::DefinitionSource::Deployed {
            dir: &dir,
            task: None,
            surplus: None,
            surplus_after: None,
            fire: None,
        },
        workflow::ScopeRoots {
            checkout: scope.clone(),
            project: ctx.workspace.project_root.clone(),
        },
        || Ok(prompt),
        false,
    )?;
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

fn calling_run(globals: &super::GlobalFlags) -> Result<resolve::Located> {
    let outside = || {
        anyhow::anyhow!(
            "caller is not part of a workflow run; use rimz workflow show <run> or rimz workflow list"
        )
    };
    let workspace = resolve::caller(globals).ok_or_else(outside)?;
    let state = paths::StatePaths::for_project_root(&workspace.project_root)?;
    let store = super::open_existing_store_at(state.clone())?.ok_or_else(outside)?;
    let snapshot = store.snapshot()?;
    let mut agent =
        rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents).map_err(|_| outside())?;
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert((&agent.kind, &agent.agent_id)) {
        if let Some(pin) = &agent.workflow {
            let paths = state.workflow(&pin.run);
            return Ok(resolve::Located {
                record: records::load(&paths)?,
                paths,
                state,
            });
        }
        agent =
            rimz::address::launched_parent(snapshot.agents.iter(), agent).ok_or_else(outside)?;
    }
    Err(outside())
}

fn show(
    target: Option<&str>,
    node: Option<&str>,
    json: bool,
    history: bool,
    globals: &super::GlobalFlags,
) -> Result<()> {
    let Some(target) = target else {
        return show::render(&calling_run(globals)?, json, history, globals);
    };
    let definition = definition::Definition::resolve(target, globals)?;
    if let Some(node) = node {
        let Some(definition) = definition else {
            return Err(super::usage::UsageError::new(
                concat!(
                    "use rimz workflow show <workflow> <node>; ",
                    "the node view takes a workflow name, not a run address or id"
                )
                .into(),
            )
            .into());
        };
        return node::render(&definition, node);
    }
    if history && resolve::names_workflow(target) {
        return Err(super::usage::UsageError::new(
            "--history requires a run address or run id, not a workflow name".into(),
        )
        .into());
    }
    if let Some(definition) = definition
        .as_ref()
        .filter(|definition| definition.is_task())
    {
        let name = definition.name();
        let runs = resolve::all()?
            .into_iter()
            .filter(|run| {
                run.record.task.as_deref() == Some(name) && !run.record.state.is_terminal()
            })
            .collect::<Vec<_>>();
        if !json {
            definition::render(definition, globals)?;
        }
        return list::render(&runs, Some(name), json, globals);
    }
    let target = target.strip_prefix("workflow:").unwrap_or(target);
    match resolve::select(target, "show", globals, None, &[])? {
        resolve::Selection::Workflow(runs) => {
            if !json && let Some(definition) = &definition {
                definition::render(definition, globals)?;
            }
            list::render(&runs, Some(target), json, globals)
        }
        resolve::Selection::Run(run) => show::render(&run, json, history, globals),
    }
}

fn validation_root(globals: &super::GlobalFlags) -> Option<PathBuf> {
    resolve::caller(globals).map(|workspace| workspace.project_root)
}

fn validate(dir: Option<PathBuf>, json: bool, globals: &super::GlobalFlags) -> Result<()> {
    let home = paths::workflows_dir();
    let mut scratch = None;
    let dirs = if let Some(dir) = dir {
        let named = dir
            .to_str()
            .map(|raw| definition::Definition::resolve(raw, globals))
            .transpose()?
            .flatten();
        let dir = named.as_ref().map_or_else(
            || workflow_view::definition_dir(&dir),
            |named| named.dir().to_owned(),
        );
        scratch = named;
        vec![dir]
    } else {
        let mut dirs = Vec::new();
        match std::fs::read_dir(&home) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry
                        .with_context(|| format!("reading {}", home.display()))?
                        .path();
                    // A dot directory is no workflow name: `loop eject` stages in `.eject/`.
                    let hidden = path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.'));
                    if path.is_dir() && !hidden {
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
    let loaded = workflow_view::definitions();
    let project_root = validation_root(globals);
    let mut report = Validation::default();
    for dir in dirs {
        let result = workflow::validate(&dir, &loaded, project_root.as_deref());
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
                    "sleep" => "sleep",
                    "run" => row.run.as_deref().unwrap_or("—"),
                    "shell" => row.shell.as_deref().unwrap_or("—"),
                    "message" => "message",
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
    drop(scratch);
    if !report.errors.is_empty() {
        bail!("{} workflow error(s)", report.errors.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwritable_home_adds_the_sandbox_remedy_only_for_a_read_only_filesystem() {
        for action in ["started", "resumed", "restarted"] {
            for kind in [
                std::io::ErrorKind::ReadOnlyFilesystem,
                std::io::ErrorKind::PermissionDenied,
            ] {
                let error = rimz::disk::lock::LockErr::Open {
                    path: "rimz-home/loops/throttle.lock".into(),
                    source: std::io::Error::from(kind),
                };
                let report = format!("{:#}", unwritable_home(error, action));
                assert!(
                    report.contains(&format!("nothing was {action}: ")),
                    "{report}"
                );
                assert!(report.contains("throttle.lock"), "{report}");
                let read_only = kind == std::io::ErrorKind::ReadOnlyFilesystem;
                for text in [
                    "this command cannot write the RimZ home",
                    "Run `rimz workflow …` as its own command",
                ] {
                    assert_eq!(report.contains(text), read_only, "{report}");
                }
                assert_eq!(
                    report.contains("the detached workflow engine needs this lock"),
                    !read_only,
                    "{report}"
                );
            }
        }
    }
}
