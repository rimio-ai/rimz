use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::disk::paths;
use rimz::harness::schedule::run_log;
use rimz::harness::signal::Signal;
use rimz::harness::workflow::{self, Validation};
use rimz::ids::{EventId, WorkflowRunId};
use rimz::launcher::workflow_start::{self, StartOrigin, WorkflowStart, refuse_unwritable_home};
use rimz::packs::one_line;
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
mod replay;
mod show;
mod wait;

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
    Some(rimz::address::named_handle(agent, &peers, include_channel))
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
                    let handle = rimz::address::agent_handle(agent, &peers, true);
                    Some((
                        seq,
                        rimz::address::handle_with_name(&handle, agent.name.as_deref()),
                    ))
                })
                .collect(),
        )
    }

    /// The card address of the agent that started the run, while its row is in the room.
    fn starter(&self, state: &rimz::StatePaths, record: &RunRecord) -> Option<String> {
        self.starter_named(state, record).map(|(handle, _)| handle)
    }

    /// [`Self::starter`] as prose names it, its name beside a differing address.
    fn starter_label(&self, state: &rimz::StatePaths, record: &RunRecord) -> Option<String> {
        self.starter_named(state, record)
            .map(|(handle, name)| rimz::address::handle_with_name(&handle, name.as_deref()))
    }

    fn starter_named(
        &self,
        state: &rimz::StatePaths,
        record: &RunRecord,
    ) -> Option<(String, Option<String>)> {
        let Some(records::RunStarter::Agent { kind, launch }) = &record.starter else {
            return None;
        };
        let peers = rimz::address::addressable_agents(self.snapshot(state)?);
        let agent = rimz::address::launch_row(peers.iter().copied(), kind, launch)?;
        Some((
            rimz::address::agent_handle(agent, &peers, false),
            agent.name.clone(),
        ))
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

/// Who started the run: its loop task, the agent by `handle` (its launch id once the agent is
/// out of the room), the run and node it was started from, or a person.
fn run_origin(record: &RunRecord, handle: Option<&str>) -> String {
    if let Some(task) = &record.task {
        return format!(
            "loop task {task}{}",
            if record.manual { ", fired by hand" } else { "" }
        );
    }
    match &record.starter {
        Some(records::RunStarter::Agent { launch, .. }) => {
            handle.map_or_else(|| format!("agent launch {launch}"), str::to_owned)
        }
        Some(records::RunStarter::Run { run, node }) => format!("run {run} at {node}"),
        None => "hand".into(),
    }
}

/// The run's starter as JSON: the record's own, with the agent's `handle` while it is in the room.
fn starter_json(record: &RunRecord, handle: Option<&str>) -> Option<serde_json::Value> {
    let mut starter = serde_json::to_value(record.starter.as_ref()?).ok()?;
    if let Some(handle) = handle {
        starter["handle"] = handle.into();
    }
    Some(starter)
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
    /// Start a workflow in the current checkout or a RimZ-owned worktree.
    Run {
        /// Workflow name, from `~/.rimz/workflows/` or an installed pack, or a directory.
        #[arg(value_name = "NAME|DIR")]
        name: PathBuf,
        /// Run in this RimZ-owned worktree instead of the current checkout, created from the
        /// configured base when missing. A workflow with `scope = "project"` refuses it.
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
        /// Start the run at this check or step instead of the definition's start.
        #[arg(long, value_name = "NODE", conflicts_with = "loop_fire_json")]
        at: Option<String>,
        /// A JSON object handed to the start node as its input: a string `outcome`, and a
        /// `key`, when given, that is a string or an integer.
        #[arg(
            long,
            value_name = "OBJECT",
            value_parser = start_input,
            conflicts_with = "loop_fire_json"
        )]
        input: Option<serde_json::Map<String, serde_json::Value>>,
        /// Run only the start node: end the run once that node has answered, naming the route the
        /// run would take, and wait for it here.
        #[arg(long, conflicts_with = "loop_fire_json")]
        once: bool,
        /// From an agent: tell it nothing when the run ends, and do not hold it open or show it
        /// waiting on the run; join the run with `rimz workflow wait` instead.
        #[arg(long, conflicts_with = "loop_fire_json")]
        detach: bool,
        /// The fire of a workflow loop task starting this run as the task's own.
        #[arg(long, hide = true, value_name = "JSON")]
        loop_fire_json: Option<String>,
    },
    /// List active and parked workflow runs across rooms; with --all, the runs that ended too.
    List {
        /// Print the active and parked runs as JSON, then with --all the finished ones.
        #[arg(long)]
        json: bool,
        /// Add the finished runs of workflows started by name, newest ending first.
        #[arg(long)]
        all: bool,
        /// Finished rows to print with --all, or finished records with --json.
        #[arg(short = 'n', long, default_value_t = 20, requires = "all")]
        limit: usize,
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
    /// Wait for runs to end, or for one to need a person.
    Wait {
        /// Run id or address `<workflow>#<checkout>`.
        #[arg(value_name = "RUN", required = true, num_args = 1..)]
        runs: Vec<String>,
        /// Return when the first run ends instead of all of them.
        #[arg(long)]
        any: bool,
        /// Give up after this long, exiting 124 and leaving the runs going. An agent's wait
        /// also exits 75 when a prompt for it is queued.
        #[arg(long, value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
        timeout: Option<std::time::Duration>,
        /// Print each run's outcome as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run and interrupt its live script.
    Cancel {
        /// Run address `<workflow>#<checkout>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        #[arg(long, hide = true, allow_hyphen_values = true, value_name = "TEXT")]
        reason: Option<String>,
    },
    /// Move a live run to a node, or re-enter a parked one.
    Resume {
        /// Run address `<workflow>#<checkout>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        /// Move the run to this node; a live attempt at its current node ends canceled.
        #[arg(long, value_name = "NODE")]
        at: Option<String>,
    },
    /// Grant a live run more visits to one step, or a new deadline; the run stays where it is.
    #[command(group(
        clap::ArgGroup::new("grant")
            .args(["visits", "deadline"])
            .required(true)
            .multiple(true)
    ))]
    Extend {
        /// Run address `<workflow>#<checkout>`, workflow name, or run id.
        #[arg(value_name = "RUN")]
        id: String,
        /// The step --visits raises; without it, the step the run is at.
        #[arg(value_name = "STEP", requires = "visits")]
        step: Option<String>,
        /// Add N visits to the step's `max-visits` for this run, written `+N`.
        #[arg(long, value_name = "+N", value_parser = parse_visits)]
        visits: Option<u64>,
        /// Set the run's deadline, re-arming one that fired; takes what `run --deadline` takes.
        #[arg(long, value_name = "WHEN", value_parser = parse_deadline)]
        deadline: Option<String>,
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
        /// Workflow directory to check. Without it, checks every workflow a name resolves to,
        /// in `~/.rimz/workflows/` and installed packs.
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Print rows, warnings, and errors as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the workflow specification's JSON Schema.
    Schema,
    /// Replay a workflow script over answers written by hand, starting nothing.
    Try {
        /// Workflow directory (its `workflow.js`), or one script file.
        #[arg(value_name = "DIR|FILE")]
        path: PathBuf,
        /// The script's global `args`, as JSON.
        #[arg(long, value_name = "JSON", allow_hyphen_values = true)]
        args: Option<String>,
        /// JSON Lines file whose line n answers call n: `{"result": ...}` or
        /// `{"failure": {"cause": "exit", ...}}`.
        #[arg(long, value_name = "FILE")]
        answers: Option<PathBuf>,
    },
    /// Print TypeScript declarations for the globals of a workflow script.
    Types,
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
            at,
            input,
            once,
            detach,
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
                at,
                input,
                once,
                starting: workflow::Starting::default(),
                detach,
                ..Default::default()
            };
            let task = loop_fire_json
                .as_deref()
                .map(serde_json::from_str::<workflow::TaskStart>)
                .transpose()
                .context("decoding --loop-fire-json")?;
            start(name, worktree, prompt, args, task, globals)
        }
        Command::List { json, all, limit } => list::run(json, all.then_some(limit), globals),
        Command::Show {
            id,
            node,
            json,
            history,
        } => show(id.as_deref(), node.as_deref(), json, history, globals),
        Command::Wait {
            runs,
            any,
            timeout,
            json,
        } => wait::run(&runs, any, timeout, json, globals),
        Command::Cancel { id, reason } => receipt::cancel(&id, reason.as_deref(), globals),
        Command::Resume { id, at } => receipt::resume(&id, at.as_deref(), globals),
        Command::Extend {
            id,
            step,
            visits,
            deadline,
        } => receipt::extend(&id, step.as_deref(), visits, deadline.as_deref(), globals),
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
            let advanced = workflow::advance(&state, &id, &host::new(&state, globals, woken_by));
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
                &host::new(state, globals, None),
            )?)
        }
        Command::Advance { .. } => unreachable!("clap requires a run id or wake signal"),
        Command::Schema => render::json_pretty(&workflow::schema()),
        Command::Try {
            path,
            args,
            answers,
        } => replay::run(&path, args.as_deref(), answers.as_deref()),
        Command::Types => Ok(std::io::stdout()
            .lock()
            .write_all(rimz::harness::block::typescript().as_bytes())?),
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
    if !record.state.is_terminal() || record.owes_report() {
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

/// `--visits +N`: the plus says the visits add to the step's cap, and N is ASCII digits worth at
/// least 1.
fn parse_visits(raw: &str) -> Result<u64, String> {
    raw.strip_prefix('+')
        .filter(|count| !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|count| count.parse::<u64>().ok())
        .filter(|count| *count > 0)
        .ok_or_else(|| {
            format!(
                "{raw:?} is not a visit grant; give +N, the visits to add to the step's \
                 max-visits, with N at least 1"
            )
        })
}

/// `extend --deadline`, checked as `run --deadline` is and kept as written for the commands a
/// refusal prints.
fn parse_deadline(raw: &str) -> Result<String, String> {
    workflow::parse_when(raw).map(|_| raw.to_owned())
}

/// A `run --input` value: one JSON object with a string `outcome`, and a `key`, when given,
/// that a run's key decodes (a string or an integer).
fn start_input(raw: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let object = answer_object(raw)?;
    workflow::check_start_input(&object)?;
    Ok(object)
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
    let owned = task.is_some();
    let once = args.once;
    let origin = match task {
        Some(task) => StartOrigin::Task(task),
        None => StartOrigin::Here(workflow_start::started_here(&ctx.store)),
    };
    let machine = super::machine_config();
    let started = workflow_start::start(
        WorkflowStart {
            name: &name,
            dir,
            worktree: worktree.as_deref(),
            worktree_arg: "-w",
            prompt,
            args,
            origin,
            workspace: &ctx.workspace,
            store: &ctx.store,
            machine: &machine,
        },
        &mut super::launch_interaction::CliInteraction,
    );
    let (record, advance) = match started {
        Ok(Some(created)) => created,
        Ok(None) => return Ok(()),
        Err(error) => {
            if owned && let Some(workflow::WorkflowErr::Exists { run, .. }) = error.downcast_ref() {
                writeln!(render::out(), "{run}")?;
                return Err(super::ExitCodeError {
                    code: workflow::TaskStart::OVERLAP_EXIT_CODE,
                    message: error.to_string(),
                }
                .into());
            }
            return Err(error);
        }
    };
    writeln!(render::out(), "{}", record.id)?;
    let paths = ctx.store.paths().workflow(&record.id);
    // A task's fire returns once its run is published; the run's report says how it went.
    let passed = if owned {
        None
    } else {
        receipt::first_pass(ctx.store.paths(), &record.id, Some(&advance))?
    };
    #[cfg(feature = "testkit")]
    rimz::testkit::rendezvous("RIMZ_TEST_WORKFLOW_BEFORE_WORK_DIR");
    // A run that ended in its first pass has its home moved to the finished directory.
    let home_is_live = paths.work.is_dir();
    let work = if home_is_live {
        paths.work.clone()
    } else {
        ctx.store.paths().finished_workflow(&record.id).work
    };
    // A try that has ended or left its live home names its finished work dir in its closing view.
    let tried = once
        && passed
            .as_ref()
            .is_some_and(|run| run.state.is_terminal() || !home_is_live);
    if !tried {
        writeln!(render::out(), "{}", work.display())?;
    }
    writeln!(
        render::out(),
        "→ {}",
        resolve::run_command(&record, "show", globals, None, None, &[])
    )?;
    match passed {
        Some(passed) if once => {
            receipt::write_waiting(&paths, &passed)?;
            receipt::finish_try(ctx.store.paths(), &record.id, globals)
        }
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
                resolve::select(run, "show", globals, None, None, &[])
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
        .filter(|definition| definition.is_task() || definition.is_path())
    {
        let name = definition.name();
        let dir = definition.dir().canonicalize().ok();
        let runs = resolve::all()?
            .into_iter()
            .filter(|run| {
                let record = &run.record;
                let of = if definition.is_task() {
                    record.task.as_deref() == Some(name)
                } else {
                    record
                        .spec_dir
                        .as_deref()
                        .and_then(|spec| spec.canonicalize().ok())
                        == dir
                };
                of && !record.state.is_terminal()
            })
            .collect::<Vec<_>>();
        if !json {
            definition::render(definition, globals)?;
        }
        return list::render(&runs, Some(name), json, globals);
    }
    let target = target.strip_prefix("workflow:").unwrap_or(target);
    match resolve::select(target, "show", globals, None, None, &[])? {
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
) -> Result<super::loop_launch::LoopLaunch> {
    let Command::Run {
        name,
        worktree,
        at,
        input,
        once,
        detach,
        loop_fire_json,
        ..
    } = &args.command
    else {
        bail!("the tail does not parse as `rimz workflow run`");
    };
    if loop_fire_json.is_some() {
        bail!("--loop-fire-json is the fire's own; remove it from the tail");
    }
    if at.is_some() {
        bail!(
            "--at starts a run by hand at a node, and a task's fire runs from the workflow's \
             start; remove --at"
        );
    }
    if input.is_some() {
        bail!(
            "--input hands a run's start an input by hand, and a task's fire gives none; remove \
             --input"
        );
    }
    if *once {
        bail!(
            "--once tries one node by hand, and a task's fire runs the whole workflow; remove \
             --once"
        );
    }
    if *detach {
        bail!("--detach drops the report a task's run owes it; remove --detach");
    }
    if !name.to_str().is_some_and(definition::one_name) {
        bail!(
            "`{}` is a path; --workflow takes a deployed workflow's name",
            name.display()
        );
    }
    if worktree.is_some() && place.each_worktree {
        bail!("-w in the tail places the run, and --each-worktree places each fire");
    }
    let described = super::loop_launch::LoopLaunch {
        worktree: worktree.clone(),
        ..Default::default()
    };
    let dir = workflow_view::deployed_definition(name, Some(place.project_root))?;
    if workflow::load(&dir)?.scope() != workflow::ScopeKind::Project {
        return Ok(described);
    }
    let remove = if place.each_worktree {
        "--each-worktree"
    } else if place.worktree.is_some() {
        "-w"
    } else if worktree.is_some() {
        "-w from the tail"
    } else {
        return Ok(described);
    };
    bail!(
        "{} has scope = project, so it runs at the project root; remove {remove}",
        name.display()
    )
}

fn validation_root(globals: &super::GlobalFlags) -> Option<PathBuf> {
    resolve::caller(globals).map(|workspace| workspace.project_root)
}

fn validate(dir: Option<PathBuf>, json: bool, globals: &super::GlobalFlags) -> Result<()> {
    let mut scratch = None;
    let mut report = Validation::default();
    let dirs = if let Some(dir) = dir {
        let named = dir
            .to_str()
            .map(|raw| definition::Definition::resolve(raw, globals))
            .transpose()?
            .flatten();
        let dir = match &named {
            Some(named) => named.dir().to_owned(),
            None => workflow_view::definition_dir(&dir)?.unwrap_or(dir),
        };
        scratch = named;
        vec![dir]
    } else {
        let mut dirs = Vec::new();
        for (name, found) in workflow_view::deployed()?.every() {
            match found {
                Ok(dir) => dirs.push(dir.to_owned()),
                Err(conflict) => report.errors.push(workflow::Issue {
                    path: "workflow".into(),
                    message: format!("workflow: {}: {}", one_line(name), one_line(conflict)),
                }),
            }
        }
        dirs
    };
    let loaded = workflow_view::definitions();
    let project_root = validation_root(globals);
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
                render::cell(one_line(&row.workflow)),
                render::cell(one_line(&row.node)),
                render::cell(&row.kind),
                render::cell(one_line(match row.runner.as_str() {
                    "condition" => "condition",
                    "sleep" => "sleep",
                    "run" => row.run.as_deref().unwrap_or("—"),
                    "shell" => row.shell.as_deref().unwrap_or("—"),
                    "message" => "message",
                    "workflow" => "workflow",
                    _ => row.agent.as_deref().unwrap_or("—"),
                })),
                render::cell(if row.agent.is_none() {
                    "—"
                } else if row.do_background {
                    "yes"
                } else {
                    "no"
                }),
                render::cell(one_line(
                    &row.edges
                        .iter()
                        .map(|edge| {
                            let mark = edge
                                .mark()
                                .map_or(String::new(), |mark| format!(", {mark}"));
                            format!("{} → {} ({}{mark})", edge.outcome, edge.target, edge.source)
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                )),
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
