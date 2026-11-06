use super::*;
use crate::config::definitions::LoadedDefinitions;
use crate::store::run::RunStatus;
use crate::store::workflow::{AttemptRun, Children};
use std::cell::{Cell, RefCell};
use std::fs;
use std::rc::Rc;

pub(super) struct FakeHost {
    pub(super) occupancy: RefCell<Occupancy>,
    launched: RefCell<Vec<StepLaunch>>,
    observed: Rc<RefCell<Observed>>,
    failure: RefCell<Option<LaunchFailure>>,
    evict_error: Cell<bool>,
    stale_after_evict: Cell<bool>,
    stops: Cell<usize>,
    evictions: Cell<usize>,
    occupancies: Cell<usize>,
    observations: Cell<usize>,
    turn_observations: Cell<usize>,
    on_stop: RefCell<Option<Box<dyn Fn()>>>,
    on_launch: RefCell<Option<Box<dyn Fn()>>>,
    on_observe: RefCell<Option<Box<dyn Fn()>>>,
    pub(super) on_occupancy: RefCell<Option<Box<dyn Fn()>>>,
}

impl FakeHost {
    pub(super) fn new() -> Self {
        Self {
            occupancy: RefCell::new(Occupancy::Free),
            launched: RefCell::new(Vec::new()),
            observed: Rc::new(RefCell::new(Observed {
                status: RunStatus::Running,
                ask: None,
                limited: None,
                children: Children::default(),
                cost_usd: Some(0.25),
                turn: None,
            })),
            failure: RefCell::new(None),
            evict_error: Cell::new(false),
            stale_after_evict: Cell::new(false),
            stops: Cell::new(0),
            evictions: Cell::new(0),
            occupancies: Cell::new(0),
            observations: Cell::new(0),
            turn_observations: Cell::new(0),
            on_stop: RefCell::new(None),
            on_launch: RefCell::new(None),
            on_observe: RefCell::new(None),
            on_occupancy: RefCell::new(None),
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
        if self.stale_after_evict.get() && self.evictions.get() > 0 {
            return Ok(Occupancy::Blocked(vec![(
                "stale occupant".into(),
                crate::harness::schedule::takeover::OpenTurn::Working,
            )]));
        }
        Ok(match &*self.occupancy.borrow() {
            Occupancy::Free => Occupancy::Free,
            Occupancy::Occupied(handles) => Occupancy::Occupied(handles.clone()),
            Occupancy::Blocked(blockers) => Occupancy::Blocked(blockers.clone()),
        })
    }

    fn evict(&self, _: &std::path::Path, _: &std::path::Path) -> Result<Vec<String>, WorkflowErr> {
        self.evictions.set(self.evictions.get() + 1);
        if self.evict_error.get() {
            return Err(WorkflowErr::Refused("occupant did not stop".into()));
        }
        *self.occupancy.borrow_mut() = Occupancy::Free;
        Ok(vec!["sweeper".into()])
    }

    fn launch(&self, step: StepLaunch) -> Result<Result<AttemptRun, LaunchFailure>, WorkflowErr> {
        self.launched.borrow_mut().push(step);
        if let Some(check) = &*self.on_launch.borrow() {
            check();
        }
        Ok(match self.failure.borrow_mut().take() {
            Some(failure) => Err(failure),
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

    fn stop(&self, _: &AttemptRun) -> Result<Children, WorkflowErr> {
        if let Some(check) = &*self.on_stop.borrow() {
            check();
        }
        self.stops.set(self.stops.get() + 1);
        Ok(self.observed.borrow().children.clone())
    }
}

mod attempts;
mod commands;
mod conditions;
mod graph;
mod inspection;
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
    let signal = crate::harness::schedule::signal::Signal {
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
        serde_json::from_str::<crate::harness::schedule::signal::Signal>(args[5].to_str().unwrap())
            .ok(),
        Some(signal)
    );
}

const VALID: &str = r#"
[workflow]
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
            "agent='claude'\nprompt-file='prompts/build.md'",
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
                            "team" => issue.message.contains("use an agent layout"),
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
            "[workflow]\nscope='project'\nstart='pick'\non.deadline='@wait'\n\
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
    let shell = "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nshell='true'\non={pass='@done',fail='@user'}\n";
    let cases = [
        (
            "shell requires pass",
            shell.replace("pass='@done',", ""),
            "checks.pick.on.pass",
        ),
        (
            "shell requires fail",
            shell.replace(",fail='@user'", ""),
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
            "then terminal",
            VALID.replace("then = \"pick\"", "then = '@done'"),
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
            "set exactly one of agent, run, or shell",
        ),
        (
            "step two runners",
            format!("{command}agent='claude'\n"),
            "steps.build",
            "set exactly one of agent, run, or shell",
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
        "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nshell='cat /absolute/path | sort > ../result'\non={pass='@done',fail='@user',invalid='@done',timeout='@user'}\n",
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
            "agent = 'pi'\nprompt-file = 'prompts/build.md'",
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
        "agent = 'claude'\nprompt-file = 'prompts/build.md'\nwindow = '5h.left>=42 && 7d.left>=10'",
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
        && edge.target == "@user"
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
        codex
            .warnings
            .iter()
            .any(|issue| issue.path == "checks.pick"
                && issue.message.contains("retries once, then parks")
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
