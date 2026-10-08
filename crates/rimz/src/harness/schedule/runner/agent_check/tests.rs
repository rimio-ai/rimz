use super::super::*;
use crate::agents::{HeadlessRequest, HeadlessVerdict};
use crate::config::{AgentCheck, Isolation};
use crate::harness::launch_plan::LaunchPlan;
use crate::harness::schedule::run_log::AgentCheckRecord;
use std::os::unix::process::ExitStatusExt;

fn entry(root: &Path, stay: bool) -> TaskEntry {
    TaskEntry {
        root: root.to_owned(),
        agent: Some("claude".into()),
        prompt: Some("repair the build\nmore instructions".into()),
        check: Some(TaskCheck::Agent(AgentCheck {
            agent: "codex".into(),
            prompt: Some("Is there work?".into()),
            prompt_file: None,
            timeout: Some("7s".into()),
            recheck: None,
        })),
        on: Some(CheckOn::Success),
        every: Some("1h".into()),
        stay,
        throttle: Some(ThrottleSwitch::Off),
        ..Default::default()
    }
}

fn fire<'a>(entry: TaskEntry, catalog: &'a TaskCatalog) -> TaskFire<'a> {
    let mut config = MachineConfig::default();
    config.agents.isolation = Isolation::Host;
    let mut fire = TaskFire::new(
        "headless-guard",
        LoadedTask::new("headless-guard", entry, catalog::TaskSource::Config),
        catalog,
        LoopRunMode::Manual,
        false,
        Timestamp::now(),
        Arc::new(config),
        None,
        CheckEcho::Capture,
        Instant::now(),
    )
    .unwrap();
    fire.check_process = pass;
    fire.run_lock_path = |file, entry| Ok(entry.root.join(file));
    fire
}

fn with_action_hooks(test: &str) -> bool {
    if std::env::var_os("RIMZ_TEST_CHECK_ACTION_HOME").is_some() {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    find_definition("claude")
        .unwrap()
        .install_hooks(&BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".into(),
            home.path().display().to_string(),
        )]))
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("harness::schedule::runner::agent_check::tests::{test}"),
            "--nocapture",
        ])
        .env("CLAUDE_CONFIG_DIR", home.path())
        .env("RIMZ_TEST_CHECK_ACTION_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn output(stdout: &[u8], timed_out: bool) -> crate::proc::BoundedOutput {
    crate::proc::BoundedOutput {
        status: std::process::ExitStatus::from_raw(0),
        stdout: stdout.to_vec(),
        stderr: b"provider diagnostics".to_vec(),
        stdin: None,
        timed_out,
    }
}

fn verdict(request: &HeadlessRequest, pass: bool) {
    assert_eq!(
        std::fs::read_to_string(&request.schema_file).unwrap(),
        crate::agents::CHECK_VERDICT_SCHEMA
    );
    assert!(
        !request.verdict_file.exists(),
        "each run must have a fresh verdict path"
    );
    std::fs::write(
        &request.verdict_file,
        serde_json::to_vec(&HeadlessVerdict {
            pass,
            reason: "There is actionable work.".into(),
        })
        .unwrap(),
    )
    .unwrap();
}

fn pass(
    _: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    _: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    assert_eq!(
        timeout,
        Duration::from_secs(7),
        "checker timeout, not task timeout"
    );
    assert!(plan.process().reminder.contains("### Check"));
    assert!(!plan.process().reminder.contains("### Loop"));
    assert_eq!(
        plan.process().env.get(LOOP_TASK_ENV).map(String::as_str),
        Some("headless-guard")
    );
    assert!(
        !plan
            .process()
            .env
            .keys()
            .any(|key| key.starts_with("RIMZ_AGENT_") || key == crate::workspace::ENV_CHANNEL)
    );
    verdict(request, true);
    Ok(output(br#"{"type":"turn.completed","usage":{"input_tokens":123,"output_tokens":45,"cached_input_tokens":0}}"#, false))
}

fn fail(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let result = pass(adapter, plan, request, timeout, interrupted)?;
    std::fs::write(
        &request.verdict_file,
        r#"{"pass":false,"reason":"No actionable work."}"#,
    )
    .unwrap();
    Ok(result)
}

fn no_verdict(
    _: &crate::agents::AgentDefinition,
    _: &LaunchPlan,
    _: &HeadlessRequest,
    _: Duration,
    _: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    Ok(output(b"not json", false))
}

fn timeout(
    _: &crate::agents::AgentDefinition,
    _: &LaunchPlan,
    _: &HeadlessRequest,
    _: Duration,
    _: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30 & printf '%s' $!; wait"])
        .stdin(Stdio::null());
    Ok(crate::proc::run_bounded_output(
        &mut command,
        Duration::from_millis(40),
    )?)
}

fn interrupted(
    _: &crate::agents::AgentDefinition,
    _: &LaunchPlan,
    _: &HeadlessRequest,
    _: Duration,
    _: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into())
}

#[test]
fn checker_account_budget_refuses_before_spawn_in_both_ladders() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };
    use crate::harness::budget::{BudgetParkStamp, DailyBudgetLedger, DailyBudgetScope};

    if with_action_hooks("checker_account_budget_refuses_before_spawn_in_both_ladders") {
        return;
    }
    fn spawned(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        std::fs::write(plan.cwd.join("checker-spawned"), b"")?;
        pass(adapter, plan, request, timeout, interrupted)
    }
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().unwrap();
    for stay in [false, true] {
        for parked in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let catalog = TaskCatalog::load(Some(root.path())).unwrap();
            let paths = StatePaths::for_project_root(root.path()).unwrap();
            let runtime = RuntimePaths::for_state(&paths).unwrap();
            runtime.ensure_dirs().unwrap();
            let mut fire = fire(entry(root.path(), stay), &catalog);
            let mut config = (*fire.config).clone();
            config.timezone = Some("UTC".into());
            config.accounts = toml::from_str("[budget]\ncodex = '10/day'\n").unwrap();
            fire.config = Arc::new(config);
            fire.now = now;
            fire.check_process = spawned;
            let key = crate::ids::LoginKey::default_for(AgentKind::new_unchecked("codex"));
            write_provider_spending_cache(
                &runtime.shared_provider_spending_path(),
                &ProviderSpendingCache {
                    day_cutoff_secs: jiff::civil::date(2026, 6, 2)
                        .to_zoned(jiff::tz::TimeZone::UTC)
                        .unwrap()
                        .timestamp()
                        .as_second() as u64,
                    day_by_login: BTreeMap::from([(
                        key.clone(),
                        SpendWindow {
                            usd: if parked { 0.0 } else { 12.0 },
                            ..Default::default()
                        },
                    )]),
                    ..Default::default()
                },
            );
            if parked {
                DailyBudgetScope::Account(key)
                    .write_ledger(
                        &runtime,
                        &paths,
                        &DailyBudgetLedger {
                            parked: Some(BudgetParkStamp {
                                at_cost: 12.0,
                                at: now,
                            }),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            let plan = fire.prepare(&mut |_| Ok(()));
            assert!(
                matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::BudgetSkipped),
                "checker account must refuse before the action: stay={stay}, parked={parked}, {plan:?}"
            );
            let TaskFirePlan::Done(done) = plan.unwrap() else {
                unreachable!()
            };
            assert!(done.record.error.as_deref().unwrap().contains("codex"));
            assert!(done.record.check.is_none());
            assert!(done.record.run_id.is_none() && done.record.message_id.is_none());
            assert!(!root.path().join("checker-spawned").exists());
        }
    }
}

#[test]
fn interrupted_agent_check_cancels_before_any_action_without_throttle() {
    if with_action_hooks("interrupted_agent_check_cancels_before_any_action_without_throttle") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for action in ["spawn", "resident", "delivery"] {
        let mut entry = entry(root.path(), action == "resident");
        if action == "delivery" {
            entry.agent = None;
            entry.wait = Some(crate::config::TaskTarget {
                kind: AgentKind::new_unchecked("claude"),
                session: "cancel-target".into(),
                handle: "target".into(),
            });
        }
        let mut fire = fire(entry, &catalog);
        fire.check_process = interrupted;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::Canceled),
            "interrupt must cancel before {action}: {plan:?}"
        );
        let TaskFirePlan::Done(done) = plan.unwrap() else {
            unreachable!()
        };
        assert_eq!(done.presentation.exit_code, Some(130));
        assert!(done.record.message_id.is_none() && done.record.run_id.is_none());
    }
}

#[test]
fn agent_check_timeout_kills_the_group_and_fails_open() {
    if with_action_hooks("agent_check_timeout_kills_the_group_and_fails_open") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), false), &catalog);
    fire.check_process = timeout;
    let started = Instant::now();
    let plan = fire.prepare(&mut |_| Ok(()));
    assert!(
        matches!(&plan, Ok(TaskFirePlan::Spawn(_))),
        "timeout without verdict must fire: {plan:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let check = fire
        .finish_error(&anyhow::anyhow!("later launch failed"))
        .record
        .check
        .unwrap();
    assert!(check.timed_out);
    assert_eq!(check.code, None);
    assert!(check.agent.unwrap().error.unwrap().contains("timed out"));
    let pid = check
        .output
        .lines()
        .find_map(|line| line.parse::<u32>().ok())
        .expect("captured child pid");
    let deadline = Instant::now() + Duration::from_secs(1);
    while crate::proc::process_is_live(pid, None) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !crate::proc::process_is_live(pid, None),
        "checker descendant must not survive timeout"
    );
}

#[test]
fn agent_check_valid_verdict_obeys_fail_and_any_polarity() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for (on, declines) in [(CheckOn::Fail, true), (CheckOn::Any, false)] {
        let mut entry = entry(root.path(), true);
        entry.on = Some(on);
        let mut fire = fire(entry, &catalog);
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::CheckSkipped)
                == declines,
            "polarity must apply to verdict: {plan:?}"
        );
        if !declines {
            assert!(matches!(plan, Ok(TaskFirePlan::Resident { .. })));
        }
    }
}

#[test]
fn agent_check_pass_augments_both_ladders_and_preserves_usage() {
    if with_action_hooks("agent_check_pass_augments_both_ladders_and_preserves_usage") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for stay in [false, true] {
        let mut entry = entry(root.path(), stay);
        entry.timeout = (!stay).then(|| "1h".into());
        let mut fire = fire(entry, &catalog);
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(
                &plan,
                Ok(TaskFirePlan::Spawn(_)) | Ok(TaskFirePlan::Resident { .. })
            ),
            "agent check must run: {plan:?}"
        );
        let prompt = match plan.unwrap() {
            TaskFirePlan::Spawn(spawn) => spawn.request.prompt,
            TaskFirePlan::Resident { prompt, .. } => prompt,
            _ => unreachable!(),
        };
        assert!(prompt.contains("--- check by `codex` (codex "), "{prompt}");
        assert!(
            prompt.contains("): pass ---\nThere is actionable work."),
            "{prompt}"
        );
        let record = fire
            .finish_error(&anyhow::anyhow!("later launch failed"))
            .record;
        let agent = record.check.as_ref().unwrap().agent.as_ref().unwrap();
        assert!(agent.verdict.as_ref().unwrap().pass);
        assert_eq!(
            (record.input_tokens, record.output_tokens),
            (Some(123), Some(45))
        );
        assert_eq!(record.cost_usd, agent.cost_usd);
    }
}

#[test]
fn agent_check_decline_records_reason_and_cost_in_both_ladders() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for stay in [false, true] {
        let mut fire = fire(entry(root.path(), stay), &catalog);
        fire.check_process = fail;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::CheckSkipped),
            "valid negative verdict must decline: {plan:?}"
        );
        let TaskFirePlan::Done(done) = plan.unwrap() else {
            unreachable!()
        };
        let check = done.record.check.unwrap();
        assert_eq!(check.code, Some(1));
        assert_eq!(check.output, "No actionable work.");
        assert_eq!(done.record.cost_usd, check.agent.unwrap().cost_usd);
        assert_eq!(done.record.input_tokens, Some(123));
    }
}

#[test]
fn agent_check_no_verdict_fails_open_under_every_polarity() {
    if with_action_hooks("agent_check_no_verdict_fails_open_under_every_polarity") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for on in [CheckOn::Success, CheckOn::Fail, CheckOn::Any] {
        let mut entry = entry(root.path(), false);
        entry.on = Some(on);
        let mut fire = fire(entry, &catalog);
        fire.check_process = no_verdict;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Spawn(_))),
            "no verdict must fire: {plan:?}"
        );
        let record = fire
            .finish_error(&anyhow::anyhow!("later launch failed"))
            .record;
        let check = record.check.unwrap();
        assert_eq!(check.code, None);
        let agent = check.agent.unwrap();
        assert!(agent.verdict.is_none());
        assert!(agent.error.is_some());
        assert!(check.output.contains("not json") && check.output.contains("provider diagnostics"));
    }
}

#[test]
fn resident_shell_checks_run_again_on_each_fire() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for _ in 0..2 {
        let mut entry = entry(root.path(), true);
        entry.check = Some("echo checked; exit 1".into());
        let mut fire = fire(entry, &catalog);
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::CheckSkipped && done.record.check.as_ref().is_some_and(|check| check.output == "checked\n")),
            "resident must execute shell check: {plan:?}"
        );
    }
}

#[test]
fn agent_check_bad_definition_errors_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    for (agent, path, expected) in [
        ("missing-checker-profile", None, "missing-checker-profile"),
        ("pi", None, "only claude and codex"),
        (
            "codex",
            Some("missing-check-prompt"),
            "missing-check-prompt",
        ),
    ] {
        let mut entry = entry(root.path(), true);
        let Some(TaskCheck::Agent(check)) = &mut entry.check else {
            unreachable!()
        };
        check.agent = agent.into();
        if let Some(path) = path {
            check.prompt = None;
            check.prompt_file = Some(PathBuf::from(path));
        }
        let mut fire = fire(entry, &catalog);
        let result = fire.prepare(&mut |_| Ok(()));
        assert!(
            result
                .as_ref()
                .is_err_and(|err| format!("{err:#}").contains(expected)),
            "fire-time validation should name the invalid setting: {result:?}"
        );
        let record = fire.finish_error(&result.unwrap_err()).record;
        assert_eq!(record.result, LoopRunResult::Errored);
    }
}

fn long_reason(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let output = pass(adapter, plan, request, timeout, interrupted)?;
    std::fs::write(
        &request.verdict_file,
        serde_json::to_vec(&HeadlessVerdict {
            pass: false,
            reason: format!("{}tail", "界".repeat(5000)),
        })
        .unwrap(),
    )
    .unwrap();
    Ok(output)
}

#[test]
fn agent_verdict_reason_is_bounded_before_decline_publication() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), true), &catalog);
    fire.check_process = long_reason;
    let TaskFirePlan::Done(done) = fire.prepare(&mut |_| Ok(())).unwrap() else {
        unreachable!()
    };
    let check = done.record.check.unwrap();
    let reason = &check.agent.unwrap().verdict.unwrap().reason;
    assert!(
        reason.len() <= 4096 && reason.ends_with("tail"),
        "verdict reason must be bounded before any consumer: {} bytes",
        reason.len()
    );
    assert_eq!(&check.output, reason);
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let declines = super::super::super::launch_ledger::load_declines(&paths).unwrap();
    assert_eq!(&declines["headless-guard"][root.path()].reason, reason);
}

#[test]
fn fail_open_check_does_not_strike_a_launched_run() {
    let mut record =
        LoopRunRecord::new("guard", LoopRunResult::Launched, LoopRunMode::Scheduled, 1);
    record.check = Some(CheckRecord {
        code: None,
        timed_out: true,
        output: "timeout".into(),
        output_path: None,
        agent: Some(AgentCheckRecord {
            profile: "codex".into(),
            kind: AgentKind::new_unchecked("codex"),
            model: None,
            verdict: None,
            error: Some("timeout".into()),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
        }),
    });
    assert_eq!(
        super::super::super::strikes::classify(&record),
        super::super::super::strikes::Signal::Reset
    );
}

#[test]
fn checker_prompt_file_uses_the_action_prompt_origin() {
    let root = tempfile::tempdir().unwrap();
    let path = Path::new("questions/check.md");
    let machine = LoadedTask::new(
        "guard",
        entry(root.path(), false),
        catalog::TaskSource::Config,
    );
    assert_eq!(
        agent_check::prompt_path(path, &machine).unwrap(),
        MachineConfig::loop_path().parent().unwrap().join(path)
    );
    let project = LoadedTask::new(
        "guard",
        entry(root.path(), false),
        catalog::TaskSource::Project {
            state: crate::trust::TrustState::Trusted,
        },
    );
    assert_eq!(
        agent_check::prompt_path(path, &project).unwrap(),
        root.path().join(".rimz").join(path)
    );
    assert_eq!(
        agent_check::prompt_path(root.path(), &project).unwrap(),
        root.path()
    );
}

#[test]
fn spawn_completion_sums_check_and_task_usage() {
    let mut record =
        LoopRunRecord::new("guard", LoopRunResult::Completed, LoopRunMode::Scheduled, 1);
    let check = CheckRecord {
        code: Some(0),
        timed_out: false,
        output: "yes".into(),
        output_path: None,
        agent: Some(AgentCheckRecord {
            profile: "codex".into(),
            kind: AgentKind::new_unchecked("codex"),
            model: None,
            verdict: Some(HeadlessVerdict {
                pass: true,
                reason: "yes".into(),
            }),
            error: None,
            cost_usd: Some(0.25),
            input_tokens: Some(10),
            output_tokens: Some(5),
        }),
    };
    let mut run = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/repo")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "job".into(),
        "/repo".into(),
    );
    run.cost_usd = Some(1.0);
    run.input_tokens = Some(100);
    run.output_tokens = Some(50);
    finish_spawn_effect(
        &mut record,
        SupervisedRunOutcome::Record(Box::new(run)),
        Some(check),
        false,
    );
    assert_eq!(
        (record.cost_usd, record.input_tokens, record.output_tokens),
        (Some(1.25), Some(110), Some(55))
    );
}

#[test]
fn headless_compile_maps_artifacts_and_clears_agent_identity_in_both_isolations() {
    use crate::harness::launch::{ExecAction, ExecRequest};
    use crate::harness::launch_plan::{LaunchPlanInputs, compile};
    let root = tempfile::tempdir().unwrap();
    let state =
        StatePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    let runtime = RuntimePaths::under(state.workspace_id.clone(), root.path()).unwrap();
    let machine = MachineConfig::default();
    for isolation in [Isolation::Host, Isolation::Sandbox] {
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), vec![]);
        request.action = ExecAction::Launch {
            prompt: Some("question".into()),
            extra_args: vec![],
        };
        request.identity.name = Some("headless-test".into());
        request.identity.params.loop_task = Some("guard".into());
        request.headless = Some(HeadlessRequest {
            schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
            schema_file: state
                .temp_unit_dir(Some("headless-test"))
                .join("schema.json"),
            verdict_file: state
                .temp_unit_dir(Some("headless-test"))
                .join("verdict.json"),
        });
        let ambient = BTreeMap::from([
            ("RIMZ_AGENT_ID".into(), "parent-id".into()),
            (crate::workspace::ENV_CHANNEL.into(), "parent-lane".into()),
        ]);
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: root.path(),
            project_root: root.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: (isolation == Isolation::Sandbox).then_some(Path::new("/bin/bwrap")),
            agents: Err("no room"),
            ambient_env: &ambient,
            agent_shell: None,
        })
        .unwrap();
        let view = crate::sandbox::TmpView::current(isolation, Some("headless-test"), &state);
        let headless = plan.request.headless.as_ref().unwrap();
        assert_eq!(
            headless.verdict_file,
            view.agent_path(&request.headless.as_ref().unwrap().verdict_file)
        );
        assert_eq!(
            headless.schema_file,
            view.agent_path(&request.headless.as_ref().unwrap().schema_file)
        );
        assert_eq!(
            view.host_path(&headless.verdict_file),
            request.headless.as_ref().unwrap().verdict_file
        );
        assert!(plan.process().unset.contains("RIMZ_AGENT_ID"));
        assert!(plan.process().unset.contains(crate::workspace::ENV_CHANNEL));
        assert!(
            !plan
                .process()
                .env
                .keys()
                .any(|key| key.starts_with("RIMZ_AGENT_"))
        );
        assert_eq!(
            plan.process().env.get(LOOP_TASK_ENV).map(String::as_str),
            Some("guard")
        );
        assert!(
            !plan
                .process()
                .env
                .contains_key(crate::harness::launch::ENV_RUNTIME_ENV)
        );
    }
}
