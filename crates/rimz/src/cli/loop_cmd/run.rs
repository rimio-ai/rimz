//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::*;

const CHECK_SUMMARY_OUTPUT_CAP: usize = 4 * 1024;

// ---- run --------------------------------------------------------------------

struct RunOutcome {
    result: LoopRunResult,
    check: Option<CheckRecord>,
    run_id: Option<String>,
    transcript_path: Option<String>,
    failure_tail: Option<String>,
    last_message: Option<String>,
    target: Option<String>,
    exit_code: Option<i32>,
    polarity: Option<CheckOn>,
    wake_subject: Option<String>,
}

#[derive(Clone, Copy)]
struct RunDisposition {
    source: TaskSource,
    mode: LoopRunMode,
}

impl RunOutcome {
    fn new(result: LoopRunResult) -> Self {
        Self {
            result,
            check: None,
            run_id: None,
            transcript_path: None,
            failure_tail: None,
            last_message: None,
            target: None,
            exit_code: None,
            polarity: None,
            wake_subject: None,
        }
    }
}

pub(super) fn run_one(
    name: &str,
    mode: LoopRunMode,
    keep: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let (entry, source) = load_task(name, globals)?
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    block_untrusted_project_task(name, &entry, source)?;
    let started = Instant::now();
    let _run_lock = match acquire_run_lock(name, &entry) {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            let duration_ms = elapsed_ms(started);
            run_log::append(&LoopRunRecord {
                task: name.to_owned(),
                at: Timestamp::now(),
                result: LoopRunResult::Overlapped,
                mode: Some(mode),
                duration_ms: Some(duration_ms),
                error: None,
                check: None,
                run_id: None,
                transcript_path: None,
                last_message: None,
                target: None,
            });
            writeln!(
                ui::out(),
                "loop `{name}`: previous run still active; skipping"
            )?;
            return Ok(());
        }
        Err(err) => {
            append_error_record(name, mode, started, &err);
            return Err(err);
        }
    };
    match execute_task(name, &entry, source, mode, keep, globals) {
        Ok(outcome) => {
            let duration_ms = elapsed_ms(started);
            let record = loop_record(name, mode, duration_ms, &outcome);
            run_log::append(&record);
            print_run_summary(name, duration_ms, &outcome)?;
            if let Some(code) = outcome.exit_code {
                std::process::exit(code);
            }
            Ok(())
        }
        Err(err) => {
            append_error_record(name, mode, started, &err);
            Err(err)
        }
    }
}

fn append_error_record(name: &str, mode: LoopRunMode, started: Instant, err: &anyhow::Error) {
    let duration_ms = elapsed_ms(started);
    let error = format!("{err:#}");
    run_log::append(&LoopRunRecord {
        task: name.to_owned(),
        at: Timestamp::now(),
        result: LoopRunResult::Errored,
        mode: Some(mode),
        duration_ms: Some(duration_ms),
        error: Some(error.clone()),
        check: None,
        run_id: None,
        transcript_path: None,
        last_message: None,
        target: None,
    });
    tracing::warn!(task = name, error = %error, "loop task run failed");
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
        } else {
            writeln!(
                ui::out(),
                "loop `{name}`: deadline expired; leaving task in place"
            )?;
        }
        return Ok(RunOutcome::new(LoopRunResult::Expired));
    }
    let mut check_detail = None;
    let prompt_override = match entry.check.as_deref() {
        Some(cmd) => {
            let echo = if mode == LoopRunMode::Manual {
                CheckEcho::Stream
            } else {
                CheckEcho::Capture
            };
            if echo == CheckEcho::Stream {
                let mut out = ui::out();
                writeln!(
                    out,
                    "loop `{name}`: check {}",
                    ui::paint(ui::palette::MUTED, cmd)
                )?;
                out.flush()?;
            }
            let outcome = run_check(
                &entry.resolved_root(),
                cmd,
                check_timeout(entry)?.unwrap_or(CHECK_DEFAULT_TIMEOUT),
                echo,
            )?;
            let record = check_record(&outcome);
            if echo == CheckEcho::Stream
                && !record.output.is_empty()
                && !record.output.ends_with('\n')
            {
                writeln!(ui::out())?;
            }
            match action {
                TaskAction::CheckOnly => {
                    if mode == LoopRunMode::Scheduled && instances::is_ephemeral(entry) {
                        let _ = remove_loaded_task(name, entry, source)?;
                    }
                    let mut run = RunOutcome::new(check_only_result(&outcome));
                    run.check = Some(record);
                    return Ok(run);
                }
                TaskAction::Spawn(_) | TaskAction::Deliver(_) => {
                    if !polarity_fires(entry.on, &outcome) {
                        let mut run = RunOutcome::new(LoopRunResult::CheckSkipped);
                        run.check = Some(record);
                        run.polarity = Some(entry.on.unwrap_or_default());
                        run.wake_subject = Some(task_subject(entry));
                        return Ok(run);
                    }
                    check_detail = Some(record);
                    Some(augment_prompt(resolve_task_prompt(entry)?, cmd, &outcome))
                }
            }
        }
        None => None,
    };
    let TaskAction::Spawn(spec) = action else {
        if let TaskAction::Deliver(target) = action {
            return execute_delivery_task(
                name,
                entry,
                RunDisposition { source, mode },
                target,
                prompt_override,
                check_detail,
                globals,
            );
        }
        unreachable!("check-only task without check is rejected by task_action");
    };
    let resolved = preflight_task(entry)?;
    let is_ping = agents_spec::virtual_ping_shape(spec);
    // The ping exists only to *start* a sliding budget window, so a token spent on
    // one already counting down buys nothing — skip it. Best-effort: an unknown or
    // cold reading falls through to the ping.
    if is_ping {
        let window_running = if entry.at_reset {
            reset_window_already_running(entry, &resolved.kind)?
        } else {
            window_already_running(entry, &resolved.kind)?
        };
        if window_running {
            writeln!(
                ui::out(),
                "loop `{name}`: {} budget window already active; skipping ping",
                resolved.kind
            )?;
            let mut run = RunOutcome::new(LoopRunResult::SkippedWindow);
            run.check = check_detail;
            return Ok(run);
        }
    }
    let prompt = match prompt_override {
        Some(prompt) => prompt,
        None => resolve_task_prompt(entry)?,
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
    let mut run_globals = globals.clone();
    run_globals.root = Some(entry.resolved_root());
    if mode == LoopRunMode::Scheduled && instances::is_ephemeral(entry) {
        // One-shot cleanup happens before the terminal run. A one-shot removed
        // pre-fire that then fails to launch is not retried.
        let _ = remove_loaded_task(name, entry, source)?;
    }
    let effort = entry
        .effort
        .clone()
        .or_else(|| is_ping.then(|| "low".to_owned()));
    let args = crate::cli::agents_cmd::AgentsArgs::for_task(crate::cli::agents_cmd::TaskRunArgs {
        spec: spec.to_owned(),
        prompt: Some(prompt),
        worktree: entry.worktree.clone(),
        mode: task_mode,
        effort,
        system_prompt_file,
        timeout,
        keep,
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
            run.exit_code = Some(status.exit_code());
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
    disposition: RunDisposition,
    target: &TaskTarget,
    prompt_override: Option<String>,
    check_record: Option<CheckRecord>,
    globals: &GlobalFlags,
) -> Result<RunOutcome> {
    if !delivery_target_alive(entry, target)? {
        if disposition.mode == LoopRunMode::Scheduled {
            writeln!(
                ui::out(),
                "loop `{name}`: target {} not alive; removing schedule",
                target.handle
            )?;
            let _ = remove_loaded_task(name, entry, disposition.source)?;
        } else {
            writeln!(
                ui::out(),
                "loop `{name}`: target {} not alive; leaving schedule in place",
                target.handle
            )?;
        }
        let mut run = RunOutcome::new(LoopRunResult::TargetGone);
        run.check = check_record;
        run.target = Some(target.handle.clone());
        return Ok(run);
    }
    let prompt = match prompt_override {
        Some(prompt) => prompt,
        None => resolve_task_prompt(entry)?,
    };
    if disposition.mode == LoopRunMode::Scheduled && instances::is_ephemeral(entry) {
        let _ = remove_loaded_task(name, entry, disposition.source)?;
    }
    let root = entry.resolved_root();
    match crate::cli::message::to_session(
        &root,
        &target.kind,
        &target.session,
        prompt,
        DeliveryGate::Done,
        globals,
    ) {
        Ok(()) => {
            let mut run = RunOutcome::new(LoopRunResult::Delivered);
            run.check = check_record;
            run.target = Some(target.handle.clone());
            Ok(run)
        }
        Err(err) if queue_resolution_miss(&err) => {
            if disposition.mode == LoopRunMode::Scheduled {
                writeln!(
                    ui::out(),
                    "loop `{name}`: target {} not alive; removing schedule",
                    target.handle
                )?;
                let _ = remove_loaded_task(name, entry, disposition.source)?;
            } else {
                writeln!(
                    ui::out(),
                    "loop `{name}`: target {} not alive; leaving schedule in place",
                    target.handle
                )?;
            }
            let mut run = RunOutcome::new(LoopRunResult::TargetGone);
            run.check = check_record;
            run.target = Some(target.handle.clone());
            Ok(run)
        }
        Err(err) => Err(err),
    }
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
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn print_run_summary(name: &str, duration_ms: u64, outcome: &RunOutcome) -> Result<()> {
    let mut out = ui::out();
    write_run_summary(&mut out, name, duration_ms, outcome)?;
    Ok(())
}

fn write_run_summary(
    out: &mut impl Write,
    name: &str,
    duration_ms: u64,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    if outcome.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(out, name, duration_ms, outcome);
    }
    let result_style = super::render::loop_result_style(outcome.result);
    let exit_label = outcome_exit_label(outcome);
    if matches!(
        outcome.result,
        LoopRunResult::Failed | LoopRunResult::TimedOut
    ) {
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
        writeln!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(tail) = outcome_failure_tail(outcome) {
            write_failure_tail(out, &tail)?;
        }
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
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::MUTED, &format!("  see: rimz loop show {name}"))
        )?;
    } else {
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_style, outcome.result.label())
        )?;
        if let Some(exit_label) = exit_label.as_deref() {
            write!(out, " {exit_label}")?;
        }
        writeln!(out, " in {}", render::format_duration_ms(duration_ms))?;
    }
    Ok(())
}

fn write_check_skipped_summary(
    out: &mut impl Write,
    name: &str,
    duration_ms: u64,
    outcome: &RunOutcome,
) -> std::io::Result<()> {
    let label = match outcome.check.as_ref() {
        Some(check) if check.timed_out => "check timed out".to_owned(),
        Some(check) if check.code == Some(0) => "check passed (exit 0)".to_owned(),
        Some(check) => match check.code {
            Some(code) => format!("check failed (exit {code})"),
            None => "check failed (signal)".to_owned(),
        },
        None => "check skipped".to_owned(),
    };
    write!(
        out,
        "loop `{name}`: {} in {}",
        ui::paint(ui::palette::MUTED, &label),
        render::format_duration_ms(duration_ms)
    )?;
    if let Some(subject) = outcome.wake_subject.as_deref() {
        if outcome.check.as_ref().is_some_and(|check| check.timed_out) {
            write!(out, " — {subject} not woken")?;
        } else if let Some(polarity) = outcome.polarity {
            write!(
                out,
                " — on={}, {subject} not woken",
                check_on_label(polarity)
            )?;
        } else {
            write!(out, " — {subject} not woken")?;
        }
    }
    writeln!(out)
}

fn check_on_label(on: CheckOn) -> &'static str {
    match on {
        CheckOn::Fail => "fail",
        CheckOn::Success => "success",
    }
}

fn outcome_exit_label(outcome: &RunOutcome) -> Option<String> {
    if let Some(exit) = outcome
        .exit_code
        .or_else(|| outcome.check.as_ref().and_then(|check| check.code))
    {
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
                let _ = remove_task(&name, source)?;
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
    fn failed_summary_links_run_transcript_and_loop_show() {
        let mut outcome = RunOutcome::new(LoopRunResult::Failed);
        outcome.exit_code = Some(1);
        outcome.run_id = Some("run_0123456789abcdef01234567".to_owned());
        outcome.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        outcome.failure_tail = Some("error: boom\nUsage: codex [OPTIONS] [PROMPT]".to_owned());
        let mut out = Vec::new();

        write_run_summary(&mut out, "wake", 1_900, &outcome).unwrap();

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&ui::paint(ui::palette::ALARM.bold(), "failed (exit 1)")));
        let out = anstream::adapter::strip_str(&raw).to_string();
        assert!(out.contains("loop `wake`: failed (exit 1) in 1.9s"));
        assert!(out.contains("  │ error: boom\n  │ Usage: codex [OPTIONS] [PROMPT]"));
        assert!(out.contains("run: run_0123456789abcdef01234567"));
        assert!(out.contains("transcript: /tmp/transcript.jsonl"));
        assert!(out.contains("see: rimz loop show wake"));
    }

    #[test]
    fn skipped_check_summary_names_polarity_and_unwoken_target() {
        let mut outcome = RunOutcome::new(LoopRunResult::CheckSkipped);
        outcome.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        outcome.polarity = Some(CheckOn::Fail);
        outcome.wake_subject = Some("codex".to_owned());
        let mut out = Vec::new();

        write_run_summary(&mut out, "wake", 7_300, &outcome).unwrap();

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&ui::paint(ui::palette::MUTED, "check passed (exit 0)")));
        let out = anstream::adapter::strip_str(&raw).to_string();
        assert_eq!(
            out,
            "loop `wake`: check passed (exit 0) in 7.3s — on=fail, codex not woken\n"
        );
    }

    #[test]
    fn skipped_failed_check_summary_names_success_guard() {
        let mut outcome = RunOutcome::new(LoopRunResult::CheckSkipped);
        outcome.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "no".to_owned(),
        });
        outcome.polarity = Some(CheckOn::Success);
        outcome.wake_subject = Some("codex".to_owned());
        let mut out = Vec::new();

        write_run_summary(&mut out, "wake", 2_000, &outcome).unwrap();

        let out = String::from_utf8(out).unwrap();
        let out = anstream::adapter::strip_str(&out).to_string();
        assert_eq!(
            out,
            "loop `wake`: check failed (exit 1) in 2.0s — on=success, codex not woken\n"
        );
    }

    #[test]
    fn skipped_timeout_summary_omits_polarity() {
        let mut outcome = RunOutcome::new(LoopRunResult::CheckSkipped);
        outcome.check = Some(CheckRecord {
            code: None,
            timed_out: true,
            output: String::new(),
        });
        outcome.polarity = Some(CheckOn::Success);
        outcome.wake_subject = Some("codex".to_owned());
        let mut out = Vec::new();

        write_run_summary(&mut out, "wake", 300_000, &outcome).unwrap();

        let out = String::from_utf8(out).unwrap();
        let out = anstream::adapter::strip_str(&out).to_string();
        assert_eq!(
            out,
            "loop `wake`: check timed out in 5m — codex not woken\n"
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
