//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::*;

const CHECK_SUMMARY_OUTPUT_CAP: usize = 4 * 1024;

// ---- run --------------------------------------------------------------------

struct RunOutcome {
    result: LoopRunResult,
    check: Option<CheckRecord>,
    check_duration_ms: Option<u64>,
    run_id: Option<String>,
    transcript_path: Option<String>,
    failure_tail: Option<String>,
    last_message: Option<String>,
    target: Option<String>,
    exit_code: Option<i32>,
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    skip_reason: Option<String>,
    streamed: bool,
}

#[derive(Clone, Copy)]
struct RunExecution<'a> {
    source: TaskSource,
    mode: LoopRunMode,
    keep: bool,
    globals: &'a GlobalFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectTrustDecision {
    Proceed,
    Prompt,
    Refuse,
}

fn project_trust_decision(
    state: TrustState,
    mode: LoopRunMode,
    is_tty: bool,
) -> ProjectTrustDecision {
    if state == TrustState::Trusted {
        ProjectTrustDecision::Proceed
    } else if mode == LoopRunMode::Manual && is_tty {
        ProjectTrustDecision::Prompt
    } else {
        ProjectTrustDecision::Refuse
    }
}

impl RunOutcome {
    fn new(result: LoopRunResult) -> Self {
        Self {
            result,
            check: None,
            check_duration_ms: None,
            run_id: None,
            transcript_path: None,
            failure_tail: None,
            last_message: None,
            target: None,
            exit_code: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            skip_reason: None,
            streamed: false,
        }
    }
}

pub(super) fn run_one(
    name: &str,
    mode: LoopRunMode,
    keep: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let (entry, source) = load_runnable_task(name, globals)?
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    gate_project_trust(name, &entry, source, mode)?;
    task_action(name, &entry)?;
    let started = Instant::now();
    if mode == LoopRunMode::Manual {
        write_manual_header(&mut ui::out(), name, &entry)?;
    }
    if mode == LoopRunMode::Manual
        && pauses::load()
            .get(name)
            .is_some_and(|entry| pauses::is_active(entry, Timestamp::now()))
    {
        writeln!(
            ui::out(),
            "{}",
            ui::paint(ui::palette::MUTED, "  task is paused; firing anyway")
        )?;
    }
    let now = Timestamp::now().to_zoned(MachineConfig::load_lenient().time_zone());
    if let Some(gate) =
        run_log::daily_budget_gate(&state_home(), name, &entry, &now).map_err(anyhow::Error::msg)?
    {
        return finish_gate_skip(
            name,
            &entry,
            mode,
            started,
            now.timestamp(),
            LoopRunResult::BudgetSkipped,
            gate.reason(),
        );
    }
    let config = MachineConfig::load_lenient();
    let scope = task_scope_target(&entry)?;
    if let Some((kind, workspace_id)) = &scope
        && let Some(reason) = rimz::harness::budget::scope_gate(
            &RuntimePaths::for_workspace(workspace_id.clone())?,
            kind,
            &config,
            now.timestamp(),
        )
    {
        return finish_gate_skip(
            name,
            &entry,
            mode,
            started,
            now.timestamp(),
            LoopRunResult::BudgetSkipped,
            reason,
        );
    }
    if let Some((kind, _)) = &scope
        && let Some(reason) = surplus_gate(&entry, kind.as_str(), now.timestamp())?
    {
        return finish_gate_skip(
            name,
            &entry,
            mode,
            started,
            now.timestamp(),
            LoopRunResult::SurplusSkipped,
            reason,
        );
    }
    let _run_lock = match acquire_run_lock(name, &entry) {
        Ok(RunLockAttempt::Acquired(guard)) => guard,
        Ok(RunLockAttempt::Held(info)) => {
            return finish_overlapped(name, &entry, mode, started, info);
        }
        Err(err) => {
            append_error_record(name, &entry, mode, started, &err);
            return Err(err);
        }
    };
    match execute_task(name, &entry, source, mode, keep, globals) {
        Ok(outcome) => {
            let duration_ms = elapsed_ms(started);
            let record = loop_record(name, mode, duration_ms, &outcome);
            record_run(name, &entry, record);
            print_run_summary(name, &entry, duration_ms, mode, keep, &outcome)?;
            if let Some(code) = outcome.exit_code {
                std::process::exit(code);
            }
            Ok(())
        }
        Err(err) => {
            append_error_record(name, &entry, mode, started, &err);
            Err(err)
        }
    }
}

fn gate_project_trust(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    mode: LoopRunMode,
) -> Result<()> {
    let TaskSource::Project { state } = source else {
        return Ok(());
    };
    match project_trust_decision(state, mode, std::io::stdin().is_terminal()) {
        ProjectTrustDecision::Proceed => {}
        ProjectTrustDecision::Prompt => {
            if !crate::cli::trust::offer_inline_grant(&entry.root, "grant trust and fire?")? {
                block_untrusted_project_task(name, entry, source)?;
            }
        }
        ProjectTrustDecision::Refuse => block_untrusted_project_task(name, entry, source)?,
    }
    Ok(())
}

fn finish_overlapped(
    name: &str,
    entry: &TaskEntry,
    mode: LoopRunMode,
    started: Instant,
    info: Option<RunLockInfo>,
) -> Result<()> {
    let detail = info.map(|info| {
        format!(
            "previous run still active (pid {}, started {}) — skipped",
            info.pid,
            ui::rel_age(info.started_at, Timestamp::now())
        )
    });
    let mut record = LoopRunRecord::new(name, LoopRunResult::Overlapped, mode, elapsed_ms(started));
    record.error = detail.clone();
    record_run(name, entry, record);
    if mode == LoopRunMode::Manual {
        write_manual_verdict(
            &mut ui::out(),
            LoopRunResult::Overlapped,
            detail
                .as_deref()
                .unwrap_or("previous run still active — skipped"),
        )?;
    } else if let Some(detail) = detail {
        writeln!(ui::out(), "loop `{name}`: {detail}")?;
    } else {
        writeln!(
            ui::out(),
            "loop `{name}`: previous run still active; skipping"
        )?;
    }
    Ok(())
}

fn task_scope_target(
    entry: &TaskEntry,
) -> Result<Option<(rimz::ids::AgentKind, rimz::ids::WorkspaceId)>> {
    let workspace = WorkspaceResolver::resolve(entry.resolved_root(), None)?;
    match task_action("budget gate", entry)? {
        TaskAction::Spawn(spec) => Ok(Some((
            rimz::ids::AgentKind::new_unchecked(resolve_task_spec(spec, &workspace)?.kind),
            workspace.workspace_id,
        ))),
        TaskAction::Deliver(target) => Ok(Some((
            rimz::ids::AgentKind::new_unchecked(target.kind.clone()),
            workspace.workspace_id,
        ))),
        TaskAction::CheckOnly => Ok(None),
    }
}

fn finish_gate_skip(
    name: &str,
    entry: &TaskEntry,
    mode: LoopRunMode,
    started: Instant,
    at: Timestamp,
    result: LoopRunResult,
    reason: String,
) -> Result<()> {
    let mut record = LoopRunRecord::new(name, result, mode, elapsed_ms(started));
    record.at = at;
    record.error = Some(reason.clone());
    record_run(name, entry, record);
    if mode == LoopRunMode::Manual {
        write_manual_verdict(
            &mut ui::out(),
            result,
            &format!("{} — {reason}", result.label()),
        )?;
    } else {
        writeln!(ui::out(), "loop `{name}`: {reason}; skipping")?;
    }
    Ok(())
}

fn append_error_record(
    name: &str,
    entry: &TaskEntry,
    mode: LoopRunMode,
    started: Instant,
    err: &anyhow::Error,
) {
    let duration_ms = elapsed_ms(started);
    let error = format!("{err:#}");
    let mut record = LoopRunRecord::new(name, LoopRunResult::Errored, mode, duration_ms);
    record.error = Some(error.clone());
    record_run(name, entry, record);
    tracing::warn!(task = name, error = %error, "loop task run failed");
}

fn record_run(name: &str, entry: &TaskEntry, record: LoopRunRecord) {
    run_log::append(&record);
    let signal = strikes::classify(&record);
    let count = match strikes::note(name, signal) {
        Ok(count) => count,
        Err(err) => {
            tracing::warn!(task = name, error = %err, "loop strike state update failed");
            return;
        }
    };
    let Some(max) = strikes::threshold(entry) else {
        return;
    };
    if signal != strikes::Signal::Strike || count < max {
        return;
    }
    let now = Timestamp::now();
    match pauses::set_if_inactive(
        name,
        PauseEntry {
            until: None,
            strikes: Some(count),
        },
        now,
    ) {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            tracing::warn!(task = name, error = %err, "loop auto-pause state update failed");
            return;
        }
    }

    let _ = writeln!(
        ui::out(),
        "loop `{name}`: paused after {count} consecutive failed fires; resume with `rimz loop resume {name}`"
    );
    notify_loop_paused(name, entry, count);
}

fn notify_loop_paused(name: &str, entry: &TaskEntry, count: u32) {
    let notification = rimz::sidebar::notify::Notification {
        agents: Vec::new(),
        notification_kind: rimz::sidebar::notify::NotificationKind::LoopPaused,
        title: format!("Rimz: loop {name} paused"),
        body: format!(
            "{count} consecutive failed fires; inspect with `rimz loop show {name}`, resume with `rimz loop resume {name}`"
        ),
        unread_count: None,
    };
    let prefs = MachineConfig::load_lenient().notifications.clone();
    rimz::sidebar::notify::spawn_notify_handlers(&prefs, &notification);

    let workspace_id = WorkspaceId::from_project_root(&entry.resolved_root());
    let runtime = match RuntimePaths::for_workspace(workspace_id) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::debug!(task = name, error = %err, "loop auto-pause runtime unavailable");
            return;
        }
    };
    let notification_kind = notification.kind_env().to_owned();
    if let Err(err) = rimz::store::wakeup::broadcast_sidebar_event(
        &runtime,
        None,
        rimz::sidebar::events::SidebarEvent::Notify {
            title: notification.title,
            body: notification.body,
            panes: Vec::new(),
            recheck_unread: false,
            notification_kind: Some(notification_kind),
        },
    ) {
        tracing::debug!(task = name, error = %err, "loop auto-pause notification broadcast failed");
    }
}

fn write_manual_header(out: &mut impl Write, name: &str, entry: &TaskEntry) -> std::io::Result<()> {
    writeln!(
        out,
        "{}{}",
        ui::paint(ui::palette::ACCENT.bold(), name),
        ui::paint(
            ui::palette::MUTED,
            &format!(" — {}", render::task_run_rule(entry))
        )
    )
}

fn write_manual_verdict(
    out: &mut impl Write,
    result: LoopRunResult,
    label: &str,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        ui::paint(
            render::loop_result_style(result),
            &format!("{} {label}", render::loop_result_glyph(result))
        )
    )
}

enum CheckPhase {
    Done(RunOutcome),
    Fire {
        check: Option<CheckRecord>,
        prompt_override: Option<String>,
    },
}

fn execute_task(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    mode: LoopRunMode,
    keep: bool,
    globals: &GlobalFlags,
) -> Result<RunOutcome> {
    let action = task_action(name, entry)?;
    if deadline_expired(entry) {
        if mode == LoopRunMode::Scheduled {
            let _ = remove_loaded_task(name, entry, source)?;
        }
        return Ok(RunOutcome::new(LoopRunResult::Expired));
    }
    let (check, prompt_override) = match run_check_phase(name, entry, action, source, mode)? {
        CheckPhase::Done(outcome) => return Ok(outcome),
        CheckPhase::Fire {
            check,
            prompt_override,
        } => (check, prompt_override),
    };
    let execution = RunExecution {
        source,
        mode,
        keep,
        globals,
    };
    match action {
        TaskAction::Spawn(spec) => {
            execute_spawn_task(name, entry, execution, spec, prompt_override, check)
        }
        TaskAction::Deliver(target) => {
            execute_delivery_task(name, entry, execution, target, prompt_override, check)
        }
        TaskAction::CheckOnly => {
            unreachable!("check-only task without check is rejected by task_action")
        }
    }
}

fn run_check_phase(
    name: &str,
    entry: &TaskEntry,
    action: TaskAction<'_>,
    source: TaskSource,
    mode: LoopRunMode,
) -> Result<CheckPhase> {
    let Some(cmd) = entry.check.as_deref() else {
        return Ok(CheckPhase::Fire {
            check: None,
            prompt_override: None,
        });
    };
    let echo = if mode == LoopRunMode::Manual {
        let mut out = ui::out();
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::MUTED, &format!("  check: {cmd}"))
        )?;
        out.flush()?;
        CheckEcho::Stream {
            prefix: ui::paint(ui::palette::FAINT, "  │ "),
        }
    } else {
        CheckEcho::Capture
    };
    let check_started = Instant::now();
    let outcome = run_check(
        &entry.resolved_root(),
        cmd,
        check_timeout(entry)?.unwrap_or(CHECK_DEFAULT_TIMEOUT),
        echo,
    )?;
    let check_duration_ms = elapsed_ms(check_started);
    let record = check_record(&outcome);
    if action == TaskAction::CheckOnly {
        if mode == LoopRunMode::Scheduled && instances::is_ephemeral(entry) {
            let _ = remove_loaded_task(name, entry, source)?;
        }
        let mut run = RunOutcome::new(check_only_result(&outcome));
        run.check = Some(record);
        run.check_duration_ms = Some(check_duration_ms);
        return Ok(CheckPhase::Done(run));
    }
    if !polarity_fires(entry.on, &outcome) {
        let mut run = RunOutcome::new(LoopRunResult::CheckSkipped);
        run.check = Some(record);
        run.check_duration_ms = Some(check_duration_ms);
        return Ok(CheckPhase::Done(run));
    }
    if mode == LoopRunMode::Manual {
        write_check_trip_line(&mut ui::out(), entry, &record, check_duration_ms)?;
    }
    Ok(CheckPhase::Fire {
        check: Some(record),
        prompt_override: Some(augment_prompt(
            resolve_task_prompt(name, entry)?,
            cmd,
            &outcome,
        )),
    })
}

fn execute_spawn_task(
    name: &str,
    entry: &TaskEntry,
    execution: RunExecution<'_>,
    spec: &str,
    prompt_override: Option<String>,
    check_detail: Option<CheckRecord>,
) -> Result<RunOutcome> {
    let resolved = preflight_task(entry)?;
    let is_ping = agents_spec::virtual_ping_shape(spec);
    // The ping exists only to start a sliding budget window, so a token spent on
    // one already counting down buys nothing. A cold reading falls through.
    if is_ping {
        let window_running = if entry.every.as_deref() == Some("reset") {
            reset_window_already_running(entry, &resolved.kind)?
        } else {
            window_already_running(entry, &resolved.kind)?
        };
        if window_running {
            if execution.mode == LoopRunMode::Scheduled {
                writeln!(
                    ui::out(),
                    "loop `{name}`: {} budget window already active; skipping ping",
                    resolved.kind
                )?;
            }
            let mut run = RunOutcome::new(LoopRunResult::SkippedWindow);
            run.check = check_detail;
            run.skip_reason = Some(format!(
                "{} budget window already counting down",
                resolved.kind
            ));
            return Ok(run);
        }
    }
    let prompt = match prompt_override {
        Some(prompt) => prompt,
        None => resolve_task_prompt(name, entry)?,
    };
    let system_prompt_file = entry
        .system_prompt_file
        .as_deref()
        .map(resolve_config_path)
        .transpose()?;
    let task_mode = entry
        .mode
        .as_deref()
        .filter(|mode| !mode.trim().is_empty())
        .map(parse_mode_value)
        .transpose()?;
    let timeout = entry
        .timeout
        .as_deref()
        .map(parse_task_timeout)
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut run_globals = execution.globals.clone();
    run_globals.root = Some(entry.resolved_root());
    if execution.mode == LoopRunMode::Scheduled && instances::is_ephemeral(entry) {
        // One-shot cleanup happens before the terminal run. A one-shot removed
        // pre-fire that then fails to launch is not retried.
        let _ = remove_loaded_task(name, entry, execution.source)?;
    }
    let effort = entry
        .effort
        .clone()
        .or_else(|| is_ping.then(|| "low".to_owned()));
    let stream = execution.mode == LoopRunMode::Manual;
    let args = crate::cli::agents_cmd::AgentsArgs::for_task(crate::cli::agents_cmd::TaskRunArgs {
        spec: spec.to_owned(),
        prompt: Some(prompt),
        worktree: entry.worktree.clone(),
        mode: task_mode,
        effort,
        budget: entry
            .budget
            .as_deref()
            .map(str::parse::<rimz::harness::budget::BudgetSpec>)
            .transpose()?,
        system_prompt_file,
        timeout,
        keep: execution.keep,
        stream,
        verify: entry.verify.clone(),
        max_attempts: entry.max_attempts,
        loop_zone: execution.mode == LoopRunMode::Scheduled,
        loop_task: Some(name.to_owned()),
    });
    match crate::cli::agents_cmd::run_blocking_task(args, &run_globals) {
        Ok(Some(record)) => {
            let status = record.status;
            let mut run = RunOutcome::new(status.into());
            run.check = check_detail;
            run.run_id = Some(record.run_id.to_string());
            run.transcript_path = record.transcript_path;
            run.failure_tail = record.failure_tail;
            run.last_message = record.last_message;
            run.cost_usd = record.cost_usd;
            run.input_tokens = record.input_tokens;
            run.output_tokens = record.output_tokens;
            run.exit_code = Some(status.exit_code());
            run.streamed = stream;
            Ok(run)
        }
        Ok(None) => {
            let mut run = RunOutcome::new(LoopRunResult::Completed);
            run.check = check_detail;
            Ok(run)
        }
        Err(err) => Err(err),
    }
}

fn execute_delivery_task(
    name: &str,
    entry: &TaskEntry,
    execution: RunExecution<'_>,
    target: &TaskTarget,
    prompt_override: Option<String>,
    check_record: Option<CheckRecord>,
) -> Result<RunOutcome> {
    if !delivery_target_alive(entry, target)? {
        if execution.mode == LoopRunMode::Scheduled {
            writeln!(
                ui::out(),
                "loop `{name}`: target {} not alive; removing schedule",
                target.handle
            )?;
            let _ = remove_loaded_task(name, entry, execution.source)?;
        }
        return Ok(target_gone_outcome(target, check_record));
    }
    let prompt = match prompt_override {
        Some(prompt) => prompt,
        None => resolve_task_prompt(name, entry)?,
    };
    if execution.mode == LoopRunMode::Scheduled && instances::is_ephemeral(entry) {
        let _ = remove_loaded_task(name, entry, execution.source)?;
    }
    let root = entry.resolved_root();
    match crate::cli::message::to_session(
        &root,
        &target.kind,
        &target.session,
        prompt,
        DeliveryGate::Done,
        execution.globals,
    ) {
        Ok(()) => {
            let mut run = RunOutcome::new(LoopRunResult::Delivered);
            run.check = check_record;
            run.target = Some(target.handle.clone());
            Ok(run)
        }
        Err(err) if queue_resolution_miss(&err) => {
            if execution.mode == LoopRunMode::Scheduled {
                writeln!(
                    ui::out(),
                    "loop `{name}`: target {} not alive; removing schedule",
                    target.handle
                )?;
                let _ = remove_loaded_task(name, entry, execution.source)?;
            }
            Ok(target_gone_outcome(target, check_record))
        }
        Err(err) => Err(err),
    }
}

fn target_gone_outcome(target: &TaskTarget, check: Option<CheckRecord>) -> RunOutcome {
    let mut run = RunOutcome::new(LoopRunResult::TargetGone);
    run.check = check;
    run.target = Some(target.handle.clone());
    run
}

fn loop_record(
    task: &str,
    mode: LoopRunMode,
    duration_ms: u64,
    outcome: &RunOutcome,
) -> LoopRunRecord {
    LoopRunRecord {
        task: task.to_owned(),
        at: Timestamp::now(),
        result: outcome.result,
        mode: Some(mode),
        duration_ms: Some(duration_ms),
        error: None,
        check: outcome.check.clone(),
        run_id: outcome.run_id.clone(),
        transcript_path: outcome.transcript_path.clone(),
        last_message: outcome.last_message.clone(),
        target: outcome.target.clone(),
        cost_usd: outcome.cost_usd,
        input_tokens: outcome.input_tokens,
        output_tokens: outcome.output_tokens,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn print_run_summary(
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    keep: bool,
    outcome: &RunOutcome,
) -> Result<()> {
    let mut out = ui::out();
    write_run_summary(&mut out, name, entry, duration_ms, mode, keep, outcome)?;
    Ok(())
}

fn write_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    keep: bool,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    match mode {
        LoopRunMode::Manual => {
            write_manual_run_summary(out, name, entry, duration_ms, keep, outcome)
        }
        LoopRunMode::Scheduled => {
            write_scheduled_run_summary(out, name, entry, duration_ms, outcome)
        }
    }
}

fn write_manual_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    keep: bool,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    if outcome.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            duration_ms,
            LoopRunMode::Manual,
            outcome,
        );
    }
    if let Some((result, label)) = manual_early_verdict(outcome, duration_ms) {
        return write_manual_verdict(out, result, &label);
    }

    let result_style = render::loop_result_style(outcome.result);
    let result_label = manual_result_label(entry, outcome);
    write!(
        out,
        "{}",
        ui::paint(
            result_style,
            &format!(
                "{} {result_label}",
                render::loop_result_glyph(outcome.result)
            )
        )
    )?;
    write!(out, " in {}", render::format_duration_ms(duration_ms))?;
    if let Some(spend) = render::spend_segments(
        outcome.cost_usd,
        outcome.input_tokens,
        outcome.output_tokens,
    ) {
        write!(out, " · {spend}")?;
    }
    writeln!(out)?;

    if is_spawn_failure(outcome.result) && !is_check_only(entry) {
        write_failure_forensics(out, name, outcome)?;
    } else if outcome.result == LoopRunResult::Completed && outcome.run_id.is_some() {
        write_completion_detail(out, name, outcome)?;
    }
    if !is_spawn_failure(outcome.result) && !keep && outcome.run_id.is_some() {
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::MUTED,
                "  pane closed; rerun with --keep to watch"
            )
        )?;
    }
    Ok(())
}

fn manual_early_verdict(outcome: &RunOutcome, duration_ms: u64) -> Option<(LoopRunResult, String)> {
    let label = match outcome.result {
        LoopRunResult::Expired => "deadline expired — task left in place".to_owned(),
        LoopRunResult::TargetGone => format!(
            "{} not alive — schedule left in place",
            outcome.target.as_deref().unwrap_or("target")
        ),
        LoopRunResult::SkippedWindow => {
            let mut label = format!("skipped in {}", render::format_duration_ms(duration_ms));
            if let Some(reason) = outcome.skip_reason.as_deref() {
                label.push_str(" — ");
                label.push_str(reason);
            }
            label
        }
        _ => return None,
    };
    Some((outcome.result, label))
}

fn is_spawn_failure(result: LoopRunResult) -> bool {
    matches!(
        result,
        LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
    )
}

fn write_scheduled_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    if outcome.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            duration_ms,
            LoopRunMode::Scheduled,
            outcome,
        );
    }
    let result_style = render::loop_result_style(outcome.result);
    let exit_label = outcome_exit_label(outcome);
    if is_spawn_failure(outcome.result) {
        let mut label = outcome.result.label().to_owned();
        if let Some(exit_label) = exit_label.as_deref() {
            label.push(' ');
            label.push_str(exit_label);
        }
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_style.bold(), &label)
        )?;
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(spend) = render::spend_segments(
            outcome.cost_usd,
            outcome.input_tokens,
            outcome.output_tokens,
        ) {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        write_failure_forensics(out, name, outcome)?;
    } else {
        let result_label = success_result_label(outcome);
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_style, &result_label)
        )?;
        if let Some(exit_label) = exit_label.as_deref() {
            write!(out, " {exit_label}")?;
        }
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(spend) = render::spend_segments(
            outcome.cost_usd,
            outcome.input_tokens,
            outcome.output_tokens,
        ) {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        if outcome.result == LoopRunResult::Completed && outcome.run_id.is_some() {
            write_completion_detail(out, name, outcome)?;
        }
    }
    Ok(())
}

fn success_result_label(outcome: &RunOutcome) -> String {
    match (outcome.result, outcome.target.as_deref()) {
        (LoopRunResult::Delivered, Some(target)) => format!("delivered to {target}"),
        _ => outcome.result.label().to_owned(),
    }
}

fn manual_result_label(entry: &TaskEntry, outcome: &RunOutcome) -> String {
    if is_check_only(entry)
        && let Some(check) = outcome.check.as_ref()
    {
        return check_result_label(check);
    }
    let mut label = success_result_label(outcome);
    if let Some(exit_label) = outcome_exit_label(outcome) {
        label.push(' ');
        label.push_str(&exit_label);
    }
    label
}

fn is_check_only(entry: &TaskEntry) -> bool {
    entry.check.is_some() && entry.agent.is_none() && entry.wake.is_none()
}

fn check_result_label(check: &CheckRecord) -> String {
    if check.timed_out {
        "check timed out".to_owned()
    } else if check.code == Some(0) {
        "check passed (exit 0)".to_owned()
    } else {
        match check.code {
            Some(code) => format!("check failed (exit {code})"),
            None => "check failed (signal)".to_owned(),
        }
    }
}

fn write_check_trip_line(
    out: &mut impl Write,
    entry: &TaskEntry,
    check: &CheckRecord,
    duration_ms: u64,
) -> std::io::Result<()> {
    let (glyph, style) = if check.timed_out || check.code != Some(0) {
        ("✗", ui::palette::ALARM)
    } else {
        ("✓", ui::palette::GOOD)
    };
    write!(
        out,
        "  {}",
        ui::paint(
            style,
            &format!(
                "{glyph} {} in {}",
                check_result_label(check),
                render::format_duration_ms(duration_ms)
            )
        )
    )?;
    writeln!(
        out,
        " {}",
        ui::paint(
            ui::palette::ACCENT,
            &format!(
                "→ {} {}",
                render::action_progressive_verb(entry),
                task_subject(entry)
            )
        )
    )
}

fn write_check_skipped_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    let label = outcome
        .check
        .as_ref()
        .map(check_result_label)
        .unwrap_or_else(|| "check skipped".to_owned());
    let check_duration_ms = outcome.check_duration_ms.unwrap_or(duration_ms);
    let duration = render::format_duration_ms(check_duration_ms);
    let (glyph, style) = render::check_skip_display(outcome.check.as_ref());
    if mode == LoopRunMode::Manual {
        write!(
            out,
            "{}",
            ui::paint(style, &format!("{glyph} {label} in {duration}"))
        )?;
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::MUTED,
                &format!(" — {}", render::check_skip_decision(entry))
            )
        )
    } else {
        write!(out, "loop `{name}`: {}", ui::paint(style, &label))?;
        writeln!(
            out,
            " in {duration} — {}",
            render::check_skip_decision(entry)
        )
    }
}

fn write_failure_forensics(
    out: &mut impl Write,
    name: &str,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    if let Some(tail) = outcome_failure_tail(outcome) {
        write_failure_tail(out, &tail)?;
    }
    write_run_links(out, outcome)?;
    writeln!(
        out,
        "{}",
        ui::paint(ui::palette::MUTED, &format!("  see: rimz loop show {name}"))
    )
}

fn write_run_links(out: &mut impl Write, outcome: &RunOutcome) -> std::io::Result<()> {
    if let Some(run_id) = outcome.run_id.as_deref() {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::MUTED, &format!("  run: {run_id}"))
        )?;
    }
    if let Some(transcript) = outcome.transcript_path.as_deref() {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::MUTED, &format!("  transcript: {transcript}"))
        )?;
    }
    Ok(())
}

fn write_completion_detail(
    out: &mut impl Write,
    name: &str,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    if !outcome.streamed {
        if let Some(message) = outcome
            .last_message
            .as_deref()
            .filter(|msg| !msg.trim().is_empty())
        {
            render::write_gutter_block(out, None, message)?;
        } else {
            writeln!(
                out,
                "{}",
                ui::paint(
                    ui::palette::MUTED,
                    &format!("  no final message; see: rimz loop show {name}")
                )
            )?;
        }
    }
    write_run_links(out, outcome)
}

fn outcome_exit_label(outcome: &RunOutcome) -> Option<String> {
    if let Some(exit) = outcome.exit_code {
        if exit == 0 {
            return None;
        }
        Some(format!("(exit {exit})"))
    } else if let Some(exit) = outcome.check.as_ref().and_then(|check| check.code) {
        Some(format!("(exit {exit})"))
    } else if outcome.check.as_ref().is_some_and(|check| check.timed_out) {
        Some("(timeout)".to_owned())
    } else {
        None
    }
}

fn outcome_failure_tail(outcome: &RunOutcome) -> Option<String> {
    if let Some(tail) = outcome
        .failure_tail
        .as_deref()
        .filter(|tail| !tail.trim().is_empty())
    {
        return Some(tail.trim_end().to_owned());
    }
    let check = outcome.check.as_ref()?;
    if !check.timed_out && check.code == Some(0) {
        return None;
    }
    let tail = tail_output(check.output.as_bytes(), CHECK_SUMMARY_OUTPUT_CAP);
    let tail = tail.trim_end();
    (!tail.trim().is_empty()).then(|| tail.to_owned())
}

fn write_failure_tail(out: &mut impl Write, tail: &str) -> std::io::Result<()> {
    render::write_gutter_block(out, Some(ui::palette::ALARM), tail)
}

fn delivery_target_alive(entry: &TaskEntry, target: &TaskTarget) -> Result<bool> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    Ok(snapshot.agents.iter().any(|agent| {
        agent.parent_agent_id.is_none()
            && agent.kind.as_str() == target.kind.as_str()
            && agent.agent_id.as_str() == target.session
    }))
}

fn queue_resolution_miss(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<rimz::TargetErr>(),
        Some(rimz::TargetErr::NoMatch { .. } | rimz::TargetErr::NoMatchInChannel { .. })
    )
}

pub(crate) fn reap_dead_delivery_schedules() -> Result<usize> {
    let mut reaped = 0;
    for (name, (entry, source)) in instances::load_all() {
        let target = match task_action(&name, &entry) {
            Ok(TaskAction::Deliver(target)) => target,
            Ok(TaskAction::Spawn(_) | TaskAction::CheckOnly) => continue,
            Err(err) => {
                tracing::debug!(task = %name, error = %err, "invalid loop task skipped by schedule gc");
                continue;
            }
        };
        match delivery_target_alive(&entry, target) {
            Ok(true) => {}
            Ok(false) => {
                let _ = remove_loaded_task(&name, &entry, source)?;
                reaped += 1;
            }
            Err(err) => {
                tracing::debug!(task = %name, error = %err, "loop schedule gc skipped task");
            }
        }
    }
    Ok(reaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_tty_prompts_for_blocked_project_trust() {
        for state in [TrustState::Untrusted, TrustState::Stale] {
            assert_eq!(
                project_trust_decision(state, LoopRunMode::Manual, true),
                ProjectTrustDecision::Prompt
            );
            assert_eq!(
                project_trust_decision(state, LoopRunMode::Manual, false),
                ProjectTrustDecision::Refuse
            );
            assert_eq!(
                project_trust_decision(state, LoopRunMode::Scheduled, true),
                ProjectTrustDecision::Refuse
            );
        }
        assert_eq!(
            project_trust_decision(TrustState::Trusted, LoopRunMode::Manual, true),
            ProjectTrustDecision::Proceed
        );
    }

    fn spawn_entry(check: bool, on: CheckOn) -> TaskEntry {
        TaskEntry {
            agent: Some("codex".to_owned()),
            check: check.then(|| "cargo test".to_owned()),
            on: Some(on),
            ..TaskEntry::default()
        }
    }

    fn wake_entry(check: bool, on: CheckOn) -> TaskEntry {
        TaskEntry {
            wake: Some(TaskTarget {
                kind: "claude".to_owned(),
                session: "sess-planner".to_owned(),
                handle: "@planner".to_owned(),
            }),
            check: check.then(|| "cargo test".to_owned()),
            on: Some(on),
            ..TaskEntry::default()
        }
    }

    fn check_entry() -> TaskEntry {
        TaskEntry {
            check: Some("cargo test".to_owned()),
            ..TaskEntry::default()
        }
    }

    fn summary(
        name: &str,
        entry: &TaskEntry,
        duration_ms: u64,
        mode: LoopRunMode,
        keep: bool,
        outcome: &RunOutcome,
    ) -> String {
        anstream::adapter::strip_str(&raw_summary(name, entry, duration_ms, mode, keep, outcome))
            .to_string()
    }

    fn raw_summary(
        name: &str,
        entry: &TaskEntry,
        duration_ms: u64,
        mode: LoopRunMode,
        keep: bool,
        outcome: &RunOutcome,
    ) -> String {
        let mut out = Vec::new();
        write_run_summary(&mut out, name, entry, duration_ms, mode, keep, outcome).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn failed_summary_links_run_transcript_and_loop_show() {
        let entry = spawn_entry(false, CheckOn::Fail);
        let mut outcome = RunOutcome::new(LoopRunResult::Failed);
        outcome.exit_code = Some(1);
        outcome.run_id = Some("run_0123456789abcdef01234567".to_owned());
        outcome.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        outcome.failure_tail = Some("error: boom\nUsage: codex [OPTIONS] [PROMPT]".to_owned());

        let out = summary(
            "watchdog",
            &entry,
            1_900,
            LoopRunMode::Manual,
            false,
            &outcome,
        );

        assert!(out.contains("✗ failed (exit 1) in 1.9s"));
        assert!(out.contains("  │ error: boom\n  │ Usage: codex [OPTIONS] [PROMPT]"));
        assert!(out.contains("run: run_0123456789abcdef01234567"));
        assert!(out.contains("transcript: /tmp/transcript.jsonl"));
        assert!(out.contains("see: rimz loop show watchdog"));
    }

    #[test]
    fn skipped_check_summary_uses_check_time_and_action_verbs() {
        let mut spawn = RunOutcome::new(LoopRunResult::CheckSkipped);
        spawn.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        spawn.check_duration_ms = Some(4_400);
        let entry = spawn_entry(true, CheckOn::Fail);
        assert_eq!(
            summary(
                "watchdog",
                &entry,
                9_000,
                LoopRunMode::Manual,
                false,
                &spawn,
            ),
            "✓ check passed (exit 0) in 4.4s — codex not started; fires when the check fails\n"
        );
        let raw = raw_summary(
            "watchdog",
            &entry,
            9_000,
            LoopRunMode::Manual,
            false,
            &spawn,
        );
        assert!(raw.contains(&ui::paint(
            ui::palette::GOOD,
            "✓ check passed (exit 0) in 4.4s"
        )));
        assert!(raw.contains(&ui::paint(
            ui::palette::MUTED,
            " — codex not started; fires when the check fails"
        )));

        let mut wake = RunOutcome::new(LoopRunResult::CheckSkipped);
        wake.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "no".to_owned(),
        });
        wake.check_duration_ms = Some(2_000);
        let entry = wake_entry(true, CheckOn::Success);
        assert_eq!(
            summary("nudge", &entry, 8_000, LoopRunMode::Manual, false, &wake,),
            "○ check failed (exit 1) in 2.0s — @planner not woken; fires when the check passes\n"
        );
    }

    #[test]
    fn scheduled_check_skip_keeps_compact_task_prefix() {
        let entry = spawn_entry(true, CheckOn::Fail);
        let mut outcome = RunOutcome::new(LoopRunResult::CheckSkipped);
        outcome.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        outcome.check_duration_ms = Some(700);

        assert_eq!(
            summary(
                "watchdog",
                &entry,
                900,
                LoopRunMode::Scheduled,
                false,
                &outcome,
            ),
            "loop `watchdog`: check passed (exit 0) in 700ms — codex not started; fires when the check fails\n"
        );
    }

    #[test]
    fn trip_line_names_check_fact_and_action() {
        let check = CheckRecord {
            code: Some(101),
            timed_out: false,
            output: "failed".to_owned(),
        };
        let mut out = Vec::new();

        write_check_trip_line(&mut out, &spawn_entry(true, CheckOn::Fail), &check, 12_000).unwrap();

        assert_eq!(
            anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
            "  ✗ check failed (exit 101) in 12s → starting codex\n"
        );
    }

    #[test]
    fn completed_spawn_summary_prints_cost_message_and_keep_hint() {
        let mut outcome = RunOutcome::new(LoopRunResult::Completed);
        outcome.exit_code = Some(0);
        outcome.run_id = Some("run_0123456789abcdef01234567".to_owned());
        outcome.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        outcome.last_message = Some("pong\n".to_owned());
        outcome.cost_usd = Some(0.42);
        outcome.input_tokens = Some(12_000);
        outcome.output_tokens = Some(3_400);

        assert_eq!(
            summary(
                "watchdog",
                &spawn_entry(false, CheckOn::Fail),
                180_000,
                LoopRunMode::Manual,
                false,
                &outcome,
            ),
            "✓ completed in 3m · $0.42 · ↘ 12k ↗ 3k\n  │ pong\n  run: run_0123456789abcdef01234567\n  transcript: /tmp/transcript.jsonl\n  pane closed; rerun with --keep to watch\n"
        );
    }

    #[test]
    fn streamed_spawn_summary_skips_repeated_message_and_links_run() {
        let mut outcome = RunOutcome::new(LoopRunResult::Completed);
        outcome.run_id = Some("run_0123456789abcdef01234567".to_owned());
        outcome.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        outcome.last_message = Some("already streamed".to_owned());
        outcome.streamed = true;

        assert_eq!(
            summary(
                "watchdog",
                &spawn_entry(false, CheckOn::Fail),
                1_000,
                LoopRunMode::Manual,
                true,
                &outcome,
            ),
            "✓ completed in 1.0s\n  run: run_0123456789abcdef01234567\n  transcript: /tmp/transcript.jsonl\n"
        );
    }

    #[test]
    fn scheduled_summary_prints_run_spend() {
        let mut outcome = RunOutcome::new(LoopRunResult::Completed);
        outcome.cost_usd = Some(0.09);
        outcome.input_tokens = Some(14_000);
        outcome.output_tokens = Some(269);

        assert_eq!(
            summary(
                "watchdog",
                &spawn_entry(false, CheckOn::Fail),
                120_000,
                LoopRunMode::Scheduled,
                false,
                &outcome,
            ),
            "loop `watchdog`: completed in 2m · $0.09 · ↘ 14k ↗ 269\n"
        );

        outcome.result = LoopRunResult::Failed;
        outcome.exit_code = Some(1);
        assert_eq!(
            summary(
                "watchdog",
                &spawn_entry(false, CheckOn::Fail),
                120_000,
                LoopRunMode::Scheduled,
                false,
                &outcome,
            ),
            "loop `watchdog`: failed (exit 1) in 2m · $0.09 · ↘ 14k ↗ 269\n  see: rimz loop show watchdog\n"
        );
    }

    #[test]
    fn completed_spawn_summary_falls_back_when_last_message_is_blank() {
        let mut outcome = RunOutcome::new(LoopRunResult::Completed);
        outcome.exit_code = Some(0);
        outcome.run_id = Some("run_0123456789abcdef01234567".to_owned());
        outcome.last_message = Some(" \n".to_owned());
        assert_eq!(
            summary(
                "watchdog",
                &spawn_entry(false, CheckOn::Fail),
                1_000,
                LoopRunMode::Manual,
                false,
                &outcome,
            ),
            "✓ completed in 1.0s\n  no final message; see: rimz loop show watchdog\n  run: run_0123456789abcdef01234567\n  pane closed; rerun with --keep to watch\n"
        );
    }

    #[test]
    fn delivered_summary_names_target_handle() {
        let mut outcome = RunOutcome::new(LoopRunResult::Delivered);
        outcome.target = Some("@planner".to_owned());

        assert_eq!(
            summary(
                "nudge",
                &wake_entry(false, CheckOn::Fail),
                90,
                LoopRunMode::Manual,
                false,
                &outcome,
            ),
            "✓ delivered to @planner in 90ms\n"
        );
    }

    #[test]
    fn check_only_verdicts_name_the_check_fact() {
        for (result, code, timed_out, expected) in [
            (
                LoopRunResult::Completed,
                Some(0),
                false,
                "✓ check passed (exit 0) in 1.2s\n",
            ),
            (
                LoopRunResult::Failed,
                Some(1),
                false,
                "✗ check failed (exit 1) in 1.2s\n",
            ),
            (
                LoopRunResult::TimedOut,
                None,
                true,
                "✗ check timed out in 1.2s\n",
            ),
        ] {
            let mut outcome = RunOutcome::new(result);
            outcome.check = Some(CheckRecord {
                code,
                timed_out,
                output: "detail".to_owned(),
            });
            assert_eq!(
                summary(
                    "certs",
                    &check_entry(),
                    1_200,
                    LoopRunMode::Manual,
                    false,
                    &outcome,
                ),
                expected
            );
        }
    }

    #[test]
    fn keep_hint_only_prints_for_manual_spawn_without_keep() {
        let mut outcome = RunOutcome::new(LoopRunResult::Completed);
        outcome.exit_code = Some(0);
        outcome.run_id = Some("run_0123456789abcdef01234567".to_owned());
        outcome.last_message = Some("done".to_owned());
        let entry = spawn_entry(false, CheckOn::Fail);

        for (mode, keep, should_hint) in [
            (LoopRunMode::Manual, false, true),
            (LoopRunMode::Manual, true, false),
            (LoopRunMode::Scheduled, false, false),
        ] {
            let stripped = summary("watchdog", &entry, 100, mode, keep, &outcome);
            assert_eq!(
                stripped.contains("pane closed; rerun with --keep to watch"),
                should_hint,
                "{mode:?} keep={keep}: {stripped}"
            );
        }
    }

    #[test]
    fn manual_early_exits_explain_what_stays_in_place() {
        let entry = wake_entry(false, CheckOn::Fail);
        let mut gone = RunOutcome::new(LoopRunResult::TargetGone);
        gone.target = Some("@planner".to_owned());
        assert_eq!(
            summary("nudge", &entry, 100, LoopRunMode::Manual, false, &gone,),
            "○ @planner not alive — schedule left in place\n"
        );

        let expired = RunOutcome::new(LoopRunResult::Expired);
        assert_eq!(
            summary("nudge", &entry, 100, LoopRunMode::Manual, false, &expired,),
            "○ deadline expired — task left in place\n"
        );
    }

    #[test]
    fn loop_record_copies_transcript_path() {
        let mut outcome = RunOutcome::new(LoopRunResult::Completed);
        outcome.transcript_path = Some("/tmp/rimz/session.jsonl".to_owned());

        let record = loop_record("wake", LoopRunMode::Manual, 123, &outcome);

        assert_eq!(
            record.transcript_path.as_deref(),
            Some("/tmp/rimz/session.jsonl")
        );
    }
}
