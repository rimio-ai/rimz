use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;
use serde_json::json;

use super::{AttemptHost, Occupancy};
use super::{WorkflowSpec, script, spec, state::Transition};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::harness::schedule::when::WindowReadings;
use crate::ids::WorkflowRunId;
use crate::store::workflow::{
    self, AttemptKind, AttemptOutcome, Host, JournalEvent, KeyVisits, LiveAttempt, ParkReason,
    RunRecord, RunState, Scope, ScopeKind,
};

#[derive(Debug, thiserror::Error)]
pub enum WorkflowErr {
    #[error("workflow {workflow} already has run {run} for {}", scope.display())]
    Exists {
        run: WorkflowRunId,
        workflow: String,
        scope: std::path::PathBuf,
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
    #[error("{0}")]
    Refused(String),
}

type Result<T> = std::result::Result<T, WorkflowErr>;

mod agent;

pub fn create(
    state: &StatePaths,
    spec_dir: &Path,
    scope: &Path,
    project_root: &Path,
    prompt: Option<String>,
    task: Option<String>,
) -> Result<RunRecord> {
    let spec_dir = spec_dir.canonicalize()?;
    let bytes = std::fs::read(crate::disk::paths::workflow_spec_file(&spec_dir))?;
    let spec: WorkflowSpec = toml::from_str(
        std::str::from_utf8(&bytes).map_err(|e| WorkflowErr::Refused(e.to_string()))?,
    )
    .map_err(|e| WorkflowErr::Refused(format!("invalid workflow spec: {e}")))?;
    let name = spec_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| WorkflowErr::Refused("workflow directory needs a name".into()))?
        .to_owned();
    let machine = crate::config::MachineConfig::load_lenient();
    let definitions = crate::config::definitions::load(
        &crate::disk::paths::agents_home(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &machine.tiers,
        machine.harness.auto_compact,
    );
    let report = super::validate::validate_spec(&spec_dir, &name, &spec, &definitions);
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
    admit(&spec)?;
    if crate::workspace::record::read(&state.workspace_record).is_err() {
        return Err(WorkflowErr::Refused(format!(
            "no room at {}; run `rimz start` there first",
            project_root.display()
        )));
    }
    let scope = scope.canonicalize()?;
    let now = Timestamp::now();
    let record = RunRecord {
        version: 1,
        id: WorkflowRunId::new(),
        workflow: name,
        task,
        spec_dir,
        spec_digest: script::digest(&bytes),
        prompt,
        scope: Scope {
            kind: ScopeKind::Worktree,
            path: scope,
        },
        project_root: project_root.into(),
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
        prev_attempt: None,
        terminal_at: None,
        check_retry: 0,
        check_retry_context: None,
    };
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
        if let Some(existing) = workflow::list(state)?.into_iter().find(|r| {
            !r.state.is_terminal() && r.workflow == record.workflow && r.scope == record.scope
        }) {
            return Err(WorkflowErr::Exists {
                run: existing.id,
                workflow: record.workflow,
                scope: record.scope.path,
            });
        }
        workflow::initialize(state, &record, &bytes, &guard)?;
    }
    spawn(&record)?;
    Ok(record)
}

fn admit(spec: &WorkflowSpec) -> Result<()> {
    if spec.workflow.scope == ScopeKind::Project {
        return Err(WorkflowErr::Refused(
            "this build does not run scope = project yet".into(),
        ));
    }
    Ok(())
}

fn spawn(record: &RunRecord) -> Result<()> {
    crate::child_process::spawn_detached_reaped(
        crate::proc::rimz_exe(),
        |command| {
            command.args(super::advance_args(
                &record.project_root,
                Some(&record.id),
                None,
            ));
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

fn window_wait_for_profile(
    record: &RunRecord,
    profile: &str,
    raw: &str,
    windows: &WindowReadings<'_>,
) -> Result<Option<String>> {
    let workspace = crate::workspace::WorkspaceResolver::resolve(
        &record.scope.path,
        Some(record.project_root.clone()),
    )
    .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let login = crate::store::writer::LaunchLogin::RoomDefault;
    let resolved = crate::harness::plan::resolve_single_agent_launch(profile, &workspace, &login)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let state = StatePaths::for_project_root(&record.project_root)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let runtime = crate::disk::paths::RuntimePaths::for_state(&state)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let login = crate::harness::schedule::runner::task_login(&login, resolved.kind(), &runtime)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    window_wait(
        raw,
        &record.scope.path,
        &crate::ids::AgentKind::new_unchecked(resolved.kind()),
        login.as_ref().map(|login| login.key().name).as_ref(),
        windows,
    )
}

enum Decision {
    Stop,
    Again,
    Evaluate(Box<RunRecord>),
    Launch(Box<(RunRecord, std::result::Result<String, String>)>),
    End(Box<RunRecord>),
    Evict(Box<RunRecord>),
}

pub fn advance(state: &StatePaths, id: &WorkflowRunId, host: &dyn AttemptHost) -> Result<()> {
    let runtime = crate::disk::paths::RuntimePaths::for_state(state)
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?;
    let paths = state.workflow(id);
    let spec = spec::load(&paths.home)?;
    let mut seen = BTreeSet::new();
    let mut eviction = None;
    loop {
        let windows = WindowReadings::new(Some(&runtime), Timestamp::now());
        let decision = prepare(
            state,
            &paths,
            &spec,
            &mut seen,
            host,
            &windows,
            &mut eviction,
        )?;
        let record = match decision {
            Decision::Stop => return Ok(()),
            Decision::Again => continue,
            Decision::Evaluate(record) => record,
            Decision::Evict(record) => {
                let error = host.evict(&record.scope.path, &record.project_root).err();
                eviction = Some((record.next_seq, error.map(|error| error.to_string())));
                continue;
            }
            Decision::Launch(launch) => {
                let (record, prompt) = *launch;
                if agent::launch(state, &paths, &spec, &record, prompt, host)? {
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
                t.finish_check(seq, result.object, result.digests, result.stderr_tail)?;
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
                if let JournalEvent::Evaluation {
                    digests,
                    cost_usd,
                    retry,
                    ..
                } = &mut entry.event
                {
                    *digests = result.digests.clone();
                    *cost_usd = result.cost_usd;
                    *retry = record
                        .attempt
                        .as_ref()
                        .and_then(|attempt| attempt.retry.clone());
                }
            }
        }
        persist(&t, &guard)?;
        if t.record.state != RunState::Active || (t.record.wait.is_some() && !recheck_woken) {
            return Ok(());
        }
    }
}

fn prepare(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    seen: &mut BTreeSet<String>,
    host: &dyn AttemptHost,
    windows: &WindowReadings<'_>,
    eviction: &mut Option<(u64, Option<String>)>,
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
    if t.record
        .attempt
        .as_ref()
        .is_some_and(|a| a.kind == AttemptKind::Agent)
    {
        return agent::prepare_end(&mut t, host, &guard);
    }
    if t.bound()? {
        persist(&t, &guard)?;
        return Ok(
            if t.record.state != RunState::Active || t.record.wait.is_some() {
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
        t.abandon_check("canceled");
        persist(&t, &guard)?;
        return Ok(Decision::Again);
    }
    if t.record.recheck_at.is_some_and(|at| at > t.now) {
        if let Some(wait) = &mut t.record.wait {
            wait.last = t.now;
            wait.count += 1;
        }
        persist(&t, &guard)?;
        return Ok(Decision::Stop);
    }
    let mut eviction_error = None;
    if let Some(step) = spec.steps.get(&t.record.node) {
        let visits = t.record.visits.get(&t.record.node).map_or(0, |v| v.total);
        let max = step
            .max_visits
            .map(|max| max.saturating_add(t.record.visit_grant));
        if let Some(max) = max
            && visits >= max
        {
            if !seen.insert(t.record.node.clone()) {
                return Ok(Decision::Stop);
            }
            t.engine_route(
                "cap",
                json!({"outcome":"cap", "step":t.record.node, "visits":visits, "max":max}),
                step.on.cap.as_deref().unwrap_or("@user"),
                ParkReason::Cap,
            )?;
            persist(&t, &guard)?;
            return Ok(Decision::Again);
        }
        if let Some(raw) = &step.window
            && let Some(detail) = window_wait_for_profile(&t.record, &step.agent, raw, windows)?
        {
            t.step_wait("window", Some(detail));
            persist(&t, &guard)?;
            return Ok(Decision::Stop);
        }
        if let Some((_, error)) = eviction.take().filter(|(seq, _)| *seq == t.record.next_seq) {
            // A confirmed stop admits only this launch; later visits read occupancy afresh.
            eviction_error = error;
        } else {
            match host.occupancy(&t.record.scope.path, &t.record.project_root)? {
                Occupancy::Free => {}
                Occupancy::Blocked(_) => {
                    t.step_wait("takeover", None);
                    persist(&t, &guard)?;
                    return Ok(Decision::Stop);
                }
                Occupancy::Occupied(_) if !step.takeover => {
                    t.step_wait("occupied", None);
                    persist(&t, &guard)?;
                    return Ok(Decision::Stop);
                }
                Occupancy::Occupied(_) => return Ok(Decision::Evict(Box::new(t.record))),
            }
        }
    } else if spec.checks.contains_key(&t.record.node) {
        let check = &spec.checks[&t.record.node];
        if let (Some(raw), Some(profile)) = (&check.window, &check.agent)
            && let Some(detail) = window_wait_for_profile(&t.record, profile, raw, windows)?
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
    let kind = if step.is_some() {
        AttemptKind::Agent
    } else if spec.checks[&t.record.node].agent.is_some() {
        AttemptKind::Judge
    } else {
        AttemptKind::Script
    };
    let mut digests = BTreeMap::new();
    let prompt = step.map(|step| {
        agent::compose(&t.record, paths, step, &mut digests).map_err(|error| error.to_string())
    });
    let head_start = if kind == AttemptKind::Script {
        None
    } else {
        agent::head(&t.record.scope.path)
    };
    t.now = Timestamp::now();
    if t.bound()? {
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
        seq,
        node: t.record.node.clone(),
        started_at: t.now,
        host: agent::identity(),
        woken: false,
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
        if let Some(reason) = eviction_error {
            prompt = Err(reason);
        }
        return Ok(Decision::Launch(Box::new((t.record, prompt))));
    }
    Ok(Decision::Evaluate(Box::new(t.record)))
}

fn write_context(record: &RunRecord, paths: &WorkflowPaths, spec: &WorkflowSpec) -> Result<()> {
    // Context is published only for the recorded attempt returned by prepare.
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
            visits
                .keys
                .iter()
                .find(|v| v.key == record.key)
                .map_or(0, |v| v.visits)
        );
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
            Some(run) => host.observe(run)?,
            None => agent::empty_observation(),
        };
        t.stage_agent_end(
            workflow::Ending {
                outcome: AttemptOutcome::Canceled,
                cause: workflow::EndCause::Cancel,
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
    t.abandon_check("canceled");
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

pub fn wake(state: &StatePaths, signal: &str) -> Result<()> {
    for record in workflow::list(state)? {
        let paths = state.workflow(&record.id);
        let result: Result<()> = (|| {
            let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
            let current = workflow::load(&paths)?;
            if current.state != RunState::Active {
                return Ok(());
            }
            let spec = spec::load(&paths.home)?;
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
                    signal: signal.into(),
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
