//! The `runs` step: the runs it starts, each in a fresh worktree cut from the run's HEAD commit,
//! and its join, which waits until every started run's ending is stored on the starting run's
//! record by that run's report.

use super::super::spec::RunEntry;
use super::super::state::{ended_well, ending_word};
use super::*;
use crate::config::{MachineConfig, WorktreeBase};
use crate::store::workflow::{
    JoinedEnding, RunClass, RunEnding, RunHeader, RunStarter, StartedRun, StartedRuns,
};

/// One reserved run per entry of the step, in its order, to be cut from `base`, the run's HEAD
/// commit; a checkout without one reserves an empty base, which the start refuses.
pub(super) fn reserve(entries: &[RunEntry], base: Option<String>) -> StartedRuns {
    StartedRuns {
        base: base.unwrap_or_default(),
        runs: entries
            .iter()
            .map(|entry| StartedRun {
                id: WorkflowRunId::new(),
                workflow: entry.workflow.clone(),
                ended: None,
            })
            .collect(),
    }
}

/// The join of the live `runs` attempt, under the lock. Once every started run's ending is
/// stored, the step follows `then`, or fails `died` naming each run that did not end done. While
/// an advance that is not this one still starts the runs, this one leaves them to it; when the
/// advance that started them died with a run never created, this one takes the start over, and
/// the reserved ids keep a run from starting twice. Otherwise the step waits.
pub(super) fn prepare(
    state: &StatePaths,
    t: &mut Transition<'_>,
    guard: &Commit<'_>,
) -> Result<Decision> {
    let attempt = t
        .record
        .attempt
        .as_ref()
        .expect("a runs step's live attempt");
    let runs = attempt.runs.clone().unwrap_or(StartedRuns {
        base: String::new(),
        runs: Vec::new(),
    });
    if joined(attempt) {
        return join(t, guard, runs.runs);
    }
    let ours = attempt.host == agent::identity();
    if !ours && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref()) {
        return Ok(Decision::Stop);
    }
    let mut missing = false;
    for run in &runs.runs {
        missing |= run.ended.is_none() && workflow::find(state, &run.id)?.is_none();
    }
    if missing && !ours {
        t.record
            .attempt
            .as_mut()
            .expect("a runs step's live attempt")
            .host = agent::identity();
        persist(t, guard)?;
        return Ok(Decision::Runs(Box::new(t.record.clone())));
    }
    let ended = runs.runs.iter().filter(|run| run.ended.is_some()).count();
    t.step_wait(
        "runs",
        Some(format!("{ended} of {} started runs ended", runs.runs.len())),
    );
    persist(t, guard)?;
    Ok(Decision::Stop)
}

/// Whether every run the `runs` attempt reserved has its ending stored.
pub(super) fn joined(attempt: &LiveAttempt) -> bool {
    attempt
        .runs
        .as_ref()
        .is_none_or(|runs| runs.runs.iter().all(|run| run.ended.is_some()))
}

/// Settle the step once every started run ended: `then` when each ended done, else `died`
/// with a reason naming each one that did not. The record keeps the runs for the next step.
fn join(t: &mut Transition<'_>, guard: &Commit<'_>, runs: Vec<StartedRun>) -> Result<Decision> {
    let failed = runs
        .iter()
        .filter_map(|run| run.ended.as_ref().map(|ended| (run, ended)))
        .filter(|(_, ended)| !ended_well(&ended.ending))
        .map(|(run, ended)| {
            let why = ended
                .reason
                .as_ref()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default();
            format!(
                "started run {} ({}) ended {}{why}",
                run.id,
                run.workflow,
                ending_word(&ended.ending)
            )
        })
        .collect::<Vec<_>>();
    let outcome = if failed.is_empty() {
        AttemptOutcome::Ended
    } else {
        AttemptOutcome::Died
    };
    t.record.joined = runs;
    t.finish_script(
        Ending {
            outcome,
            cause: EndCause::Lifecycle,
            route: None,
            reason: None,
        },
        None,
        None,
        (!failed.is_empty()).then(|| failed.join("; ")),
    )?;
    persist(t, guard)?;
    Ok(if t.record.state == RunState::Active {
        Decision::Again
    } else {
        Decision::Stop
    })
}

/// Start each reserved run that has none yet, off the lock. A start that refuses, or a run that
/// cannot be created, fails the step `died` once the run is reread under the lock, as a command
/// step settles; runs already started keep running. Returns whether the advance goes on.
pub(super) fn start(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    record: &RunRecord,
    reader: &Reader,
    host: &dyn AttemptHost,
) -> Result<bool> {
    let machine = MachineConfig::load_lenient();
    let at = Attempt {
        state,
        paths,
        spec,
        record,
        reader,
        host,
    };
    match start_runs(&at, &machine)? {
        Start::Done => Ok(true),
        Start::Left(go_on) => Ok(go_on),
        // A failure always settles the attempt it is still current at.
        Start::Refused(reason) => Ok(at.recheck(Some(reason))?.unwrap_or(false)),
    }
}

/// The live `runs` attempt an advance starts runs for, off the lock.
struct Attempt<'a> {
    state: &'a StatePaths,
    paths: &'a WorkflowPaths,
    spec: &'a Definition,
    record: &'a RunRecord,
    reader: &'a Reader,
    host: &'a dyn AttemptHost,
}

impl Attempt<'_> {
    /// Read the run again under the lock, against its definition reread now. While the attempt
    /// is still the run's live one, no misfit ends it and no bound has passed, returns `None`,
    /// or settles `failure` as `died`; otherwise whether the advance goes on, after closing a
    /// misfit or settling the bound that passed. A run another settler moved is left to it.
    fn recheck(&self, failure: Option<String>) -> Result<Option<bool>> {
        let fresh = self.reader.reread(self.paths, self.record);
        let spec = fresh.as_ref().unwrap_or(self.spec);
        let guard = Commit::acquire(self.state, Some(self.host))?;
        let Some(current) = reload(self.paths)? else {
            return Ok(Some(false));
        };
        let mut t = Transition {
            record: current,
            spec,
            paths: self.paths,
            now: Timestamp::now(),
            entries: Vec::new(),
        };
        let seq = self.record.attempt.as_ref().map(|attempt| attempt.seq);
        if !seq.is_some_and(|seq| t.current(seq)) {
            return Ok(Some(false));
        }
        if let Some(reason) = fit::misfit(spec, &t.record) {
            let closing = fit::Closing::Fail(reason);
            fit::close(self.state, self.paths, t, guard, self.host, closing)?;
            return Ok(Some(false));
        }
        if !t.bound()? {
            let Some(reason) = failure else {
                return Ok(None);
            };
            t.finish_script(
                Ending {
                    outcome: AttemptOutcome::Died,
                    cause: EndCause::Lifecycle,
                    route: None,
                    reason: None,
                },
                None,
                None,
                Some(reason),
            )?;
        }
        persist(&mut t, &guard)?;
        Ok(Some(
            t.record.state == RunState::Active && t.record.wait.is_none(),
        ))
    }
}

/// How a start ended: every pending run started, the attempt left to the settler that moved
/// it (with whether the advance goes on), or a refusal that fails the step `died`.
enum Start {
    Done,
    Left(bool),
    Refused(String),
}

fn start_runs(at: &Attempt<'_>, machine: &MachineConfig) -> Result<Start> {
    let record = at.record;
    let attempt = record.attempt.as_ref().expect("a runs step's live attempt");
    let runs = attempt
        .runs
        .as_ref()
        .expect("a runs attempt reserves its runs");
    if let Err(error) = machine.require_readable_core("create a worktree") {
        return Ok(Start::Refused(error.to_string()));
    }
    // The misfit check ends a run whose step is no longer a runs step before it gets here.
    let (Some(step), Some(Kind::Runs(entries))) =
        (at.spec.step(&record.node), at.spec.step_kind(&record.node))
    else {
        return Ok(Start::Refused(format!(
            "{} is no longer a runs step",
            record.node
        )));
    };
    let Some(paired) = pair(&runs.runs, entries) else {
        let reserved = runs
            .runs
            .iter()
            .map(|run| run.workflow.as_str())
            .collect::<Vec<_>>();
        return Ok(Start::Refused(format!(
            "{} no longer lists the runs it reserved ({})",
            record.node,
            reserved.join(", ")
        )));
    };
    let mut pending = Vec::new();
    for (run, entry) in paired {
        if run.ended.is_some() {
            continue;
        }
        match workflow::find(at.state, &run.id) {
            Ok(Some(_)) => {}
            Ok(None) => pending.push((run, entry)),
            Err(error) => return Ok(Start::Refused(error.to_string())),
        }
    }
    let starting = Starting {
        starter: Some(RunStarter::Run {
            run: record.id.clone(),
            node: record.node.clone(),
        }),
        depth: record.depth + 1,
    };
    let first = pending.len() == runs.runs.len();
    let listed = pending.iter().map(|(_, entry)| *entry).collect::<Vec<_>>();
    let dirs = match refuse(record, &runs.base, &starting, &listed, first, machine) {
        Ok(dirs) => dirs,
        Err(reason) => return Ok(Start::Refused(reason)),
    };
    let mut accounted = !first;
    for ((run, entry), dir) in pending.into_iter().zip(dirs) {
        if let Some(go_on) = at.recheck(None)? {
            return Ok(Start::Left(go_on));
        }
        let prompt = match compose(record, at.paths, step, entry) {
            Ok(prompt) => prompt,
            Err(error) => return Ok(Start::Refused(error.to_string())),
        };
        let created = match crate::worktree::create(
            &record.project_root,
            &machine.agents.worktree,
            None,
            Some(WorktreeBase::Explicit(runs.base.clone())),
            None,
            false,
            crate::worktree::LaunchBase::Configured,
        ) {
            Ok(created) => created,
            Err(error) => {
                return Ok(Start::Refused(format!(
                    "{}: its worktree could not be created: {error}",
                    entry.workflow
                )));
            }
        };
        #[cfg(any(test, feature = "testkit"))]
        crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_RUNS_GATE");
        if let Some(go_on) = at.recheck(None)? {
            discard(record, &created.marker, machine);
            return Ok(Start::Left(go_on));
        }
        announce(at.state, record, &created.marker);
        // `create` publishes under a commit lock of its own, so a cancel or a bound can still
        // land between the recheck above and the publish: the run then starts after the step
        // left the attempt, and its report finds no attempt to join.
        let published = create(
            at.state,
            DefinitionSource::Deployed {
                dir: &dir,
                task: None,
                loop_fire: None,
                fire: None,
                fire_facts: None,
            },
            ScopeRoots {
                checkout: created.marker.worktree_path.clone(),
                project: record.project_root.clone(),
            },
            || Ok((!prompt.is_empty()).then_some(prompt)),
            false,
            StartArgs {
                id: Some(run.id.clone()),
                join: true,
                starting: starting.clone(),
                ..StartArgs::default()
            },
        );
        if let Err(error) = published {
            return Ok(Start::Refused(format!(
                "{} could not start: {error}",
                entry.workflow
            )));
        }
        if !accounted {
            accounting::step(record, None);
            accounted = true;
        }
    }
    Ok(Start::Done)
}

/// Each reserved run beside the live `start` entry it was reserved for: the first unpaired
/// entry naming the run's stored workflow, in order. None when the list and the reservation
/// no longer pair one to one.
fn pair<'a>(
    runs: &'a [StartedRun],
    entries: &'a [RunEntry],
) -> Option<Vec<(&'a StartedRun, &'a RunEntry)>> {
    if runs.len() != entries.len() {
        return None;
    }
    let mut taken = vec![false; entries.len()];
    runs.iter()
        .map(|run| {
            let index = (0..entries.len())
                .find(|&index| !taken[index] && entries[index].workflow == run.workflow)?;
            taken[index] = true;
            Some((run, &entries[index]))
        })
        .collect()
}

/// Announce a worktree the step created as `worktree.created` in the run's room, as every
/// other creator does; a room whose store cannot open hears nothing.
fn announce(state: &StatePaths, record: &RunRecord, marker: &crate::worktree::WorktreeMarker) {
    let store = crate::disk::paths::RuntimePaths::for_state(state)
        .ok()
        .and_then(|runtime| crate::Store::open_existing(state.clone(), runtime));
    let workspace = crate::workspace::WorkspaceResolver::resolve(&record.project_root, None).ok();
    let (Some(store), Some(workspace)) = (store, workspace) else {
        tracing::debug!(run = %record.id, "skipping worktree signal: store unavailable");
        return;
    };
    crate::worktree::emit_worktree_created(&store, &workspace, marker);
}

/// Remove the worktree made for a run the step then did not start. A removal that fails leaves
/// it in place, as a crash between the creation and the publish does.
fn discard(record: &RunRecord, marker: &crate::worktree::WorktreeMarker, machine: &MachineConfig) {
    let memo = crate::worktree::LandedMemo::load(&record.project_root);
    if let Err(error) = crate::worktree::remove_marked_worktree(
        &record.project_root,
        &marker.worktree_path,
        marker,
        true,
        &machine.agents.worktree.hooks,
        &memo,
    ) {
        tracing::warn!(
            run = %record.id,
            worktree = %marker.worktree_path.display(),
            %error,
            "an unstarted run's worktree could not be removed"
        );
    }
}

/// Each of `entries`, the reserved runs not yet created, with the directory its deployed name
/// leads to, or why the step starts none of them, checked before any of them exists: the run's
/// checkout has no commit; the started runs would sit past `[agents] max-workflow-depth`; or a
/// listed workflow is no deployed name, is not admitted, or has `scope = "project"`, which gets
/// no worktree of its own. Only a `first`
/// start, before any run of the attempt exists, refuses a checkout with uncommitted changes,
/// untracked files included, which the started runs' worktrees would not see: a takeover
/// finishes cutting runs from the commit its first start already took.
fn refuse(
    record: &RunRecord,
    base: &str,
    starting: &Starting,
    entries: &[&RunEntry],
    first: bool,
    machine: &MachineConfig,
) -> std::result::Result<Vec<std::path::PathBuf>, String> {
    let checkout = &record.scope.path;
    if base.is_empty() {
        return Err(format!(
            "the run's checkout {} has no commit to start runs from",
            checkout.display()
        ));
    }
    if first {
        clean(checkout)?;
    }
    starting
        .admit(machine.agents.max_workflow_depth)
        .map_err(|error| error.to_string())?;
    let definitions = agent_definitions();
    let deployed = super::super::Deployed::load(&crate::disk::paths::agents_home())
        .map_err(|error| format!("the deployed workflows could not be read: {error}"))?;
    let mut dirs = Vec::new();
    for entry in entries {
        let workflow = &entry.workflow;
        let dir = match deployed.find(workflow) {
            Ok(Some(dir)) if crate::disk::paths::workflow_spec_file(dir).is_file() => dir,
            Ok(_) => return Err(format!("{workflow} is not a deployed workflow")),
            Err(conflict) => {
                return Err(format!("{workflow} is not a deployed workflow: {conflict}"));
            }
        };
        let spec =
            super::super::validate::admit(dir, workflow, &definitions, Some(&record.project_root))
                .map_err(|error| format!("{workflow} is not admitted: {error}"))?;
        if spec.scope() == ScopeKind::Project {
            return Err(format!(
                r#"{workflow} has scope = "project" and gets no worktree of its own"#
            ));
        }
        dirs.push(dir.to_owned());
    }
    Ok(dirs)
}

/// Refuse a checkout with uncommitted changes, untracked files included.
fn clean(checkout: &std::path::Path) -> std::result::Result<(), String> {
    let status = crate::proc::git_command(checkout)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("git status in {}: {error}", checkout.display()))?;
    if !status.status.success() {
        return Err(format!(
            "git status in {}: {}",
            checkout.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    if !status.stdout.is_empty() {
        return Err(format!(
            "the run's checkout {} has uncommitted changes, which the started runs' worktrees \
             would not see; commit them first",
            checkout.display()
        ));
    }
    Ok(())
}

/// A started run's prompt: the step's prompt file, the entry's, the step's sign for this run,
/// then the run's own prompt, each one present in that order. The fire's evidence stays out.
fn compose(
    record: &RunRecord,
    paths: &WorkflowPaths,
    step: &Node,
    entry: &RunEntry,
) -> Result<String> {
    let dir = record.definition_dir(paths);
    let mut parts = Vec::new();
    for file in step
        .prompt_file()
        .into_iter()
        .chain(entry.prompt_file.as_deref())
    {
        parts.push(std::fs::read_to_string(spec::confined_file(&dir, file)?)?);
    }
    parts.extend(agent::sign(record, paths)?);
    parts.extend(record.prompt.clone());
    Ok(parts.join(
        "

",
    ))
}

/// Store how the finished run `child` ended on the run whose `runs` step started it, under the
/// workspace lock the caller holds, and wake that run with an advance, which waits for the lock.
/// Only a run still active at the attempt that reserved `child`, with no ending stored for it
/// yet, takes the ending, so a second report, or one that arrives after the parent left the
/// attempt, changes nothing. A wake that cannot spawn leaves the parent to the tick's advance.
pub fn join_report(
    state: &StatePaths,
    child: &WorkflowRunId,
    header: &RunHeader,
    guard: &WorkspaceLock,
) -> Result<()> {
    let Some(RunStarter::Run { run: parent, .. }) = &header.starter else {
        return Ok(());
    };
    let paths = state.workflow(parent);
    let RunClass::Current(mut record) = workflow::classify(&paths)? else {
        return Ok(());
    };
    if record.state != RunState::Active {
        return Ok(());
    }
    let Some(started) = record
        .attempt
        .as_mut()
        .filter(|attempt| attempt.kind == AttemptKind::Workflow)
        .and_then(|attempt| attempt.runs.as_mut())
        .and_then(|runs| runs.runs.iter_mut().find(|run| &run.id == child))
        .filter(|run| run.ended.is_none())
    else {
        return Ok(());
    };
    started.ended = Some(ended(state, header));
    workflow::write(&paths, &record, guard)?;
    if let Err(error) = super::spawn(&record) {
        tracing::warn!(run = %record.id, %error, "joined run's advance could not be spawned");
    }
    Ok(())
}

/// The child's ending as its record's header names it, with where it worked and its notes.
fn ended(state: &StatePaths, header: &RunHeader) -> JoinedEnding {
    let ending = header.ending.clone().unwrap_or(match header.state {
        Some(RunState::Done) => RunEnding::Done,
        Some(RunState::Canceled) => RunEnding::Canceled,
        _ => RunEnding::Failed,
    });
    let reason = match &ending {
        RunEnding::CapacityGaveUp { reason, .. } => header.reason.clone().or(reason.clone()),
        _ => header.reason.clone(),
    };
    JoinedEnding {
        ending,
        reason,
        branch: header
            .scope_path
            .as_deref()
            .and_then(crate::worktree::current_branch),
        checkout: header.scope_path.clone(),
        notes: header
            .notes
            .as_ref()
            .map(|notes| state.notes_run(crate::disk::paths::Class::AutomationNotes, notes)),
    }
}
