use super::super::*;
use crate::agents::{HeadlessRequest, HeadlessVerdict};
use crate::config::{Isolation, Judge};
use crate::harness::launch_plan::LaunchPlan;
use crate::harness::schedule::run_log::AgentCheckRecord;
use std::os::unix::process::ExitStatusExt;

fn chain_entry(root: &Path, command: &str, on: CheckOn) -> TaskEntry {
    let mut entry = entry(root, true);
    let Some(TaskCheck::Agent(mut judge)) = entry.check.take() else {
        unreachable!()
    };
    judge.prompt = Some("tier-one".into());
    let mut second = judge.clone();
    second.prompt = Some("tier-two".into());
    entry.judge = vec![judge, second];
    entry.check = Some(TaskCheck::Shell(command.into()));
    entry.on = Some(on);
    entry
}

fn chain_judge(
    _: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    _: Duration,
    _: &dyn Fn() -> bool,
    _: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let crate::harness::launch::ExecAction::Launch {
        prompt: Some(prompt),
        ..
    } = &plan.request.action
    else {
        panic!("judge launch prompt")
    };
    assert!(plan.process().reminder.contains("### Judge"));
    assert!(plan.process().reminder.contains("after the shell check"));
    assert!(
        prompt.contains("check `"),
        "shell evidence at the judge's decision point: {prompt}"
    );
    let count_path = plan.cwd.join("judge-count");
    let count = std::fs::read_to_string(&count_path)
        .unwrap_or_default()
        .parse::<usize>()
        .unwrap_or(0);
    std::fs::write(count_path, (count + 1).to_string())?;
    if prompt.starts_with("no-answer") {
        return Ok(output(b"not json", false));
    }
    if prompt.starts_with("tier-two") {
        assert!(
            prompt.contains("tier one reason") || prompt.contains("no verdict"),
            "earlier judge evidence: {prompt}"
        );
    }
    let reason = if prompt.starts_with("tier-two") {
        "tier two reason"
    } else {
        "tier one reason"
    };
    let pass = plan.cwd.join("positive").exists() && !prompt.starts_with("tier-two");
    std::fs::write(&request.verdict_file, answer(pass, reason))?;
    Ok(output(br#"{"type":"turn.completed","usage":{"input_tokens":123,"output_tokens":45,"cached_input_tokens":0}}"#, false))
}

#[test]
fn shell_polarity_short_circuits_or_feeds_two_judges_and_the_action() {
    for code in [0, 1] {
        let root = tempfile::tempdir().unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut run = fire(
            chain_entry(
                root.path(),
                &format!("printf shell-evidence; exit {code}"),
                CheckOn::Fail,
            ),
            &catalog,
        );
        run.check_process = chain_judge;
        let plan = run.prepare(&mut |_| Ok(())).unwrap();
        if code == 0 {
            assert!(
                matches!(plan, TaskFirePlan::Done(done) if done.record.result == LoopRunResult::CheckSkipped)
            );
            assert!(!root.path().join("judge-count").exists());
            continue;
        }
        let TaskFirePlan::Resident { prompt, .. } = plan else {
            panic!("action must run")
        };
        assert_eq!(
            std::fs::read_to_string(root.path().join("judge-count")).unwrap_or_default(),
            "2",
            "both judges run after shell failure"
        );
        assert!(prompt.find("shell-evidence").unwrap() < prompt.find("tier one reason").unwrap());
        assert!(prompt.find("tier one reason").unwrap() < prompt.find("tier two reason").unwrap());
        let row = run
            .finish(TaskFireEffect::Resident {
                checkout: root.path().to_owned(),
                leader: "worker".into(),
                handles: vec!["worker".into()],
                stopped: Vec::new(),
            })
            .unwrap()
            .record;
        assert!(row.check.as_ref().unwrap().agent.is_none());
        assert_eq!(row.judge.len(), 2);
        assert_eq!(row.input_tokens, Some(246));
    }
}

#[test]
fn two_failed_attempts_continue_to_the_next_judge_whose_decline_still_declines() {
    for on in [CheckOn::Fail, CheckOn::Success] {
        let root = tempfile::tempdir().unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut entry = chain_entry(root.path(), "printf shell-evidence; exit 1", on);
        entry.check = Some(TaskCheck::Shell(
            if on == CheckOn::Fail {
                "printf shell-evidence; exit 1"
            } else {
                "printf shell-evidence"
            }
            .into(),
        ));
        entry.judge[0].prompt = Some("no-answer".into());
        let mut run = fire(entry, &catalog);
        run.check_process = chain_judge;
        let plan = run.prepare(&mut |_| Ok(())).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("judge-count")).unwrap_or_default(),
            "3",
            "the first judge runs twice, then the second judge once"
        );
        let record = match plan {
            TaskFirePlan::Resident { prompt, .. } => {
                assert_eq!(
                    on,
                    CheckOn::Fail,
                    "a second-tier fail fires under on = fail"
                );
                assert!(
                    prompt.contains("no verdict") && prompt.contains("tier two reason"),
                    "{prompt}"
                );
                run.finish(TaskFireEffect::Resident {
                    checkout: root.path().to_owned(),
                    leader: "worker".into(),
                    handles: vec!["worker".into()],
                    stopped: Vec::new(),
                })
                .unwrap()
                .record
            }
            TaskFirePlan::Done(done) => {
                assert_eq!(on, CheckOn::Success, "{:?}", done.record.result);
                assert_eq!(done.record.result, LoopRunResult::CheckSkipped);
                done.record
            }
            _ => panic!("unexpected plan"),
        };
        assert_eq!(record.judge.len(), 2);
        assert_eq!(record.judge[0].verdict, None);
        assert!(record.judge[0].retry.is_some(), "{:?}", record.judge[0]);
        assert_eq!(record.judge[1].verdict, Some(false));
        assert_eq!(record.judge[1].retry, None);
    }
}

#[test]
fn resident_shell_cap_overrides_the_action_timeout_and_counts_as_failure() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut entry = chain_entry(
        root.path(),
        "printf shell-evidence; sleep 2",
        CheckOn::Success,
    );
    entry.timeout = Some("30s".into());
    entry.check_timeout = Some("1s".into());
    let mut run = fire(entry, &catalog);
    run.check_process = |_, _, _, _, _, _| panic!("timed out shell stops before judges");
    let plan = run.prepare(&mut |_| Ok(())).unwrap();
    assert!(
        matches!(&plan, TaskFirePlan::Done(done) if done.record.result == LoopRunResult::CheckSkipped),
        "{plan:?}"
    );
    let TaskFirePlan::Done(done) = plan else {
        unreachable!()
    };
    let shell = done.record.check.unwrap();
    assert!(shell.timed_out);
    assert!(shell.duration_ms.is_some());
}

#[test]
fn interrupt_between_judges_preserves_completed_stages() {
    use std::sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    };
    static INTERRUPT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    fn interrupt_after_judge(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
        record_hold: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        assert!(!interrupted(), "no second judge may start");
        let output = chain_judge(adapter, plan, request, timeout, interrupted, record_hold)?;
        INTERRUPT.get().unwrap().store(true, Ordering::SeqCst);
        Ok(output)
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let host = throttle::tests::FakeHost::new();
    INTERRUPT.set(Arc::clone(&host.interrupt)).unwrap();
    let mut run = fire(
        chain_entry(root.path(), "printf shell-evidence; exit 1", CheckOn::Fail),
        &catalog,
    );
    run.throttle_host = host.host();
    run.check_process = interrupt_after_judge;
    let plan = run.prepare(&mut |_| Ok(())).unwrap();
    assert!(
        matches!(&plan, TaskFirePlan::Done(done) if done.record.result == LoopRunResult::Canceled),
        "{plan:?}"
    );
    let TaskFirePlan::Done(done) = plan else {
        unreachable!()
    };
    assert_eq!(done.presentation.exit_code, Some(130));
    assert_eq!(done.record.error.as_deref(), Some("check interrupted"));
    assert!(done.record.check.is_some());
    assert_eq!(done.record.judge.len(), 1);
}

#[test]
fn held_second_tier_replays_after_the_shell_without_new_spend_or_assist() {
    use super::super::super::launch_ledger::{decline_path, load_declines};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("positive"), "").unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let entry = chain_entry(
        root.path(),
        "printf fire >> shell-count; printf shell-evidence",
        CheckOn::Success,
    );
    let mut first = fire(entry.clone(), &catalog);
    first.check_process = chain_judge;
    let plan = first.prepare(&mut |_| Ok(())).unwrap();
    assert!(
        matches!(&plan, TaskFirePlan::Done(done) if done.record.result == LoopRunResult::CheckSkipped),
        "{plan:?}"
    );
    drop(first);
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    assert_eq!(
        load_declines(&paths).unwrap()["headless-guard"][root.path()].tier,
        1
    );
    let before = std::fs::read(decline_path(&paths)).unwrap();
    let declines_for_checkout = || {
        assist_log::recent(&logs_dir(), None).iter().filter(|record| matches!(&record.assist, Assist::JudgeDecline { task, checkout, .. } if task == "headless-guard" && checkout == root.path())).count()
    };
    let assists_before = declines_for_checkout();
    let mut next = fire(entry, &catalog);
    next.mode = LoopRunMode::Scheduled;
    next.check_process = |_, _, _, _, _, _| panic!("held fire must not start any judge");
    let TaskFirePlan::Done(done) = next.prepare(&mut |_| Ok(())).unwrap() else {
        panic!("held verdict skips the action")
    };
    assert_eq!(
        std::fs::read_to_string(root.path().join("shell-count")).unwrap(),
        "firefire"
    );
    assert_eq!(done.record.result, LoopRunResult::CheckSkipped);
    assert!(done.record.check.is_some());
    assert_eq!(done.record.judge.len(), 1);
    let replay = &done.record.judge[0];
    assert!(replay.reused);
    assert_eq!(replay.verdict, Some(false));
    assert_eq!(replay.reason.as_deref(), Some("tier two reason"));
    assert_eq!(
        (
            replay.cost_usd,
            replay.input_tokens,
            replay.output_tokens,
            replay.duration_ms
        ),
        (Some(0.0), Some(0), Some(0), Some(0))
    );
    assert_eq!(std::fs::read(decline_path(&paths)).unwrap(), before);
    assert_eq!(declines_for_checkout(), assists_before);
}

#[test]
fn shell_decline_clears_held_judge_before_the_next_fire() {
    use super::super::super::launch_ledger::load_declines;
    for mode in [LoopRunMode::Scheduled, LoopRunMode::Manual] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("positive"), "").unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let entry = chain_entry(root.path(), "test -e positive", CheckOn::Success);
        let mut first = fire(entry.clone(), &catalog);
        first.check_process = chain_judge;
        assert!(
            matches!(first.prepare(&mut |_| Ok(())).unwrap(), TaskFirePlan::Done(done) if done.record.result == LoopRunResult::CheckSkipped)
        );
        drop(first);
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        assert_eq!(
            load_declines(&paths).unwrap()["headless-guard"][root.path()].tier,
            1
        );

        std::fs::remove_file(root.path().join("positive")).unwrap();
        // A second shell decline also succeeds when no hold remains.
        for _ in 0..2 {
            let mut next = fire(entry.clone(), &catalog);
            next.mode = mode;
            next.check_process = |_, _, _, _, _, _| panic!("shell decline must not ask judges");
            assert!(
                matches!(next.prepare(&mut |_| Ok(())).unwrap(), TaskFirePlan::Done(done) if done.record.result == LoopRunResult::CheckSkipped && done.record.judge.is_empty())
            );
            assert!(
                !load_declines(&paths)
                    .unwrap()
                    .contains_key("headless-guard")
            );
        }

        std::fs::write(root.path().join("positive"), "").unwrap();
        let mut next = fire(entry, &catalog);
        next.mode = LoopRunMode::Scheduled;
        next.check_process = chain_judge;
        let TaskFirePlan::Done(done) = next.prepare(&mut |_| Ok(())).unwrap() else {
            panic!("fresh judge declines")
        };
        assert_eq!(done.record.judge.len(), 2);
        assert!(done.record.judge.iter().all(|judge| !judge.reused));
        assert_eq!(
            std::fs::read_to_string(root.path().join("judge-count")).unwrap(),
            "4"
        );
    }
}

#[test]
fn judge_budget_refusal_keeps_the_preceding_shell_stage() {
    use crate::harness::budget::{BudgetParkStamp, DailyBudgetLedger, DailyBudgetScope};
    if with_action_hooks("judge_budget_refusal_keeps_the_preceding_shell_stage") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut run = fire(
        chain_entry(root.path(), "printf shell-evidence; exit 1", CheckOn::Fail),
        &catalog,
    );
    let mut config = (*run.config).clone();
    config.accounts = toml::from_str("[budget]\ncodex = '10/day'\n").unwrap();
    run.config = Arc::new(config);
    DailyBudgetScope::Account(crate::ids::LoginKey::default_for(AgentKind::new_unchecked(
        "codex",
    )))
    .write_ledger(
        &runtime,
        &paths,
        &DailyBudgetLedger {
            parked: Some(BudgetParkStamp {
                at_cost: 12.0,
                at: run.now,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    run.check_process = |_, _, _, _, _, _| panic!("budget refuses before judge process");
    let plan = run.prepare(&mut |_| Ok(())).unwrap();
    assert!(
        matches!(&plan, TaskFirePlan::Done(done) if done.record.result == LoopRunResult::BudgetSkipped),
        "{plan:?}"
    );
    let TaskFirePlan::Done(done) = plan else {
        unreachable!()
    };
    assert_eq!(done.record.check.unwrap().output, "shell-evidence");
    assert!(done.record.judge.is_empty());
}

fn entry(root: &Path, stay: bool) -> TaskEntry {
    TaskEntry {
        root: root.to_owned(),
        agent: Some("claude".into()),
        prompt: Some("repair the build\nmore instructions".into()),
        check: Some(TaskCheck::Agent(Judge {
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
        .install_hooks(
            &BTreeMap::from([(
                "CLAUDE_CONFIG_DIR".into(),
                home.path().display().to_string(),
            )]),
            Default::default(),
        )
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("harness::schedule::runner::agent_check::tests::{test}"),
            "--nocapture",
        ])
        .env("CLAUDE_CONFIG_DIR", home.path())
        .env("RIMZ_UNIT_TEST_RIMZ_HOME", home.path().join(".rimz"))
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

fn answer(pass: bool, reason: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "outcome": if pass { "pass" } else { "fail" },
        "reason": reason,
    }))
    .unwrap()
}

/// Counts judge attempts in the checkout and returns this attempt's number.
fn count_attempt(plan: &LaunchPlan) -> usize {
    let path = plan.cwd.join("judge-attempts");
    let count = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .parse::<usize>()
        .unwrap_or(0)
        + 1;
    std::fs::write(path, count.to_string()).unwrap();
    count
}

fn attempts(root: &Path) -> usize {
    std::fs::read_to_string(root.join("judge-attempts"))
        .unwrap_or_default()
        .parse()
        .unwrap_or(0)
}

const USAGE: &[u8] = br#"{"type":"turn.completed","usage":{"input_tokens":123,"output_tokens":45,"cached_input_tokens":0}}"#;

fn verdict(request: &HeadlessRequest, pass: bool) {
    assert_eq!(
        std::fs::read_to_string(&request.schema_file).unwrap(),
        crate::harness::headless::judge::LOOP_ANSWER_SCHEMA
    );
    assert!(
        !request.verdict_file.exists(),
        "each run must have a fresh verdict path"
    );
    std::fs::write(
        &request.verdict_file,
        answer(pass, "There is actionable work."),
    )
    .unwrap();
}

fn pass(
    _: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    _: &dyn Fn() -> bool,
    _: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    assert!(
        timeout <= Duration::from_secs(7) && timeout > Duration::from_secs(6),
        "judge deadline, not task timeout: {timeout:?}"
    );
    assert!(plan.process().reminder.contains("### Judge"));
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
    record_hold: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let result = pass(adapter, plan, request, timeout, interrupted, record_hold)?;
    std::fs::write(&request.verdict_file, answer(false, "No actionable work.")).unwrap();
    Ok(result)
}

fn no_verdict(
    _: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    _: &HeadlessRequest,
    _: Duration,
    _: &dyn Fn() -> bool,
    _: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    count_attempt(plan);
    Ok(output(&[b"not json\n".as_slice(), USAGE].concat(), false))
}

/// The first attempt answers outside the outcome pair; the second declines.
fn invalid_then_fail(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
    record_hold: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    if count_attempt(plan) == 2 {
        return fail(adapter, plan, request, timeout, interrupted, record_hold);
    }
    std::fs::write(
        &request.verdict_file,
        br#"{"outcome":"maybe","reason":"unsure"}"#,
    )?;
    Ok(output(USAGE, false))
}

/// The first attempt passes but exits nonzero; the second passes cleanly.
fn failed_exit_then_pass(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
    record_hold: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let attempt = count_attempt(plan);
    let mut result = pass(adapter, plan, request, timeout, interrupted, record_hold)?;
    if attempt == 1 {
        result.status = std::process::ExitStatus::from_raw(1 << 8);
    }
    Ok(result)
}

fn timeout(
    _: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    _: Duration,
    interrupted: &dyn Fn() -> bool,
    _: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    count_attempt(plan);
    let ready = request.schema_file.with_file_name("descendant.ready");
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30 & printf '%s' $!; : > \"$1\"; wait", "sh"])
        .arg(&ready)
        .stdin(Stdio::null());
    // Hold the pump until the pid is in the pipe: a slow `sh` would otherwise miss
    // the 40 ms bound and leave the judge error without it.
    Ok(crate::proc::run_bounded_output_hosted(
        &mut command,
        Duration::from_millis(40),
        interrupted,
        |_| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.exists() {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::other("judge descendant never started"));
                }
                std::thread::sleep(CHECK_POLL_INTERVAL);
            }
            Ok(())
        },
    )?)
}

fn interrupted(
    _: &crate::agents::AgentDefinition,
    _: &LaunchPlan,
    _: &HeadlessRequest,
    _: Duration,
    _: &dyn Fn() -> bool,
    _: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into())
}

#[test]
fn judge_account_budget_refuses_before_spawn_in_both_ladders() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };
    use crate::harness::budget::{BudgetParkStamp, DailyBudgetLedger, DailyBudgetScope};

    if with_action_hooks("judge_account_budget_refuses_before_spawn_in_both_ladders") {
        return;
    }
    fn spawned(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
        record_hold: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        std::fs::write(plan.cwd.join("judge-spawned"), b"")?;
        pass(adapter, plan, request, timeout, interrupted, record_hold)
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
                "judge account must refuse before the action: stay={stay}, parked={parked}, {plan:?}"
            );
            let TaskFirePlan::Done(done) = plan.unwrap() else {
                unreachable!()
            };
            assert!(done.record.error.as_deref().unwrap().contains("codex"));
            assert!(done.record.check.is_none());
            assert!(done.record.run_id.is_none() && done.record.message_id.is_none());
            assert!(!root.path().join("judge-spawned").exists());
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
fn agent_check_timeout_kills_the_group_and_acts_without_a_retry() {
    if with_action_hooks("agent_check_timeout_kills_the_group_and_acts_without_a_retry") {
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
    let judge = fire
        .finish_error(&anyhow::anyhow!("later launch failed"))
        .record
        .judge
        .remove(0);
    assert_eq!(judge.verdict, None);
    assert_eq!(judge.retry, None, "a timeout is never retried");
    assert_eq!(attempts(root.path()), 1);
    let error = judge.error.unwrap();
    assert!(error.contains("timed out after 7s"), "judge error: {error}");
    let pid = error
        .lines()
        .find_map(|line| line.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("captured child pid in judge error: {error}"));
    let deadline = Instant::now() + Duration::from_secs(1);
    while crate::proc::process_is_live(pid, None) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !crate::proc::process_is_live(pid, None),
        "judge descendant must not survive timeout"
    );
}

#[test]
fn agent_check_outcome_words_obey_every_polarity() {
    for (answer, on, fires) in [
        ("pass", CheckOn::Fail, false),
        ("pass", CheckOn::Success, true),
        ("pass", CheckOn::Any, true),
        ("fail", CheckOn::Fail, true),
        ("fail", CheckOn::Success, false),
        ("fail", CheckOn::Any, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut entry = entry(root.path(), true);
        entry.on = Some(on);
        let mut fire = fire(entry, &catalog);
        fire.check_process = if answer == "pass" { pass } else { fail };
        let plan = fire.prepare(&mut |_| Ok(()));
        let case = format!("outcome {answer} under {on:?}");
        if fires {
            assert!(
                matches!(plan, Ok(TaskFirePlan::Resident { .. })),
                "{case}: {plan:?}"
            );
        } else {
            assert!(
                matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::CheckSkipped),
                "{case}: {plan:?}"
            );
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
            "judge must run: {plan:?}"
        );
        let prompt = match plan.unwrap() {
            TaskFirePlan::Spawn(spawn) => spawn.request.prompt,
            TaskFirePlan::Resident { prompt, .. } => prompt,
            _ => unreachable!(),
        };
        assert!(prompt.contains("--- judge `codex` (codex "), "{prompt}");
        assert!(
            prompt.contains("): pass ---\nThere is actionable work."),
            "{prompt}"
        );
        let record = fire
            .finish_error(&anyhow::anyhow!("later launch failed"))
            .record;
        let judge = &record.judge[0];
        assert_eq!(judge.verdict, Some(true));
        assert_eq!(
            (record.input_tokens, record.output_tokens),
            (Some(123), Some(45))
        );
        assert_eq!(record.cost_usd, judge.cost_usd);
    }
}

#[test]
fn agent_check_pass_preserves_launch_warnings_in_record_and_action_prompt() {
    if with_action_hooks("agent_check_pass_preserves_launch_warnings_in_record_and_action_prompt") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut entry = entry(root.path(), false);
    entry.timeout = Some("1h".into());
    let Some(TaskCheck::Agent(judge)) = &mut entry.check else {
        unreachable!()
    };
    judge.agent = "warning-judge".into();
    let mut fire = fire(entry, &catalog);
    Arc::make_mut(&mut fire.config).agents.profiles.0.insert(
        "warning-judge".into(),
        toml::from_str("agent = 'codex'\nargs = '--model first --model gpt-5.6'").unwrap(),
    );
    let TaskFirePlan::Spawn(spawn) = fire.prepare(&mut |_| Ok(())).unwrap() else {
        panic!("passing judge must launch the action")
    };
    let record = fire
        .finish_error(&anyhow::anyhow!("later launch failed"))
        .record;
    let judge = &record.judge[0];
    assert_eq!(judge.verdict, Some(true));
    assert!(
        !judge.warnings.is_empty(),
        "successful judge lost launch warnings"
    );
    let warning =
        "warning: profile `warning-judge` args set --model first; later model gpt-5.6 wins";
    assert!(
        judge.warnings.iter().any(|text| text == warning),
        "{:?}",
        judge.warnings
    );
    assert!(
        spawn.request.prompt.contains(warning),
        "{}",
        spawn.request.prompt
    );
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
        let judge = &done.record.judge[0];
        assert_eq!(judge.verdict, Some(false));
        assert_eq!(judge.reason.as_deref(), Some("No actionable work."));
        assert_eq!(done.record.cost_usd, judge.cost_usd);
        assert_eq!(done.record.input_tokens, Some(123));
    }
}

#[test]
fn agent_check_second_failure_acts_under_every_polarity() {
    if with_action_hooks("agent_check_second_failure_acts_under_every_polarity") {
        return;
    }
    for on in [CheckOn::Success, CheckOn::Fail, CheckOn::Any] {
        let root = tempfile::tempdir().unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut entry = entry(root.path(), false);
        entry.on = Some(on);
        let mut fire = fire(entry, &catalog);
        fire.check_process = no_verdict;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Spawn(_))),
            "a second failure must act: {plan:?}"
        );
        assert_eq!(attempts(root.path()), 2, "one retry after a dead judge");
        let record = fire
            .finish_error(&anyhow::anyhow!("later launch failed"))
            .record;
        let judge = &record.judge[0];
        assert!(judge.verdict.is_none());
        assert!(judge.retry.is_some(), "{judge:?}");
        assert_eq!(
            (judge.input_tokens, judge.output_tokens),
            (Some(246), Some(90)),
            "usage sums both attempts"
        );
        let error = judge.error.as_ref().unwrap();
        assert!(error.contains("not json") && error.contains("provider diagnostics"));
    }
}

#[test]
fn agent_check_retried_answer_decides_and_declines_as_a_first_answer_does() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), true), &catalog);
    fire.check_process = invalid_then_fail;
    let plan = fire.prepare(&mut |_| Ok(()));
    let Ok(TaskFirePlan::Done(done)) = plan else {
        panic!("the retried fail must decline under on = success: {plan:?}")
    };
    assert_eq!(done.record.result, LoopRunResult::CheckSkipped);
    assert_eq!(attempts(root.path()), 2);
    let judge = &done.record.judge[0];
    assert_eq!(judge.verdict, Some(false));
    assert_eq!(judge.reason.as_deref(), Some("No actionable work."));
    assert!(
        judge
            .retry
            .as_deref()
            .is_some_and(|retry| retry.contains("maybe")),
        "{judge:?}"
    );
    assert_eq!(judge.input_tokens, Some(246));
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let declines = super::super::super::launch_ledger::load_declines(&paths).unwrap();
    assert_eq!(
        declines["headless-guard"][root.path()].reason,
        "No actionable work."
    );
    let assists: Vec<_> = assist_log::recent(&logs_dir(), None)
        .into_iter()
        .filter_map(|record| match record.assist {
            Assist::JudgeDecline {
                task,
                checkout,
                reason,
                ..
            } if task == "headless-guard" && checkout == root.path() => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(
        assists,
        ["No actionable work."],
        "the retried decline's assist"
    );
}

/// The first attempt dies without an answer; the second ends as `second` says.
fn dies_then(
    plan: &LaunchPlan,
    second: impl FnOnce() -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    if count_attempt(plan) == 1 {
        return Ok(output(&[b"not json\n".as_slice(), USAGE].concat(), false));
    }
    second()
}

#[test]
fn a_refused_retry_keeps_the_first_attempts_spend_in_history() {
    fn refused_retry(
        _: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        _: &HeadlessRequest,
        _: Duration,
        _: &dyn Fn() -> bool,
        _: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        dies_then(plan, || {
            Err(agent_check::ProcessErr::BindGuard(
                crate::sandbox::BindGuardErr::Init(std::io::Error::other("watch limit")),
            ))
        })
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), false), &catalog);
    fire.check_process = refused_retry;
    let error = fire
        .prepare(&mut |_| Ok(()))
        .expect_err("a refused retry stops the fire");
    assert_eq!(attempts(root.path()), 2);
    let record = fire.finish_error(&error).record;
    assert_eq!(record.result, LoopRunResult::Errored);
    assert_eq!(record.judge.len(), 1, "{record:?}");
    let judge = &record.judge[0];
    assert_eq!(
        (judge.input_tokens, judge.output_tokens),
        (Some(123), Some(45))
    );
    assert_eq!(record.input_tokens, Some(123), "{record:?}");
    assert!(
        judge
            .error
            .as_deref()
            .is_some_and(|error| error.contains("not json")),
        "{judge:?}"
    );
}

#[test]
fn agent_check_provider_error_and_io_error_are_retried() {
    if with_action_hooks("agent_check_provider_error_and_io_error_are_retried") {
        return;
    }
    fn provider_error(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
        record_hold: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        if count_attempt(plan) == 1 {
            return Ok(output(
                br#"{"is_error":true,"result":"internal server error"}"#,
                false,
            ));
        }
        pass(adapter, plan, request, timeout, interrupted, record_hold)
    }
    fn io_error(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
        record_hold: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        if count_attempt(plan) == 1 {
            return Err(std::io::Error::other("judge pipe broke").into());
        }
        pass(adapter, plan, request, timeout, interrupted, record_hold)
    }
    for (agent, runner, first_error) in [
        (
            "claude",
            provider_error as agent_check::ProcessRunner,
            "internal server error",
        ),
        ("codex", io_error, "judge pipe broke"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut entry = entry(root.path(), false);
        let Some(TaskCheck::Agent(judge)) = &mut entry.check else {
            unreachable!()
        };
        judge.agent = agent.into();
        let mut fire = fire(entry, &catalog);
        fire.check_process = runner;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(matches!(&plan, Ok(TaskFirePlan::Spawn(_))), "{plan:?}");
        assert_eq!(attempts(root.path()), 2, "{first_error}");
        let record = fire
            .finish_error(&anyhow::anyhow!("later launch failed"))
            .record;
        let judge = &record.judge[0];
        assert_eq!(judge.verdict, Some(true), "{judge:?}");
        assert!(
            judge
                .retry
                .as_deref()
                .is_some_and(|retry| retry.contains(first_error)),
            "{judge:?}"
        );
    }
}

#[test]
fn an_interrupt_during_the_retry_cancels_the_fire() {
    fn dies_then_interrupted(
        _: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        _: &HeadlessRequest,
        _: Duration,
        _: &dyn Fn() -> bool,
        _: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        dies_then(plan, || {
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into())
        })
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), false), &catalog);
    fire.check_process = dies_then_interrupted;
    let plan = fire.prepare(&mut |_| Ok(())).unwrap();
    let TaskFirePlan::Done(done) = plan else {
        panic!("an interrupted retry must cancel the fire: {plan:?}")
    };
    assert_eq!(done.record.result, LoopRunResult::Canceled);
    assert_eq!(attempts(root.path()), 2);
    assert_eq!(done.record.judge.len(), 1);
    assert_eq!(
        done.record.judge[0].input_tokens,
        Some(123),
        "{:?}",
        done.record
    );
}

#[test]
fn agent_check_nonzero_exit_with_an_answer_is_retried() {
    if with_action_hooks("agent_check_nonzero_exit_with_an_answer_is_retried") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), false), &catalog);
    fire.check_process = failed_exit_then_pass;
    let plan = fire.prepare(&mut |_| Ok(()));
    assert!(matches!(&plan, Ok(TaskFirePlan::Spawn(_))), "{plan:?}");
    assert_eq!(attempts(root.path()), 2, "a nonzero exit is a dead judge");
    let record = fire
        .finish_error(&anyhow::anyhow!("later launch failed"))
        .record;
    let judge = &record.judge[0];
    assert_eq!(judge.verdict, Some(true));
    assert!(
        judge
            .retry
            .as_deref()
            .is_some_and(|retry| retry.contains("exited 1")),
        "{judge:?}"
    );
}

/// Records the span each attempt gets, then dies without an answer after `JUDGE_DIES_AFTER`.
fn dies_late(
    _: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    _: &HeadlessRequest,
    timeout: Duration,
    _: &dyn Fn() -> bool,
    _: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let spans = plan.cwd.join("judge-spans");
    let mut recorded = std::fs::read_to_string(&spans).unwrap_or_default();
    recorded.push_str(&format!("{}\n", timeout.as_millis()));
    std::fs::write(&spans, recorded).unwrap();
    let dies_after = std::fs::read_to_string(plan.cwd.join("judge-dies-after-ms"))
        .unwrap()
        .parse()
        .unwrap();
    if count_attempt(plan) == 1 {
        std::thread::sleep(Duration::from_millis(dies_after));
    }
    Ok(crate::proc::BoundedOutput {
        status: std::process::ExitStatus::from_raw(1 << 8),
        ..output(USAGE, false)
    })
}

fn judge_spans(root: &Path) -> Vec<u128> {
    std::fs::read_to_string(root.join("judge-spans"))
        .unwrap_or_default()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect()
}

#[test]
fn a_late_death_leaves_the_retry_only_what_remains_of_the_judge_deadline() {
    if with_action_hooks("a_late_death_leaves_the_retry_only_what_remains_of_the_judge_deadline") {
        return;
    }
    for (dies_after_ms, attempts_run) in [(300, 2), (1_100, 1)] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("judge-dies-after-ms"),
            dies_after_ms.to_string(),
        )
        .unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut entry = entry(root.path(), false);
        let Some(TaskCheck::Agent(judge)) = &mut entry.check else {
            unreachable!()
        };
        judge.timeout = Some("1s".into());
        let mut fire = fire(entry, &catalog);
        fire.check_process = dies_late;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(matches!(&plan, Ok(TaskFirePlan::Spawn(_))), "{plan:?}");
        assert_eq!(attempts(root.path()), attempts_run, "{dies_after_ms} ms");
        let spans = judge_spans(root.path());
        assert_eq!(spans[0], 1_000, "the first attempt gets the whole deadline");
        let record = fire
            .finish_error(&anyhow::anyhow!("later launch failed"))
            .record;
        let judge = &record.judge[0];
        if attempts_run == 2 {
            assert!(
                spans[1] <= 1_000 - dies_after_ms as u128,
                "the retry runs on the remainder: {spans:?}"
            );
            assert!(
                judge
                    .retry
                    .as_deref()
                    .is_some_and(|r| r.contains("check verdict")),
                "{judge:?}"
            );
            continue;
        }
        assert_eq!(judge.retry, None, "no second attempt ran: {judge:?}");
        let error = judge.error.as_deref().unwrap_or_default();
        assert!(
            error.starts_with("judge timed out after 1s") && error.contains("check verdict"),
            "{judge:?}"
        );
    }
}

#[test]
fn an_attempt_with_no_time_left_after_admission_spawns_nothing() {
    fn no_time_left(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        _: Duration,
        interrupted: &dyn Fn() -> bool,
        record_hold: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        count_attempt(plan);
        // The host polls before it spawns, so a poll means a spawn was attempted.
        let polled = || {
            HOSTED.store(true, std::sync::atomic::Ordering::SeqCst);
            interrupted()
        };
        agent_check::execute(adapter, plan, request, Duration::ZERO, &polled, record_hold)
    }
    static HOSTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if with_action_hooks("an_attempt_with_no_time_left_after_admission_spawns_nothing") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut fire = fire(entry(root.path(), false), &catalog);
    fire.check_process = no_time_left;
    let plan = fire.prepare(&mut |_| Ok(()));
    assert!(matches!(&plan, Ok(TaskFirePlan::Spawn(_))), "{plan:?}");
    assert!(
        !HOSTED.load(std::sync::atomic::Ordering::SeqCst),
        "no judge process was started"
    );
    assert_eq!(attempts(root.path()), 1, "a timeout is never retried");
    let record = fire
        .finish_error(&anyhow::anyhow!("later launch failed"))
        .record;
    let judge = &record.judge[0];
    assert_eq!(judge.retry, None, "{judge:?}");
    assert!(
        judge
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("judge timed out after 7s")),
        "{judge:?}"
    );
}

#[test]
fn agent_check_provider_limit_acts_without_a_retry() {
    if with_action_hooks("agent_check_provider_limit_acts_without_a_retry") {
        return;
    }
    fn limited(
        _: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        _: &HeadlessRequest,
        _: Duration,
        _: &dyn Fn() -> bool,
        _: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        count_attempt(plan);
        Ok(output(
            br#"{"is_error":true,"result":"quota exhausted"}"#,
            false,
        ))
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let mut entry = entry(root.path(), false);
    let Some(TaskCheck::Agent(judge)) = &mut entry.check else {
        unreachable!()
    };
    judge.agent = "claude".into();
    let mut fire = fire(entry, &catalog);
    fire.check_process = limited;
    let plan = fire.prepare(&mut |_| Ok(()));
    assert!(matches!(&plan, Ok(TaskFirePlan::Spawn(_))), "{plan:?}");
    assert_eq!(
        attempts(root.path()),
        1,
        "a provider limit is never retried"
    );
    let record = fire
        .finish_error(&anyhow::anyhow!("later launch failed"))
        .record;
    let judge = &record.judge[0];
    assert_eq!((judge.verdict, judge.retry.as_ref()), (None, None));
    assert!(
        judge
            .error
            .as_deref()
            .is_some_and(|error| error.contains("quota exhausted")),
        "{judge:?}"
    );
}

#[test]
fn interrupt_between_attempts_cancels_without_a_second_attempt() {
    use std::sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    };
    static INTERRUPT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    fn dies_then_interrupts(
        adapter: &crate::agents::AgentDefinition,
        plan: &LaunchPlan,
        request: &HeadlessRequest,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
        record_hold: &mut Option<CheckSignalGuard>,
    ) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
        assert!(!interrupted(), "no retry may start after the interrupt");
        let output = no_verdict(adapter, plan, request, timeout, interrupted, record_hold)?;
        INTERRUPT.get().unwrap().store(true, Ordering::SeqCst);
        Ok(output)
    }
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let host = throttle::tests::FakeHost::new();
    INTERRUPT.set(Arc::clone(&host.interrupt)).unwrap();
    let mut run = fire(entry(root.path(), true), &catalog);
    run.throttle_host = host.host();
    run.check_process = dies_then_interrupts;
    let plan = run.prepare(&mut |_| Ok(())).unwrap();
    let TaskFirePlan::Done(done) = plan else {
        panic!("an interrupt must cancel the fire: {plan:?}")
    };
    assert_eq!(done.record.result, LoopRunResult::Canceled);
    assert_eq!(attempts(root.path()), 1);
    assert_eq!(done.record.judge.len(), 1);
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
        ("missing-judge-profile", None, "missing-judge-profile"),
        ("pi", None, "only claude and codex"),
        (
            "codex",
            Some("missing-judge-prompt"),
            "missing-judge-prompt",
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

#[test]
fn resident_decline_does_not_hold_an_edited_or_in_flight_replacement() {
    use super::super::super::launch_ledger::{holding_decline, load_declines};
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let original = entry(root.path(), true);
    let mut pending = fire(original.clone(), &catalog);
    pending.check_process = fail;
    assert!(matches!(
        pending.prepare(&mut |_| Ok(())),
        Ok(TaskFirePlan::Done(_))
    ));
    drop(pending);
    let declines = load_declines(&paths).unwrap();
    assert!(
        holding_decline(
            &declines,
            "headless-guard",
            &original,
            root.path(),
            None,
            Timestamp::now()
        )
        .is_some()
    );
    for field in ["prompt", "agent", "on"] {
        let mut replacement = original.clone();
        let Some(TaskCheck::Agent(check)) = &mut replacement.check else {
            unreachable!()
        };
        match field {
            "prompt" => check.prompt = Some("A different question".into()),
            "agent" => check.agent = "pi".into(),
            _ => replacement.on = Some(CheckOn::Any),
        }
        assert!(
            holding_decline(
                &declines,
                "headless-guard",
                &replacement,
                root.path(),
                None,
                Timestamp::now()
            )
            .is_none(),
            "old verdict must not hold an edited {field}"
        );
        if field == "agent" {
            let mut next = fire(replacement, &catalog);
            next.mode = LoopRunMode::Scheduled;
            let error = next.prepare(&mut |_| Ok(())).unwrap_err();
            assert!(
                error.to_string().contains("only claude and codex"),
                "{error}"
            );
            assert_eq!(
                next.finish_error(&error).record.result,
                LoopRunResult::Errored
            );
        }
    }
}

#[test]
fn legacy_declines_without_a_definition_do_not_hold() {
    use super::super::super::launch_ledger::{Declines, holding_decline};
    let root = tempfile::tempdir().unwrap();
    let declines: Declines = serde_json::from_value(serde_json::json!({
        "headless-guard": {root.path().display().to_string(): {
            "at": Timestamp::now(), "since": null, "reason": "old verdict", "profile": "codex"
        }}
    }))
    .unwrap();
    assert!(
        holding_decline(
            &declines,
            "headless-guard",
            &entry(root.path(), true),
            root.path(),
            None,
            Timestamp::now()
        )
        .is_none(),
        "an unidentified old verdict cannot suppress a current definition"
    );
}

#[test]
fn manual_firing_verdict_clears_only_its_checkouts_decline() {
    use super::super::super::launch_ledger::{decline_path, load_declines};
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let mut first = fire(entry(root.path(), true), &catalog);
    first.check_process = fail;
    assert!(matches!(
        first.prepare(&mut |_| Ok(())),
        Ok(TaskFirePlan::Done(_))
    ));
    drop(first);
    let mut declines = load_declines(&paths).unwrap();
    let copy = declines["headless-guard"][root.path()].clone();
    declines
        .get_mut("headless-guard")
        .unwrap()
        .insert(other.path().into(), copy);
    crate::disk::atomic::write_temp_then_rename(&decline_path(&paths), &declines).unwrap();
    let mut next = fire(entry(root.path(), true), &catalog);
    let plan = next.prepare(&mut |_| Ok(()));
    assert!(
        matches!(plan, Ok(TaskFirePlan::Resident { .. })),
        "{plan:?}"
    );
    let declines = load_declines(&paths).unwrap();
    assert!(
        !declines["headless-guard"].contains_key(root.path()),
        "firing hand verdict must clear its old decline before launch"
    );
    assert!(declines["headless-guard"].contains_key(other.path()));
}

fn long_reason(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
    record_hold: &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, agent_check::ProcessErr> {
    let output = pass(adapter, plan, request, timeout, interrupted, record_hold)?;
    std::fs::write(
        &request.verdict_file,
        answer(false, &format!("{}tail", "界".repeat(5000))),
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
    let reason = done.record.judge[0].reason.as_ref().unwrap();
    assert!(
        reason.len() <= 4096 && reason.ends_with("tail"),
        "verdict reason must be bounded before any consumer: {} bytes",
        reason.len()
    );
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let declines = super::super::super::launch_ledger::load_declines(&paths).unwrap();
    assert_eq!(&declines["headless-guard"][root.path()].reason, reason);
}

#[test]
fn resident_decline_holds_only_scheduled_matching_clocks_until_recheck() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let at = Timestamp::from_second(1000).unwrap();
    let since = Timestamp::from_second(900).unwrap();
    let mut memory = serde_json::json!({"headless-guard": {root.path().display().to_string(): {
        "at": at, "since": since, "reason": "No actionable work.", "profile": "codex"
    }}});
    for (mode, clock, recheck, now, held) in [
        (LoopRunMode::Scheduled, Some(since), None, 1100, true),
        (LoopRunMode::Manual, Some(since), None, 1100, false),
        (LoopRunMode::Scheduled, Some(at), None, 1100, false),
        (LoopRunMode::Scheduled, Some(since), Some("2m"), 1119, true),
        (LoopRunMode::Scheduled, Some(since), Some("2m"), 1120, false),
        (LoopRunMode::Scheduled, Some(since), Some("0"), 1000, false),
    ] {
        let mut entry = entry(root.path(), true);
        if let Some(TaskCheck::Agent(check)) = &mut entry.check {
            check.recheck = recheck.map(str::to_owned);
        }
        memory["headless-guard"][root.path().display().to_string()]["fingerprint"] =
            serde_json::to_value(super::super::super::launch_ledger::check_fingerprint(
                &entry,
            ))
            .unwrap();
        crate::disk::atomic::write_temp_then_rename(
            &super::super::super::launch_ledger::decline_path(&paths),
            &memory,
        )
        .unwrap();
        let mut fire = fire(entry, &catalog).with_condition(Some(
            super::super::super::when::ConditionEvidence {
                since: clock,
                when: "team.stage=Done".into(),
                hold: None,
                held_ms: 0,
                readings: BTreeMap::new(),
            },
        ));
        fire.mode = mode;
        fire.now = Timestamp::from_second(now).unwrap();
        fire.check_process = fail;
        let plan = fire.prepare(&mut |_| Ok(()));
        if held {
            assert!(
                matches!(plan, Ok(TaskFirePlan::DeclineHeld)),
                "must hold without running or recording: {plan:?}"
            );
        } else {
            assert!(
                matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::CheckSkipped),
                "must re-ask: {plan:?}"
            );
        }
    }
}

#[test]
fn resident_decline_is_durable_and_polarity_based_but_ordinary_checks_have_no_memory() {
    for (stay, on, process) in [
        (true, CheckOn::Success, fail as agent_check::ProcessRunner),
        (true, CheckOn::Fail, pass),
        (false, CheckOn::Success, fail),
    ] {
        if !stay
            && with_action_hooks(
                "resident_decline_is_durable_and_polarity_based_but_ordinary_checks_have_no_memory",
            )
        {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut entry = entry(root.path(), stay);
        entry.on = Some(on);
        let mut fire = fire(entry, &catalog);
        fire.check_process = process;
        let plan = fire.prepare(&mut |_| Ok(()));
        assert!(
            matches!(&plan, Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::CheckSkipped),
            "{plan:?}"
        );
        assert_eq!(
            assist_log::recent(&logs_dir(), None)
                .iter()
                .filter(|record| matches!(&record.assist, Assist::JudgeDecline { task, checkout, .. } if task == "headless-guard" && checkout == root.path()))
                .count(),
            1,
            "every agent decline counts one assist, including ordinary tasks"
        );
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let declines = super::super::super::launch_ledger::load_declines(&paths).unwrap();
        assert_eq!(
            declines.contains_key("headless-guard"),
            stay,
            "only resident polarity misses persist"
        );
        if stay {
            assert_eq!(declines["headless-guard"][root.path()].profile, "codex");
        }
    }
}

#[test]
fn fail_open_check_does_not_strike_a_launched_run() {
    let mut record =
        LoopRunRecord::new("guard", LoopRunResult::Launched, LoopRunMode::Scheduled, 1);
    record.check = Some(CheckRecord {
        duration_ms: None,
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
fn judge_prompt_file_uses_the_action_prompt_origin() {
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
        duration_ms: None,
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
    record.check = Some(check);
    finish_spawn_effect(
        &mut record,
        SupervisedRunOutcome::Record(Box::new(run)),
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
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), vec![]);
        request.action = ExecAction::Launch {
            prompt: Some("question".into()),
            extra_args: vec![],
        };
        request.identity.name = Some("headless-test".into());
        request.identity.params.loop_task = Some("guard".into());
        request.headless = Some(HeadlessRequest {
            schema_open: false,
            schema: crate::harness::headless::judge::LOOP_ANSWER_SCHEMA.into(),
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
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: root.path(),
            project_root: root.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            machine: &machine,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation,
            bwrap: (isolation.is_scoped()).then_some(Path::new("/bin/bwrap")),
            agents: Err("no room"),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
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
