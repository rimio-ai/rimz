//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::*;

const CHECK_SUMMARY_OUTPUT_CAP: usize = 4 * 1024;

// ---- run --------------------------------------------------------------------

struct RunSummary<'a> {
    record: &'a LoopRunRecord,
    presentation: &'a LoopRunPresentation,
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

pub(super) fn run_one(
    name: &str,
    mode: LoopRunMode,
    keep: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let catalog = task_catalog(globals)?;
    let loaded = catalog
        .for_run(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    let entry = loaded.entry().clone();
    let source = loaded.source();
    gate_project_trust(name, &entry, source, mode)?;
    let action = loaded.action().cloned().map_err(Clone::clone)?;
    refresh_reset_ping_usage(&loaded);
    let started = Instant::now();
    if mode == LoopRunMode::Manual {
        write_manual_header(&mut ui::out(), name, &entry, &action)?;
    }
    if mode == LoopRunMode::Manual
        && pauses::load()
            .get(name)
            .is_some_and(|entry| pauses::is_active(entry, Timestamp::now()))
    {
        writeln!(
            ui::out(),
            "{}",
            ui::paint(ui::palette::muted(), "  task is paused; firing anyway")
        )?;
    }
    let config = MachineConfig::load_lenient();
    let check_echo = match mode {
        LoopRunMode::Scheduled => CheckEcho::Capture,
        LoopRunMode::Manual => CheckEcho::Stream {
            announcement: entry.check.as_deref().map(|cmd| {
                format!(
                    "{}\n",
                    ui::paint(ui::palette::muted(), &format!("  check: {cmd}"))
                )
            }),
            prefix: ui::paint(ui::palette::faint(), "  │ "),
        },
    };
    let mut fire = rimz::harness::schedule::runner::TaskFire::new(
        name,
        loaded,
        &catalog,
        mode,
        keep,
        Timestamp::now(),
        config,
        check_echo,
        started,
    )?;
    let plan = fire.prepare();
    if mode == LoopRunMode::Manual
        && let Some(trip) = fire.take_check_trip()
        && let Err(source) =
            write_check_trip_line(&mut ui::out(), &action, &trip.record, trip.duration_ms)
    {
        let err = source.into();
        if matches!(
            &plan,
            Ok(rimz::harness::schedule::runner::TaskFirePlan::Done(_))
        ) {
            return Err(err);
        }
        return Err(record_task_error(&mut fire, name, &entry, err));
    }
    let plan = match plan {
        Ok(plan) => plan,
        Err(err) => {
            return Err(record_task_error(&mut fire, name, &entry, err));
        }
    };
    let finished = match plan {
        rimz::harness::schedule::runner::TaskFirePlan::Done(finished) => finished,
        rimz::harness::schedule::runner::TaskFirePlan::Spawn(prepared) => {
            let mut run_globals = globals.clone();
            run_globals.root = Some(prepared.root.clone());
            let effect = crate::cli::supervised::run::run_supervised(
                prepared.request,
                crate::cli::supervised::SupervisedPresentation::text(prepared.stream),
                &run_globals,
            )
            .map(rimz::harness::schedule::runner::TaskFireEffect::Spawn);
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
        rimz::harness::schedule::runner::TaskFirePlan::Deliver(prepared) => {
            let effect = execute_prepared_delivery(prepared, globals);
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
    };
    present_finished(name, &entry, &action, mode, keep, &finished)?;
    if let Some(code) = finished.presentation.exit_code {
        std::process::exit(code);
    }
    Ok(())
}

fn refresh_reset_ping_usage(task: &LoadedTask) {
    let Some(kind) = task.reset_ping_kind() else {
        return;
    };
    let entry = task.entry();
    let Some(runtime) = runtime_for_root(&entry.resolved_root()) else {
        return;
    };
    let _ = rimz::sidebar::refresh::usage::refresh_account_usage_now(&runtime, kind.as_str());
}

fn gate_project_trust(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    mode: LoopRunMode,
) -> Result<()> {
    let Some(state) = source.blocked_state() else {
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

fn finish_task_effect(
    fire: &mut rimz::harness::schedule::runner::TaskFire<'_>,
    effect: Result<rimz::harness::schedule::runner::TaskFireEffect>,
    name: &str,
    entry: &TaskEntry,
) -> Result<rimz::harness::schedule::runner::TaskFireFinished> {
    match effect {
        Ok(effect) => match fire.finish(effect) {
            Ok(finished) => Ok(finished),
            Err(err) => Err(record_task_error(fire, name, entry, err)),
        },
        Err(err) => Err(record_task_error(fire, name, entry, err)),
    }
}

fn record_task_error(
    fire: &mut rimz::harness::schedule::runner::TaskFire<'_>,
    name: &str,
    entry: &TaskEntry,
    err: anyhow::Error,
) -> anyhow::Error {
    let finished = fire.finish_error(&err);
    handle_run_transition(name, entry, finished.transition);
    tracing::warn!(task = name, error = %err, "loop task run failed");
    err
}

fn handle_run_transition(name: &str, entry: &TaskEntry, transition: RunTransition) {
    if let RunTransition::AutoPaused { strikes } = transition {
        let _ = writeln!(
            ui::out(),
            "loop `{name}`: paused after {strikes} consecutive failed fires; resume with `rimz loop resume {name}`"
        );
        notify_loop_paused(name, entry, strikes);
    }
}

fn notify_loop_paused(name: &str, entry: &TaskEntry, count: u32) {
    let notification = rimz::sidebar::notify::Notification {
        agents: Vec::new(),
        notification_kind: rimz::sidebar::notify::NotificationKind::LoopPaused,
        title: format!("RimZ: loop {name} paused"),
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

fn present_finished(
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    mode: LoopRunMode,
    keep: bool,
    finished: &rimz::harness::schedule::runner::TaskFireFinished,
) -> Result<()> {
    use rimz::harness::schedule::runner::TaskFireNotice;

    handle_run_transition(name, entry, finished.transition);
    match &finished.notice {
        TaskFireNotice::Gate { reason } => {
            if mode == LoopRunMode::Manual {
                write_manual_verdict(
                    &mut ui::out(),
                    finished.record.result,
                    &format!("{} — {reason}", finished.record.result.label()),
                )?;
            } else {
                writeln!(ui::out(), "loop `{name}`: {reason}; skipping")?;
            }
            return Ok(());
        }
        TaskFireNotice::Overlap { detail } => {
            let stop_hint = format!("stop it with `rimz loop stop {name}`");
            if mode == LoopRunMode::Manual {
                let detail = detail
                    .as_ref()
                    .map(|detail| format!("{detail}; {stop_hint}"))
                    .unwrap_or_else(|| format!("previous run still active — skipped; {stop_hint}"));
                write_manual_verdict(&mut ui::out(), LoopRunResult::Overlapped, &detail)?;
            } else if let Some(detail) = detail {
                writeln!(ui::out(), "loop `{name}`: {detail}; {stop_hint}")?;
            } else {
                writeln!(
                    ui::out(),
                    "loop `{name}`: previous run still active; skipping; {stop_hint}"
                )?;
            }
            return Ok(());
        }
        TaskFireNotice::PingWindow { kind } if mode == LoopRunMode::Scheduled => {
            writeln!(
                ui::out(),
                "loop `{name}`: {kind} budget window already active; skipping ping"
            )?;
        }
        TaskFireNotice::TargetGone { handle } if mode == LoopRunMode::Scheduled => {
            writeln!(
                ui::out(),
                "loop `{name}`: target {handle} not alive; removing schedule"
            )?;
        }
        TaskFireNotice::None
        | TaskFireNotice::PingWindow { .. }
        | TaskFireNotice::TargetGone { .. } => {}
    }
    let summary = RunSummary {
        record: &finished.record,
        presentation: &finished.presentation,
    };
    print_run_summary(name, entry, action, mode, keep, &summary)
}

fn execute_prepared_delivery(
    prepared: rimz::harness::schedule::runner::PreparedDelivery,
    globals: &GlobalFlags,
) -> Result<rimz::harness::schedule::runner::TaskFireEffect> {
    let workspace = WorkspaceResolver::resolve_participant(".", Some(prepared.root))?;
    let store = crate::cli::open_store(&workspace)?;
    let channel = crate::cli::current_channel(&workspace);
    let sender = crate::cli::send::sender_from_env(channel.as_deref(), false);
    tracing::debug!(
        kind = prepared.target.kind,
        session = prepared.target.session,
        "queueing loop wake-up"
    );
    let dispatched = rimz::message::dispatch::dispatch(
        &workspace,
        &store,
        rimz::message::dispatch::DispatchRequest {
            target: format!("@{}", prepared.target.session),
            text: prepared.prompt,
            target_scope: None,
            current_channel: channel,
            sender,
            automated: true,
            allow_fanout: false,
            reply: None,
            mux: globals.mux,
            mode: rimz::message::dispatch::DispatchMode::Boundary {
                enter: true,
                gate: DeliveryGate::Done,
                force: false,
                auto_compact: None,
                not_before: None,
                after: Vec::new(),
                when: Vec::new(),
            },
        },
    );
    match dispatched {
        Ok(result) => {
            crate::cli::send::report_dispatch(
                crate::cli::send::ReportMode::Boundary,
                &prepared.target.handle,
                &result.outcomes,
                &result.compacted,
            )?;
            Ok(rimz::harness::schedule::runner::TaskFireEffect::Delivered)
        }
        Err(rimz::message::dispatch::DispatchErr::Recipient(
            rimz::TargetErr::NoMatch { .. } | rimz::TargetErr::NoMatchInChannel { .. },
        )) => Ok(rimz::harness::schedule::runner::TaskFireEffect::TargetGone),
        Err(err) => Err(err.into()),
    }
}

fn write_manual_header(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{}{}",
        ui::paint(ui::palette::header(), name),
        ui::paint(
            ui::palette::muted(),
            &format!(" — {}", render::task_run_rule(entry, action))
        )
    )
}

fn write_manual_verdict(
    out: &mut impl Write,
    result: LoopRunResult,
    label: &str,
) -> std::io::Result<()> {
    let mark = render::loop_result_mark(result);
    writeln!(
        out,
        "{}",
        ui::paint(mark.style, &format!("{} {label}", mark.glyph))
    )
}

fn print_run_summary(
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    mode: LoopRunMode,
    keep: bool,
    summary: &RunSummary<'_>,
) -> Result<()> {
    let mut out = ui::out();
    write_run_summary(&mut out, name, entry, action, mode, keep, summary)?;
    Ok(())
}

fn write_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    mode: LoopRunMode,
    keep: bool,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let action_kind = action.kind();
    match mode {
        LoopRunMode::Manual => {
            write_manual_run_summary(out, name, entry, action, action_kind, keep, summary)
        }
        LoopRunMode::Scheduled => write_scheduled_run_summary(out, name, entry, action, summary),
    }
}

fn write_manual_run_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    action_kind: TaskActionKind,
    keep: bool,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let record = summary.record;
    let duration_ms = record.duration_ms.unwrap_or_default();
    if record.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            action,
            duration_ms,
            LoopRunMode::Manual,
            summary,
        );
    }
    if let Some((result, label)) = manual_early_verdict(summary, duration_ms) {
        return write_manual_verdict(out, result, &label);
    }

    let result_mark = render::loop_result_mark(record.result);
    let result_label = manual_result_label(action_kind, summary);
    write!(
        out,
        "{}",
        ui::paint(
            result_mark.style,
            &format!("{} {result_label}", result_mark.glyph)
        )
    )?;
    write!(out, " in {}", render::format_duration_ms(duration_ms))?;
    if let Some(spend) =
        render::spend_segments(record.cost_usd, record.input_tokens, record.output_tokens)
    {
        write!(out, " · {spend}")?;
    }
    writeln!(out)?;

    if is_spawn_failure(record.result) && !action_kind.is_check_only() {
        write_failure_forensics(out, name, summary)?;
    } else if record.result == LoopRunResult::Completed && record.run_id.is_some() {
        write_completion_detail(out, name, summary)?;
    }
    if !is_spawn_failure(record.result) && !keep && record.run_id.is_some() {
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::muted(),
                "  pane closed; rerun with --keep to watch"
            )
        )?;
    }
    Ok(())
}

fn manual_early_verdict(
    summary: &RunSummary<'_>,
    duration_ms: u64,
) -> Option<(LoopRunResult, String)> {
    let label = match summary.record.result {
        LoopRunResult::Expired => "deadline expired — task left in place".to_owned(),
        LoopRunResult::TargetGone => format!(
            "{} not alive — schedule left in place",
            summary.record.target.as_deref().unwrap_or("target")
        ),
        LoopRunResult::SkippedWindow => {
            let mut label = format!("skipped in {}", render::format_duration_ms(duration_ms));
            if let Some(reason) = &summary.presentation.skip_reason {
                label.push_str(" — ");
                label.push_str(reason);
            }
            label
        }
        _ => return None,
    };
    Some((summary.record.result, label))
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
    action: &TaskAction,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let record = summary.record;
    let duration_ms = record.duration_ms.unwrap_or_default();
    if record.result == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            action,
            duration_ms,
            LoopRunMode::Scheduled,
            summary,
        );
    }
    let result_mark = render::loop_result_mark(record.result);
    let exit_label = outcome_exit_label(summary);
    if is_spawn_failure(record.result) {
        let mut label = record.result.label().to_owned();
        if let Some(exit_label) = exit_label.as_deref() {
            label.push(' ');
            label.push_str(exit_label);
        }
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_mark.style.bold(), &label)
        )?;
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(spend) =
            render::spend_segments(record.cost_usd, record.input_tokens, record.output_tokens)
        {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        write_failure_forensics(out, name, summary)?;
    } else {
        let result_label = success_result_label(record);
        write!(
            out,
            "loop `{name}`: {}",
            ui::paint(result_mark.style, &result_label)
        )?;
        if let Some(exit_label) = exit_label.as_deref() {
            write!(out, " {exit_label}")?;
        }
        write!(out, " in {}", render::format_duration_ms(duration_ms))?;
        if let Some(spend) =
            render::spend_segments(record.cost_usd, record.input_tokens, record.output_tokens)
        {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        if record.result == LoopRunResult::Completed && record.run_id.is_some() {
            write_completion_detail(out, name, summary)?;
        }
    }
    Ok(())
}

fn success_result_label(record: &LoopRunRecord) -> String {
    match (record.result, record.target.as_deref()) {
        (LoopRunResult::Delivered, Some(target)) => format!("delivered to {target}"),
        _ => record.result.label().to_owned(),
    }
}

fn manual_result_label(action_kind: TaskActionKind, summary: &RunSummary<'_>) -> String {
    if action_kind.is_check_only()
        && let Some(check) = &summary.record.check
    {
        return check_result_label(check);
    }
    let mut label = success_result_label(summary.record);
    if let Some(exit_label) = outcome_exit_label(summary) {
        label.push(' ');
        label.push_str(&exit_label);
    }
    label
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
    action: &TaskAction,
    check: &CheckRecord,
    duration_ms: u64,
) -> std::io::Result<()> {
    let (glyph, style) = if check.timed_out || check.code != Some(0) {
        ("✗", ui::palette::alarm())
    } else {
        ("✓", ui::palette::good())
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
            ui::palette::accent(),
            &format!("→ {}", render::action_progressive_phrase(action))
        )
    )
}

fn write_check_skipped_summary(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    action: &TaskAction,
    duration_ms: u64,
    mode: LoopRunMode,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    let label = summary
        .record
        .check
        .as_ref()
        .map(check_result_label)
        .unwrap_or_else(|| "check skipped".to_owned());
    let check_duration_ms = summary
        .presentation
        .check_duration_ms
        .unwrap_or(duration_ms);
    let duration = render::format_duration_ms(check_duration_ms);
    let (glyph, style) = render::check_skip_display(summary.record.check.as_ref());
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
                ui::palette::muted(),
                &format!(" — {}", render::check_skip_decision(entry, action))
            )
        )
    } else {
        write!(out, "loop `{name}`: {}", ui::paint(style, &label))?;
        writeln!(
            out,
            " in {duration} — {}",
            render::check_skip_decision(entry, action)
        )
    }
}

fn write_failure_forensics(
    out: &mut impl Write,
    name: &str,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    if let Some(tail) = outcome_failure_tail(summary) {
        write_failure_tail(out, &tail)?;
    }
    write_run_links(out, summary.record)?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::muted(),
            &format!("  see: rimz loop show {name}")
        )
    )
}

fn write_run_links(out: &mut impl Write, record: &LoopRunRecord) -> std::io::Result<()> {
    if let Some(run_id) = &record.run_id {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  run: {run_id}"))
        )?;
    }
    if let Some(transcript) = &record.transcript_path {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  transcript: {transcript}"))
        )?;
    }
    Ok(())
}

fn write_completion_detail(
    out: &mut impl Write,
    name: &str,
    summary: &RunSummary<'_>,
) -> std::io::Result<()> {
    if !summary.presentation.streamed {
        if let Some(message) = summary
            .record
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
                    ui::palette::muted(),
                    &format!("  no final message; see: rimz loop show {name}")
                )
            )?;
        }
    }
    write_run_links(out, summary.record)
}

fn outcome_exit_label(summary: &RunSummary<'_>) -> Option<String> {
    if let Some(exit) = summary.presentation.exit_code {
        if exit == 0 {
            return None;
        }
        Some(format!("(exit {exit})"))
    } else if let Some(exit) = summary.record.check.as_ref().and_then(|check| check.code) {
        Some(format!("(exit {exit})"))
    } else if summary
        .record
        .check
        .as_ref()
        .is_some_and(|check| check.timed_out)
    {
        Some("(timeout)".to_owned())
    } else {
        None
    }
}

fn outcome_failure_tail(summary: &RunSummary<'_>) -> Option<String> {
    if let Some(tail) = summary
        .presentation
        .failure_tail
        .as_deref()
        .filter(|tail| !tail.trim().is_empty())
    {
        return Some(tail.trim_end().to_owned());
    }
    let check = summary.record.check.as_ref()?;
    if !check.timed_out && check.code == Some(0) {
        return None;
    }
    let tail = rimz::proc::tail_output(check.output.as_bytes(), CHECK_SUMMARY_OUTPUT_CAP);
    let tail = tail.trim_end();
    (!tail.trim().is_empty()).then(|| tail.to_owned())
}

fn write_failure_tail(out: &mut impl Write, tail: &str) -> std::io::Result<()> {
    render::write_gutter_block(out, Some(ui::palette::alarm()), tail)
}

#[cfg(test)]
#[path = "run/tests.rs"]
mod tests;
