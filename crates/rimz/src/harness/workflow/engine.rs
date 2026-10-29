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
    self, AttemptKind, AttemptOutcome, EndCause, Ending, Host, JournalEvent, KeyVisits,
    LiveAttempt, ParkReason, RunRecord, RunState, Scope, ScopeKind,
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

pub fn create(
    state: &StatePaths,
    spec_dir: &Path,
    roots: ScopeRoots,
    prompt: impl FnOnce() -> std::result::Result<Option<String>, String>,
    task: Option<String>,
    manual: bool,
) -> Result<RunRecord> {
    let spec_dir = spec_dir.canonicalize()?;
    let name = spec_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| WorkflowErr::Refused("workflow directory needs a name".into()))?
        .to_owned();
    let roots = ScopeRoots {
        checkout: roots.checkout.canonicalize()?,
        project: roots.project.canonicalize()?,
    };
    // A run already holding the scope wins before the definition or the
    // prompt is read, so an edit to either mid-run reads as a skip.
    refuse_existing(state, &name, &roots)?;
    let prompt = prompt().map_err(WorkflowErr::Refused)?;
    let bytes = std::fs::read(crate::disk::paths::workflow_spec_file(&spec_dir))?;
    let spec: WorkflowSpec = toml::from_str(
        std::str::from_utf8(&bytes).map_err(|e| WorkflowErr::Refused(e.to_string()))?,
    )
    .map_err(|e| WorkflowErr::Refused(format!("invalid workflow spec: {e}")))?;
    let machine = crate::config::MachineConfig::load_lenient();
    let definitions = crate::config::definitions::load(
        &crate::disk::paths::agents_home(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &crate::config::MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact,
    );
    let report =
        super::validate::validate_spec(&spec_dir, &name, &spec, &definitions, Some(&roots.project));
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
    if crate::workspace::record::read(&state.workspace_record).is_err() {
        return Err(WorkflowErr::Refused(format!(
            "no room at {}; run `rimz start` there first",
            roots.project.display()
        )));
    }
    let now = Timestamp::now();
    let record = RunRecord {
        version: if spec
            .steps
            .values()
            .any(|step| step.lifecycle == Some(workflow::StepLifecycle::Until))
        {
            2
        } else {
            1
        },
        id: WorkflowRunId::new(),
        workflow: name,
        task,
        manual,
        spec_dir,
        spec_digest: script::digest(&bytes),
        prompt,
        scope,
        project_root: roots.project.clone(),
        created_at: now,
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
        prev_attempt: None,
        terminal_at: None,
        check_retry: 0,
        check_retry_context: None,
    };
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
        refuse_existing(state, &record.workflow, &roots)?;
        workflow::initialize(state, &record, &bytes, &guard)?;
    }
    spawn(&record)?;
    Ok(record)
}

fn refuse_existing(state: &StatePaths, name: &str, roots: &ScopeRoots) -> Result<()> {
    match workflow::list(state)?.into_iter().find(|r| {
        !r.state.is_terminal() && r.workflow == name && r.scope.path == roots.path(r.scope.kind)
    }) {
        Some(existing) => Err(WorkflowErr::Exists {
            run: existing.id,
            workflow: name.into(),
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

fn persist(t: &Transition<'_>, guard: &WorkspaceLock) -> Result<()> {
    for entry in &t.entries {
        workflow::append(t.paths, entry, guard)?;
    }
    workflow::write(t.paths, &t.record, guard)?;
    accounting::transition(t);
    Ok(())
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

fn launch_groups(record: &RunRecord, agent: &str) -> Result<Vec<throttle::StartGroup>> {
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
        let availability = crate::harness::plan::LaunchAvailability::read(
            &runtime,
            &state,
            &machine,
            Timestamp::now(),
        );
        effective.route(
            &machine.tiers,
            crate::config::effective::ProfileScope::Agents,
            Some(agent),
            None,
            None,
            None,
            |kind, model| availability.unavailable(kind, model),
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
        let requested = cell.launch.login.clone().map_or(
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
    window_wait_for_groups(record, &launch_groups(record, agent)?, raw, windows)
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
    Evict(Box<RunRecord>),
    Admit(Box<(u64, String, throttle::Run)>),
}

#[derive(Default)]
struct Admission {
    eviction: Option<(u64, String, Option<String>)>,
    capacity: Option<(u64, String, std::result::Result<throttle::Attempt, String>)>,
}

pub fn advance(state: &StatePaths, id: &WorkflowRunId, host: &dyn AttemptHost) -> Result<()> {
    let runtime = crate::disk::paths::RuntimePaths::for_state(state)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let paths = state.workflow(id);
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
                    throttle::try_admit(&config.r#loop.throttle, &run)
                        .map_err(|error| error.to_string()),
                ));
                #[cfg(feature = "testkit")]
                crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_ADMISSION_GATE");
                continue;
            }
            Decision::Evict(record) => {
                let error = host.evict(&record.scope.path, &record.project_root).err();
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
            Decision::End(record) => {
                if agent::finish(state, &paths, &spec, &record, host, None)? {
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
        persist(&t, &guard)?;
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
    let result = script::execute_step(state, paths, spec, record, || accounting::step(record));
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
        },
        code,
        head_end,
        reason,
    )?;
    persist(&t, &guard)?;
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
    if t.bound()? {
        admission.capacity = None;
        persist(&t, &guard)?;
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
        let reason = format!("lost command-step host {}", attempt.host.pid);
        t.finish_script(
            Ending {
                outcome: AttemptOutcome::Canceled,
                cause: EndCause::Lifecycle,
                route: None,
            },
            None,
            None,
            Some(reason),
        )?;
        persist(&t, &guard)?;
        return Ok(Decision::Again);
    }
    if t.record.recheck_at.is_some_and(|at| at > t.now)
        && !t.record.wait.as_ref().is_some_and(|wait| wait.is_hold())
    {
        if let Some(wait) = &mut t.record.wait {
            wait.last = t.now;
            wait.count += 1;
        }
        persist(&t, &guard)?;
        return Ok(Decision::Stop);
    }
    let mut launch_error = None;
    if let Some(step) = spec.steps.get(&t.record.node) {
        let visits = t.record.visits.get(&t.record.node);
        for (bound, max, visits) in [
            ("max-visits", step.max_visits, visits.map_or(0, |v| v.total)),
            (
                "max-visits-per-key",
                step.max_visits_per_key,
                visits.map_or(0, |v| v.for_key(t.record.key.as_ref())),
            ),
        ] {
            let Some(max) = max.map(|max| max.saturating_add(t.record.visit_grant)) else {
                continue;
            };
            if visits < max {
                continue;
            }
            if !seen.insert(t.record.node.clone()) {
                return Ok(Decision::Stop);
            }
            t.engine_route(
                "cap",
                json!({
                    "outcome":"cap", "step":t.record.node, "bound":bound,
                    "visits":visits, "max":max
                }),
                step.on.cap.as_deref().unwrap_or("@user"),
                ParkReason::Cap,
            )?;
            admission.capacity = None;
            persist(&t, &guard)?;
            return Ok(Decision::Again);
        }
        if let Some(spec::StepKind::Agent(agent)) = step.kind() {
            let gated = t.record.task.is_some()
                && !t.record.manual
                && step.throttle != Some(crate::config::ThrottleSwitch::Off);
            if gated && let Some((_, _, Err(reason))) = &admission.capacity {
                launch_error = Some(reason.clone());
            }
            let groups = if gated || step.window.is_some() {
                match launch_groups(&t.record, agent) {
                    Ok(groups) => groups,
                    Err(error) => {
                        launch_error = Some(error.to_string());
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            if launch_error.is_none()
                && let Some(raw) = &step.window
                && let Some(detail) = window_wait_for_groups(&t.record, &groups, raw, windows)?
            {
                t.step_wait("window", Some(detail));
                persist(&t, &guard)?;
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
            } else if launch_error.is_none() {
                match host.occupancy(&t.record.scope.path, &t.record.project_root)? {
                    Occupancy::Free => {}
                    Occupancy::Blocked(_) => {
                        t.step_wait("takeover", None);
                        persist(&t, &guard)?;
                        return Ok(Decision::Stop);
                    }
                    Occupancy::Occupied(_) if !step.takeover.unwrap_or_default() => {
                        t.step_wait("occupied", None);
                        persist(&t, &guard)?;
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
            persist(&t, &guard)?;
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
            _ => AttemptKind::Script,
        }
    };
    let mut digests = BTreeMap::new();
    let prompt = agent_step.map(|step| {
        agent::compose(&t.record, paths, step, &mut digests).map_err(|error| error.to_string())
    });
    let head_start = if step.is_none() && kind == AttemptKind::Script {
        None
    } else {
        super::head(&t.record.scope.path)
    };
    t.now = Timestamp::now();
    if t.bound()? {
        admission.capacity = None;
        persist(&t, &guard)?;
        return Ok(Decision::Again);
    }
    if step.is_some() {
        t.close_wait();
        seen.clear();
        let visits = t.record.visits.entry(t.record.node.clone()).or_default();
        visits.total += 1;
        if let Some(key) = visits.keys.iter_mut().find(|v| v.key == t.record.key) {
            key.visits += 1;
        } else {
            visits.keys.push(KeyVisits {
                key: t.record.key.clone(),
                visits: 1,
            });
        }
        t.record.entered_at = t.now;
    }
    let seq = t.record.next_seq;
    t.record.next_seq += 1;
    t.record.attempt = Some(LiveAttempt {
        kind,
        lifecycle: step.and_then(|step| step.lifecycle).unwrap_or_default(),
        seq,
        node: t.record.node.clone(),
        started_at: t.now,
        host: agent::identity(),
        woken: false,
        asking: None,
        asked_ms: 0,
        judged: None,
        child: None,
        run: None,
        head_start,
        digests,
        ending: None,
        retry: t.record.check_retry_context.clone(),
    });
    persist(&t, &guard)?;
    write_context(&t.record, paths, spec)?;
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
    let Some(task) =
        t.record.task.as_ref().filter(|_| {
            !t.record.manual && step.throttle != Some(crate::config::ThrottleSwitch::Off)
        })
    else {
        admission.capacity = None;
        return Ok(None);
    };
    if let Some(wait) = t
        .record
        .wait
        .as_ref()
        .filter(|wait| wait.outcome == "capacity")
        && wait.gives_up_at.is_some_and(|at| t.now >= at)
    {
        let object = json!({
            "outcome":"capacity", "step":t.record.node, "reason":wait.detail,
            "kinds":wait.kinds, "waited_ms":t.now.duration_since(wait.first).as_millis().max(0)
        });
        t.engine_route(
            "capacity",
            object,
            step.on.capacity.as_deref().unwrap_or("@failed"),
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
                task: task.clone(),
                root: t.record.project_root.clone(),
                checkout: t.record.scope.path.clone(),
                disk: t.record.scope.path.clone(),
                workspace: crate::ids::WorkspaceId::from_project_root(&t.record.project_root),
                groups,
                gives_up_ms: t
                    .record
                    .wait
                    .as_ref()
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
        context["visit"] = json!(visits.for_key(record.key.as_ref()));
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
            "visit":visits.for_key(record.key.as_ref()), "visits":visits.total,
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

pub fn cancel(state: &StatePaths, id: &WorkflowRunId, host: &dyn AttemptHost) -> Result<bool> {
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
            },
            &observed,
        );
        if !busy {
            t.record.attempt.as_mut().expect("agent cancel").host = agent::identity();
        }
        persist(&t, &guard)?;
        let record = t.record;
        drop(guard);
        if !busy {
            agent::finish(state, &paths, &spec, &record, host, None)?;
        }
        return Ok(true);
    }
    if t.finish_script(
        Ending {
            outcome: AttemptOutcome::Canceled,
            cause: EndCause::Cancel,
            route: None,
        },
        None,
        None,
        None,
    )? {
        persist(&t, &guard)?;
        return Ok(true);
    }
    t.record.state = RunState::Canceled;
    t.record.terminal_at = Some(t.now);
    t.push(None, JournalEvent::Cancel);
    t.push(
        None,
        JournalEvent::Terminal {
            state: RunState::Canceled,
        },
    );
    persist(&t, &guard)?;
    Ok(true)
}

pub fn resume(
    state: &StatePaths,
    id: &WorkflowRunId,
    at: Option<&str>,
    extend: &[String],
) -> Result<()> {
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
        if let Some(node) = at
            && !spec.checks.contains_key(node)
            && !spec.steps.contains_key(node)
        {
            return Err(WorkflowErr::Refused(format!(
                "unknown sealed workflow node {node}"
            )));
        }
        if record.state == RunState::Active && (at.is_some() || extend.is_empty()) {
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
        if t.record.state == RunState::Parked {
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
        persist(&t, &guard)?;
        t.record
    };
    spawn(&record)
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
        workflow::write(&state.workflow(&record.id), &record, &guard)?;
        reassigned.push(record.id);
    }
    Ok(reassigned)
}

pub fn wake(state: &StatePaths, signal: &Signal) -> Result<()> {
    for record in workflow::list(state)? {
        let paths = state.workflow(&record.id);
        let result: Result<()> = (|| {
            let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
            let current = workflow::load(&paths)?;
            if current.state != RunState::Active {
                return Ok(());
            }
            let spec = spec::load(&paths.home)?;
            let Some(check) = spec.checks.get(&current.node) else {
                return Ok(());
            };
            let subscribed = check
                .subscriptions()?
                .into_iter()
                .any(|(selector, mut matches)| {
                    match arm::default_signal_match_key(&selector, &matches) {
                        Some("path") => {
                            matches.insert(
                                "path".into(),
                                current.scope.path.to_string_lossy().into_owned(),
                            );
                        }
                        Some("instance") if !matches.contains_key("board") => {
                            matches.insert(
                                "board".into(),
                                crate::harness::scratch::durable_board_path(&current.scope.path)
                                    .to_string_lossy()
                                    .into_owned(),
                            );
                        }
                        _ => {}
                    }
                    resolve_subscription(&selector, &matches, signal) == SignalResolution::Deliver
                });
            if !subscribed {
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
            if let Some(wait) = &mut t.record.wait {
                wait.recheck_at = None;
            }
            t.push(
                None,
                JournalEvent::Wake {
                    signal: signal.name.as_str().into(),
                },
            );
            persist(&t, &guard)?;
            drop(guard);
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
