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
    /// The start groups admission sees, in place of resolving the step's agent.
    pub(super) groups: RefCell<Option<Vec<crate::harness::throttle::StartGroup>>>,
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
pub(in crate::harness) fn resolved_edges(spec: &Definition, node: &str) -> Option<Vec<Edge>> {
    spec.resolved_edges(node).map(<[Edge]>::to_vec)
}

/// The authored tables a compiled graph came from, for tests that read what the compiler wrote.
pub(in crate::harness) fn authored(spec: &Definition) -> &spec::WorkflowSpec {
    spec.authored()
}

/// The prompt each of `step`'s subscriptions hands the step.
pub(in crate::harness) fn subscription_prompts(step: &spec::Step) -> Vec<Option<&str>> {
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
    spec: &Definition,
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
                detail: serde_json::json!({}),
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
pub(in crate::harness) fn declines(spec: &Definition, node: &str, outcome: &str) -> bool {
    script::declines(spec, node, outcome)
}

/// Evaluate the judge `record` holds: its failure kind and its answer.
pub(in crate::harness) fn evaluate(
    state: &crate::disk::paths::StatePaths,
    spec: &Definition,
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
            groups: RefCell::new(None),
        }
    }

    /// The checkout's occupancy without counting a read.
    fn current_occupancy(&self) -> Occupancy {
        if self.stale_after_evict.get() && self.evictions.get() > 0 {
            return Occupancy::Blocked(vec![(
                "stale occupant".into(),
                crate::harness::takeover::OpenTurn::Working,
            )]);
        }
        match &*self.occupancy.borrow() {
            Occupancy::Free => Occupancy::Free,
            Occupancy::Occupied(handles) => Occupancy::Occupied(handles.clone()),
            Occupancy::Blocked(blockers) => Occupancy::Blocked(blockers.clone()),
        }
    }
}

impl AttemptHost for FakeHost {
    fn occupancy(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
    ) -> Result<Occupancy, WorkflowErr> {
        self.occupancies.set(self.occupancies.get() + 1);
        if let Some(check) = &*self.on_occupancy.borrow() {
            check();
        }
        Ok(self.current_occupancy())
    }

    fn launch(&self, step: StepLaunch) -> Result<Result<AttemptRun, NotLaunched>, WorkflowErr> {
        if let Some(arrived) = self.arrives_at_launch.borrow_mut().take() {
            *self.occupancy.borrow_mut() = arrived;
        }
        // As the CLI host does: read the checkout again once the launch is prepared.
        if step.evict.is_some() {
            match self.current_occupancy() {
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
        match &*self.groups.borrow() {
            Some(groups) => Ok(groups.clone()),
            None => launch_groups(record, agent, account),
        }
    }

    /// The fake's launches never report to the machine's start queue, which every test
    /// process shares, so a gated start would queue behind them.
    fn machine_gated(&self) -> bool {
        false
    }
}

mod attempts;
mod commands;
mod conditions;
mod definition;
mod failures;
mod graph;
mod inspection;
mod judges;
mod message;
mod refusals;
mod sleep;
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
scope = "worktree"
start = "pick"
[script.pick]
run = "python3 checks/pick.py"
on = { ready = "build", done = "@done", pending = "@wait" }
timeout = "1d"
[agent.build]
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
fn rest_check_lists_validate_every_entry_and_then_names_one_target() {
    let (_home, dir) = fixture();
    let base = VALID
        .replace(
            "lifecycle = \"turn\"",
            "lifecycle = 'session'\nrest-check = ['pick', 'review']",
        )
        .replace("done = \"@done\"", "done = 'review'")
        + "[script.review]\nrun='true'\non.ok='@done'\non.no='build'\n";
    for (text, path, error) in [
        (base.clone(), "", None),
        (base.replace("done = 'review'", "done = '@done'"), "", None),
        (
            base.replace("['pick', 'review']", "['pick', 'build']"),
            "agent.build.rest-check",
            Some("rest-check lists only check nodes"),
        ),
        (
            base.replace(
                "[script.review]\nrun='true'\non.ok='@done'\non.no='build'",
                "[sleep.review]\nsleep='1m'\nthen='@done'",
            ),
            "agent.build.rest-check",
            Some("rest-check lists only check nodes"),
        ),
        (
            base.replace("then = \"pick\"", "then = ['pick']"),
            "agent.build.then",
            Some("invalid type: sequence"),
        ),
        (
            base.replace(
                "[agent.build]\nagent = \"claude\"\nprompt-file = \"prompts/build.md\"\nlifecycle = 'session'\nrest-check = ['pick', 'review']",
                "[command.build]\nshell='true'\ntimeout='1m'",
            )
            .replace("then = \"pick\"", "then = ['pick']"),
            "command.build.then",
            Some("invalid type: sequence"),
        ),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        match error {
            Some(message) => assert!(
                matches!(
                    report.errors.as_slice(),
                    [issue] if issue.path == path && issue.message.contains(message)
                ),
                "{text}\n{report:?}"
            ),
            None => assert!(report.errors.is_empty(), "{text}\n{report:?}"),
        }
    }
}

#[test]
fn validation_carries_check_and_step_summaries() {
    let (_home, dir) = fixture();
    let text = VALID
        .replace(
            "[script.pick]",
            "[script.pick]\nsummary = 'Choose the next item'",
        )
        .replace(
            "[agent.build]",
            "[agent.build]\nsummary = ' Build the chosen item '",
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
        load(&dir).unwrap().authored().workflow.summary.as_deref(),
        Some(summary)
    );
}

#[test]
fn validation_refuses_empty_or_multiline_summaries_at_the_field() {
    let (_home, dir) = fixture();
    let mut actual = Vec::new();
    for (kind, node) in [
        ("workflow", None),
        ("script", Some("pick")),
        ("agent", Some("build")),
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
            let refused = matches!(
                report.errors.as_slice(),
                [issue] if issue.path == path
                    && issue.message.contains("a summary is one nonempty line")
            );
            actual.push((path, summary, refused, format!("{report:?}")));
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
            "session",
            VALID.replace("lifecycle = \"turn\"", "lifecycle='session'"),
            None,
        ),
        (
            "session with a rest check",
            VALID.replace(
                "lifecycle = \"turn\"",
                "lifecycle='session'\nrest-check=['pick']",
            ),
            None,
        ),
        (
            "no agent",
            VALID.replace("agent = \"claude\"", "agent = 'term'"),
            Some("agent.build.agent"),
        ),
        (
            "ambiguous leader",
            VALID.replace("agent = \"claude\"", "agent = 'claude,claude'"),
            Some("agent.build.agent"),
        ),
        (
            "team",
            VALID.replace("agent = \"claude\"", "agent = 'forge'"),
            Some("agent.build.agent"),
        ),
        (
            "team session with a rest check",
            VALID
                .replace("agent = \"claude\"", "agent = 'forge'")
                .replace(
                    "lifecycle = \"turn\"",
                    "lifecycle='session'\nrest-check=['pick']",
                ),
            Some("agent.build.agent"),
        ),
        (
            "session team",
            VALID
                .replace("agent = \"claude\"", "agent = 'forge'")
                .replace("lifecycle = \"turn\"", "lifecycle='session'"),
            None,
        ),
        (
            "judge layout",
            VALID
                .replace(
                    "[script.pick]",
                    "[judge.pick]\nsummary='Picks the next item'",
                )
                .replace(
                    "run = \"python3 checks/pick.py\"",
                    "agent='claude,codex'\nprompt-file='prompts/build.md'",
                ),
            Some("judge.pick.agent"),
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
                                report.errors.len() == 1
                                    && issue.message
                                        == "example: agent.build.agent: a team runs only in a \
                                            session with no rest-check; set lifecycle = \
                                            \"session\" and remove rest-check"
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
fn a_rest_checked_session_warns_about_unreported_background_work_and_auto_continue() {
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        VALID
            .replace("agent = \"claude\"", "agent = 'codex'")
            .replace(
                "lifecycle = \"turn\"",
                "lifecycle='session'\nrest-check=['pick']",
            ),
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
                    "workflow.start: example: workflow.start: @user is not a route target; route to a node, @done, or @failed".into(),
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
    let shell = "[workflow]\nscope='worktree'\nstart='pick'\n[test.pick]\nshell='true'\non={pass='@done',fail='@failed'}\n";
    let cases = [
        (
            "shell requires pass",
            shell.replace("pass='@done',", ""),
            "test.pick.on.pass",
        ),
        (
            "shell requires fail",
            shell.replace(",fail='@failed'", ""),
            "test.pick.on.fail",
        ),
        (
            "empty shell",
            shell.replace("shell='true'", "shell='  '"),
            "test.pick.shell",
        ),
        (
            "shell and run",
            shell.replace("shell='true'", "shell='true'\nrun='true'"),
            "test.pick",
        ),
        (
            "shell and agent",
            shell.replace("shell='true'", "shell='true'\nagent='claude'"),
            "test.pick.agent",
        ),
        (
            "shell and cases",
            format!("{shell}cases=[{{when=['ci=passed'],outcome='pass'}}]\n"),
            "test.pick.cases",
        ),
        (
            "unknown start",
            VALID.replace("start = \"pick\"", "start = \"missing\""),
            "workflow.start",
        ),
        (
            "unknown edge",
            VALID.replace("ready = \"build\"", "ready = \"missing\""),
            "script.pick.on.ready",
        ),
        (
            "duplicate node",
            format!("{VALID}\n[script.build]\nrun = 'true'\non.ok = '@done'\n"),
            "build",
        ),
        (
            "no runner",
            VALID.replace("run = \"python3 checks/pick.py\"", ""),
            "script.pick",
        ),
        (
            "two runners",
            VALID.replace(
                "run = \"python3 checks/pick.py\"",
                "run = 'true'\nagent = 'claude'",
            ),
            "script.pick.agent",
        ),
        (
            "judge prompt",
            VALID
                .replace(
                    "[script.pick]",
                    "[judge.pick]\nsummary = 'Picks the next item'",
                )
                .replace("run = \"python3 checks/pick.py\"", "agent = 'claude'"),
            "judge.pick.prompt-file",
        ),
        (
            "then missing",
            VALID.replace("then = \"pick\"", ""),
            "agent.build.then",
        ),
        (
            "then step",
            VALID.replace("then = \"pick\"", "then = 'build'"),
            "agent.build.then",
        ),
        (
            "rest check on a turn",
            VALID.replace("then = \"pick\"", "then = \"pick\"\nrest-check = ['pick']"),
            "agent.build.rest-check",
        ),
        (
            "unknown profile",
            VALID.replace("agent = \"claude\"", "agent = 'missing-profile'"),
            "agent.build.agent",
        ),
        (
            "missing prompt",
            VALID.replace("prompts/build.md", "prompts/missing.md"),
            "agent.build.prompt-file",
        ),
        (
            "missing script",
            VALID.replace("checks/pick.py", "checks/missing.py"),
            "script.pick.run",
        ),
        (
            "absolute path",
            VALID.replace("prompts/build.md", "/etc/passwd"),
            "agent.build.prompt-file",
        ),
        (
            "parent path",
            VALID.replace("prompts/build.md", "prompts/../build.md"),
            "agent.build.prompt-file",
        ),
        (
            "unbounded step cycle",
            VALID.replace("max-visits = 3", ""),
            "agent.build",
        ),
        (
            "unbounded capacity cycle",
            format!(
                "{}\n[script.after]\nrun='true'\non.done='@done'\n",
                VALID
                    .replace("max-visits = 3", "")
                    .replace("then = \"pick\"", "then = 'after'\non.capacity = 'pick'")
            ),
            "agent.build",
        ),
        (
            "hot check cycle",
            VALID
                .replace("ready = \"build\"", "ready = 'pick'")
                .replace(", pending = \"@wait\"", ""),
            "script.pick",
        ),
        (
            "wait timeout",
            VALID.replace("timeout = \"1d\"", ""),
            "script.pick.timeout",
        ),
        (
            "timeout cannot wait",
            VALID.replace("timeout = \"1d\"", "timeout = '1s'").replace(
                "pending = \"@wait\"",
                "pending = '@wait', timeout = '@wait'",
            ),
            "script.pick.on.timeout",
        ),
        (
            "step then cannot wait",
            VALID.replace("then = \"pick\"", "then = '@wait'"),
            "agent.build.then",
        ),
        (
            "visits cannot wait",
            format!("{VALID}\non.visits = '@wait'\n"),
            "agent.build.on.visits",
        ),
        (
            "asked cannot wait",
            format!("{VALID}\non.asked = '@wait'\n"),
            "agent.build.on.asked",
        ),
        (
            "visits cannot target itself",
            format!("{VALID}\non.visits = 'build'\n"),
            "agent.build.on.visits",
        ),
        (
            "bad duration",
            VALID.replace("timeout = \"1d\"", "timeout = 'tomorrow'"),
            "script.pick.timeout",
        ),
        (
            "bad node name",
            VALID.replace("script.pick", "script.'bad name'"),
            "bad name",
        ),
        (
            "bad command",
            VALID.replace("python3 checks/pick.py", "'unclosed"),
            "script.pick.run",
        ),
    ];
    let cycle_cases = [(
        "step cycle without max-visits",
        VALID.replace("max-visits = 3", ""),
        "agent.build",
        "cycle through steps has no max-visits bound",
    )];
    let command = VALID.replace(
        "[agent.build]\nagent = \"claude\"\nprompt-file = \"prompts/build.md\"\nlifecycle = \"turn\"",
        "[command.build]\nshell = 'true'\ntimeout = '1m'",
    );
    let command_cases = [
        (
            "step no runner",
            VALID.replace("agent = \"claude\"", ""),
            "agent.build",
            "missing field `agent`",
        ),
        (
            "step two runners",
            format!("{command}agent='claude'\n"),
            "command.build.agent",
            "unknown field `agent`",
        ),
        (
            "agent prompt required",
            VALID.replace("prompt-file = \"prompts/build.md\"", ""),
            "agent.build.prompt-file",
            "an agent step requires prompt-file",
        ),
        (
            "shell timeout required",
            command.replace("timeout = '1m'", ""),
            "command.build.timeout",
            "a run or shell step requires timeout",
        ),
        (
            "run timeout required",
            command.replace("shell = 'true'\ntimeout = '1m'", "run = 'true'"),
            "command.build.timeout",
            "a run or shell step requires timeout",
        ),
        (
            "run argv",
            command.replace("shell = 'true'", "run = \"'unclosed\""),
            "command.build.run",
            "invalid run command",
        ),
        (
            "run confinement",
            command.replace("shell = 'true'", "run = 'sh /etc/passwd'"),
            "command.build.run",
            "invalid workflow path",
        ),
        (
            "shell blank",
            command.replace("shell = 'true'", "shell = '  '"),
            "command.build.shell",
            "set a nonempty shell command",
        ),
        (
            "two occupancies",
            format!("{VALID}admission.occupancy='takeover-and-share'\n"),
            "agent.build.admission.occupancy",
            "unknown variant",
        ),
    ];
    let mut missed = Vec::new();
    for (label, text, path, message) in cases
        .into_iter()
        .map(|(label, text, path)| (label, text, path, ""))
        .chain(cycle_cases)
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
fn bounded_command_cycles_validate_without_a_never_waits_warning() {
    let (_home, dir) = fixture();
    let base = "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nrun='true'\ntimeout='1m'\nthen='pick'\nmax-visits=3\n[script.pick]\nrun='true'\non.fail='build'\n";
    let two = base.replace("on.fail='build'", "on.fail='pack'")
        + "[command.pack]\nshell='true'\ntimeout='1m'\nthen='back'\nmax-visits=7\n[script.back]\nrun='true'\non.fail='build'\n";
    let route = "[script.pick]\nrun='true'\non.fail='build'";
    let condition = "[condition.pick]\ncases=[{when=['pr=merged'],outcome='yes'}]\nelse='no'\n\
                     on={yes='@done',no='build',unknown='@wait'}\ntimeout='1m'";
    let held_condition = "[condition.pick]\ncases=[{when=['pr=merged'],outcome='yes',for='1m'}]\nelse='no'\n\
                          on={yes='build',no='@done',unknown='@failed'}";
    let helper = "[agent.helper]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompts/build.md'\n\
                  then='back'\nmax-visits=4\n[script.back]\nrun='true'\non.fail='build'\n";
    let judge = "[judge.pick]\nagent='claude'\nprompt-file='prompts/build.md'\n\
                 summary='Review the work'\non.fail='build'";
    let cases = [
        base.to_owned(),
        base.replace("[command.build]\nrun=", "[command.build]\nshell="),
        two,
        base.to_owned() + "on.pending='@wait'\ntimeout='1m'\n",
        base.replace(route, "[sleep.pick]\nsleep='1m'\nthen='build'"),
        base.replace(route, condition),
        base.replace(route, held_condition),
        base.replace("on.fail='build'", "on.fail='helper'") + helper,
        base.replace(route, judge),
    ];
    for text in cases {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{text}: {report:?}");
        assert!(
            report
                .warnings
                .iter()
                .all(|issue| !issue.message.contains("never waits")),
            "{text}: {report:?}"
        );
    }
}

#[test]
fn validation_refuses_script_and_judge_checks_without_an_on_table() {
    let head = "[workflow]\nscope='worktree'\nstart='pick'\n";
    let cases = [
        ("script", "[script.pick]\nrun='true'\n"),
        (
            "judge",
            "[judge.pick]\nagent='claude'\nprompt-file='prompts/build.md'\nsummary='Review the work'\n",
        ),
    ];
    let results: Vec<_> = cases
        .into_iter()
        .map(|(kind, node)| {
            let (_home, dir) = fixture();
            fs::write(dir.join("workflow.toml"), format!("{head}{node}")).unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let errors: Vec<_> = report
                .errors
                .iter()
                .map(|issue| issue.message.clone())
                .collect();
            (kind, errors)
        })
        .collect();
    assert_eq!(
        results,
        [
            (
                "script",
                vec![
                    "example: script.pick.on: a script check requires an on table that routes its answers"
                        .to_owned()
                ]
            ),
            (
                "judge",
                vec![
                    "example: judge.pick.on: a judge check requires an on table that routes its answers"
                        .to_owned()
                ]
            ),
        ]
    );
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
                    "[agent.build]\nagent = \"claude\"",
                    &format!("[command.build]\n{runner} = 'echo ready'\ntimeout='1m'"),
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
        "[workflow]\nscope='worktree'\nstart='pick'\n[test.pick]\nshell='cat /absolute/path | sort > ../result'\non={pass='@done',fail='@failed',died='@done',timeout='@failed'}\n",
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
        VALID.replace("[script.pick]", "[judge.pick]").replace(
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
            .any(|issue| issue.path == "judge.pick.agent" && issue.message.contains("headless")),
        "{report:?}"
    );
}

#[test]
fn validation_resolves_edges_and_warns_about_auto_continue() {
    let (_home, dir) = fixture();
    let text = VALID.replace("[script.pick]", "[judge.pick]").replace(
        "run = \"python3 checks/pick.py\"",
        "agent = 'claude'\nsummary='Review the work'\nprompt-file = 'prompts/build.md'\nadmission.window = '5h.left>=42 && 7d.left>=10'",
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
            .any(|issue| issue.path == "judge.pick"
                && issue.message.contains("ends the attempt as limited"))
    );
    let row = report.rows.iter().find(|row| row.node == "pick").unwrap();
    assert_eq!(serde_json::to_value(row).unwrap()["do_background"], true);
    assert!(row.edges.iter().any(|edge| edge.outcome == "died"
        && edge.target == "@failed"
        && edge.source == EdgeSource::Default));
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
        codex.warnings.iter().any(|issue| issue.path == "judge.pick"
            && issue.message.contains("retries once, then fails invalid")
            && !issue.message.contains("ends the attempt as limited")),
        "{codex:?}"
    );
    let cases = [
        ("lifecycle='turn'", None),
        ("lifecycle='session'", None),
        ("lifecycle='turn'\non.asked='pick'", Some("pick")),
        ("lifecycle='session'\non.asked='pick'", Some("pick")),
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
                .map(|edge| (edge.target.to_string(), edge.source))
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
            .any(|issue| issue.path == "agent.build.prompt-file")
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
            // R10: pick leads back to build, so the capacity route ends the run.
            "admission.give-up='2h'\nadmission.throttle='off'\non.capacity='@done'",
            None,
        ),
        ("admission.give-up='forever'", None),
        (
            "admission.give-up='0s'",
            Some("agent.build.admission.give-up"),
        ),
        (
            "admission.throttle='sometimes'",
            Some("agent.build.admission.throttle"),
        ),
        ("on.capacity='@user'", Some("agent.build.on.capacity")),
        ("on.capacity='@wait'", Some("agent.build.on.capacity")),
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
                                            "@done"
                                        } else {
                                            "@failed"
                                        }
                            })
                    })
            }
        });
    }
    for (extra, field) in [
        ("admission.throttle='off'", "command.build.admission"),
        ("admission.give-up='1h'", "command.build.admission"),
        ("on.capacity='@failed'", "command.build.on.capacity"),
    ] {
        fs::write(
            dir.join("workflow.toml"),
            format!("{}{extra}\n", command(VALID)),
        )
        .unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        actual.push(
            report
                .errors
                .iter()
                .map(|issue| issue.path.as_str())
                .eq([field]),
        );
    }
    assert_eq!(actual, vec![true; 9]);
    assert!(super::spec::reserved("capacity"));
}

/// `text` with its `build` agent rewritten as a command step.
fn command(text: &str) -> String {
    text.replace(
        "[agent.build]\nagent = \"claude\"\nprompt-file = \"prompts/build.md\"\nlifecycle = \"turn\"",
        "[command.build]\nshell='true'\ntimeout='1m'",
    )
}

#[test]
fn limits_validate_both_scopes_and_refuse_invalid_values() {
    let (_home, dir) = fixture();
    let mut actual = Vec::new();
    for (scope, table) in [
        ("workflow.limit", "workflow.limit"),
        ("agent.build.admission", "agent.build.admission"),
    ] {
        for (value, suffix) in [
            ("max-parallel=3", None),
            ("stagger='10m'", None),
            ("max-parallel=4\nstagger='2m'", None),
            ("", (scope == "workflow.limit").then_some("")),
            ("max-parallel=0", Some(".max-parallel")),
            ("stagger='0s'", Some(".stagger")),
            ("stagger='soon'", Some(".stagger")),
            ("max-parallel='three'", Some(".max-parallel")),
            ("unknown=1", Some(".unknown")),
        ] {
            fs::write(
                dir.join("workflow.toml"),
                format!("{VALID}\n[{table}]\n{value}\n"),
            )
            .unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let paths = report
                .errors
                .iter()
                .map(|issue| issue.path.clone())
                .collect::<Vec<_>>();
            let valid = match suffix {
                None => paths.is_empty(),
                Some(suffix) => paths == [format!("{scope}{suffix}")],
            };
            actual.push((scope, value, valid, report.errors));
        }
    }
    fs::write(
        dir.join("workflow.toml"),
        format!(
            "{}\n[command.build.admission]\nmax-parallel=1\n",
            command(VALID)
        ),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    actual.push((
        "command",
        "max-parallel=1",
        report
            .errors
            .iter()
            .map(|issue| issue.path.as_str())
            .eq(["command.build.admission"]),
        report.errors,
    ));
    assert!(actual.iter().all(|(_, _, valid, _)| *valid), "{actual:#?}");
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
    assert!(
        value["$defs"]["AgentNode"]["properties"]
            .get("max-visits-per-outcome")
            .is_none()
    );
    for item in [
        "Header",
        "AgentNode",
        "CommandNode",
        "MessageNode",
        "TestNode",
        "ScriptNode",
        "JudgeNode",
        "ConditionNode",
        "SleepNode",
    ] {
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
