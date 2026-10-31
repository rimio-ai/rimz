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
fn validation_carries_check_and_step_summaries() {
    let (home, dir) = fixture();
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
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
    let rows = serde_json::to_value(&report.rows).unwrap();
    assert_eq!(rows[0]["summary"], "Choose the next item");
    assert_eq!(rows[1]["summary"], " Build the chosen item ");
}

#[test]
fn validation_refuses_empty_or_multiline_summaries_at_the_field() {
    let (home, dir) = fixture();
    let mut actual = Vec::new();
    for (kind, node) in [("checks", "pick"), ("steps", "build")] {
        for summary in [
            "",
            " \t ",
            "first\nsecond",
            "first\rsecond",
            "first\r\nsecond",
            "line\n",
        ] {
            let mut text: toml::Value = toml::from_str(VALID).unwrap();
            text[kind][node]
                .as_table_mut()
                .unwrap()
                .insert("summary".into(), summary.into());
            fs::write(dir.join("workflow.toml"), toml::to_string(&text).unwrap()).unwrap();
            let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
            let path = format!("{kind}.{node}.summary");
            let refused = report.errors.len() == 1
                && report.errors[0].path == path
                && report.errors[0]
                    .message
                    .contains("a summary is one nonempty line");
            actual.push((path, summary, refused, format!("{report:?}")));
        }
    }
    assert_eq!(
        actual
            .iter()
            .map(|(_, _, refused, _)| *refused)
            .collect::<Vec<_>>(),
        vec![true; 12],
        "{actual:#?}"
    );
}

#[test]
fn validation_accepts_step_layouts_and_refuses_invalid_stay_fields() {
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
        ("stay", format!("{VALID}lifecycle='stay'\n"), None),
        ("until", format!("{VALID}lifecycle='until'\n"), None),
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
            "stay timeout",
            format!("{VALID}lifecycle='stay'\ntimeout='1h'\n"),
            Some("steps.build.timeout"),
        ),
        (
            "stay asked",
            format!("{VALID}lifecycle='stay'\non.asked='@user'\n"),
            Some("steps.build.on.asked"),
        ),
        (
            "until timeout",
            format!("{VALID}lifecycle='until'\ntimeout='1h'\n"),
            Some("steps.build.timeout"),
        ),
        (
            "until asked",
            format!("{VALID}lifecycle='until'\non.asked='@user'\n"),
            Some("steps.build.on.asked"),
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
            let (home, dir) = fixture();
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let mut definitions = LoadedDefinitions::default();
            definitions.teams.0.insert(
                "forge".into(),
                crate::config::Team {
                    layout: Some("claude".into()),
                    ..Default::default()
                },
            );
            let report = validate(&dir, home.path(), &definitions, None);
            let valid = match path {
                Some(path) => report.errors.iter().any(|issue| {
                    issue.path == path
                        && match label {
                            "team" => issue.message.contains("use an agent layout"),
                            "no agent" => issue.message.contains("add an agent"),
                            "ambiguous leader" => issue.message.contains("choose an unambiguous"),
                            "stay timeout" => issue.message.contains("remove timeout"),
                            "stay asked" => issue.message.contains("remove on.asked"),
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
fn until_warns_about_unreported_background_work_but_not_auto_continue() {
    let (home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        format!(
            "{}lifecycle='until'\n",
            VALID.replace("agent = \"claude\"", "agent = 'codex'")
        ),
    )
    .unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
        !report
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
            "shell dead outcome",
            shell.replace("fail='@user'", "fail='@user',other='@done'"),
            "checks.pick.on.other",
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
            "shell prompt",
            shell.replace(
                "shell='true'",
                "shell='true'\nprompt-file='prompts/build.md'",
            ),
            "checks.pick.prompt-file",
        ),
        (
            "shell window",
            shell.replace("shell='true'", "shell='true'\nwindow='5h.left>=42'"),
            "checks.pick.window",
        ),
        (
            "shell else",
            shell.replace("shell='true'", "shell='true'\nelse='pass'"),
            "checks.pick.else",
        ),
        (
            "shell provider",
            shell.replace("shell='true'", "shell='true'\nprovider='claude'"),
            "checks.pick.provider",
        ),
        (
            "shell account",
            shell.replace("shell='true'", "shell='true'\naccount='default'"),
            "checks.pick.account",
        ),
        (
            "unknown field",
            VALID.replace("timeout = \"1d\"", "surprise = true"),
            "checks.pick",
        ),
        (
            "start step",
            VALID.replace("start = \"pick\"", "start = \"build\""),
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
            "step outcome",
            format!("{VALID}\non.done = '@done'\n"),
            "build",
        ),
        (
            "unknown profile",
            VALID.replace("agent = \"claude\"", "agent = 'missing-profile'"),
            "steps.build.agent",
        ),
        (
            "script window",
            VALID.replace("timeout = \"1d\"", "timeout = '1d'\nwindow = '5h.left>=42'"),
            "checks.pick.window",
        ),
        (
            "bad window",
            format!("{VALID}\nwindow = '5h.left=>42'\n"),
            "steps.build.window",
        ),
        (
            "non-window",
            format!("{VALID}\nwindow = 'ci=passed'\n"),
            "steps.build.window",
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
            "zero visits",
            VALID.replace("max-visits = 3", "max-visits = 0"),
            "steps.build.max-visits",
        ),
        (
            "bad duration",
            VALID.replace("timeout = \"1d\"", "timeout = 'tomorrow'"),
            "checks.pick.timeout",
        ),
        (
            "bad deadline",
            VALID.replace("start = \"pick\"", "start = 'pick'\ndeadline = 'never'"),
            "workflow.deadline",
        ),
        (
            "header outcome",
            VALID.replace("start = \"pick\"", "start = 'pick'\non.other = '@done'"),
            "workflow",
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
    let keyed_cases = [
        (
            "zero per-key visits",
            format!("{VALID}\nmax-visits-per-key = 0\n"),
            "steps.build.max-visits-per-key",
            "must be at least 1",
        ),
        (
            "per-key cap equals total",
            format!("{VALID}\nmax-visits-per-key = 3\n"),
            "steps.build.max-visits-per-key",
            "must be below max-visits",
        ),
        (
            "per-key cap exceeds total",
            format!("{VALID}\nmax-visits-per-key = 4\n"),
            "steps.build.max-visits-per-key",
            "must be below max-visits",
        ),
        (
            "per-key cap does not bound a cycle",
            VALID.replace("max-visits = 3", "max-visits-per-key = 2"),
            "steps.build",
            "a per-key bound does not bound the cycle: a check can name a new key every time; max-visits is needed",
        ),
    ];
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
        (
            "command takeover",
            format!("{command}takeover=false\n"),
            "steps.build.takeover",
            "nothing to take over",
        ),
        (
            "command lifecycle",
            format!("{command}lifecycle='turn'\n"),
            "steps.build.lifecycle",
            "lifecycle is for agent steps",
        ),
        (
            "command window",
            format!("{command}window='5h.left>=42'\n"),
            "steps.build.window",
            "windows gate provider usage",
        ),
        (
            "command asked",
            format!("{command}on.asked='@user'\n"),
            "steps.build.on.asked",
            "a command cannot ask",
        ),
        (
            "command prompt",
            format!("{command}prompt-file='prompts/build.md'\n"),
            "steps.build.prompt-file",
            "a command step takes no prompt",
        ),
    ];
    let mut missed = Vec::new();
    for (label, text, path, message) in cases
        .into_iter()
        .map(|(label, text, path)| (label, text, path, ""))
        .chain(keyed_cases)
        .chain(command_cases)
    {
        let (home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
            let (home, dir) = fixture();
            let text = VALID
                .replace(
                    "agent = \"claude\"",
                    &format!("{runner} = 'echo ready'\ntimeout='1m'"),
                )
                .replace("prompt-file = \"prompts/build.md\"", "");
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
    let (home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nshell='cat /absolute/path | sort > ../result'\non={pass='@done',fail='@user',invalid='@done',timeout='@user'}\n",
    ).unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
    assert!(
        report.errors.is_empty(),
        "shell checks must accept shell syntax and external paths: {report:?}"
    );
    assert_eq!(report.rows[0].runner, "shell");
}

#[test]
fn validation_refuses_judges_without_a_headless_launch() {
    let (home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace(
            "run = \"python3 checks/pick.py\"",
            "agent = 'pi'\nprompt-file = 'prompts/build.md'",
        ),
    )
    .unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
    let (home, dir) = fixture();
    let text = VALID.replace(
        "run = \"python3 checks/pick.py\"",
        "agent = 'claude'\nprompt-file = 'prompts/build.md'\nwindow = '5h.left>=42 && 7d.left>=10'",
    );
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.rows.len(), 2);
    assert_eq!(report.warnings.len(), 2, "{report:?}");
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|issue| issue
                .message
                .contains("a provider limit mid-attempt ends the attempt as limited"))
            .count(),
        2
    );
    let row = report.rows.iter().find(|row| row.node == "pick").unwrap();
    assert_eq!(serde_json::to_value(row).unwrap()["do_background"], true);
    assert!(row.edges.iter().any(|edge| edge.outcome == "invalid"
        && edge.target == "@user"
        && edge.source == EdgeSource::Default));
    assert!(row.edges.iter().any(|edge| edge.outcome == "ready"
        && edge.target == "build"
        && edge.source == EdgeSource::Explicit));
    let cases = [
        ("", None),
        ("lifecycle='stay'", None),
        ("on.asked='pick'", Some("pick")),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(extra, _)| {
            fs::write(dir.join("workflow.toml"), format!("{VALID}\n{extra}\n")).unwrap();
            let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
fn validation_confines_directories_and_symlinks_and_reports_broken_profiles() {
    let (home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), VALID).unwrap();
    let report = validate(
        &dir.join("checks/.."),
        home.path(),
        &LoadedDefinitions::default(),
        None,
    );
    assert!(
        report.errors.is_empty(),
        "a confined directory alias is valid: {report:?}"
    );
    assert!(report.rows.iter().all(|row| row.workflow == "example"));
    let outside = tempfile::tempdir().unwrap();
    assert!(
        !validate(&dir, outside.path(), &LoadedDefinitions::default(), None)
            .errors
            .is_empty()
    );
    std::os::unix::fs::symlink(outside.path(), dir.join("escape")).unwrap();
    fs::write(outside.path().join("prompt.md"), "outside").unwrap();
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace("prompts/build.md", "escape/prompt.md"),
    )
    .unwrap();
    assert!(
        validate(&dir, home.path(), &LoadedDefinitions::default(), None)
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
        validate(&dir, home.path(), &definitions, None)
            .errors
            .iter()
            .any(|issue| issue.message.contains("broken definition"))
    );
}

#[test]
fn capacity_step_fields_validate_and_resolve_only_agent_edges() {
    let (home, dir) = fixture();
    let mut actual = Vec::new();
    for (extra, field) in [
        (
            "capacity-wait='2h'\nthrottle='off'\non.capacity='pick'",
            None,
        ),
        ("capacity-wait='forever'", None),
        ("capacity-wait='0s'", Some("steps.build")),
        ("throttle='sometimes'", Some("steps.build")),
        ("on.capacity='@user'", Some("steps.build.on.capacity")),
        ("on.capacity='@wait'", Some("steps.build.on.capacity")),
    ] {
        fs::write(dir.join("workflow.toml"), format!("{VALID}{extra}\n")).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
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
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
        actual.push(report.errors.iter().any(|issue| issue.path == field));
    }
    assert_eq!(actual, vec![true; 9]);
    assert!(super::spec::RESERVED_OUTCOMES.contains(&"capacity"));
}

#[test]
fn schema_is_derived_from_the_loader_types() {
    let value = serde_json::to_value(schema()).unwrap();
    assert_eq!(value["additionalProperties"], false);
    assert!(value["properties"]["workflow"].is_object());
    assert_eq!(
        value["$defs"]["Step"]["properties"]["max-visits-per-key"]["minimum"],
        1
    );
    for node in ["Check", "Step"] {
        assert_eq!(
            value["$defs"][node]["properties"]["summary"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert!(
            !value["$defs"][node]["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "summary"))
        );
    }
    insta::assert_json_snapshot!("schema", value);
}
