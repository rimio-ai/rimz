use super::*;
use crate::config::definitions::LoadedDefinitions;
use crate::store::run::RunStatus;
use crate::store::workflow::{AttemptRun, Children};
use std::cell::{Cell, RefCell};
use std::fs;
use std::rc::Rc;

pub(super) struct FakeHost {
    occupancy: RefCell<Occupancy>,
    launched: RefCell<Vec<StepLaunch>>,
    observed: Rc<RefCell<Observed>>,
    failure: RefCell<Option<LaunchFailure>>,
    evict_error: Cell<bool>,
    stale_after_evict: Cell<bool>,
    stops: Cell<usize>,
    evictions: Cell<usize>,
    on_stop: RefCell<Option<Box<dyn Fn()>>>,
    on_launch: RefCell<Option<Box<dyn Fn()>>>,
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
            })),
            failure: RefCell::new(None),
            evict_error: Cell::new(false),
            stale_after_evict: Cell::new(false),
            stops: Cell::new(0),
            evictions: Cell::new(0),
            on_stop: RefCell::new(None),
            on_launch: RefCell::new(None),
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

    fn observe(&self, _: &AttemptRun) -> Result<Observed, WorkflowErr> {
        Ok(self.observed.borrow().clone())
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
mod conditions;

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
    assert_eq!(advance_args(root, Some(&id), None), expected);
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
    let args = advance_args(root, None, Some(&signal));
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
    let mut missed = Vec::new();
    for (label, text, path) in cases {
        let (home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
        if !report
            .errors
            .iter()
            .any(|issue| issue.path.contains(path) || issue.message.contains(path))
        {
            missed.push(format!("{label}: expected {path}; {report:?}"));
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
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
fn validation_resolves_edges_and_warns_about_judge_independence() {
    let (home, dir) = fixture();
    let text = VALID.replace(
        "run = \"python3 checks/pick.py\"",
        "agent = 'claude'\nprompt-file = 'prompts/build.md'\nwindow = '5h.left>=42 && 7d.left>=10'",
    );
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.rows.len(), 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|issue| issue.message.contains("same profile"))
    );
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
        && edge.source == "default"));
    assert!(row.edges.iter().any(|edge| edge.outcome == "ready"
        && edge.target == "build"
        && edge.source == "explicit"));
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
fn schema_is_derived_from_the_loader_types() {
    let value = serde_json::to_value(schema()).unwrap();
    assert_eq!(value["additionalProperties"], false);
    assert!(value["properties"]["workflow"].is_object());
    insta::assert_json_snapshot!("schema", value);
}
