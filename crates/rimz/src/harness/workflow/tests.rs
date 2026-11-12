use super::*;
use crate::config::definitions::LoadedDefinitions;
use crate::store::run::RunStatus;
use crate::store::workflow::{AttemptRun, Children};
use std::cell::{Cell, RefCell};
use std::fs;
use std::rc::Rc;

pub(in crate::harness) struct FakeHost {
    pub(super) occupancy: Rc<RefCell<Occupancy>>,
    pub(in crate::harness) launched: RefCell<Vec<StepLaunch>>,
    pub(in crate::harness) observed: Rc<RefCell<Observed>>,
    pub(in crate::harness) failure: RefCell<Option<LaunchFailure>>,
    /// Report the injected failure as one that started nothing.
    unstarted: Cell<bool>,
    /// Fail the launch with a host error after it opened, giving the engine no verdict.
    host_error: Cell<bool>,
    evict_error: Cell<bool>,
    stale_after_evict: Cell<bool>,
    /// The occupancy the checkout reaches while a launch is prepared, after its first read.
    arrives_at_launch: RefCell<Option<Occupancy>>,
    stops: Cell<usize>,
    detaches: Cell<usize>,
    evictions: Cell<usize>,
    occupancies: Cell<usize>,
    observations: Cell<usize>,
    turn_observations: Cell<usize>,
    on_stop: RefCell<Option<Box<dyn Fn()>>>,
    on_detach: RefCell<Option<Box<dyn Fn()>>>,
    on_launch: RefCell<Option<Box<dyn Fn()>>>,
    on_observe: RefCell<Option<Box<dyn Fn()>>>,
    pub(super) on_occupancy: RefCell<Option<Box<dyn Fn()>>>,
    pushes: RefCell<std::collections::BTreeMap<crate::ids::MessageId, String>>,
    pub(in crate::harness) push_calls: RefCell<Vec<crate::ids::MessageId>>,
    push_error: Cell<bool>,
    on_push: RefCell<Option<Box<dyn Fn()>>>,
    forwards: RefCell<Vec<(crate::ids::MessageId, String)>>,
    forward_calls: Cell<usize>,
    forward_error: Cell<bool>,
    pub(super) messages: RefCell<Vec<(String, crate::ids::MessageId, String)>>,
    pub(in crate::harness) message_sessions: RefCell<Vec<Option<crate::ids::AgentSessionId>>>,
    pub(in crate::harness) message_answer: RefCell<Result<Result<String, String>, String>>,
    pub(super) on_message: RefCell<Option<Box<dyn Fn()>>>,
    pub(super) retired: RefCell<Vec<crate::ids::MessageId>>,
    /// The session the leader's supervised run recorded.
    pub(in crate::harness) leader_session: RefCell<Option<crate::ids::AgentSessionId>>,
}

/// Engine internals the loop scheduler's compiled-task tests drive on the graphs it compiles;
/// those tests live beside the compiler, since workflow imports nothing from the scheduler.
pub(in crate::harness) fn resolved_edges(spec: &WorkflowSpec, node: &str) -> Option<Vec<Edge>> {
    spec.resolved_edges(node)
}

/// The prompt each of `step`'s subscriptions hands the step.
pub(in crate::harness) fn subscription_prompts(step: &Step) -> Vec<Option<&str>> {
    step.subscribe
        .iter()
        .flatten()
        .map(spec::Wake::prompt)
        .collect()
}

/// Finish the judge attempt `1` that `record` holds with each answer in turn, each against the
/// same attempt, returning the record with the outcome a terminal write of the last answer's
/// journal entries gives it.
pub(in crate::harness) fn finish_judge(
    spec: &WorkflowSpec,
    paths: &crate::disk::paths::WorkflowPaths,
    record: crate::store::workflow::RunRecord,
    results: &[(Result<serde_json::Value, String>, &'static str)],
) -> crate::store::workflow::RunRecord {
    let mut t = state::Transition {
        now: record.entered_at,
        record,
        spec,
        paths,
        entries: Vec::new(),
    };
    let attempt = t.record.attempt.clone();
    for (object, failure) in results {
        t.record.attempt.clone_from(&attempt);
        t.finish_judge(
            1,
            script::Evaluation {
                object: object.clone(),
                failure,
                stderr_tail: None,
                limited: None,
                cost_usd: None,
            },
        )
        .unwrap();
    }
    if t.record.state.is_terminal() {
        let acted = engine::entered_act(spec, &t.record);
        t.record.outcome = Some(engine::outcome(&t.record, &t.entries, acted));
    }
    t.record
}

/// Whether `outcome` out of judge `node` declines the act.
pub(in crate::harness) fn declines(spec: &WorkflowSpec, node: &str, outcome: &str) -> bool {
    script::declines(spec, node, outcome)
}

/// Evaluate the judge `record` holds: its failure kind and its answer.
pub(in crate::harness) fn evaluate(
    state: &crate::disk::paths::StatePaths,
    spec: &WorkflowSpec,
    record: &crate::store::workflow::RunRecord,
) -> (&'static str, Result<serde_json::Value, String>) {
    let judged = script::evaluate(state, &state.workflow(&record.id), spec, record);
    (judged.failure, judged.object)
}

impl FakeHost {
    pub(in crate::harness) fn new() -> Self {
        Self {
            occupancy: Rc::new(RefCell::new(Occupancy::Free)),
            launched: RefCell::new(Vec::new()),
            observed: Rc::new(RefCell::new(Observed {
                status: RunStatus::Running,
                ask: None,
                limited: None,
                stopped: None,
                children: Children::default(),
                cost_usd: Some(0.25),
                turn: None,
            })),
            failure: RefCell::new(None),
            unstarted: Cell::new(false),
            host_error: Cell::new(false),
            evict_error: Cell::new(false),
            stale_after_evict: Cell::new(false),
            arrives_at_launch: RefCell::new(None),
            stops: Cell::new(0),
            detaches: Cell::new(0),
            evictions: Cell::new(0),
            occupancies: Cell::new(0),
            observations: Cell::new(0),
            turn_observations: Cell::new(0),
            on_stop: RefCell::new(None),
            on_detach: RefCell::new(None),
            on_launch: RefCell::new(None),
            on_observe: RefCell::new(None),
            on_occupancy: RefCell::new(None),
            pushes: RefCell::new(std::collections::BTreeMap::new()),
            push_calls: RefCell::new(Vec::new()),
            push_error: Cell::new(false),
            on_push: RefCell::new(None),
            forwards: RefCell::new(Vec::new()),
            forward_calls: Cell::new(0),
            forward_error: Cell::new(false),
            messages: RefCell::new(Vec::new()),
            message_sessions: RefCell::new(Vec::new()),
            message_answer: RefCell::new(Ok(Ok("@planner".into()))),
            on_message: RefCell::new(None),
            retired: RefCell::new(Vec::new()),
            leader_session: RefCell::new(None),
        }
    }

    /// The checkout's occupancy without counting a read, `excluded` left out.
    fn current_occupancy(&self, excluded: &[String]) -> Occupancy {
        if self.stale_after_evict.get() && self.evictions.get() > 0 {
            return Occupancy::Blocked(vec![(
                "stale occupant".into(),
                crate::harness::takeover::OpenTurn::Working,
            )]);
        }
        let mut occupancy = match &*self.occupancy.borrow() {
            Occupancy::Free => Occupancy::Free,
            Occupancy::Occupied(handles) => Occupancy::Occupied(handles.clone()),
            Occupancy::Blocked(blockers) => Occupancy::Blocked(blockers.clone()),
        };
        match &mut occupancy {
            Occupancy::Occupied(handles) if !handles.is_empty() => {
                handles.retain(|handle| !excluded.contains(handle));
                if handles.is_empty() {
                    return Occupancy::Free;
                }
            }
            Occupancy::Blocked(blockers) if !blockers.is_empty() => {
                blockers.retain(|(handle, _)| !excluded.contains(handle));
                if blockers.is_empty() {
                    return Occupancy::Free;
                }
            }
            _ => {}
        }
        occupancy
    }
}

impl AttemptHost for FakeHost {
    fn occupancy(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        excluded: &[String],
    ) -> Result<Occupancy, WorkflowErr> {
        self.occupancies.set(self.occupancies.get() + 1);
        if let Some(check) = &*self.on_occupancy.borrow() {
            check();
        }
        Ok(self.current_occupancy(excluded))
    }

    fn launch(&self, step: StepLaunch) -> Result<Result<AttemptRun, NotLaunched>, WorkflowErr> {
        if let Some(arrived) = self.arrives_at_launch.borrow_mut().take() {
            *self.occupancy.borrow_mut() = arrived;
        }
        // As the CLI host does: read the checkout again once the launch is prepared.
        if let Some(evict) = &step.evict {
            match self.current_occupancy(&evict.excluded) {
                Occupancy::Free => {}
                Occupancy::Occupied(_) if step.takeover => {
                    self.evictions.set(self.evictions.get() + 1);
                    if self.evict_error.get() {
                        if let Some(check) = &*self.on_launch.borrow() {
                            check();
                        }
                        return Ok(Err(NotLaunched::Unstarted(LaunchFailure {
                            outcome: AttemptOutcome::Died,
                            reason: "occupant did not stop".into(),
                        })));
                    }
                    *self.occupancy.borrow_mut() = Occupancy::Free;
                }
                taken => return Ok(Err(NotLaunched::Taken(taken))),
            }
        }
        self.launched.borrow_mut().push(step);
        if let Some(check) = &*self.on_launch.borrow() {
            check();
        }
        if self.host_error.get() {
            return Err(WorkflowErr::Refused("launch opened no leader run".into()));
        }
        Ok(match self.failure.borrow_mut().take() {
            Some(failure) if self.unstarted.get() => Err(NotLaunched::Unstarted(failure)),
            Some(failure) => Err(failure.into()),
            None => Ok(AttemptRun {
                run_id: crate::ids::RunId::new(),
                handle: "@builder".into(),
            }),
        })
    }

    fn observe(&self, _: &AttemptRun, observation: Observation) -> Result<Observed, WorkflowErr> {
        self.observations.set(self.observations.get() + 1);
        if let Some(check) = &*self.on_observe.borrow() {
            check();
        }
        let mut observed = self.observed.borrow().clone();
        match observation {
            Observation::Attempt => observed.turn = None,
            Observation::LeaderTurn => self.turn_observations.set(self.turn_observations.get() + 1),
        }
        Ok(observed)
    }

    fn push(
        &self,
        _: &AttemptRun,
        push: &crate::store::workflow::PushBack,
        text: &str,
    ) -> Result<Pushed, WorkflowErr> {
        self.push_calls.borrow_mut().push(push.message.clone());
        let callback = self.on_push.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
        if self.push_error.replace(false) {
            return Err(WorkflowErr::Refused("push interrupted".into()));
        }
        self.pushes
            .borrow_mut()
            .entry(push.message.clone())
            .or_insert_with(|| text.into());
        Ok(Pushed { queued: true })
    }

    fn forward(
        &self,
        _: &AttemptRun,
        message: &crate::ids::MessageId,
        text: &str,
    ) -> Result<(), WorkflowErr> {
        self.forward_calls.set(self.forward_calls.get() + 1);
        if self.forward_error.get() {
            return Err(WorkflowErr::Refused("leader left the room".into()));
        }
        self.forwards
            .borrow_mut()
            .push((message.clone(), text.into()));
        Ok(())
    }

    fn message(
        &self,
        _: &std::path::Path,
        target: &str,
        session: Option<&crate::ids::AgentSessionId>,
        message: &crate::ids::MessageId,
        text: &str,
    ) -> Result<Result<String, String>, WorkflowErr> {
        self.message_sessions.borrow_mut().push(session.cloned());
        self.messages
            .borrow_mut()
            .push((target.into(), message.clone(), text.into()));
        let callback = self.on_message.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
        self.message_answer
            .borrow()
            .clone()
            .map_err(WorkflowErr::Refused)
    }

    fn retire(&self, message: &crate::ids::MessageId) -> Result<(), WorkflowErr> {
        self.retired.borrow_mut().push(message.clone());
        Ok(())
    }

    fn detach(&self, _: &AttemptRun) -> Result<Children, WorkflowErr> {
        if let Some(callback) = self.on_detach.borrow_mut().take() {
            callback();
        }
        self.detaches.set(self.detaches.get() + 1);
        Ok(self.observed.borrow().children.clone())
    }

    fn stop(&self, _: &AttemptRun) -> Result<Children, WorkflowErr> {
        if let Some(check) = &*self.on_stop.borrow() {
            check();
        }
        self.stops.set(self.stops.get() + 1);
        Ok(self.observed.borrow().children.clone())
    }

    /// The library's own lookup, so engine tests find the run a dead launcher left.
    fn orphan(
        &self,
        state: &crate::disk::paths::StatePaths,
        attempt: &WorkflowAttemptRef,
    ) -> Result<Option<AttemptRun>, WorkflowErr> {
        orphan(state, attempt)
    }

    fn leader_session(&self, _: &AttemptRun) -> Option<crate::ids::AgentSessionId> {
        self.leader_session.borrow().clone()
    }

    /// The library's own resolver, so engine tests exercise what a launch admits.
    fn start_groups(
        &self,
        record: &crate::store::workflow::RunRecord,
        agent: &str,
        account: Option<&crate::ids::LoginName>,
    ) -> Result<Vec<crate::harness::throttle::StartGroup>, WorkflowErr> {
        launch_groups(record, agent, account)
    }
}

mod attempts;
mod commands;
mod conditions;
mod graph;
mod inspection;
mod judges;
mod message;
mod sleep;
mod tolerance;
mod turns;

#[test]
fn advance_arguments_address_a_run_or_a_room_signal_without_shell_parsing() {
    let root = std::path::Path::new("/project with spaces");
    let id = crate::ids::WorkflowRunId::new();
    let expected: Vec<std::ffi::OsString> = [
        "--root",
        "/project with spaces",
        "workflow",
        "advance",
        id.as_str(),
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    assert_eq!(advance_args(root, Some(&id), None, None), expected);
    let signal = crate::harness::signal::Signal {
        name: "pr.ready".parse().unwrap(),
        payload: serde_json::from_value(serde_json::json!({
            "path": "/another checkout", "note": "a quote \" and\nnewline"
        }))
        .unwrap(),
        source: crate::store::event::SignalSource::Forge,
        watch: None,
    };
    let expected: Vec<std::ffi::OsString> = [
        "--root",
        "/project with spaces",
        "workflow",
        "advance",
        "--wake-signal",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let args = advance_args(root, None, Some(&signal), None);
    assert_eq!(args[..5], expected);
    assert_eq!(
        serde_json::from_str::<crate::harness::signal::Signal>(args[5].to_str().unwrap()).ok(),
        Some(signal)
    );
}

const VALID: &str = r#"
[workflow]
on-break = "@failed"
scope = "worktree"
start = "pick"
[checks.pick]
run = "python3 checks/pick.py"
on = { ready = "build", done = "@done", pending = "@wait" }
timeout = "1d"
[steps.build]
agent = "claude"
prompt-file = "prompts/build.md"
lifecycle = "turn"
then = "pick"
max-visits = 3
"#;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("example");
    fs::create_dir_all(dir.join("checks")).unwrap();
    fs::create_dir_all(dir.join("prompts")).unwrap();
    fs::write(dir.join("checks/pick.py"), "print('{}')").unwrap();
    fs::write(dir.join("prompts/build.md"), "Build it").unwrap();
    (home, dir)
}

#[test]
fn then_lists_validate_every_entry_and_direct_link() {
    let (_home, dir) = fixture();
    let base = VALID
        .replace("lifecycle = \"turn\"", "lifecycle = 'until'")
        .replace("then = \"pick\"", "then = ['pick', 'review']")
        .replace("done = \"@done\"", "done = 'review'")
        + "[checks.review]\nrun='true'\non.ok='@done'\n";
    for (text, error, warning) in [
        (base.clone(), None, false),
        (
            base.replace("['pick', 'review']", "[]"),
            Some("nonempty"),
            false,
        ),
        (
            base.replace("['pick', 'review']", "['pick', 'build']"),
            Some("must name a check"),
            false,
        ),
        (
            base.replace("done = 'review'", "done = '@done'"),
            Some("direct route"),
            false,
        ),
        (
            base.replace("run='true'\non.ok='@done'", "sleep='1m'\nthen='@done'"),
            Some("cannot use a sleep"),
            false,
        ),
        (
            base.replace("lifecycle = 'until'", "lifecycle = 'turn'"),
            None,
            true,
        ),
        (
            base.replace("lifecycle = 'until'", "lifecycle = 'turn'")
                .replace("['pick', 'review']", "['pick']"),
            None,
            false,
        ),
        (
            base.replace("lifecycle = 'until'", "lifecycle = 'stay'"),
            None,
            true,
        ),
        (
            base.replace("agent = \"claude\"", "shell='true'\ntimeout='1m'"),
            None,
            true,
        ),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        match error {
            Some(message) => assert!(
                report.errors.iter().any(
                    |issue| issue.path == "steps.build.then" && issue.message.contains(message)
                ),
                "{report:?}"
            ),
            None => assert!(report.errors.is_empty(), "{report:?}"),
        }
        assert_eq!(
            report
                .warnings
                .iter()
                .any(|issue| issue.path == "steps.build.then"
                    && issue
                        .message
                        .contains("only an until step keeps its agent open")),
            warning,
            "{report:?}"
        );
        if let Ok(spec) = load(&dir) {
            assert!(
                spec.ignored
                    .iter()
                    .all(|issue| issue.path != "steps.build.then"),
                "{:?}",
                spec.ignored
            );
        }
    }
}

#[test]
fn delays_between_listed_checks_are_ignored_but_closing_delays_remain() {
    let (_home, dir) = fixture();
    let text = VALID
        .replace("lifecycle = \"turn\"", "lifecycle = 'until'")
        .replace("then = \"pick\"", "then = ['pick', 'review']")
        .replace("done = \"@done\"", "done = {to='review',delay='1m'}")
        + "[checks.review]\nrun='true'\non.ok={to='@done',delay='30m'}\n";
    fs::write(dir.join("workflow.toml"), &text).unwrap();
    let spec = load(&dir).unwrap();
    assert!(spec.checks["pick"].on["done"].delay().is_none());
    assert_eq!(spec.checks["review"].on["ok"].delay(), Some("30m"));
    assert!(
        spec.ignored
            .iter()
            .any(|issue| issue.path == "checks.pick.on.done.delay"
                && issue.message.contains("closes the held session")),
        "{:?}",
        spec.ignored
    );
    let land = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompts/build.md'\nthen=['ruled','ruled-read']\n[checks.ruled]\nrun='true'\non={read='ruled-read',settled={to='@done',delay='30m'}}\n[checks.ruled-read]\nrun='true'\non.settled={to='@done',delay='30m'}\n";
    fs::write(dir.join("workflow.toml"), land).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
    let spec = load(&dir).unwrap();
    assert_eq!(spec.checks["ruled"].on["settled"].delay(), Some("30m"));
    assert_eq!(spec.checks["ruled-read"].on["settled"].delay(), Some("30m"));
}

#[test]
fn validation_rejects_unanswerable_resets_and_requires_a_total_after_cleanup() {
    let (_home, dir) = fixture();
    let text = format!(
        "{}[checks.after]\nrun='true'\non.ok='@done'\n",
        VALID.replace("then = \"pick\"", "then='after'")
    );
    let actual = [
        ("max-visits=3", "{limit=2,reset-on='missing'}"),
        ("max-visits=3", "{limit=2,reset-on='timeout'}"),
        ("max-visits=3", "{limit=2,reset-on='unknown'}"),
        ("", "{limit=2,reset-on='done'}"),
        ("max-visits=0", "{limit=2,reset-on='done'}"),
        ("max-visits=3", "{limit=0,reset-on='missing'}"),
        ("max-visits=3", "{limit=3,reset-on='missing'}"),
    ]
    .map(|(total, cap)| {
        fs::write(
            dir.join("workflow.toml"),
            text.replace(
                "max-visits = 3",
                &format!("{total}\nmax-visits-per-outcome={cap}"),
            ),
        )
        .unwrap();
        validate(&dir, &LoadedDefinitions::default(), None)
            .errors
            .into_iter()
            .map(|issue| issue.path)
            .collect::<Vec<_>>()
    });
    let path = "steps.build.max-visits-per-outcome.reset-on".to_owned();
    assert_eq!(
        actual,
        [
            vec![path.clone()],
            vec![path.clone()],
            vec![path.clone()],
            vec![path.clone()],
            vec![path],
            vec![],
            vec![],
        ]
    );
}

#[test]
fn validation_warns_for_one_inbound_outcome_unless_reset_or_start() {
    let (_home, dir) = fixture();
    let capped = format!("{VALID}max-visits-per-outcome=2\n");
    let actual = [
        capped.clone(),
        capped.replace(
            "max-visits-per-outcome=2",
            "max-visits-per-outcome={limit=2,reset-on='done'}",
        ),
        capped.replace("ready = \"build\"", "ready = \"build\", again = \"build\""),
        capped.replace("start = \"pick\"", "start='build'"),
        capped
            .replace("run = \"python3 checks/pick.py\"", "shell='true'")
            .replace(
                "on = { ready = \"build\", done = \"@done\", pending = \"@wait\" }",
                "on={pass='build',fail='@done'}",
            ),
        capped.replace(
            "run = \"python3 checks/pick.py\"",
            "agent='claude'\nsummary='Review the work'\nprompt-file='prompts/build.md'",
        ),
        format!("{capped}[checks.other]\nrun='true'\non.ready='build'\n"),
    ]
    .map(|text| {
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{report:?}");
        let warnings = report
            .warnings
            .iter()
            .filter(|issue| issue.path == "steps.build.max-visits-per-outcome")
            .collect::<Vec<_>>();
        (
            warnings.len(),
            warnings.iter().all(|issue| {
                issue.message.contains("repeats max-visits")
                    && issue.message.contains("unless a check answers keys")
            }),
        )
    });
    assert_eq!(
        actual,
        [
            (1, true),
            (0, true),
            (0, true),
            (0, true),
            (1, true),
            (1, true),
            (1, true)
        ]
    );
}

#[test]
fn validation_carries_check_and_step_summaries() {
    let (_home, dir) = fixture();
    let text = VALID
        .replace(
            "[checks.pick]",
            "[checks.pick]\nsummary = 'Choose the next item'",
        )
        .replace(
            "[steps.build]",
            "[steps.build]\nsummary = ' Build the chosen item '",
        );
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
    let rows = serde_json::to_value(&report.rows).unwrap();
    assert_eq!(rows[0]["summary"], "Choose the next item");
    assert_eq!(rows[1]["summary"], " Build the chosen item ");
}

#[test]
fn validation_carries_workflow_summary() {
    let (_home, dir) = fixture();
    let summary = " Build the next item, then check it ";
    let text = VALID.replace("[workflow]", &format!("[workflow]\nsummary = '{summary}'"));
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report.errors.is_empty(),
        "workflow summaries must load: {report:?}"
    );
    assert_eq!(
        load(&dir).unwrap().workflow.summary.as_deref(),
        Some(summary)
    );
}

#[test]
fn validation_ignores_empty_or_multiline_summaries_at_the_field() {
    let (_home, dir) = fixture();
    let mut actual = Vec::new();
    for (kind, node) in [
        ("workflow", None),
        ("checks", Some("pick")),
        ("steps", Some("build")),
    ] {
        for summary in [
            "",
            " \t ",
            "first\nsecond",
            "first\rsecond",
            "first\r\nsecond",
            "line\n",
        ] {
            let mut text: toml::Value = toml::from_str(VALID).unwrap();
            let table = match node {
                Some(node) => &mut text[kind][node],
                None => &mut text[kind],
            };
            table
                .as_table_mut()
                .unwrap()
                .insert("summary".into(), summary.into());
            fs::write(dir.join("workflow.toml"), toml::to_string(&text).unwrap()).unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let path = match node {
                Some(node) => format!("{kind}.{node}.summary"),
                None => format!("{kind}.summary"),
            };
            let ignored = report.errors.is_empty()
                && report.warnings.iter().any(|issue| {
                    issue.path == path && issue.message.contains("a summary is one nonempty line")
                });
            actual.push((path, summary, ignored, format!("{report:?}")));
        }
    }
    assert_eq!(
        actual
            .iter()
            .map(|(_, _, refused, _)| *refused)
            .collect::<Vec<_>>(),
        vec![true; 18],
        "{actual:#?}"
    );
}

#[test]
fn validation_accepts_step_layouts() {
    let cases = [
        (
            "mixed layout",
            VALID.replace("agent = \"claude\"", "agent = 'claude,term'"),
            None,
        ),
        (
            "virtual cell",
            VALID.replace("agent = \"claude\"", "agent = 'claude-auto,term'"),
            None,
        ),
        (
            "stay",
            VALID.replace("lifecycle = \"turn\"", "lifecycle='stay'"),
            None,
        ),
        (
            "until",
            VALID.replace("lifecycle = \"turn\"", "lifecycle='until'"),
            None,
        ),
        (
            "no agent",
            VALID.replace("agent = \"claude\"", "agent = 'term'"),
            Some("steps.build.agent"),
        ),
        (
            "ambiguous leader",
            VALID.replace("agent = \"claude\"", "agent = 'claude,claude'"),
            Some("steps.build.agent"),
        ),
        (
            "team",
            VALID.replace("agent = \"claude\"", "agent = 'forge'"),
            Some("steps.build.agent"),
        ),
        (
            "team until",
            VALID
                .replace("agent = \"claude\"", "agent = 'forge'")
                .replace("lifecycle = \"turn\"", "lifecycle='until'"),
            Some("steps.build.agent"),
        ),
        (
            "team omitted lifecycle",
            VALID
                .replace("agent = \"claude\"", "agent = 'forge'")
                .replace("lifecycle = \"turn\"", ""),
            Some("steps.build.agent"),
        ),
        (
            "team stay",
            VALID
                .replace("agent = \"claude\"", "agent = 'forge'")
                .replace("lifecycle = \"turn\"", "lifecycle='stay'"),
            None,
        ),
        (
            "judge layout",
            VALID.replace(
                "run = \"python3 checks/pick.py\"",
                "agent='claude,codex'\nprompt-file='prompts/build.md'",
            ),
            Some("checks.pick.agent"),
        ),
    ];
    let results: Vec<_> = cases
        .into_iter()
        .map(|(label, text, path)| {
            let (_home, dir) = fixture();
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let mut definitions = LoadedDefinitions::default();
            definitions.teams.0.insert(
                "forge".into(),
                crate::config::Team {
                    layout: Some("claude".into()),
                    ..Default::default()
                },
            );
            let report = validate(&dir, &definitions, None);
            let valid = match path {
                Some(path) => report.errors.iter().any(|issue| {
                    issue.path == path
                        && match label {
                            label if label.starts_with("team") => {
                                issue.message
                                    == "example: steps.build.agent: a team runs only in a stay \
                                        step; set lifecycle = \"stay\""
                            }
                            "no agent" => issue.message.contains("add an agent"),
                            "ambiguous leader" => issue.message.contains("choose an unambiguous"),
                            _ => true,
                        }
                }),
                None => report.errors.is_empty(),
            };
            (label, valid, report.errors)
        })
        .collect();
    assert!(results.iter().all(|(_, valid, _)| *valid), "{results:#?}");
}

#[test]
fn until_warns_about_unreported_background_work_and_auto_continue() {
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        VALID
            .replace("agent = \"claude\"", "agent = 'codex'")
            .replace("lifecycle = \"turn\"", "lifecycle='until'"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(
        report
            .warnings
            .iter()
            .any(|issue| issue.message.contains("background reporting")),
        "{:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|issue| issue.message.contains("auto-continue")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn deadline_wait_does_not_require_a_check_timeout_or_allow_a_check_only_cycle() {
    let results = ["@done", "pick"].map(|target| {
        let spec: WorkflowSpec = toml::from_str(&format!(
            "[workflow]\non-break='@failed'\nscope='project'\nstart='pick'\non.deadline='@wait'\n\
             [checks.pick]\nrun='true'\non.ready='{target}'\n"
        ))
        .unwrap();
        let report = super::validate::validate_spec(
            std::path::Path::new("."),
            "review",
            &spec,
            &LoadedDefinitions::default(),
            None,
        );
        report
            .errors
            .into_iter()
            .map(|issue| issue.message)
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results,
        [
            vec![],
            vec!["review: checks.pick: check-only cycle has no @wait".to_owned()]
        ]
    );
}

#[test]
fn validation_accepts_check_or_step_starts_and_reports_invalid_targets() {
    let results = [
        "pick", "build", "missing", "@done", "@user", "@wait", "@failed",
    ]
    .map(|start| {
        let (_home, dir) = fixture();
        fs::write(
            dir.join("workflow.toml"),
            VALID.replace("start = \"pick\"", &format!("start = {start:?}")),
        )
        .unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        report
            .errors
            .into_iter()
            .map(|issue| (issue.path, issue.message))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results,
        [
            "pick", "build", "missing", "@done", "@user", "@wait", "@failed"
        ]
        .map(|start| {
            if matches!(start, "pick" | "build") {
                Vec::new()
            } else if start == "@user" {
                vec![(
                    "workflow.start".into(),
                    "workflow.start: example: workflow.start: @user is not a route target; route to a node, @done, or @failed, or set [workflow] on-break".into(),
                )]
            } else {
                vec![(
                    "workflow.start".into(),
                    format!("example: workflow.start: {start} must name a check or a step"),
                )]
            }
        })
    );
}

#[test]
fn validation_rejects_each_invalid_spec() {
    let shell = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pick'\n[checks.pick]\nshell='true'\non={pass='@done',fail='@failed'}\n";
    let cases = [
        (
            "shell requires pass",
            shell.replace("pass='@done',", ""),
            "checks.pick.on.pass",
        ),
        (
            "shell requires fail",
            shell.replace(",fail='@failed'", ""),
            "checks.pick.on.fail",
        ),
        (
            "empty shell",
            shell.replace("shell='true'", "shell='  '"),
            "checks.pick.shell",
        ),
        (
            "shell and run",
            shell.replace("shell='true'", "shell='true'\nrun='true'"),
            "checks.pick",
        ),
        (
            "shell and agent",
            shell.replace("shell='true'", "shell='true'\nagent='claude'"),
            "checks.pick",
        ),
        (
            "shell and cases",
            format!("{shell}[[checks.pick.cases]]\nwhen=['ci=passed']\noutcome='pass'\n"),
            "checks.pick",
        ),
        (
            "unknown start",
            VALID.replace("start = \"pick\"", "start = \"missing\""),
            "workflow.start",
        ),
        (
            "unknown edge",
            VALID.replace("ready = \"build\"", "ready = \"missing\""),
            "checks.pick.on.ready",
        ),
        (
            "duplicate node",
            format!("{VALID}\n[checks.build]\nrun = 'true'\non.ok = '@done'\n"),
            "build",
        ),
        (
            "no runner",
            VALID.replace("run = \"python3 checks/pick.py\"", ""),
            "checks.pick",
        ),
        (
            "two runners",
            VALID.replace(
                "run = \"python3 checks/pick.py\"",
                "run = 'true'\nagent = 'claude'",
            ),
            "checks.pick",
        ),
        (
            "judge prompt",
            VALID.replace("run = \"python3 checks/pick.py\"", "agent = 'claude'"),
            "checks.pick.prompt-file",
        ),
        (
            "then missing",
            VALID.replace("then = \"pick\"", ""),
            "steps.build.then",
        ),
        (
            "then step",
            VALID.replace("then = \"pick\"", "then = 'build'"),
            "steps.build.then",
        ),
        (
            "until then terminal",
            VALID.replace(
                "lifecycle = \"turn\"\nthen = \"pick\"",
                "lifecycle = \"until\"\nthen = '@done'",
            ),
            "steps.build.then",
        ),
        (
            "unknown profile",
            VALID.replace("agent = \"claude\"", "agent = 'missing-profile'"),
            "steps.build.agent",
        ),
        (
            "missing prompt",
            VALID.replace("prompts/build.md", "prompts/missing.md"),
            "steps.build.prompt-file",
        ),
        (
            "missing script",
            VALID.replace("checks/pick.py", "checks/missing.py"),
            "checks.pick.run",
        ),
        (
            "absolute path",
            VALID.replace("prompts/build.md", "/etc/passwd"),
            "steps.build.prompt-file",
        ),
        (
            "parent path",
            VALID.replace("prompts/build.md", "prompts/../build.md"),
            "steps.build.prompt-file",
        ),
        (
            "unbounded step cycle",
            VALID.replace("max-visits = 3", ""),
            "steps.build",
        ),
        (
            "unbounded capacity cycle",
            format!(
                "{}\n[checks.after]\nrun='true'\non.done='@done'\n",
                VALID
                    .replace("max-visits = 3", "")
                    .replace("then = \"pick\"", "then = 'after'\non.capacity = 'pick'")
            ),
            "steps.build",
        ),
        (
            "hot check cycle",
            VALID
                .replace("ready = \"build\"", "ready = 'pick'")
                .replace(", pending = \"@wait\"", ""),
            "checks.pick",
        ),
        (
            "wait timeout",
            VALID.replace("timeout = \"1d\"", ""),
            "checks.pick.timeout",
        ),
        (
            "timeout cannot wait",
            VALID.replace("timeout = \"1d\"", "timeout = '1s'").replace(
                "pending = \"@wait\"",
                "pending = '@wait', timeout = '@wait'",
            ),
            "checks.pick.on.timeout",
        ),
        (
            "step then cannot wait",
            VALID.replace("then = \"pick\"", "then = '@wait'"),
            "steps.build.then",
        ),
        (
            "cap cannot wait",
            format!("{VALID}\non.cap = '@wait'\n"),
            "steps.build.on.cap",
        ),
        (
            "asked cannot wait",
            format!("{VALID}\non.asked = '@wait'\n"),
            "steps.build.on.asked",
        ),
        (
            "cap cannot target itself",
            format!("{VALID}\non.cap = 'build'\n"),
            "steps.build.on.cap",
        ),
        (
            "bad duration",
            VALID.replace("timeout = \"1d\"", "timeout = 'tomorrow'"),
            "checks.pick.timeout",
        ),
        (
            "bad node name",
            VALID.replace("checks.pick", "checks.'bad name'"),
            "bad name",
        ),
        (
            "bad command",
            VALID.replace("python3 checks/pick.py", "'unclosed"),
            "checks.pick.run",
        ),
    ];
    let keyed_cases = [(
        "per-outcome cap does not bound a cycle",
        VALID.replace("max-visits = 3", "max-visits-per-outcome = 2"),
        "steps.build",
        "a per-outcome bound does not bound the cycle: a check can name a new outcome or key every time; max-visits is needed",
    )];
    let command = VALID
        .replace("agent = \"claude\"", "shell = 'true'\ntimeout = '1m'")
        .replace("prompt-file = \"prompts/build.md\"", "");
    let command_cases = [
        (
            "step no runner",
            VALID.replace("agent = \"claude\"", ""),
            "steps.build",
            "set exactly one of agent, run, shell, or message",
        ),
        (
            "step two runners",
            format!("{command}agent='claude'\n"),
            "steps.build",
            "set exactly one of agent, run, shell, or message",
        ),
        (
            "agent prompt required",
            VALID.replace("prompt-file = \"prompts/build.md\"", ""),
            "steps.build.prompt-file",
            "an agent step requires prompt-file",
        ),
        (
            "shell timeout required",
            command.replace("timeout = '1m'", ""),
            "steps.build.timeout",
            "a run or shell step requires timeout",
        ),
        (
            "run timeout required",
            command.replace("shell = 'true'\ntimeout = '1m'", "run = 'true'"),
            "steps.build.timeout",
            "a run or shell step requires timeout",
        ),
        (
            "run argv",
            command.replace("shell = 'true'", "run = \"'unclosed\""),
            "steps.build.run",
            "invalid run command",
        ),
        (
            "run confinement",
            command.replace("shell = 'true'", "run = 'sh /etc/passwd'"),
            "steps.build.run",
            "invalid workflow path",
        ),
        (
            "shell blank",
            command.replace("shell = 'true'", "shell = '  '"),
            "steps.build.shell",
            "set a nonempty shell command",
        ),
        (
            "takeover and share",
            format!("{VALID}takeover=true\nshare=true\n"),
            "steps.build.share",
            "takeover stops an idle occupant; share launches beside it",
        ),
    ];
    let mut missed = Vec::new();
    for (label, text, path, message) in cases
        .into_iter()
        .map(|(label, text, path)| (label, text, path, ""))
        .chain(keyed_cases)
        .chain(command_cases)
    {
        let (_home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        if !report.errors.iter().any(|issue| {
            (issue.path.contains(path) || issue.message.contains(path))
                && issue.message.contains(message)
        }) {
            missed.push(format!("{label}: expected {path}: {message}; {report:?}"));
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
}

#[test]
fn validation_warns_only_for_bounded_command_cycles_that_never_wait() {
    let (_home, dir) = fixture();
    let base = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n[steps.build]\nrun='true'\ntimeout='1m'\nthen='pick'\nmax-visits=3\n[checks.pick]\nrun='true'\non.fail='build'\n";
    let two = base.replace("on.fail='build'", "on.fail='pack'")
        + "[steps.pack]\nshell='true'\ntimeout='1m'\nthen='back'\nmax-visits=7\n[checks.back]\nrun='true'\non.fail='build'\n";
    let route = "run='true'\non.fail='build'";
    let condition = "cases=[{when=['pr=merged'],outcome='yes'}]\nelse='no'\n\
                     on={yes='@done',no='build',unknown='@wait'}\ntimeout='1m'";
    let held_condition = "cases=[{when=['pr=merged'],outcome='yes',for='1m'}]\nelse='no'\n\
                          on={yes='build',no='@done',unknown='@failed'}";
    let helper = "[steps.helper]\nagent='claude'\nprompt-file='prompts/build.md'\n\
                  then='back'\nmax-visits=4\n[checks.back]\nrun='true'\non.fail='build'\n";
    let judge = "agent='claude'\nprompt-file='prompts/build.md'\n\
                 summary='Review the work'\non.fail='build'";
    let list = base
        .replace("then='pick'", "then=['pick','after']")
        .replace("on.fail='build'", "on={fail='build',ok='after'}")
        + "[checks.after]\nrun='true'\non.ok='@done'\n";
    let cases = [
        (base.to_owned(), vec![("build", 3)]),
        (
            base.replace("[steps.build]\nrun=", "[steps.build]\nshell="),
            vec![("build", 3)],
        ),
        (two, vec![("build", 3), ("pack", 7)]),
        (
            base.replace("max-visits=3", "max-visits=3\nlimit={stagger='1m'}"),
            vec![("build", 3)],
        ),
        (list, vec![("build", 3)]),
        (
            base.replace("start='build'", "start='build'\non.deadline='@wait'"),
            vec![("build", 3)],
        ),
        (
            base.to_owned() + "on.pending='@wait'\ntimeout='1m'\n",
            vec![],
        ),
        (base.replace(route, "sleep='1m'\nthen='build'"), vec![]),
        (base.replace(route, condition), vec![]),
        (base.replace(route, held_condition), vec![("build", 3)]),
        (
            base.replace("on.fail='build'", "on.fail='helper'") + helper,
            vec![],
        ),
        (base.replace(route, judge), vec![("build", 3)]),
    ];
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (text, warnings) in cases {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{text}: {report:?}");
        actual.push(
            report
                .warnings
                .iter()
                .filter(|issue| {
                    issue
                        .message
                        .contains("cycle through command or message steps")
                })
                .map(|issue| (issue.path.clone(), issue.message.clone()))
                .collect::<Vec<_>>(),
        );
        let mut step_warnings = Vec::new();
        for (node, visits) in warnings {
            let message = format!(
                "example: steps.{node}: cycle through command or message steps never waits and \
                 can spend all {visits} visits in one advance; route it through a check that can \
                 wait (@wait) if that is not intended"
            );
            step_warnings.push((format!("steps.{node}"), message));
        }
        expected.push(step_warnings);
    }
    assert_eq!(actual, expected);
}

#[test]
fn validation_accepts_command_steps_without_agent_warnings() {
    let results: Vec<_> = ["run", "shell"]
        .into_iter()
        .map(|runner| {
            let (_home, dir) = fixture();
            let text = VALID
                .replace("start = \"pick\"", "start = \"build\"")
                .replace(
                    "agent = \"claude\"",
                    &format!("{runner} = 'echo ready'\ntimeout='1m'"),
                )
                .replace("prompt-file = \"prompts/build.md\"", "")
                .replace("lifecycle = \"turn\"", "");
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let valid = report.errors.is_empty()
                && report.warnings.is_empty()
                && report.rows.iter().all(|row| {
                    serde_json::to_value(row).unwrap().get("summary")
                        == Some(&serde_json::Value::Null)
                })
                && report.rows.iter().any(|row| {
                    row.node == "build"
                        && row.runner == runner
                        && row.agent.is_none()
                        && !row.do_background
                        && row.edges.iter().all(|edge| edge.outcome != "asked")
                });
            (runner, valid, format!("{report:?}"))
        })
        .collect();
    assert!(results.iter().all(|(_, valid, _)| *valid), "{results:#?}");
}

#[test]
fn validation_accepts_shell_checks_without_confining_the_command() {
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pick'\n[checks.pick]\nshell='cat /absolute/path | sort > ../result'\non={pass='@done',fail='@failed',invalid='@done',timeout='@failed'}\n",
    ).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report.errors.is_empty(),
        "shell checks must accept shell syntax and external paths: {report:?}"
    );
    assert_eq!(report.rows[0].runner, "shell");
}

#[test]
fn validation_refuses_judges_without_a_headless_launch() {
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace(
            "run = \"python3 checks/pick.py\"",
            "agent = 'pi'\nsummary='Review the work'\nprompt-file = 'prompts/build.md'",
        ),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report
            .errors
            .iter()
            .any(|issue| issue.path == "checks.pick.agent" && issue.message.contains("headless")),
        "{report:?}"
    );
}

#[test]
fn validation_resolves_edges_and_warns_about_auto_continue() {
    let (_home, dir) = fixture();
    let text = VALID.replace(
        "run = \"python3 checks/pick.py\"",
        "agent = 'claude'\nsummary='Review the work'\nprompt-file = 'prompts/build.md'\nwindow = '5h.left>=42 && 7d.left>=10'",
    );
    fs::write(dir.join("workflow.toml"), &text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.rows.len(), 2);
    assert_eq!(report.warnings.len(), 2, "{report:?}");
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|issue| issue
                .message
                .contains("auto-continue is off; a step paused on a provider limit waits for you"))
            .count(),
        1
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|issue| issue.path == "checks.pick"
                && issue.message.contains("ends the attempt as limited"))
    );
    let row = report.rows.iter().find(|row| row.node == "pick").unwrap();
    assert_eq!(serde_json::to_value(row).unwrap()["do_background"], true);
    assert!(row.edges.iter().any(|edge| edge.outcome == "invalid"
        && edge.target == "@failed"
        && edge.source == EdgeSource::OnBreak));
    assert!(row.edges.iter().any(|edge| edge.outcome == "ready"
        && edge.target == "build"
        && edge.source == EdgeSource::Explicit));
    fs::write(
        dir.join("workflow.toml"),
        text.replace("agent = 'claude'", "agent = 'codex'"),
    )
    .unwrap();
    let codex = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        codex
            .warnings
            .iter()
            .any(|issue| issue.path == "checks.pick"
                && issue
                    .message
                    .contains("retries once, then follows on.invalid")
                && !issue.message.contains("ends the attempt as limited")),
        "{codex:?}"
    );
    let cases = [
        ("lifecycle='turn'", None),
        ("lifecycle='stay'", None),
        ("lifecycle='turn'\non.asked='pick'", Some("pick")),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(extra, _)| {
            fs::write(
                dir.join("workflow.toml"),
                format!("{}\n{extra}\n", VALID.replace("lifecycle = \"turn\"", "")),
            )
            .unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            assert!(report.errors.is_empty(), "{report:?}");
            report
                .rows
                .iter()
                .find(|row| row.node == "build")
                .unwrap()
                .edges
                .iter()
                .filter(|edge| edge.outcome == "asked")
                .map(|edge| (edge.target.clone(), edge.source))
                .collect::<Vec<_>>()
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, target)| {
            target
                .iter()
                .map(|target| (target.to_string(), EdgeSource::Explicit))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn validation_takes_any_directory_confines_symlinks_and_reports_broken_profiles() {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), VALID).unwrap();
    let report = validate(&dir.join("checks/.."), &LoadedDefinitions::default(), None);
    assert!(
        report.errors.is_empty(),
        "a directory outside the workflows home is valid: {report:?}"
    );
    assert!(report.rows.iter().all(|row| row.workflow == "example"));
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join("escape")).unwrap();
    fs::write(outside.path().join("prompt.md"), "outside").unwrap();
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace("prompts/build.md", "escape/prompt.md"),
    )
    .unwrap();
    assert!(
        validate(&dir, &LoadedDefinitions::default(), None)
            .errors
            .iter()
            .any(|issue| issue.path == "steps.build.prompt-file")
    );
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace("agent = \"claude\"", "agent = 'broken'"),
    )
    .unwrap();
    let mut definitions = LoadedDefinitions::default();
    definitions
        .failed
        .insert("broken".into(), [dir.join("broken.md")].into());
    assert!(
        validate(&dir, &definitions, None)
            .errors
            .iter()
            .any(|issue| issue.message.contains("broken definition"))
    );
}

#[test]
fn capacity_step_fields_validate_and_resolve_only_agent_edges() {
    let (_home, dir) = fixture();
    let mut actual = Vec::new();
    for (extra, field) in [
        (
            "capacity-wait='2h'\nthrottle='off'\non.capacity='pick'",
            None,
        ),
        ("capacity-wait='forever'", None),
        ("capacity-wait='0s'", None),
        ("throttle='sometimes'", None),
        ("on.capacity='@user'", Some("steps.build.on.capacity")),
        ("on.capacity='@wait'", Some("steps.build.on.capacity")),
    ] {
        fs::write(dir.join("workflow.toml"), format!("{VALID}{extra}\n")).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        actual.push(match field {
            Some(field) => report.errors.iter().any(|issue| issue.path == field),
            None => {
                report.errors.is_empty()
                    && report.rows.iter().any(|row| {
                        row.node == "build"
                            && row.edges.iter().any(|edge| {
                                edge.outcome == "capacity"
                                    && edge.target
                                        == if extra.contains("on.capacity") {
                                            "pick"
                                        } else {
                                            "@failed"
                                        }
                            })
                    })
            }
        });
    }
    let command = VALID
        .replace("agent = \"claude\"", "shell='true'\ntimeout='1m'")
        .replace("prompt-file = \"prompts/build.md\"", "");
    for (extra, field) in [
        ("throttle='off'", "steps.build.throttle"),
        ("capacity-wait='1h'", "steps.build.capacity-wait"),
        ("on.capacity='@failed'", "steps.build.on.capacity"),
    ] {
        fs::write(dir.join("workflow.toml"), format!("{command}{extra}\n")).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        actual.push(
            report.errors.is_empty() && report.warnings.iter().any(|issue| issue.path == field),
        );
    }
    assert_eq!(actual, vec![true; 9]);
    assert!(super::spec::RESERVED_OUTCOMES.contains(&"capacity"));
}

#[test]
fn limits_validate_both_scopes_and_ignore_invalid_values_with_a_warning() {
    let (_home, dir) = fixture();
    let mut actual = Vec::new();
    for scope in ["workflow", "steps.build"] {
        for (value, suffix) in [
            ("max-parallel=3", None),
            ("stagger='10m'", None),
            ("max-parallel=4\nstagger='2m'", None),
            ("", Some("")),
            ("max-parallel=0", Some(".max-parallel")),
            ("stagger='0s'", Some(".stagger")),
            ("stagger='soon'", Some(".stagger")),
            ("max-parallel='three'", Some(".max-parallel")),
            ("unknown=1", Some(".unknown")),
        ] {
            fs::write(
                dir.join("workflow.toml"),
                format!("{VALID}\n[{scope}.limit]\n{value}\n"),
            )
            .unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let valid = report.errors.is_empty()
                && match suffix {
                    None => !report
                        .warnings
                        .iter()
                        .any(|issue| issue.path.starts_with(&format!("{scope}.limit"))),
                    Some(suffix) => report
                        .warnings
                        .iter()
                        .any(|issue| issue.path == format!("{scope}.limit{suffix}")),
                };
            actual.push((scope, value, valid, report.errors, report.warnings));
        }
    }
    let command = VALID
        .replace("agent = \"claude\"", "shell='true'\ntimeout='1m'")
        .replace("prompt-file = \"prompts/build.md\"", "");
    fs::write(
        dir.join("workflow.toml"),
        format!("{command}\n[steps.build.limit]\nmax-parallel=1\n"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    actual.push((
        "command",
        "max-parallel=1",
        report.errors.is_empty()
            && report.warnings.iter().any(|issue| {
                issue.path == "steps.build.limit"
                    && issue
                        .message
                        .contains("a command step has no turn to count")
            }),
        report.errors,
        report.warnings,
    ));
    assert!(
        actual.iter().all(|(_, _, valid, _, _)| *valid),
        "{actual:#?}"
    );
}

#[test]
fn schema_is_derived_from_the_loader_types() {
    let value = serde_json::to_value(schema()).unwrap();
    assert_eq!(value["additionalProperties"], false);
    assert!(value["properties"]["workflow"].is_object());
    assert_eq!(value["$defs"]["Limit"]["additionalProperties"], false);
    assert_eq!(
        value["$defs"]["Limit"]["properties"]["max-parallel"]["minimum"],
        1
    );
    assert!(value["$defs"]["Case"]["properties"].get("key").is_none());
    assert!(value["$defs"]["Step"]["properties"]["max-visits-per-outcome"].is_object());
    for item in ["Header", "Check", "Step"] {
        assert_eq!(
            value["$defs"][item]["properties"]["summary"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert!(
            !value["$defs"][item]["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "summary"))
        );
    }
    insta::assert_json_snapshot!("schema", value);
}
