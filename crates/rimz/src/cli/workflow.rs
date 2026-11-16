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
mod reconcile;
mod show;

use host::Host;
pub(super) use receipt::restart;

/// The current step's agent while it is still in the room, by its card address.
fn workflow_agent(
    record: &RunRecord,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    include_channel: bool,
    bound: Option<&rimz::store::run::RunRecord>,
) -> Option<String> {
    let attempt = record.session_attempt()?;
    let peers = rimz::address::addressable_agents(snapshot);
    let agent = attempt_agent(record, &peers, attempt.seq, bound)?;
    Some(rimz::address::agent_handle(agent, &peers, include_channel))
}

/// The agent of attempt `seq` while it is still in the room. Once a hook has bound the
/// attempt's supervised run (`bound`) to a session, the current row of that session's launch,
/// whether or not a kept agent's detach cleared its workflow pin; before that, the root the
/// run's current attempt pins as its step.
fn attempt_agent<'a>(
    record: &RunRecord,
    peers: &[&'a rimz::agents::AgentState],
    seq: u64,
    bound: Option<&rimz::store::run::RunRecord>,
) -> Option<&'a rimz::agents::AgentState> {
    if let Some((kind, id)) = bound.and_then(|run| Some((&run.kind, run.agent_id.as_ref()?))) {
        return rimz::address::launch_row(peers.iter().copied(), kind, id)
            .filter(|agent| agent.ended_at.is_none());
    }
    record
        .session_attempt()
        .filter(|attempt| attempt.seq == seq)
        .and_then(|_| record.step_agent(peers))
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
        let bound = rimz::harness::run::load(state, &run.run_id).ok();
        Some(
            self.snapshot(state)
                .and_then(|snapshot| {
                    workflow_agent(record, snapshot, include_channel, bound.as_ref())
                })
                .unwrap_or_else(|| run.handle.clone()),
        )
    }

    /// The run's step agents still in the room, each by the attempt's seq and its card address,
    /// found as the run view finds its step agent: every attempt the journal ended with a
    /// supervised run, and the live one.
    fn attempt_agents(
        &self,
        state: &rimz::StatePaths,
        record: &RunRecord,
        journal: &[records::JournalEntry],
    ) -> workflow_view::history::LiveAgents {
        let Some(snapshot) = self.snapshot(state) else {
            return workflow_view::history::LiveAgents::default();
        };
        let peers = rimz::address::addressable_agents(snapshot);
        let ended = journal.iter().filter_map(|entry| match &entry.event {
            records::JournalEvent::Attempt {
                run_id: Some(run_id),
                ..
            } => Some((entry.seq?, run_id)),
            _ => None,
        });
        let live = record.attempt.as_ref().and_then(|attempt| {
            let run = attempt.run.as_ref()?;
            Some((attempt.seq, &run.run_id))
        });
        workflow_view::history::LiveAgents::new(
            ended
                .chain(live)
                .filter_map(|(seq, run_id)| {
                    let bound = rimz::harness::run::load(state, run_id).ok();
                    let agent = attempt_agent(record, &peers, seq, bound.as_ref())?;
                    Some((seq, rimz::address::agent_handle(agent, &peers, true)))
                })
                .collect(),
        )
    }

    fn snapshot(
        &self,
        state: &rimz::StatePaths,
    ) -> Option<&rimz::store::snapshot::SidebarSnapshot> {
        self.snapshot
            .get_or_init(|| {
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
            })
            .as_ref()
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
        /// one. A workflow with `scope = "project"` refuses it.
        #[arg(short = 'w', long, value_name = "WORKTREE")]
        worktree: Option<String>,
        /// Text appended to every agent step's prompt for this run, and exposed as `prompt` in
        /// `context.json`.
        #[arg(long, allow_hyphen_values = true, value_name = "TEXT")]
        prompt: Option<String>,
        /// End the run through its `deadline` routes at this time, in place of the definition's
        /// `[workflow].deadline`: a duration, `HH:MM`, `<weekday> HH:MM`, a date, a date-time, or
        /// a timestamp.
        #[arg(long, value_name = "WHEN", value_parser = workflow::parse_when)]
        deadline: Option<workflow::When>,
        /// Give up a capacity wait at this time, at a step that sets no `admission.give-up`; takes
        /// what --deadline takes.
        #[arg(long, value_name = "WHEN", value_parser = workflow::parse_when)]
        give_up: Option<workflow::When>,
        /// Launch past every capacity hold instead of waiting, recording each one on the
        /// attempt.
        #[arg(long)]
        skip_capacity: bool,
        /// The fire of a workflow loop task starting this run as the task's own.
        #[arg(long, hide = true, value_name = "JSON")]
        loop_fire_json: Option<String>,
    },
    /// List active and parked workflow runs across rooms.
    List {
        /// Print the active and parked runs as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show a definition, one node, or a run's progress and history.
    Show {
        /// Run address `<workflow>#<checkout>`, workflow name, or run id; defaults to the caller's run.
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
        /// Run address `<workflow>#<checkout>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        #[arg(long, hide = true, allow_hyphen_values = true, value_name = "TEXT")]
        reason: Option<String>,
    },
    /// Move a live run to a node, re-enter a parked one, or extend a run's bounds.
    Resume {
        /// Run address `<workflow>#<checkout>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        /// Move the run to this node; a live attempt at its current node ends canceled.
        #[arg(long, value_name = "NODE")]
        at: Option<String>,
        /// Grant this run more bounds: `visits=+N` or `deadline=<WHEN>`, where WHEN takes what
        /// `run --deadline` takes. Repeatable; an active run takes it without --at.
        #[arg(long, value_name = "GRANT")]
        extend: Vec<String>,
    },
    /// Replace the current agent step, resetting its visits.
    Restart {
        /// Run address `<workflow>#<checkout>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        /// Resume the current conversation instead of starting fresh; refuses if unavailable.
        #[arg(long = "continue")]
        continued: bool,
    },
    /// Answer the workflow check this agent judges, from the judge's own pane.
    Submit {
        /// The answer's outcome: one the check routes.
        #[arg(value_name = "OUTCOME")]
        outcome: String,
        /// Why, in one line.
        #[arg(long, allow_hyphen_values = true, value_name = "TEXT")]
        reason: Option<String>,
        /// The item the answer is about, as text; without it the run keeps its current item.
        #[arg(long, allow_hyphen_values = true, value_name = "KEY")]
        key: Option<String>,
        /// Look again at this time, for an outcome that waits; takes what `run --deadline`
        /// takes.
        #[arg(long, value_name = "WHEN", value_parser = workflow::parse_when)]
        recheck_at: Option<workflow::When>,
        /// A JSON object whose fields, beneath the flags', are carried to the next node as its
        /// input.
        #[arg(long, value_name = "OBJECT", value_parser = answer_object)]
        json: Option<serde_json::Map<String, serde_json::Value>>,
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
    #[command(hide = true)]
    Reconcile,
}

pub(super) fn run(args: WorkflowArgs, globals: &super::GlobalFlags) -> Result<()> {
    match args.command {
        Command::Run {
            name,
            worktree,
            prompt,
            deadline,
            give_up,
            skip_capacity,
            loop_fire_json,
        } => {
            let args = workflow::StartArgs {
                give_up,
                deadline,
                capacity: if skip_capacity {
                    records::StartCapacity::Skip
                } else {
                    records::StartCapacity::Wait
                },
            };
            let task = loop_fire_json
                .as_deref()
                .map(serde_json::from_str::<workflow::TaskStart>)
                .transpose()
                .context("decoding --loop-fire-json")?;
            start(name, worktree, prompt, args, task, globals)
        }
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
        Command::Submit {
            outcome,
            reason,
            key,
            recheck_at,
            json,
        } => {
            let mut answer = json.unwrap_or_default();
            answer.insert("outcome".into(), outcome.into());
            for (field, value) in [("reason", reason), ("key", key)] {
                if let Some(value) = value {
                    answer.insert(field.into(), value.into());
                }
            }
            submit(answer, recheck_at, globals)
        }
        Command::Advance {
            id: Some(id),
            woken_by,
            ..
        } => {
            let (state, found) = resolve::locate(&id)?;
            if !matches!(found.class, records::RunClass::Current(_)) || found.finished {
                let other = resolve::Other { state, id, found };
                if other.is_stranded() {
                    reconcile::end_named(&other.state, &other.id, globals, true)?;
                    return Ok(());
                }
                return match other.refusal() {
                    Some(refusal) => bail!("{refusal}"),
                    None => Ok(()),
                };
            }
            // A terminal write can precede a later error, so the run is collected either way.
            let advanced = workflow::advance(&state, &id, &Host::new(&state, globals, woken_by));
            run_log::collect_run(&state, &id);
            advanced?;
            finish_run(&state, &id)?;
            Ok(())
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
        Command::Reconcile => reconcile::run(globals),
        Command::Validate { dir, json } => validate(dir, json, globals),
    }
}

/// The run's record in the live directory, or none once it has left it, or when this RimZ does
/// not read it.
fn live_record(state: &rimz::StatePaths, id: &WorkflowRunId) -> Result<Option<RunRecord>> {
    Ok(match records::find(state, id)? {
        Some(records::FoundRun {
            class: records::RunClass::Current(record),
            finished: false,
            ..
        }) => Some(*record),
        _ => None,
    })
}

/// What [`finish_run`] found and did.
struct Finish {
    /// The record as last read in the live directory, or none when the run is not a live run
    /// this RimZ reads.
    record: Option<RunRecord>,
    /// This call moved the home to the finished directory.
    moved: bool,
}

/// The tail of a command that may have ended a run: once the run has ended and owes its loop
/// nothing, save its rendered history in its home and in its notes archive, and move the home to
/// the finished directory. A home already moved has the move's barriers completed. A failure
/// warns and leaves the home where it is.
fn finish_run(state: &rimz::StatePaths, id: &WorkflowRunId) -> Result<Finish> {
    let unmoved = |record| Finish {
        record,
        moved: false,
    };
    let found = match records::find(state, id) {
        Ok(Some(found)) if !found.finished => found,
        Ok(Some(_)) => {
            move_home(state, id, None, None)?;
            return Ok(unmoved(None));
        }
        Ok(None) => return Ok(unmoved(None)),
        Err(error) => {
            writeln!(render::err(), "warning: could not read run `{id}`: {error}")?;
            return Ok(unmoved(None));
        }
    };
    let records::RunClass::Current(record) = found.class else {
        return Ok(unmoved(None));
    };
    if !record.state.is_terminal() || record.owes_loop_report() {
        return Ok(unmoved(Some(*record)));
    }
    let history =
        workflow_view::history::saved(&found.paths, &state.finished_workflow(id), &record);
    let moved = move_home(state, id, record.notes.as_deref(), Some(&history))?;
    Ok(Finish {
        record: Some(*record),
        moved,
    })
}

/// Save an ended run's history, when given, in its notes archive and its home, then move the
/// home to the finished directory. Returns whether this call moved it. A failure warns, and a
/// home it left in the live directory stays there.
fn move_home(
    state: &rimz::StatePaths,
    id: &WorkflowRunId,
    notes: Option<&std::path::Path>,
    history: Option<&[u8]>,
) -> Result<bool> {
    let moved = history
        .map_or(Ok(()), |history| {
            workflow_view::history::copy_to_notes(state, notes, history)
        })
        .and_then(|()| {
            let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock)?;
            Ok(records::finish(state, id, history, &guard)?)
        });
    match moved {
        Ok(records::Finished::Moved) => Ok(true),
        Ok(records::Finished::Collision) => {
            writeln!(
                render::err(),
                "warning: run `{id}` stays in the live directory: the finished directory already \
                 holds a run of that id"
            )?;
            Ok(false)
        }
        Ok(_) => Ok(false),
        // The rename is the move; an error after it leaves the home finished.
        Err(error) if !state.workflow(id).home.exists() => {
            writeln!(
                render::err(),
                "warning: run `{id}` moved to the finished directory, but completing the move \
                 failed: {error}"
            )?;
            Ok(true)
        }
        Err(error) => {
            writeln!(
                render::err(),
                "warning: could not finish run `{id}`: {error}; it stays in the live directory"
            )?;
            Ok(false)
        }
    }
}

/// A `submit --json` value: one JSON object.
fn answer_object(raw: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match serde_json::from_str(raw) {
        Ok(serde_json::Value::Object(object)) => Ok(object),
        _ => Err("expected one JSON object".into()),
    }
}

/// Hand the calling judge's answer to its run. The judge is the calling agent's own row, with
/// no walk to a parent, so a judge's child cannot answer for it.
fn submit(
    answer: serde_json::Map<String, serde_json::Value>,
    recheck_at: Option<workflow::When>,
    globals: &super::GlobalFlags,
) -> Result<()> {
    let outside = || {
        anyhow::anyhow!(
            "caller is not a workflow judge; rimz workflow submit answers a check from its judge's own pane"
        )
    };
    let workspace = resolve::caller(globals).ok_or_else(outside)?;
    let state = paths::StatePaths::for_project_root(&workspace.project_root)?;
    let store = super::open_existing_store_at(state.clone())?.ok_or_else(outside)?;
    let snapshot = store.snapshot()?;
    let agent =
        rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents).map_err(|_| outside())?;
    let pin = agent.workflow.as_ref().ok_or_else(outside)?;
    if pin.role != rimz::agents::WorkflowAttemptRole::Judge {
        bail!("caller is a workflow step's agent, not a judge; a step ends by finishing its turn");
    }
    refuse_unwritable_home("submitted")?;
    let outcome = answer["outcome"].as_str().unwrap_or_default().to_owned();
    workflow::submit(&state, &pin.run, pin.seq, answer, recheck_at)?;
    writeln!(
        render::out(),
        "accepted {outcome} for workflow run {}; this pane closes once the run settles it",
        pin.run
    )?;
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
    args: workflow::StartArgs,
    task: Option<workflow::TaskStart>,
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
    // A workflow task's fire names its run as the task's own. Any other run started under a
    // loop fire carries the fire: listed and stopped by its task, reported to nobody.
    let manual = task.as_ref().is_some_and(|task| task.manual);
    let owned = task.is_some();
    let source = match task {
        Some(task) => workflow::DefinitionSource::Deployed {
            dir: &dir,
            task: Some(task.task),
            loop_fire: Some(task.loop_fire),
            fire: Some(task.fire),
            fire_facts: Some(task.fire_facts),
        },
        None => workflow::DefinitionSource::Deployed {
            dir: &dir,
            task: None,
            loop_fire: rimz::harness::loop_task::read_fire_env().map(|fire| fire.fire),
            fire: None,
            fire_facts: None,
        },
    };
    let created = workflow::create(
        ctx.store.paths(),
        source,
        workflow::ScopeRoots {
            checkout: scope.clone(),
            project: ctx.workspace.project_root.clone(),
        },
        || Ok(prompt),
        manual,
        args,
    );
    let (record, advance) = match created {
        Err(ref error @ workflow::WorkflowErr::Exists { ref run, .. }) if owned => {
            writeln!(render::out(), "{run}")?;
            return Err(super::ExitCodeError {
                code: workflow::TaskStart::OVERLAP_EXIT_CODE,
                message: error.to_string(),
            }
            .into());
        }
        created => created?,
    };
    writeln!(render::out(), "{}", record.id)?;
    let paths = ctx.store.paths().workflow(&record.id);
    // A task's fire returns once its run is published; the run's report says how it went.
    let passed = if owned {
        None
    } else {
        receipt::first_pass(ctx.store.paths(), &record.id, Some(&advance))?
    };
    // A run that ended in its first pass has its home moved to the finished directory.
    let work = if paths.work.is_dir() {
        paths.work.clone()
    } else {
        ctx.store.paths().finished_workflow(&record.id).work
    };
    writeln!(
        render::out(),
        "{}\n→ {}",
        work.display(),
        resolve::run_command(&record, "show", globals, None, &[], &[])
    )?;
    match passed {
        Some(passed) => receipt::write_held(&paths, &passed, ""),
        None => Ok(()),
    }
}

fn calling_run(globals: &super::GlobalFlags) -> Result<resolve::Selection> {
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
            let id = pin.run.clone();
            let found = records::find(&state, &id)?
                .with_context(|| format!("workflow run {id} is no longer available"))?;
            return Ok(match found.class {
                records::RunClass::Current(record) if !found.finished => {
                    resolve::Selection::Run(Box::new(resolve::Located {
                        paths: found.paths,
                        record: *record,
                        state,
                    }))
                }
                _ => {
                    let other = resolve::Other { state, id, found };
                    if let Some(refusal) = other.refusal() {
                        bail!("{refusal}");
                    }
                    resolve::Selection::Other(Box::new(other))
                }
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
        return match calling_run(globals)? {
            resolve::Selection::Run(run) => show::render(&run, json, history, globals),
            resolve::Selection::Other(other) => show::render_other(&other, json, history),
            resolve::Selection::Workflow(_) => unreachable!("a pin names one run"),
        };
    };
    let definition = definition::Definition::resolve(target, globals)?;
    if let Some(node) = node {
        let Some(definition) = definition else {
            let run = target.strip_prefix("workflow:").unwrap_or(target);
            if let Ok(resolve::Selection::Other(other)) =
                resolve::select(run, "show", globals, None, &[])
                && other.found.finished
            {
                bail!(
                    "workflow run {} is finished, so it has no node view; `{}` prints its saved \
                     history",
                    other.id,
                    other.show_command(globals)
                );
            }
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
        resolve::Selection::Other(other) => show::render_other(&other, json, history),
    }
}

impl WorkflowArgs {
    /// Whether this parse is `rimz workflow run`.
    pub(super) fn is_run(&self) -> bool {
        matches!(self.command, Command::Run { .. })
    }
}

/// `rimz loop add --workflow`'s check: the target is a deployed workflow's name that validates,
/// and the placement flags fit the workflow's scope.
pub(super) fn check_loop_launch(
    args: &WorkflowArgs,
    place: &super::loop_launch::LoopLaunchPlace<'_>,
) -> Result<()> {
    let Command::Run {
        name,
        worktree,
        loop_fire_json,
        ..
    } = &args.command
    else {
        bail!("the tail does not parse as `rimz workflow run`");
    };
    if loop_fire_json.is_some() {
        bail!("--loop-fire-json is the fire's own; remove it from the tail");
    }
    if name.components().count() != 1 {
        bail!(
            "`{}` is a path; --workflow takes a deployed workflow's name",
            name.display()
        );
    }
    if worktree.is_some() && place.each_worktree {
        bail!("-w in the tail places the run, and --each-worktree places each fire");
    }
    let dir = workflow_view::deployed_definition(name, Some(place.project_root))?;
    if workflow::load(&dir)?.scope() != workflow::ScopeKind::Project {
        return Ok(());
    }
    if place.each_worktree {
        bail!(
            "{} has scope = project, so it runs at the project root; remove --each-worktree",
            name.display()
        );
    }
    if worktree.is_some() {
        bail!(
            "{} has scope = project, so it runs at the project root; remove -w from the tail",
            name.display()
        );
    }
    Ok(())
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
                    // A dot directory is no workflow name.
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
