use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;
use serde_json::json;

use super::{AttemptHost, Observation, Occupancy, ScopeRoots};
use super::{WorkflowSpec, script, spec, state::Transition};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::harness::schedule::{
    arm,
    signal::{Signal, SignalResolution, resolve_subscription},
    throttle,
    when::WindowReadings,
};
use crate::ids::WorkflowRunId;
use crate::store::workflow::{
    self, AttemptKind, AttemptOutcome, EndCause, Ending, Host, JournalEvent, LiveAttempt,
    ParkReason, RunOrigin, RunRecord, RunState, Scope, ScopeKind, Verdict,
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
}

type Result<T> = std::result::Result<T, WorkflowErr>;

mod agent;

/// Where a new run's definition comes from.
pub enum DefinitionSource<'a> {
    /// A deployed workflow directory; its basename names the run.
    Deployed { dir: &'a Path, task: Option<String> },
    /// A loop task's compiled definition, sealed into the run's own root and named for the task.
    Inline {
        task: String,
        definition: InlineDefinition,
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
        DefinitionSource::Deployed { dir, task } => {
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
            }
        }
        DefinitionSource::Inline { task, .. } => RunStart {
            origin: RunOrigin::Inline,
            name: task.clone(),
            task: Some(task.clone()),
            spec_dir: std::path::PathBuf::new(),
        },
    };
    // A run already holding the scope wins before the definition or the
    // prompt is read, so an edit to either mid-run reads as a skip.
    refuse_existing(
        state,
        start.origin,
        &start.name,
        start.task.as_deref(),
        &roots,
    )?;
    let prompt = prompt().map_err(WorkflowErr::Refused)?;
    let id = WorkflowRunId::new();
    let record = match source {
        DefinitionSource::Deployed { .. } => publish(state, id, start, roots, prompt, manual)?,
        DefinitionSource::Inline { definition, .. } => {
            let paths = state.workflow(&id);
            let published = workflow::seal_definition(&paths, &definition.text, &definition.assets)
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
        kind: spec.workflow.scope,
        path: roots.path(spec.workflow.scope).to_path_buf(),
    };
    // An agent or a message needs the room, and so does a loop task's run, whose advance finds
    // it through the room's record; a hand-started run of checks and scripts runs without one.
    let in_room = spec
        .steps
        .values()
        .any(|step| step.agent.is_some() || step.message.is_some())
        || spec.checks.values().any(|check| check.agent.is_some());
    let room = crate::workspace::record::read(&state.workspace_record).is_ok();
    // A deployed run in a room keeps its notes and history; a loop task's run of checks fires too
    // often to keep a folder each time.
    let keeps_notes = in_room || (start.origin == RunOrigin::Deployed && room);
    if (in_room || start.task.is_some()) && !room {
        return Err(WorkflowErr::Refused(format!(
            "no room at {}; run `rimz start` there first",
            roots.project.display()
        )));
    }
    let now = Timestamp::now();
    let mut record = RunRecord {
        version: workflow::VERSION,
        default_lifecycle: workflow::StepLifecycle::Until,
        route_defaults: workflow::RouteDefaults::OnBreak,
        spec_version: spec::SPEC_VERSION,
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
        project_root: roots.project.clone(),
        created_at: now,
        notes: None,
        state: RunState::Active,
        park: None,
        node: spec.workflow.start.clone(),
        entered_at: now,
        input: None,
        key: None,
        recheck_at: None,
        wait: None,
        visits: BTreeMap::new(),
        visit_grant: 0,
        deadline: spec
            .workflow
            .deadline
            .as_deref()
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
        check_retry: 0,
        check_retry_context: None,
    };
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    refuse_existing(
        state,
        record.origin,
        &record.workflow,
        record.task.as_deref(),
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
/// or its task, so a task cannot dodge overlap by changing what it runs.
fn refuse_existing(
    state: &StatePaths,
    origin: RunOrigin,
    name: &str,
    task: Option<&str>,
    roots: &ScopeRoots,
) -> Result<()> {
    match workflow::list(state)?.into_iter().find(|r| {
        !r.state.is_terminal()
            && r.scope.path == roots.path(r.scope.kind)
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
    for entry in &t.entries {
        workflow::append(t.paths, entry, guard)?;
    }
    workflow::write(t.paths, &t.record, guard)?;
    accounting::transition(t);
    report_to_loop(t.paths, &mut t.record, guard);
    Ok(())
}

/// Report a finished task-started run to its loop task once, after its terminal record is
/// durable; a crash before the mark leaves the report to the scheduler's backstop advance.
fn report_to_loop(paths: &WorkflowPaths, record: &mut RunRecord, guard: &WorkspaceLock) {
    if !record.owes_loop_report() {
        return;
    }
    let journal = workflow::journal(paths).unwrap_or_else(|error| {
        tracing::warn!(run = %record.id, %error, "workflow journal unreadable for its loop report");
        Vec::new()
    });
    crate::harness::schedule::run_log::report_run(record, &journal);
    record.loop_report = workflow::LoopReport::Sent;
    if let Err(error) = workflow::write(paths, record, guard) {
        tracing::warn!(run = %record.id, %error, "workflow loop report mark not written");
    }
}

fn window_wait(
    raw: &str,
    scope: &Path,
    provider: &crate::ids::AgentKind,
    account: Option<&crate::ids::LoginName>,
    windows: &WindowReadings<'_>,
) -> Result<Option<String>> {
    let expr = spec::parse_window(raw)?;
    let verdict = crate::harness::schedule::when::evaluate(
        &expr,
        scope,
        None,
        Some(provider),
        account,
        windows,
    );
    Ok((!verdict.ok).then(|| expr.blocking_evidence(&verdict).replacen("window.", "", 1)))
}

/// The start groups an agent launch admits, by kind and account. A step's `account` pins every
/// cell, so availability routing is skipped, as the launcher does for a pinned login.
fn launch_groups(
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
        let login = crate::harness::schedule::runner::task_login(&requested, &cell.kind, &runtime)
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
    raw: &str,
    windows: &WindowReadings<'_>,
) -> Result<Option<String>> {
    window_wait_for_groups(record, &launch_groups(record, agent, None)?, raw, windows)
}

fn window_wait_for_groups(
    record: &RunRecord,
    groups: &[throttle::StartGroup],
    raw: &str,
    windows: &WindowReadings<'_>,
) -> Result<Option<String>> {
    for group in groups {
        if let Some(detail) = window_wait(
            raw,
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
    Launch(
        Box<(
            RunRecord,
            std::result::Result<String, String>,
            Option<throttle::Turn>,
        )>,
    ),
    End(Box<RunRecord>),
    Push(Box<RunRecord>),
    Evict(Box<RunRecord>),
    Message(Box<Notice>),
    Admit(Box<(u64, String, throttle::Run)>),
}

#[derive(Default)]
struct Admission {
    eviction: Option<(u64, String, Option<String>)>,
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
    if record.owes_loop_report() {
        let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
        let mut record = workflow::load(&paths)?;
        report_to_loop(&paths, &mut record, &guard);
        return Ok(());
    }
    let spec = spec::load_run(&paths.home, &record)?;
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
            Decision::Evict(record) => {
                let error = host
                    .evict(
                        &record.scope.path,
                        &record.project_root,
                        &record.break_agents,
                    )
                    .err();
                admission.eviction = Some((
                    record.next_seq,
                    record.node,
                    error.map(|error| error.to_string()),
                ));
                continue;
            }
            Decision::Launch(launch) => {
                let (record, prompt, turn) = *launch;
                if agent::launch(state, &paths, &spec, &record, prompt, turn, host)? {
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
        if spec.steps.contains_key(&record.node) {
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
    spec: &WorkflowSpec,
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
        spec: &WorkflowSpec,
        text: std::result::Result<String, String>,
    ) -> Option<Self> {
        let attempt = record.attempt.as_ref()?;
        let step = spec.steps.get(&attempt.node)?;
        let Some(spec::StepKind::Message(target)) = step.kind() else {
            return None;
        };
        Some(Self {
            seq: attempt.seq,
            message: attempt.message.clone()?,
            target: target.into(),
            session: step.session.clone(),
            record,
            text,
        })
    }

    /// The WAIT notice body: the run's line, then the step's composed prompt.
    fn text(
        record: &RunRecord,
        paths: &WorkflowPaths,
        step: &spec::Step,
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
    spec: &WorkflowSpec,
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
    spec: &WorkflowSpec,
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
            && let Some(step) = spec.steps.get(&t.record.node)
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
    if let Some(step) = spec.steps.get(&t.record.node) {
        if let Some((bound, visits, max)) = step.tripped_visit_bound(&t.record) {
            if !seen.insert(t.record.node.clone()) {
                return Ok(Decision::Stop);
            }
            let route = t.cap_route(bound, visits, max);
            t.route(&route.target, route.object, route.reason)?;
            admission.capacity = None;
            persist(&mut t, &guard)?;
            return Ok(Decision::Again);
        }
        if let Some(spec::StepKind::Agent(agent)) = step.kind() {
            let admits = spec.workflow.limit.is_some()
                || step.limit.is_some()
                || t.record.restart.is_some()
                || (t.record.task.is_some() && !t.record.manual);
            if admits && let Some((_, _, Err(reason))) = &admission.capacity {
                launch_error = Some(reason.to_string());
            }
            let groups = if admits || step.window.is_some() {
                match launch_groups(&t.record, agent, step.account.as_ref()) {
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
                && let Some(raw) = &step.window
                && let Some(detail) = window_wait_for_groups(&t.record, &groups, raw, windows)?
            {
                t.step_wait("window", Some(detail));
                persist(&mut t, &guard)?;
                return Ok(Decision::Stop);
            }
            let mut evict = false;
            if let Some((_, _, error)) = admission
                .eviction
                .take()
                .filter(|(seq, node, _)| *seq == t.record.next_seq && *node == t.record.node)
            {
                // A confirmed stop admits only this launch; later visits read occupancy afresh.
                launch_error = launch_error.or(error);
            } else if launch_error.is_none() && !step.share.unwrap_or_default() {
                match host.occupancy(
                    &t.record.scope.path,
                    &t.record.project_root,
                    &t.record.break_agents,
                )? {
                    Occupancy::Free => {}
                    Occupancy::Blocked(_) => {
                        t.step_wait("takeover", None);
                        persist(&mut t, &guard)?;
                        return Ok(Decision::Stop);
                    }
                    Occupancy::Occupied(_) if !step.takeover.unwrap_or_default() => {
                        t.step_wait("occupied", None);
                        persist(&mut t, &guard)?;
                        return Ok(Decision::Stop);
                    }
                    Occupancy::Occupied(_) => evict = true,
                }
            }
            t.now = Timestamp::now();
            if launch_error.is_none() {
                if let Some(decision) = prepare_capacity(&mut t, step, groups, admission, &guard)? {
                    return Ok(decision);
                }
                if evict {
                    return Ok(Decision::Evict(Box::new(t.record)));
                }
            }
        }
    } else if spec.checks.contains_key(&t.record.node) {
        let check = &spec.checks[&t.record.node];
        if let (Some(raw), Some(spec::CheckKind::Agent(profile))) = (&check.window, check.kind())
            && let Some(detail) = window_wait_for_launch(&t.record, profile, raw, windows)?
        {
            t.step_wait("window", Some(detail));
            persist(&mut t, &guard)?;
            return Ok(Decision::Stop);
        }
        if check.window.is_some()
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
    let step = spec.steps.get(&t.record.node);
    let agent_step = step.filter(|step| matches!(step.kind(), Some(spec::StepKind::Agent(_))));
    let kind = if agent_step.is_some() {
        AttemptKind::Agent
    } else if step.is_some() {
        AttemptKind::Script
    } else {
        match spec.checks[&t.record.node].kind() {
            Some(spec::CheckKind::Agent(_)) => AttemptKind::Judge,
            Some(spec::CheckKind::Sleep) => AttemptKind::Script,
            _ => AttemptKind::Script,
        }
    };
    let mut digests = BTreeMap::new();
    let prompt = agent_step.map(|step| {
        agent::compose(&t.record, paths, step, &mut digests).map_err(|error| error.to_string())
    });
    let notice = step
        .filter(|step| matches!(step.kind(), Some(spec::StepKind::Message(_))))
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
        lifecycle: step.map_or(workflow::StepLifecycle::Turn, spec::Step::lifecycle),
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
                    at: Some(t.now),
                    target: "@failed".into(),
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
        return Ok(Decision::Launch(Box::new((t.record, prompt, turn))));
    }
    Ok(Decision::Evaluate(Box::new(t.record)))
}

fn prepare_capacity(
    t: &mut Transition<'_>,
    step: &super::Step,
    groups: Vec<throttle::StartGroup>,
    admission: &mut Admission,
    guard: &WorkspaceLock,
) -> Result<Option<Decision>> {
    let may_wait = t.record.task.is_some() && !t.record.manual && t.record.restart.is_none();
    let machine_gated = may_wait && step.throttle != Some(crate::config::ThrottleSwitch::Off);
    let limits =
        (t.spec.workflow.limit.is_some() || step.limit.is_some() || t.record.restart.is_some())
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
                workflow_limit: t.spec.workflow.limit.clone(),
                step_limit: step.limit.clone(),
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
                disk: t.record.scope.path.clone(),
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
    if let Ok(throttle::Attempt::Held(hold)) = answer {
        t.step_wait("capacity", Some(hold.text.clone()));
        let wait = t
            .record
            .wait
            .as_mut()
            .expect("capacity wait was just recorded");
        wait.kinds = hold.kinds.clone();
        wait.hold = Some(hold.kind);
        if wait.kinds.is_empty() {
            wait.kinds = groups.into_iter().map(|group| group.kind).collect();
            wait.kinds.sort();
            wait.kinds.dedup();
        }
        wait.gives_up_at = match step.capacity_wait {
            Some(crate::config::CapacityWait::Duration(duration)) => {
                wait.first.checked_add(duration).ok()
            }
            _ => None,
        };
        persist(t, guard)?;
        return Ok(Some(Decision::Stop));
    }
    Ok(None)
}

fn write_context(record: &RunRecord, paths: &WorkflowPaths, spec: &WorkflowSpec) -> Result<()> {
    // Context belongs to the published attempt, including a restored until session.
    let attempt = record
        .attempt
        .as_ref()
        .expect("context belongs to a recorded attempt");
    let step = spec.steps.contains_key(&record.node);
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
    let spec = spec::load_run(&paths.home, &record)?;
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
    let spec = spec::load_run(&paths.home, &workflow::load(&paths)?)?;
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
    if !spec
        .steps
        .get(&step)
        .is_some_and(|step| matches!(step.kind(), Some(spec::StepKind::Agent(_))))
    {
        let kind = if spec.checks.contains_key(&step) {
            "a check"
        } else if spec
            .steps
            .get(&step)
            .is_some_and(|step| matches!(step.kind(), Some(spec::StepKind::Message(_))))
        {
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
            lifecycle: spec.steps[&step].lifecycle(),
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
    let spec = spec::load_run(&paths.home, &workflow::load(&paths)?)?;
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
            if !spec.checks.contains_key(node) && !spec.steps.contains_key(node) {
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
                        at: Some(t.now),
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
        if !record.started_by(old) {
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

/// Fill a run's checkout and board defaults into one subscription before it is matched.
fn delivers(
    record: &RunRecord,
    (selector, mut matches): (
        crate::harness::schedule::signal::SignalSelector,
        BTreeMap<String, String>,
    ),
    signal: &Signal,
) -> bool {
    match arm::default_signal_match_key(&selector, &matches) {
        Some("path") => {
            matches.insert(
                "path".into(),
                record.scope.path.to_string_lossy().into_owned(),
            );
        }
        Some("instance") if !matches.contains_key("board") => {
            matches.insert(
                "board".into(),
                crate::harness::scratch::durable_board_path(&record.scope.path)
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
        let text = forward_text(record, &self.step, signal);
        if let Err(error) = host.forward(&self.run, &self.message, &text) {
            tracing::warn!(
                run = %record.id, step = %self.step, %error,
                "could not forward a subscribed signal to the step leader"
            );
        }
    }
}

/// The live staying session a subscribed signal goes to.
fn subscriber(spec: &WorkflowSpec, record: &RunRecord, signal: &Signal) -> Result<Option<Forward>> {
    let Some(attempt) = record.session_attempt() else {
        return Ok(None);
    };
    let (Some(step), Some(run)) = (spec.steps.get(&attempt.node), &attempt.run) else {
        return Ok(None);
    };
    if attempt.lifecycle.is_turn() || attempt.ending.is_some() {
        return Ok(None);
    }
    let subscribed = step
        .subscriptions()?
        .into_iter()
        .any(|subscription| delivers(record, subscription, signal));
    Ok(subscribed.then(|| Forward {
        step: attempt.node.clone(),
        seq: attempt.seq,
        run: run.clone(),
        message: crate::ids::MessageId::new(),
    }))
}

/// The SIGNAL notice a subscribed step's leader receives.
fn forward_text(record: &RunRecord, step: &str, signal: &Signal) -> String {
    let subject = crate::harness::schedule::runner::signal_subject(&signal.name, &signal.payload);
    let mut lines = vec![if subject.is_empty() {
        signal.name.to_string()
    } else {
        format!("{} {subject}", signal.name)
    }];
    lines.extend(crate::harness::schedule::runner::signal_details(signal));
    lines.push(format!(
        "Workflow `{}` step `{step}` subscribes to this signal (run {id}; `rimz workflow show {id}`).",
        record.workflow,
        id = record.id,
    ));
    lines.join("\n")
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
            let spec = spec::load_run(&paths.home, &current)?;
            let woken = match spec.checks.get(&current.node) {
                Some(check) => check
                    .subscriptions()?
                    .into_iter()
                    .any(|subscription| delivers(&current, subscription, signal)),
                None => false,
            };
            let forward = subscriber(&spec, &current, signal)?;
            let entry = forward.as_ref().map(|forward| forward.entry(signal));
            if !woken {
                let (Some(forward), Some(entry)) = (forward, entry) else {
                    return Ok(());
                };
                workflow::append(&paths, &entry, &guard)?;
                drop(guard);
                forward.send(host, &current, signal);
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
            t.entries.extend(entry);
            persist(&mut t, &guard)?;
            drop(guard);
            if let Some(forward) = forward {
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
