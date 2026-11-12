use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;
use serde_json::json;

use super::definition::Parsed;
use super::{AttemptHost, Observation, Occupancy, ScopeRoots};
use super::{Definition, Kind, Node, SpecErr, Target, script, spec, state::Transition};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::harness::signal::{
    Signal, SignalResolution, default_signal_match_key, resolve_subscription,
};
use crate::harness::throttle::{self};
use crate::harness::when::{WhenExpr, WindowReadings};
use crate::ids::WorkflowRunId;
use crate::store::workflow::{
    self, AttemptKind, AttemptOutcome, EndCause, Ending, Host, JournalEntry, JournalEvent,
    LiveAttempt, ParkReason, RunFire, RunOrigin, RunRecord, RunState, Scope, ScopeKind, Verdict,
};

mod accounting;
mod until;

#[derive(Debug, thiserror::Error)]
pub enum WorkflowErr {
    #[error("workflow {workflow} already has run {run} for {}", scope.display())]
    Exists {
        run: WorkflowRunId,
        workflow: String,
        scope: std::path::PathBuf,
        task: Option<String>,
        kind: ScopeKind,
    },
    #[error(transparent)]
    Store(#[from] workflow::WorkflowStoreErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Spec(#[from] super::SpecErr),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Time(#[from] crate::utils::time::TimeInputErr),
    #[error(transparent)]
    Throttle(#[from] throttle::ThrottleError),
    #[error("{0}")]
    Refused(String),
    #[error("no room at {}; run `rimz start` there first", .0.display())]
    NoRoom(std::path::PathBuf),
}

type Result<T> = std::result::Result<T, WorkflowErr>;

mod agent;

/// Where a new run's definition comes from. A loop fire passes its task row's `surplus` and
/// `surplus-after` values as written, which the run's judges read in place of the live row, and
/// the `fire` its briefs name.
pub enum DefinitionSource<'a> {
    /// A deployed workflow directory; its basename names the run.
    Deployed {
        dir: &'a Path,
        task: Option<String>,
        surplus: Option<String>,
        surplus_after: Option<String>,
        fire: Option<RunFire>,
    },
    /// A loop task's compiled definition, sealed into the run's own root and named for the task.
    /// `fire_tree` marks a checkout that is the fire's own fresh worktree: overlap then keys on
    /// the project root, where the task fired. `capacity_until` is the fire's default capacity
    /// give-up, for an agent step that sets no `capacity-wait`.
    Inline {
        task: String,
        definition: InlineDefinition,
        fire_tree: bool,
        capacity_until: Option<Timestamp>,
        surplus: Option<String>,
        surplus_after: Option<String>,
        fire: Option<RunFire>,
    },
}

/// Definition text plus the files it names by relative path, written beside it.
pub struct InlineDefinition {
    pub text: String,
    pub assets: BTreeMap<String, Vec<u8>>,
}

pub fn create(
    state: &StatePaths,
    source: DefinitionSource<'_>,
    roots: ScopeRoots,
    prompt: impl FnOnce() -> std::result::Result<Option<String>, String>,
    manual: bool,
) -> Result<RunRecord> {
    let roots = ScopeRoots {
        checkout: roots.checkout.canonicalize()?,
        project: roots.project.canonicalize()?,
    };
    let start = match &source {
        DefinitionSource::Deployed {
            dir,
            task,
            surplus,
            surplus_after,
            fire,
        } => {
            let dir = dir.canonicalize()?;
            RunStart {
                origin: RunOrigin::Deployed,
                name: dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| WorkflowErr::Refused("workflow directory needs a name".into()))?
                    .to_owned(),
                task: task.clone(),
                spec_dir: dir,
                fire_tree: false,
                capacity_until: None,
                surplus: surplus.clone(),
                surplus_after: surplus_after.clone(),
                fire: fire.clone(),
            }
        }
        DefinitionSource::Inline {
            task,
            fire_tree,
            capacity_until,
            surplus,
            surplus_after,
            fire,
            ..
        } => RunStart {
            origin: RunOrigin::Inline,
            name: task.clone(),
            task: Some(task.clone()),
            spec_dir: std::path::PathBuf::new(),
            fire_tree: *fire_tree,
            capacity_until: *capacity_until,
            surplus: surplus.clone(),
            surplus_after: surplus_after.clone(),
            fire: fire.clone(),
        },
    };
    // A run already holding the scope wins before the definition or the
    // prompt is read, so an edit to either mid-run reads as a skip.
    refuse_existing(
        state,
        start.origin,
        &start.name,
        start.task.as_deref(),
        start.fire_tree,
        &roots,
    )?;
    let prompt = prompt().map_err(WorkflowErr::Refused)?;
    let id = WorkflowRunId::new();
    let record = match source {
        DefinitionSource::Deployed { .. } => publish(state, id, start, roots, prompt, manual)?,
        DefinitionSource::Inline { definition, .. } => {
            let paths = state.workflow(&id);
            let published =
                workflow::seal_definition(&paths.definition, &definition.text, &definition.assets)
                    .map_err(WorkflowErr::from)
                    .and_then(|()| Ok(paths.definition.canonicalize()?))
                    .and_then(|spec_dir| {
                        publish(
                            state,
                            id,
                            RunStart { spec_dir, ..start },
                            roots,
                            prompt,
                            manual,
                        )
                    });
            if published.is_err() {
                // Nothing was published, so the sealed root has no record to keep it.
                let _ = std::fs::remove_dir_all(&paths.home);
            }
            published?
        }
    };
    spawn(&record)?;
    Ok(record)
}

/// A run's identity before its record exists.
struct RunStart {
    origin: RunOrigin,
    name: String,
    task: Option<String>,
    spec_dir: std::path::PathBuf,
    fire_tree: bool,
    capacity_until: Option<Timestamp>,
    surplus: Option<String>,
    surplus_after: Option<String>,
    fire: Option<RunFire>,
}

/// Validate the definition at `start.spec_dir` and publish the run's record.
fn publish(
    state: &StatePaths,
    id: WorkflowRunId,
    start: RunStart,
    roots: ScopeRoots,
    prompt: Option<String>,
    manual: bool,
) -> Result<RunRecord> {
    let bytes = std::fs::read(crate::disk::paths::workflow_spec_file(&start.spec_dir))?;
    let spec =
        spec::parse(std::str::from_utf8(&bytes).map_err(|e| WorkflowErr::Refused(e.to_string()))?)?;
    let machine = crate::config::MachineConfig::load_lenient();
    let definitions = crate::config::definitions::load(
        &crate::disk::paths::agents_home(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &crate::config::MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact,
    );
    let report = super::validate::validate_spec(
        &start.spec_dir,
        &start.name,
        &spec,
        &definitions,
        Some(&roots.project),
    );
    if !report.errors.is_empty() {
        return Err(WorkflowErr::Refused(
            report
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    let scope = Scope {
        kind: spec.scope(),
        path: roots.path(spec.scope()).to_path_buf(),
    };
    // An agent or a message needs the room, and so does a loop task's run, whose advance finds
    // it through the room's record; a hand-started run of checks and scripts runs without one.
    let in_room = spec
        .steps()
        .any(|(_, step)| matches!(step.kind(), Some(Kind::Agent(_) | Kind::Message(_))))
        || spec
            .checks()
            .any(|(_, check)| matches!(check.kind(), Some(Kind::Judge(_))));
    let room = crate::workspace::record::read(&state.workspace_record).is_ok();
    // A deployed run in a room keeps its notes and history; a loop task's run of checks fires too
    // often to keep a folder each time.
    let keeps_notes = in_room || (start.origin == RunOrigin::Deployed && room);
    if (in_room || start.task.is_some()) && !room {
        return Err(WorkflowErr::NoRoom(roots.project.clone()));
    }
    let now = Timestamp::now();
    let mut record = RunRecord {
        version: workflow::VERSION,
        break_agents: Vec::new(),
        id,
        origin: start.origin,
        workflow: start.name,
        loop_report: if start.task.is_some() {
            workflow::LoopReport::Owed
        } else {
            workflow::LoopReport::None
        },
        task: start.task,
        manual,
        spec_dir: start.spec_dir,
        spec_digest: script::digest(&bytes),
        prompt,
        scope,
        fire_tree: start.fire_tree,
        capacity_until: start.capacity_until,
        surplus: start.surplus,
        surplus_after: start.surplus_after,
        fire: start.fire,
        project_root: roots.project.clone(),
        created_at: now,
        notes: None,
        state: RunState::Active,
        park: None,
        node: spec.start().to_owned(),
        entered_at: now,
        input: None,
        key: None,
        recheck_at: None,
        wait: None,
        visits: BTreeMap::new(),
        visit_grant: 0,
        deadline: spec
            .deadline()
            .map(spec::parse_deadline)
            .transpose()
            .map_err(|e| WorkflowErr::Refused(e.to_string()))?,
        deadline_fired: false,
        next_seq: 1,
        attempt: None,
        held: None,
        restart: None,
        prev_attempt: None,
        terminal_at: None,
        outcome: None,
        check_retry: 0,
        check_retry_context: None,
        launch_retry: false,
    };
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    refuse_existing(
        state,
        record.origin,
        &record.workflow,
        record.task.as_deref(),
        record.fire_tree,
        &roots,
    )?;
    if keeps_notes {
        record.notes = Some(crate::store::notes::allocate(
            state,
            crate::store::notes::Kind::Workflow {
                workflow: record.workflow.clone(),
                run: record.id.clone(),
            },
            &record.scope.path,
            record.created_at,
        )?);
    }
    workflow::initialize(state, &record, &bytes, &guard)?;
    Ok(record)
}

/// A live run blocks a new one on the same scope when it shares the new run's origin and name,
/// or its task, so a task cannot dodge overlap by changing what it runs. A run in its loop fire's
/// own tree holds the project root it fired from, so each fire's fresh tree still overlaps.
fn refuse_existing(
    state: &StatePaths,
    origin: RunOrigin,
    name: &str,
    task: Option<&str>,
    fire_tree: bool,
    roots: &ScopeRoots,
) -> Result<()> {
    match workflow::list(state)?.into_iter().find(|r| {
        let checkout = if fire_tree {
            &roots.project
        } else {
            roots.path(r.scope.kind)
        };
        !r.state.is_terminal()
            && r.firing_checkout() == checkout
            && ((r.origin == origin && r.workflow == name)
                || task.is_some_and(|task| r.task.as_deref() == Some(task)))
    }) {
        Some(existing) => Err(WorkflowErr::Exists {
            run: existing.id,
            workflow: existing.workflow,
            scope: existing.scope.path,
            task: existing.task,
            kind: existing.scope.kind,
        }),
        None => Ok(()),
    }
}

fn spawn_args(record: &RunRecord) -> Vec<std::ffi::OsString> {
    super::advance_args(
        &record.project_root,
        Some(&record.id),
        None,
        crate::harness::hook_drain::frame_event_id().as_ref(),
    )
}

/// Name the applying hook frame so this advance does not wait on its own wake before the
/// drainer publishes the applied cursor. Advances outside a frame carry no ingress id.
fn spawn(record: &RunRecord) -> Result<()> {
    crate::child_process::spawn_detached_reaped(
        crate::proc::rimz_exe(),
        |command| {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;

                // The advance must survive closing the pane that woke it.
                command.process_group(0);
            }
            // A task's run carries neither its firer's lane nor its agent identity.
            if record.task.is_some() {
                command.env_remove(crate::workspace::ENV_CHANNEL);
                for (key, _) in std::env::vars_os()
                    .filter(|(key, _)| key.as_encoded_bytes().starts_with(b"RIMZ_AGENT_"))
                {
                    command.env_remove(key);
                }
            }
            command.args(spawn_args(record));
            Ok(())
        },
        "workflow advance",
    )?;
    Ok(())
}

fn persist(t: &mut Transition<'_>, guard: &WorkspaceLock) -> Result<()> {
    t.record.version = workflow::VERSION;
    if t.record.state != RunState::Active {
        t.record.restart = None;
    }
    if !t.record.state.is_terminal() {
        t.record.outcome = None;
    } else if t.record.outcome.is_none() {
        // The terminal write never waits on its outcome's inputs: without the journal the
        // outcome says what the record alone does, the ending its state names.
        let journal = match workflow::journal(t.paths) {
            Ok(mut journal) => {
                journal.extend(t.entries.iter().cloned());
                journal
            }
            Err(error) => {
                tracing::warn!(
                    path = %t.paths.journal.display(), %error,
                    "workflow journal unreadable; the run's outcome carries no cost or reason"
                );
                Vec::new()
            }
        };
        let acted = entered_act(t.spec, &t.record);
        t.record.outcome = Some(outcome(&t.record, &journal, acted));
    }
    for entry in &t.entries {
        workflow::append(t.paths, entry, guard)?;
    }
    workflow::write(t.paths, &t.record, guard)?;
    accounting::transition(t);
    Ok(())
}

/// How a terminal run ended, read from its whole journal: the decision that ended it, its summed
/// cost, and why a failed run's last attempt ended.
pub(super) fn outcome(
    record: &RunRecord,
    journal: &[JournalEntry],
    acted: bool,
) -> workflow::RunOutcome {
    let ending = match record.state {
        RunState::Done => workflow::RunEnding::Done,
        RunState::Canceled => workflow::RunEnding::Canceled,
        _ => failure(journal),
    };
    let mut costs = journal
        .iter()
        .filter_map(|entry| match entry.event {
            JournalEvent::Attempt { cost_usd, .. } | JournalEvent::Evaluation { cost_usd, .. } => {
                cost_usd
            }
            _ => None,
        })
        .peekable();
    let cost_usd = costs
        .peek()
        .is_some()
        .then(|| costs.fold(0.0, |sum, cost| sum + cost));
    let reason = matches!(
        ending,
        workflow::RunEnding::Failed | workflow::RunEnding::Budget { .. }
    )
    .then(|| {
        journal.iter().rev().find_map(|entry| match &entry.event {
            JournalEvent::Attempt { reason, .. } => reason.clone(),
            JournalEvent::Route { outcome, input } if outcome == "budget" => {
                input["reason"].as_str().map(str::to_owned)
            }
            _ => None,
        })
    })
    .flatten();
    workflow::RunOutcome {
        ending,
        cost_usd,
        acted,
        reason,
    }
}

/// Why a failed run failed: its last decision was the `capacity` route out of an agent step, its
/// last attempt ended at its budget, or neither.
fn failure(journal: &[JournalEntry]) -> workflow::RunEnding {
    let capacity = journal.iter().rev().find_map(|entry| match &entry.event {
        JournalEvent::Route { outcome, input } => Some((outcome == "capacity").then_some(input)),
        JournalEvent::Attempt { .. } => Some(None),
        _ => None,
    });
    if let Some(Some(input)) = capacity {
        return workflow::RunEnding::CapacityGaveUp {
            waited_ms: input["waited_ms"].as_u64().unwrap_or(0),
            reason: input["reason"].as_str().map(str::to_owned),
        };
    }
    let budget = journal.iter().rev().find_map(|entry| match &entry.event {
        JournalEvent::Attempt {
            outcome, handle, ..
        } => Some((*outcome == AttemptOutcome::Limited).then_some(handle.is_some())),
        JournalEvent::Route { outcome, .. } if outcome == "budget" => Some(Some(false)),
        _ => None,
    });
    match budget {
        Some(Some(turned)) => workflow::RunEnding::Budget { turned },
        _ => workflow::RunEnding::Failed,
    }
}

/// Whether the run entered an agent or message step, read against its sealed graph; a graph
/// that no longer reads counts any visited step.
fn acted(paths: &WorkflowPaths, record: &RunRecord) -> bool {
    match spec::load(&paths.home) {
        Ok(spec) => entered_act(&spec, record),
        Err(_) => !record.visits.is_empty(),
    }
}

pub(super) fn entered_act(spec: &Definition, record: &RunRecord) -> bool {
    record.visits.keys().any(|step| {
        matches!(
            spec.step_kind(step),
            Some(Kind::Agent(_) | Kind::Message(_))
        )
    })
}

fn window_wait(
    window: &Parsed<WhenExpr, SpecErr>,
    scope: &Path,
    provider: &crate::ids::AgentKind,
    account: Option<&crate::ids::LoginName>,
    windows: &WindowReadings<'_>,
) -> Result<Option<String>> {
    let expr = window.value().map_err(SpecErr::clone)?;
    let verdict =
        crate::harness::when::evaluate(expr, scope, None, Some(provider), account, windows);
    Ok((verdict.truth != Some(true))
        .then(|| expr.blocking_evidence(&verdict).replacen("window.", "", 1)))
}

/// The start groups an agent launch admits, by kind and account, which an attempt host's
/// `start_groups` resolves through. A step's `account` pins every cell, so availability routing
/// is skipped, as the launcher does for a pinned login.
pub fn launch_groups(
    record: &RunRecord,
    agent: &str,
    account: Option<&crate::ids::LoginName>,
) -> Result<Vec<throttle::StartGroup>> {
    let workspace = crate::workspace::WorkspaceResolver::resolve(
        &record.scope.path,
        Some(record.project_root.clone()),
    )
    .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let state = StatePaths::for_project_root(&record.project_root)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let runtime = crate::disk::paths::RuntimePaths::for_state(&state)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let resolved = (|| -> anyhow::Result<_> {
        let machine = crate::config::MachineConfig::load_lenient();
        let mut effective = crate::config::effective::load(&machine, &workspace.project_root)?;
        let availability = account.is_none().then(|| {
            crate::harness::plan::LaunchAvailability::read(
                &runtime,
                &state,
                &machine,
                Timestamp::now(),
            )
        });
        effective.route(
            &machine.tiers,
            crate::config::effective::ProfileScope::Agents,
            Some(agent),
            None,
            None,
            None,
            |kind, model| {
                availability
                    .as_ref()
                    .and_then(|usage| usage.unavailable(kind, model))
            },
        )?;
        Ok(crate::harness::plan::resolve_launch(
            &effective,
            crate::config::effective::ProfileScope::Agents,
            &machine.agents.commands,
            Some(agent),
            None,
        )?)
    })()
    .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let mut groups: Vec<throttle::StartGroup> = Vec::new();
    for cell in resolved.layout.agent_cells() {
        let requested = account.or(cell.launch.login.as_ref()).cloned().map_or(
            crate::store::writer::LaunchLogin::RoomDefault,
            crate::store::writer::LaunchLogin::Pinned,
        );
        let login = crate::harness::task_login::task_login(&requested, &cell.kind, &runtime)
            .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
        let account = login.as_ref().map(|login| login.name().clone());
        match groups
            .iter_mut()
            .find(|group| group.kind == cell.kind && group.account == account)
        {
            Some(group) => group.agents += 1,
            None => groups.push(throttle::StartGroup {
                kind: cell.kind.clone(),
                account,
                agents: 1,
            }),
        }
    }
    Ok(groups)
}

fn window_wait_for_launch(
    record: &RunRecord,
    agent: &str,
    window: &Parsed<WhenExpr, SpecErr>,
    windows: &WindowReadings<'_>,
    host: &dyn AttemptHost,
) -> Result<Option<String>> {
    window_wait_for_groups(
        record,
        &host.start_groups(record, agent, None)?,
        window,
        windows,
    )
}

fn window_wait_for_groups(
    record: &RunRecord,
    groups: &[throttle::StartGroup],
    window: &Parsed<WhenExpr, SpecErr>,
    windows: &WindowReadings<'_>,
) -> Result<Option<String>> {
    for group in groups {
        if let Some(detail) = window_wait(
            window,
            &record.scope.path,
            &group.kind,
            group.account.as_ref(),
            windows,
        )? {
            return Ok(Some(detail));
        }
    }
    Ok(None)
}

enum Decision {
    Stop,
    Again,
    Evaluate(Box<RunRecord>),
    Launch(Box<agent::Launch>),
    End(Box<RunRecord>),
    Push(Box<RunRecord>),
    Message(Box<Notice>),
    Admit(Box<(u64, String, throttle::Run)>),
}

#[derive(Default)]
struct Admission {
    capacity: Option<(
        u64,
        String,
        std::result::Result<throttle::Attempt, throttle::ThrottleError>,
    )>,
}

pub fn advance(state: &StatePaths, id: &WorkflowRunId, host: &dyn AttemptHost) -> Result<()> {
    let runtime = crate::disk::paths::RuntimePaths::for_state(state)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let paths = state.workflow(id);
    let record = workflow::load(&paths)?;
    if record.state.is_terminal() {
        return Ok(());
    }
    let spec = spec::load(&paths.home)?;
    let mut seen = BTreeSet::new();
    let mut admission = Admission::default();
    loop {
        let windows = WindowReadings::new(Some(&runtime), Timestamp::now());
        let decision = prepare(
            state,
            &paths,
            &spec,
            &mut seen,
            host,
            &windows,
            &mut admission,
        )?;
        let record = match decision {
            Decision::Stop => return Ok(()),
            Decision::Again => continue,
            Decision::Evaluate(record) => record,
            Decision::Admit(request) => {
                let (seq, node, run) = *request;
                let config = crate::config::MachineConfig::load_lenient();
                admission.capacity = Some((
                    seq,
                    node,
                    throttle::check_group_size(&config.throttle, &run)
                        .and_then(|()| throttle::try_admit(&config.throttle, &run)),
                ));
                #[cfg(feature = "testkit")]
                crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_ADMISSION_GATE");
                continue;
            }
            Decision::Launch(launch) => {
                if agent::launch(state, &paths, &spec, *launch, host)? {
                    continue;
                }
                return Ok(());
            }
            Decision::Push(record) => {
                if until::push(state, &paths, &spec, &record, host)? {
                    continue;
                }
                return Ok(());
            }
            Decision::End(record) => {
                if agent::finish(state, &paths, &spec, &record, host, None)? {
                    continue;
                }
                return Ok(());
            }
            Decision::Message(notice) => {
                if message_step(state, &paths, &spec, *notice, host)? {
                    continue;
                }
                return Ok(());
            }
        };
        // prepare publishes an attempt before returning Evaluate.
        let seq = record
            .attempt
            .as_ref()
            .expect("prepared evaluation has an attempt")
            .seq;
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_CONTEXT_GATE");
        if spec.step(&record.node).is_some() {
            if command_step(state, &paths, &spec, &record)? {
                continue;
            }
            return Ok(());
        }
        let result = script::evaluate(state, &paths, &spec, &record);
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_RESULT_GATE");
        let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
        let mut t = Transition {
            record: workflow::load(&paths)?,
            spec: &spec,
            paths: &paths,
            now: Timestamp::now(),
            entries: Vec::new(),
        };
        if !t.current(seq) {
            return Ok(());
        }
        let mut recheck_woken = false;
        if !t.bound()? {
            let woken = t
                .record
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.woken);
            let retry = if record
                .attempt
                .as_ref()
                .is_some_and(|a| a.kind == AttemptKind::Judge)
            {
                t.finish_judge(seq, result)?
            } else {
                t.finish_check(seq, result.object, result.stderr_tail)?;
                false
            };
            if retry {
                seen.remove(&record.node);
            }
            if woken && let Some(wait) = &mut t.record.wait {
                wait.recheck_at = None;
                t.record.recheck_at = None;
                seen.remove(&record.node);
                recheck_woken = true;
            }
        } else {
            for entry in &mut t.entries {
                if let JournalEvent::Evaluation { cost_usd, .. } = &mut entry.event {
                    *cost_usd = result.cost_usd;
                }
            }
        }
        persist(&mut t, &guard)?;
        if t.record.state != RunState::Active
            || (t.record.wait.is_some() && !recheck_woken && !until::has_pending_verdict(&t.record))
        {
            return Ok(());
        }
    }
}

fn command_step(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    record: &RunRecord,
) -> Result<bool> {
    let result = script::execute_step(state, paths, spec, record, || {
        accounting::step(record, None)
    });
    let head_end = super::head(&record.scope.path);
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_RESULT_GATE");
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(paths)?,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    // prepare publishes every attempt before command execution.
    let seq = record.attempt.as_ref().expect("prepared command step").seq;
    if !t.current(seq) {
        return Ok(false);
    }
    let (mut outcome, code, reason, interrupted) = match result {
        Ok(output) if !output.interrupted() => (
            if output.passed() {
                AttemptOutcome::Ended
            } else {
                AttemptOutcome::Died
            },
            output.code(),
            (!output.passed()).then(|| {
                output.code().map_or_else(
                    || "killed by signal".into(),
                    |code| format!("exited {code}"),
                )
            }),
            false,
        ),
        Ok(_) | Err(script::ChildError::Interrupted) => {
            (AttemptOutcome::Canceled, None, None, true)
        }
        Err(script::ChildError::Failed(reason)) => {
            (AttemptOutcome::Died, None, Some(reason), false)
        }
    };
    let cause = if !interrupted {
        EndCause::Lifecycle
    } else if !t.record.deadline_fired && t.record.deadline.is_some_and(|at| at <= t.now) {
        EndCause::Deadline
    } else if t.timeout_at()?.is_some_and(|at| at <= t.now) {
        outcome = AttemptOutcome::Timeout;
        EndCause::Timeout
    } else {
        EndCause::Lifecycle
    };
    t.finish_script(
        Ending {
            outcome,
            cause,
            route: None,
            reason: None,
        },
        code,
        head_end,
        reason,
    )?;
    persist(&mut t, &guard)?;
    Ok(t.record.state == RunState::Active)
}

/// A published message attempt, ready to queue its notice off-lock.
struct Notice {
    record: RunRecord,
    seq: u64,
    target: String,
    session: Option<crate::ids::AgentSessionId>,
    message: crate::ids::MessageId,
    text: std::result::Result<String, String>,
}

impl Notice {
    /// The send for the record's live message attempt, or `None` when it is not one.
    fn of(
        record: RunRecord,
        spec: &Definition,
        text: std::result::Result<String, String>,
    ) -> Option<Self> {
        let attempt = record.attempt.as_ref()?;
        let step = spec.step(&attempt.node)?;
        let Some(Kind::Message(target)) = step.kind() else {
            return None;
        };
        Some(Self {
            seq: attempt.seq,
            message: attempt.message.clone()?,
            target: target.into(),
            session: step.session().cloned(),
            record,
            text,
        })
    }

    /// The WAIT notice body: the run's line, then the step's composed prompt.
    fn text(
        record: &RunRecord,
        paths: &WorkflowPaths,
        step: &Node,
        digests: &mut BTreeMap<String, String>,
    ) -> std::result::Result<String, String> {
        let header = format!(
            "Workflow `{}` step `{}` sends you this message (run {id}; `rimz workflow show {id}`).",
            record.workflow,
            record.node,
            id = record.id,
        );
        let prompt =
            agent::compose(record, paths, step, digests).map_err(|error| error.to_string())?;
        Ok(if prompt.is_empty() {
            header
        } else {
            format!("{header}\n\n{prompt}")
        })
    }
}

/// The notice of a live message attempt whose host died before settling it: once another
/// ending settles the attempt, no host is left to withdraw it.
fn orphaned_notice(record: &RunRecord) -> Option<crate::ids::MessageId> {
    let attempt = record.attempt.as_ref()?;
    if crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref()) {
        return None;
    }
    attempt.message.clone()
}

/// Queue a message step's notice, then follow `then`, or `on.undelivered` when no one agent took it.
fn message_step(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    notice: Notice,
    host: &dyn AttemptHost,
) -> Result<bool> {
    let result = match notice.text {
        Ok(text) => host.message(
            &notice.record.scope.path,
            &notice.target,
            notice.session.as_ref(),
            &notice.message,
            &text,
        )?,
        Err(reason) => Err(reason),
    };
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_RESULT_GATE");
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(paths)?,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if !t.current(notice.seq) {
        // A lost host's retry delivers under the same id, so only another ending withdraws it.
        let delivered = workflow::journal(paths)?.iter().any(|entry| {
            entry.seq == Some(notice.seq)
                && matches!(
                    entry.event,
                    JournalEvent::Attempt {
                        outcome: AttemptOutcome::Ended,
                        ..
                    }
                )
        });
        drop(guard);
        if result.is_ok() && !delivered {
            host.retire(&notice.message)?;
        }
        return Ok(false);
    }
    let handle = result.as_ref().ok().cloned();
    match result {
        Ok(handle) => t.finish_message(AttemptOutcome::Ended, Some(handle), None)?,
        Err(reason) => t.finish_message(AttemptOutcome::Died, None, Some(reason))?,
    }
    persist(&mut t, &guard)?;
    accounting::step(&notice.record, handle);
    Ok(t.record.state == RunState::Active)
}

fn prepare(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    seen: &mut BTreeSet<String>,
    host: &dyn AttemptHost,
    windows: &WindowReadings<'_>,
    admission: &mut Admission,
) -> Result<Decision> {
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(paths)?,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if t.record.state != RunState::Active {
        return Ok(Decision::Stop);
    }
    if admission
        .capacity
        .as_ref()
        .is_some_and(|(seq, node, _)| *seq != t.record.next_seq || *node != t.record.node)
    {
        admission.capacity = None;
    }
    if t.record.held.is_some()
        && let Some(decision) = until::prepare(&mut t, host, &guard, seen)?
    {
        return Ok(decision);
    }
    if t.record
        .attempt
        .as_ref()
        .is_some_and(|a| a.kind == AttemptKind::Agent)
    {
        return agent::prepare_end(&mut t, host, &guard);
    }
    let orphan = orphaned_notice(&t.record);
    if t.bound()? {
        admission.capacity = None;
        persist(&mut t, &guard)?;
        drop(guard);
        if let Some(message) = orphan {
            host.retire(&message)?;
        }
        return Ok(
            if t.record.state != RunState::Active
                || (t.record.wait.is_some() && !until::has_pending_verdict(&t.record))
            {
                Decision::Stop
            } else {
                Decision::Again
            },
        );
    }
    if let Some(attempt) = &t.record.attempt {
        if crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref()) {
            return Ok(Decision::Stop);
        }
        if attempt.message.is_some()
            && let Some(step) = spec.step(&t.record.node)
        {
            // A lost message host re-sends under the same id, which the queue takes once.
            let mut digests = BTreeMap::new();
            let text = Notice::text(&t.record, paths, step, &mut digests);
            let attempt = t.record.attempt.as_mut().expect("live message attempt");
            attempt.host = agent::identity();
            attempt.digests = digests;
            persist(&mut t, &guard)?;
            return Ok(
                Notice::of(t.record, spec, text).map_or(Decision::Again, |notice| {
                    Decision::Message(Box::new(notice))
                }),
            );
        }
        let reason = format!("lost command-step host {}", attempt.host.pid);
        t.finish_script(
            Ending {
                outcome: AttemptOutcome::Canceled,
                cause: EndCause::Lifecycle,
                route: None,
                reason: None,
            },
            None,
            None,
            Some(reason),
        )?;
        persist(&mut t, &guard)?;
        return Ok(Decision::Again);
    }
    if t.record.recheck_at.is_some_and(|at| at > t.now)
        && !t.record.wait.as_ref().is_some_and(|wait| wait.is_hold())
    {
        if t.record.held.is_none()
            && let Some(wait) = &mut t.record.wait
        {
            wait.last = t.now;
            wait.count += 1;
        }
        persist(&mut t, &guard)?;
        return Ok(Decision::Stop);
    }
    let mut launch_error = None;
    let mut evict = None;
    if let Some(step) = spec.step(&t.record.node) {
        if let Some((bound, visits, max)) = step.tripped_visit_bound(&t.record) {
            if !seen.insert(t.record.node.clone()) {
                return Ok(Decision::Stop);
            }
            let route = t.cap_route(bound, visits, max);
            t.route(&Target::parse(&route.target), route.object, route.reason)?;
            admission.capacity = None;
            persist(&mut t, &guard)?;
            return Ok(Decision::Again);
        }
        if let Some(Kind::Agent(agent)) = spec.step_kind(&t.record.node) {
            let admits = spec.limit().is_some()
                || step.limit().is_some()
                || t.record.restart.is_some()
                || (t.record.task.is_some() && !t.record.manual);
            if admits
                && let Some((_, _, Err(reason))) = &admission.capacity
                && !queue_lock_timed_out(reason)
            {
                launch_error = Some(reason.to_string());
            }
            let groups = if admits || step.window().is_some() {
                match host.start_groups(&t.record, agent, step.account()) {
                    Ok(groups) => groups,
                    Err(error) => {
                        launch_error = Some(error.to_string());
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            if t.record.restart.is_none()
                && launch_error.is_none()
                && let Some(window) = step.window()
                && let Some(detail) = window_wait_for_groups(&t.record, &groups, window, windows)?
            {
                t.step_wait("window", Some(detail));
                persist(&mut t, &guard)?;
                return Ok(Decision::Stop);
            }
            if launch_error.is_none() && !step.share().unwrap_or_default() {
                // A step launched into a named worktree occupies that checkout, not the run's.
                let destination = destination(&t.record, step)?;
                let occupancy =
                    host.occupancy(&destination, &t.record.project_root, &t.record.break_agents)?;
                if let Some(wait) = occupancy_wait(&occupancy, step.takeover().unwrap_or_default())
                {
                    t.step_wait(wait, None);
                    persist(&mut t, &guard)?;
                    return Ok(Decision::Stop);
                }
                // An occupant can arrive while the launch is prepared, so the host reads again.
                evict = Some(super::Eviction {
                    checkout: destination,
                    project_root: t.record.project_root.clone(),
                    excluded: t.record.break_agents.clone(),
                });
            }
            t.now = Timestamp::now();
            if launch_error.is_none()
                && let Some(decision) = prepare_capacity(&mut t, step, groups, admission, &guard)?
            {
                return Ok(decision);
            }
        }
    } else if let Some(node) = spec.check(&t.record.node) {
        if let (Some(window), Some(Kind::Judge(profile))) = (node.window(), node.kind())
            && let Some(detail) = window_wait_for_launch(&t.record, profile, window, windows, host)?
        {
            t.step_wait("window", Some(detail));
            persist(&mut t, &guard)?;
            return Ok(Decision::Stop);
        }
        if node.window().is_some()
            && t.record
                .wait
                .as_ref()
                .is_some_and(|wait| wait.outcome == "window")
        {
            t.close_wait();
        }
        if !seen.insert(t.record.node.clone()) {
            return Ok(Decision::Stop);
        }
    } else {
        return Err(WorkflowErr::Refused(format!(
            "unknown sealed workflow node {}",
            t.record.node
        )));
    }
    let step = spec.step(&t.record.node);
    let step_kind = step.and_then(Node::kind);
    let agent_step = step.filter(|_| matches!(step_kind, Some(Kind::Agent(_))));
    let kind = if agent_step.is_some() {
        AttemptKind::Agent
    } else if step.is_some() {
        AttemptKind::Script
    } else {
        match spec.check_kind(&t.record.node) {
            Some(Kind::Judge(_)) => AttemptKind::Judge,
            Some(Kind::Sleep(_)) => AttemptKind::Script,
            _ => AttemptKind::Script,
        }
    };
    let mut digests = BTreeMap::new();
    let prompt = agent_step.map(|step| {
        agent::compose(&t.record, paths, step, &mut digests).map_err(|error| error.to_string())
    });
    let notice = step
        .filter(|_| matches!(step_kind, Some(Kind::Message(_))))
        .map(|step| Notice::text(&t.record, paths, step, &mut digests));
    let head_start = if step.is_none() && kind == AttemptKind::Script {
        None
    } else {
        super::head(&t.record.scope.path)
    };
    t.now = Timestamp::now();
    if t.bound()? {
        admission.capacity = None;
        persist(&mut t, &guard)?;
        return Ok(Decision::Again);
    }
    if step.is_some() {
        t.close_wait();
        seen.clear();
        let bucket = t.record.visit_bucket();
        let bucket = (bucket.0.map(str::to_owned), bucket.1.cloned());
        t.record
            .visits
            .entry(t.record.node.clone())
            .or_default()
            .bump(bucket);
        t.record.entered_at = t.now;
    }
    let seq = t.record.next_seq;
    t.record.next_seq += 1;
    if let Some(held) = &mut t.record.held {
        held.attempt.woken = false;
    }
    t.record.attempt = Some(LiveAttempt {
        kind,
        lifecycle: step.map_or(workflow::StepLifecycle::Turn, Node::lifecycle),
        seq,
        node: t.record.node.clone(),
        visit: step.map(|_| t.record.visits[&t.record.node].for_bucket(t.record.visit_bucket())),
        started_past_limit: admission
            .capacity
            .as_ref()
            .and_then(|(_, _, answer)| match answer {
                Ok(throttle::Attempt::Turn(turn)) => turn.started_past_limit().map(str::to_owned),
                _ => None,
            }),
        started_at: t.now,
        host: agent::identity(),
        woken: false,
        asking: None,
        held_since: None,
        asked_ms: 0,
        judged: None,
        push: None,
        message: notice.is_some().then(crate::ids::MessageId::new),
        child: None,
        run: None,
        head_start,
        digests,
        ending: None,
        retry: t.record.check_retry_context.clone(),
    });
    if let Some((_, _, Err(error @ throttle::ThrottleError::Oversized { .. }))) =
        &admission.capacity
    {
        t.stage_agent_end(
            Ending {
                outcome: AttemptOutcome::Died,
                cause: EndCause::Lifecycle,
                route: Some(Verdict {
                    node: t.record.node.clone(),
                    entered_at: t.record.entered_at,
                    at: t.now,
                    target: Target::Failed.to_string(),
                    object: json!({"outcome":"died", "reason":error.to_string()}),
                    reason: ParkReason::User,
                }),
                reason: Some(error.to_string()),
            },
            &agent::empty_observation(),
        );
    }
    persist(&mut t, &guard)?;
    write_context(&t.record, paths, spec)?;
    if let Some(text) = notice {
        return Ok(
            Notice::of(t.record, spec, text).map_or(Decision::Again, |notice| {
                Decision::Message(Box::new(notice))
            }),
        );
    }
    if let Some(mut prompt) = prompt {
        if let Some(reason) = launch_error {
            prompt = Err(reason);
        }
        let turn = match admission.capacity.take() {
            Some((_, _, Ok(throttle::Attempt::Turn(turn)))) => Some(turn),
            _ => None,
        };
        return Ok(Decision::Launch(Box::new((t.record, prompt, turn, evict))));
    }
    Ok(Decision::Evaluate(Box::new(t.record)))
}

fn prepare_capacity(
    t: &mut Transition<'_>,
    step: &Node,
    groups: Vec<throttle::StartGroup>,
    admission: &mut Admission,
    guard: &WorkspaceLock,
) -> Result<Option<Decision>> {
    let may_wait = t.record.task.is_some() && !t.record.manual && t.record.restart.is_none();
    let machine_gated = may_wait && step.throttle() != Some(crate::config::ThrottleSwitch::Off);
    let limits = (t.spec.limit().is_some() || step.limit().is_some() || t.record.restart.is_some())
        .then(|| throttle::WorkflowStart {
            workflow: t.record.workflow.clone(),
            step: t.record.node.clone(),
            run: t.record.id.clone(),
            arrival: t
                .record
                .wait
                .as_ref()
                .filter(|wait| wait.outcome == "capacity")
                .map_or(t.now, |wait| wait.first),
            workflow_limit: t.spec.limit().map(start_limit),
            step_limit: step.limit().map(start_limit),
            may_wait,
        });
    if !may_wait && limits.is_none() {
        admission.capacity = None;
        return Ok(None);
    }
    if let Some(wait) = t
        .record
        .wait
        .as_ref()
        .filter(|wait| may_wait && wait.outcome == "capacity")
        && wait.gives_up_at.is_some_and(|at| t.now >= at)
    {
        let object = json!({
            "outcome":"capacity", "step":t.record.node, "reason":wait.detail,
            "kinds":wait.kinds, "waited_ms":t.now.duration_since(wait.first).as_millis().max(0)
        });
        t.engine_route(
            "capacity",
            object,
            &t.spec.engine_target(&t.record.node, "capacity"),
            ParkReason::User,
        )?;
        admission.capacity = None;
        persist(t, guard)?;
        return Ok(Some(Decision::Again));
    }
    let Some((_, _, answer)) = &admission.capacity else {
        return Ok(Some(Decision::Admit(Box::new((
            t.record.next_seq,
            t.record.node.clone(),
            throttle::Run {
                machine_gated,
                limits,
                task: t.record.task.clone().unwrap_or_default(),
                root: t.record.project_root.clone(),
                checkout: t.record.scope.path.clone(),
                // A worktree not made yet is measured where it will be made.
                disk: destination(&t.record, step)
                    .ok()
                    .and_then(|dir| {
                        dir.ancestors()
                            .find(|dir| dir.exists())
                            .map(Path::to_path_buf)
                    })
                    .unwrap_or_else(|| t.record.scope.path.clone()),
                workspace: crate::ids::WorkspaceId::from_project_root(&t.record.project_root),
                groups,
                gives_up_ms: t
                    .record
                    .wait
                    .as_ref()
                    .filter(|_| may_wait)
                    .and_then(|wait| wait.gives_up_at)
                    .and_then(|at| u64::try_from(at.as_millisecond()).ok()),
            },
        )))));
    };
    let hold = match answer {
        Ok(throttle::Attempt::Held(hold)) => hold.clone(),
        // Admission could not answer; a standing capacity wait keeps what it last learned.
        Err(error) if queue_lock_timed_out(error) => {
            if t.record
                .wait
                .as_ref()
                .is_some_and(|wait| wait.outcome == "capacity")
            {
                return Ok(Some(Decision::Stop));
            }
            throttle::Hold {
                kind: crate::ids::HoldKind::Queue,
                text: format!("waiting for the start queue: {error}"),
                kinds: Vec::new(),
            }
        }
        _ => return Ok(None),
    };
    t.step_wait("capacity", Some(hold.text));
    let wait = t
        .record
        .wait
        .as_mut()
        .expect("capacity wait was just recorded");
    wait.kinds = hold.kinds;
    wait.hold = Some(hold.kind);
    if wait.kinds.is_empty() {
        wait.kinds = groups.into_iter().map(|group| group.kind).collect();
        wait.kinds.sort();
        wait.kinds.dedup();
    }
    wait.gives_up_at = match step.capacity_wait() {
        _ if !may_wait => None,
        Some(crate::config::CapacityWait::Duration(duration)) => {
            wait.first.checked_add(duration).ok()
        }
        Some(crate::config::CapacityWait::Forever) => None,
        None => t.record.capacity_until,
    };
    persist(t, guard)?;
    Ok(Some(Decision::Stop))
}

/// Contention on the machine's start queue, which a later advance retries as a capacity wait.
fn queue_lock_timed_out(error: &throttle::ThrottleError) -> bool {
    matches!(
        error,
        throttle::ThrottleError::Lock(crate::disk::lock::LockErr::Timeout { .. })
    )
}

/// Whether a lifecycle signal came from the attempt's own leader: its handle, compared
/// without a channel, or the session its supervised run recorded, read through `session` only
/// when the handle does not match and the signal names a session.
fn from_leader(
    leader: &workflow::AttemptRun,
    signal: &Signal,
    session: impl FnOnce(&workflow::AttemptRun) -> Option<crate::ids::AgentSessionId>,
) -> bool {
    if signal.source != crate::store::event::SignalSource::Lifecycle {
        return false;
    }
    let name = |handle: &str| {
        handle
            .split_once('#')
            .map_or(handle, |(name, _)| name)
            .to_owned()
    };
    let field = |key: &str| signal.payload.get(key).and_then(serde_json::Value::as_str);
    if field("handle").is_some_and(|handle| name(handle) == name(&leader.handle)) {
        return true;
    }
    field("session")
        .is_some_and(|recorded| session(leader).is_some_and(|agent| agent.as_str() == recorded))
}

/// The wait an occupied checkout puts an agent step in, or `None` when the step may launch: an
/// open turn blocks every step, and any occupant blocks a step without `takeover`.
fn occupancy_wait(occupancy: &Occupancy, takeover: bool) -> Option<&'static str> {
    match occupancy {
        Occupancy::Blocked(_) => Some("takeover"),
        Occupancy::Occupied(_) if !takeover => Some("occupied"),
        Occupancy::Free | Occupancy::Occupied(_) => None,
    }
}

fn start_limit(limit: &super::definition::Limit) -> throttle::StartLimit {
    throttle::StartLimit {
        max_parallel: limit.max_parallel,
        stagger: limit.stagger.as_ref().map(Parsed::result),
    }
}

/// The checkout a step launches into: its named worktree, else the run's scope.
fn destination(record: &RunRecord, step: &Node) -> Result<std::path::PathBuf> {
    let Some(name) = step.worktree() else {
        return Ok(record.scope.path.clone());
    };
    crate::worktree::worktree_path(
        &record.project_root,
        &crate::config::MachineConfig::load_lenient().agents.worktree,
        name,
    )
    .map_err(|error| WorkflowErr::Refused(format!("{error:#}")))
}

fn write_context(record: &RunRecord, paths: &WorkflowPaths, spec: &Definition) -> Result<()> {
    // Context belongs to the published attempt, including a restored until session.
    let attempt = record
        .attempt
        .as_ref()
        .expect("context belongs to a recorded attempt");
    let step = spec.step(&record.node).is_some();
    let output_path = (attempt.kind == AttemptKind::Judge).then(|| paths.check_output(attempt.seq));
    let mut context = json!({"run":record.id, "prompt":record.prompt, "node":record.node, "seq":attempt.seq, "key":record.key, "input":record.input, "output_path":output_path, "retry":attempt.retry, "signs_path":paths.signs(step.then_some(record.node.as_str())), "prev_attempt":record.prev_attempt});
    if step {
        let visits = &record.visits[&record.node];
        context["visits"] = json!(visits.total);
        context["visit"] = json!(
            attempt
                .visit
                .unwrap_or_else(|| visits.for_bucket(record.visit_bucket()))
        );
    }
    if let Some(session) = record
        .held
        .as_ref()
        .map(|held| &held.attempt)
        .or(record.attempt.as_ref())
        .filter(|attempt| {
            attempt.lifecycle == workflow::StepLifecycle::Until && attempt.ending.is_none()
        })
    {
        let visits = &record.visits[&session.node];
        context["live_attempt"] = json!({
            "step":session.node, "seq":session.seq,
            "visit":session.visit.unwrap_or_else(|| visits.for_bucket(record.visit_bucket())),
            "visits":visits.total,
            "key":record.key, "started_at":session.started_at,
            "handle":session.run.as_ref().map(|run| &run.handle),
            "run_id":session.run.as_ref().map(|run| &run.run_id),
            "claim_path":paths.claim(session.seq), "head_start":session.head_start,
            "turn_end":session.judged
        });
    }
    crate::disk::atomic::write_temp_then_rename(&paths.context, &context)?;
    Ok(())
}

pub fn cancel(
    state: &StatePaths,
    id: &WorkflowRunId,
    host: &dyn AttemptHost,
    reason: Option<&str>,
) -> Result<bool> {
    let paths = state.workflow(id);
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let record = workflow::load(&paths)?;
    if record.state.is_terminal() {
        return Ok(false);
    }
    let spec = spec::load(&paths.home)?;
    let mut t = Transition {
        record,
        spec: &spec,
        paths: &paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if t.record.held.is_some() {
        until::interrupt(&mut t, "cancel");
    }
    t.close_wait();
    if let Some(attempt) = &t.record.attempt
        && attempt.kind == AttemptKind::Agent
    {
        let busy = (attempt.ending.is_some() || attempt.run.is_none())
            && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref());
        if busy
            && attempt
                .ending
                .as_ref()
                .is_some_and(|ending| ending.cause == workflow::EndCause::Cancel)
        {
            return Ok(true);
        }
        let observed = match &attempt.run {
            Some(run) => host.observe(run, Observation::Attempt)?,
            None => agent::empty_observation(),
        };
        if let Some(verdict) = attempt
            .ending
            .as_ref()
            .and_then(|ending| ending.route.as_ref())
        {
            let attempt = attempt.clone();
            let verdict = verdict.clone();
            t.turn_end(
                &attempt,
                workflow::TurnEndAction::Interrupted,
                Some(&verdict),
                Some("cancel".into()),
            );
        }
        t.stage_agent_end(
            workflow::Ending {
                outcome: AttemptOutcome::Canceled,
                cause: workflow::EndCause::Cancel,
                route: None,
                reason: reason.map(str::to_owned),
            },
            &observed,
        );
        if !busy {
            t.record.attempt.as_mut().expect("agent cancel").host = agent::identity();
        }
        persist(&mut t, &guard)?;
        let record = t.record;
        drop(guard);
        if !busy {
            agent::finish(state, &paths, &spec, &record, host, None)?;
        }
        return Ok(true);
    }
    t.record.restart = None;
    let orphan = orphaned_notice(&t.record);
    if t.finish_script(
        Ending {
            outcome: AttemptOutcome::Canceled,
            cause: EndCause::Cancel,
            route: None,
            reason: reason.map(str::to_owned),
        },
        None,
        None,
        None,
    )? {
        persist(&mut t, &guard)?;
        drop(guard);
        if let Some(message) = orphan {
            host.retire(&message)?;
        }
        return Ok(true);
    }
    t.record.state = RunState::Canceled;
    t.record.terminal_at = Some(t.now);
    t.push(
        None,
        JournalEvent::Cancel {
            reason: reason.map(str::to_owned),
        },
    );
    t.push(
        None,
        JournalEvent::Terminal {
            state: RunState::Canceled,
        },
    );
    persist(&mut t, &guard)?;
    Ok(true)
}

pub fn restart(
    state: &StatePaths,
    id: &WorkflowRunId,
    resume: Option<(u64, crate::ids::AgentSessionId)>,
    host: &dyn AttemptHost,
) -> Result<RunRecord> {
    let paths = state.workflow(id);
    let spec = spec::load(&paths.home)?;
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(&paths)?,
        spec: &spec,
        paths: &paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if t.record.state.is_terminal() {
        return Err(WorkflowErr::Refused(format!(
            "workflow run {id} is {}; cannot restart",
            t.record.state.as_str()
        )));
    }
    if t.record
        .session_attempt()
        .and_then(|attempt| attempt.ending.as_ref())
        .is_some_and(|ending| ending.cause == EndCause::Cancel)
    {
        return Err(WorkflowErr::Refused(format!(
            "workflow run {id} is canceling; cannot restart"
        )));
    }
    let step = t
        .record
        .session_attempt()
        .map_or(&t.record.node, |attempt| &attempt.node)
        .clone();
    if !matches!(spec.step_kind(&step), Some(Kind::Agent(_))) {
        let kind = if spec.check(&step).is_some() {
            "a check"
        } else if matches!(spec.step_kind(&step), Some(Kind::Message(_))) {
            "a message step"
        } else {
            "a command step"
        };
        let reason = t
            .record
            .park
            .as_ref()
            .map(|park| format!("; @user: {}", park.label()))
            .or_else(|| {
                t.record
                    .wait
                    .as_ref()
                    .map(|wait| format!("; waiting on {}", wait.outcome))
            })
            .unwrap_or_default();
        return Err(WorkflowErr::Refused(format!(
            "node {step} is {kind}{reason}; restart requires an agent step"
        )));
    }
    if let Some((seq, _)) = &resume
        && t.record
            .session_attempt()
            .map(|attempt| attempt.seq)
            .or_else(|| {
                t.record
                    .prev_attempt
                    .as_ref()
                    .filter(|previous| {
                        t.record.state == RunState::Parked && previous.step == t.record.node
                    })
                    .map(|previous| previous.seq)
            })
            != Some(*seq)
    {
        return Err(WorkflowErr::Refused(
            "the step attempt changed during --continue preflight; retry the restart".into(),
        ));
    }
    if t.record.restart.is_some() {
        return Err(WorkflowErr::Refused(
            "this step is already restarting".into(),
        ));
    }
    let continued = resume.is_some();
    if t.record.held.is_some() {
        until::interrupt(&mut t, "restart");
    }
    t.record.restart = Some(workflow::Restart {
        resume: resume.map(|(_, session)| session),
        settled: None,
    });
    if continued
        && t.record.attempt.is_none()
        && t.record.state == RunState::Parked
        && let Some(previous) = t.record.prev_attempt.as_ref()
        && previous.step == step
        && let Some(run_id) = &previous.run_id
        && let Some(handle) = &previous.handle
    {
        t.record.restart.as_mut().expect("staged restart").settled = Some(previous.seq);
        t.record.attempt = Some(LiveAttempt {
            kind: AttemptKind::Agent,
            lifecycle: spec.step(&step).expect("restarted step").lifecycle(),
            seq: previous.seq,
            node: step.clone(),
            visit: Some(previous.visit),
            started_past_limit: None,
            started_at: previous.started_at,
            host: agent::identity(),
            run: Some(workflow::AttemptRun {
                run_id: run_id.clone(),
                handle: handle.clone(),
            }),
            head_start: previous.head_start.clone(),
            child: None,
            digests: BTreeMap::new(),
            ending: None,
            retry: None,
            woken: false,
            asking: None,
            held_since: None,
            asked_ms: 0,
            judged: None,
            push: None,
            message: None,
        });
        t.record.state = RunState::Active;
        t.record.park = None;
    }
    let busy = if let Some(attempt) = &t.record.attempt {
        let busy = (attempt.ending.is_some() || attempt.run.is_none())
            && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref());
        let observed = match &attempt.run {
            Some(run) => host.observe(run, Observation::Attempt)?,
            None => agent::empty_observation(),
        };
        t.stage_agent_end(
            Ending {
                outcome: AttemptOutcome::Canceled,
                cause: EndCause::Restart { continued },
                route: None,
                reason: None,
            },
            &observed,
        );
        if !busy {
            t.record.attempt.as_mut().expect("staged restart").host = agent::identity();
        }
        busy
    } else {
        t.restart_step(&step, continued);
        false
    };
    persist(&mut t, &guard)?;
    let record = t.record;
    drop(guard);
    if !busy && record.attempt.is_some() {
        agent::finish(state, &paths, &spec, &record, host, None)?;
    }
    let record = workflow::load(&paths)?;
    if record.state == RunState::Active {
        spawn(&record)?;
    }
    Ok(record)
}

pub fn resume(
    state: &StatePaths,
    id: &WorkflowRunId,
    at: Option<&str>,
    extend: &[String],
) -> Result<RunRecord> {
    let paths = state.workflow(id);
    let spec = spec::load(&paths.home)?;
    let record = {
        let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
        let record = workflow::load(&paths)?;
        if record.state.is_terminal() {
            return Err(WorkflowErr::Refused(
                "terminal workflow runs cannot resume".into(),
            ));
        }
        if let Some(node) = at {
            if spec.is_break(node) {
                return Err(WorkflowErr::Refused(
                    "@break is reserved; resume at the source or another sealed node".into(),
                ));
            }
            if spec.node(node).is_none() {
                return Err(WorkflowErr::Refused(format!(
                    "unknown sealed workflow node {node}"
                )));
            }
        }
        let at_break = spec.is_break(&record.node);
        if at_break
            && record.attempt.as_ref().is_some_and(|attempt| {
                (attempt.ending.is_some() || attempt.run.is_none())
                    && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref())
            })
        {
            return Err(WorkflowErr::Refused(
                "the on-break attempt is settling; retry resume after it finishes".into(),
            ));
        }
        if record.state == RunState::Active && !at_break && (at.is_some() || extend.is_empty()) {
            return Err(WorkflowErr::Refused(
                "an active run accepts only --extend; park it before repositioning".into(),
            ));
        }
        let mut t = Transition {
            record,
            spec: &spec,
            paths: &paths,
            now: Timestamp::now(),
            entries: Vec::new(),
        };
        for grant in extend {
            if let Some(value) = grant
                .strip_prefix("visits=+")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|v| *v > 0)
            {
                t.record.visit_grant = t
                    .record
                    .visit_grant
                    .checked_add(value)
                    .ok_or_else(|| WorkflowErr::Refused("visit grant overflow".into()))?;
            } else if let Some(raw) = grant.strip_prefix("deadline=") {
                t.record.deadline = Some(spec::parse_deadline(raw).map_err(|e| {
                    WorkflowErr::Refused(format!("invalid deadline extension: {e}"))
                })?);
                t.record.deadline_fired = false;
            } else {
                return Err(WorkflowErr::Refused(format!(
                    "invalid extension {grant:?}; use visits=+N or deadline=<when>"
                )));
            }
        }
        if at_break {
            t.close_wait();
            if let Some(input) = &t.record.input
                && input["edge"] == "cap"
                && let Some(step) = input["from"].as_str().map(str::to_owned)
            {
                t.reset_visits(&step, "cap");
            }
            let target = at
                .map(str::to_owned)
                .or_else(|| {
                    t.record
                        .input
                        .as_ref()?
                        .get("from")?
                        .as_str()
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    WorkflowErr::Refused("break context has no source node; use --at <node>".into())
                })?;
            if let Some(attempt) = &mut t.record.attempt {
                attempt.ending = Some(Ending {
                    outcome: AttemptOutcome::Canceled,
                    cause: EndCause::Lifecycle,
                    route: Some(workflow::Verdict {
                        node: t.record.node.clone(),
                        entered_at: t.record.entered_at,
                        at: t.now,
                        target,
                        object: t.record.input.clone().unwrap_or_else(|| json!({})),
                        reason: ParkReason::User,
                    }),
                    reason: None,
                });
                if attempt.run.is_some() {
                    // No off-lock owner is launching this published session.
                    attempt.host = workflow::Host {
                        pid: 0,
                        start: None,
                    };
                }
            } else {
                t.record.node = target;
                t.record.entered_at = t.now;
                t.record.check_retry = 0;
                t.record.check_retry_context = None;
                t.record.launch_retry = false;
                t.push(
                    None,
                    JournalEvent::Resume {
                        visit_grant: t.record.visit_grant,
                        deadline: t.record.deadline,
                    },
                );
            }
            persist(&mut t, &guard)?;
            let record = t.record;
            drop(guard);
            spawn(&record)?;
            return Ok(record);
        }
        if t.record.state == RunState::Parked {
            if t.record
                .park
                .as_ref()
                .is_some_and(|park| park.reason == ParkReason::Cap)
            {
                let step = t.record.node.clone();
                t.reset_visits(&step, "cap");
            }
            t.close_wait();
            if let Some(node) = at {
                t.record.node = node.into();
            }
            t.record.entered_at = t.now;
            t.record.check_retry = 0;
            t.record.check_retry_context = None;
            t.record.launch_retry = false;
            t.record.park = None;
            t.record.state = RunState::Active;
        }
        t.push(
            None,
            JournalEvent::Resume {
                visit_grant: t.record.visit_grant,
                deadline: t.record.deadline,
            },
        );
        persist(&mut t, &guard)?;
        t.record
    };
    spawn(&record)?;
    Ok(record)
}

/// The task's live runs that have not entered an agent or message step: the runs a removal or
/// redefinition of the task cancels, since nothing they started outlives them.
pub(crate) fn unacted_task_runs(state: &StatePaths, task: &str) -> Result<Vec<WorkflowRunId>> {
    if !state.root.exists() {
        return Ok(Vec::new());
    }
    Ok(workflow::list(state)?
        .into_iter()
        .filter(|record| record.started_by(task) && !acted(&state.workflow(&record.id), record))
        .map(|record| record.id)
        .collect())
}

pub(crate) fn reassign_task(
    state: &StatePaths,
    old: &str,
    new: Option<&str>,
) -> Result<Vec<WorkflowRunId>> {
    if !state.root.exists() {
        return Ok(Vec::new());
    }
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut reassigned = Vec::new();
    for mut record in workflow::list(state)? {
        // A finished run still owed its collection reports and forgets launch memory under its
        // task's name, so a rename moves it too; a removal leaves it to report under the old one.
        let owed = new.is_some() && record.ran_for(old) && record.owes_loop_report();
        if !record.started_by(old) && !owed {
            continue;
        }
        record.task = new.map(str::to_owned);
        if new.is_none() {
            record.loop_report = workflow::LoopReport::None;
        }
        workflow::write(&state.workflow(&record.id), &record, &guard)?;
        reassigned.push(record.id);
    }
    Ok(reassigned)
}

/// Fill a checkout's path, board and cohort defaults into one subscription before it is matched.
///
/// `team.stage` defaults to the checkout's board; another team signal, which carries no board,
/// defaults to the cohort of its team that launches in the checkout.
fn delivers(
    project_root: &Path,
    checkout: &Path,
    (selector, mut matches): (
        crate::harness::signal::SignalSelector,
        BTreeMap<String, String>,
    ),
    signal: &Signal,
) -> bool {
    match default_signal_match_key(&selector, &matches) {
        Some("path") => {
            matches.insert("path".into(), checkout.to_string_lossy().into_owned());
        }
        Some("instance") if signal.name.as_str() != "team.stage" => {
            if let Some(team) = signal
                .payload
                .get("team")
                .and_then(serde_json::Value::as_str)
            {
                let own = crate::harness::lanes::own_lane(project_root, checkout);
                let lane = if own.is_trunk() {
                    Some(crate::harness::lanes::in_place_team_name(
                        project_root,
                        team,
                    ))
                } else {
                    Path::new(own.path())
                        .file_name()
                        .map(|_| own.routing_name())
                };
                let Ok(instance) = crate::ids::TeamInstanceId::new(team, lane) else {
                    return false;
                };
                matches.insert("instance".into(), instance.to_string());
            }
        }
        Some("instance") if !matches.contains_key("board") => {
            matches.insert(
                "board".into(),
                crate::harness::scratch::durable_board_path(checkout)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        _ => {}
    }
    resolve_subscription(&selector, &matches, signal) == SignalResolution::Deliver
}

/// A subscribed signal bound for a live staying session's leader.
struct Forward {
    step: String,
    /// The delivering binding's `prompt`, sent after the signal.
    prompt: Option<String>,
    seq: u64,
    run: workflow::AttemptRun,
    message: crate::ids::MessageId,
}

impl Forward {
    /// The journal row the forward is recorded by, keyed to the session's step and seq.
    fn entry(&self, signal: &Signal) -> workflow::JournalEntry {
        workflow::JournalEntry {
            seq: Some(self.seq),
            at: Timestamp::now(),
            node: self.step.clone(),
            event: JournalEvent::Signal {
                signal: signal.name.as_str().into(),
                message: self.message.clone(),
            },
        }
    }

    /// Queue the journaled forward off-lock; a failure leaves the journal row and warns.
    fn send(&self, host: &dyn AttemptHost, record: &RunRecord, signal: &Signal) {
        let text = forward_text(record, &self.step, self.prompt.as_deref(), signal);
        if let Err(error) = host.forward(&self.run, &self.message, &text) {
            tracing::warn!(
                run = %record.id, step = %self.step, %error,
                "could not forward a subscribed signal to the step leader"
            );
        }
    }
}

/// The forwards a live staying session's leader gets, one per distinct delivering binding.
///
/// Defaults resolve against the checkout the leader launched in; bindings with the same
/// selector and filter coalesce into the first. The leader never hears its own lifecycle.
fn subscribers(
    spec: &Definition,
    record: &RunRecord,
    signal: &Signal,
    host: &dyn AttemptHost,
) -> Result<Vec<Forward>> {
    let Some(attempt) = record.session_attempt() else {
        return Ok(Vec::new());
    };
    let (Some(node), Some(run)) = (spec.step(&attempt.node), &attempt.run) else {
        return Ok(Vec::new());
    };
    if attempt.lifecycle.is_turn()
        || attempt.ending.is_some()
        || from_leader(run, signal, |run| host.leader_session(run))
    {
        return Ok(Vec::new());
    }
    let checkout = destination(record, node)?;
    let mut seen = Vec::new();
    let mut forwards = Vec::new();
    for wake in node.wakes() {
        let subscription = wake.subscription().value().map_err(SpecErr::clone)?.clone();
        if seen.contains(&subscription) {
            continue;
        }
        seen.push(subscription.clone());
        if !delivers(&record.project_root, &checkout, subscription, signal) {
            continue;
        }
        forwards.push(Forward {
            step: attempt.node.clone(),
            prompt: wake.prompt().map(str::to_owned),
            seq: attempt.seq,
            run: run.clone(),
            message: crate::ids::MessageId::new(),
        });
    }
    Ok(forwards)
}

/// The SIGNAL notice a subscribed step's leader receives.
fn forward_text(record: &RunRecord, step: &str, prompt: Option<&str>, signal: &Signal) -> String {
    let subject = crate::harness::signal::signal_subject(&signal.name, &signal.payload);
    let mut lines = vec![if subject.is_empty() {
        signal.name.to_string()
    } else {
        format!("{} {subject}", signal.name)
    }];
    lines.extend(crate::harness::signal::signal_details(signal));
    lines.push(format!(
        "Workflow `{}` step `{step}` subscribes to this signal (run {id}; `rimz workflow show {id}`).",
        record.workflow,
        id = record.id,
    ));
    if let Some(prompt) = prompt {
        lines.extend([String::new(), prompt.to_owned()]);
    }
    lines.join("\n")
}

/// Whether a live staying session subscribes to the family of a lifecycle signal, the only
/// receiver a lifecycle signal has among workflow runs. Reads records and sealed specs, and the
/// leader's recorded session through `session` as `from_leader` needs it; it runs where a signal
/// fires, with no attempt host.
pub(crate) fn hears_lifecycle(
    state: &StatePaths,
    signal: &Signal,
    session: &dyn Fn(&workflow::AttemptRun) -> Option<crate::ids::AgentSessionId>,
) -> Result<bool> {
    for record in workflow::list(state)? {
        let Some(attempt) = record.session_attempt() else {
            continue;
        };
        if record.state != RunState::Active
            || attempt.lifecycle.is_turn()
            || attempt.ending.is_some()
            || attempt
                .run
                .as_ref()
                .is_some_and(|run| from_leader(run, signal, session))
        {
            continue;
        }
        let spec = spec::load(&state.workflow(&record.id).home)?;
        let Some(step) = spec.step(&attempt.node) else {
            continue;
        };
        if step
            .subscriptions()?
            .iter()
            .any(|(selector, _)| selector.family() == signal.name.family())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn wake(state: &StatePaths, signal: &Signal, host: &dyn AttemptHost) -> Result<()> {
    for record in workflow::list(state)? {
        let paths = state.workflow(&record.id);
        let result: Result<()> = (|| {
            let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
            let current = workflow::load(&paths)?;
            if current.state != RunState::Active {
                return Ok(());
            }
            let spec = spec::load(&paths.home)?;
            let woken = match spec.check(&current.node) {
                Some(check) => check.subscriptions()?.into_iter().any(|subscription| {
                    delivers(
                        &current.project_root,
                        &current.scope.path,
                        subscription,
                        signal,
                    )
                }),
                None => false,
            };
            let forwards = subscribers(&spec, &current, signal, host)?;
            let entries = forwards.iter().map(|forward| forward.entry(signal));
            if !woken {
                if forwards.is_empty() {
                    return Ok(());
                }
                for entry in entries {
                    workflow::append(&paths, &entry, &guard)?;
                }
                drop(guard);
                for forward in &forwards {
                    forward.send(host, &current, signal);
                }
                return Ok(());
            }
            let mut t = Transition {
                record: current,
                spec: &spec,
                paths: &paths,
                now: Timestamp::now(),
                entries: Vec::new(),
            };
            t.record.recheck_at = None;
            if let Some(attempt) = &mut t.record.attempt {
                attempt.woken = true;
            }
            if let Some(held) = &mut t.record.held {
                held.attempt.woken = true;
            }
            if let Some(wait) = &mut t.record.wait {
                wait.recheck_at = None;
            }
            t.push(
                None,
                JournalEvent::Wake {
                    signal: signal.name.as_str().into(),
                },
            );
            t.entries.extend(entries);
            persist(&mut t, &guard)?;
            drop(guard);
            for forward in &forwards {
                forward.send(host, &t.record, signal);
            }
            spawn(&t.record)
        })();
        if let Err(error) = result {
            tracing::warn!(path = %paths.home.display(), %error, "could not wake workflow; continuing with other runs");
        }
    }
    Ok(())
}

pub fn wake_run(project_root: &Path, id: &WorkflowRunId) -> Result<()> {
    let state = StatePaths::for_project_root(project_root)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    spawn(&workflow::load(&state.workflow(id))?)
}

#[cfg(test)]
mod tests;
