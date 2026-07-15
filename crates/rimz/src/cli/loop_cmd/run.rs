//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::*;

const CHECK_SUMMARY_OUTPUT_CAP: usize = 4 * 1024;

// ---- run --------------------------------------------------------------------

type RunOutcome = LoopRunOutcome;

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
    let entry = loaded.entry;
    let source = loaded.source;
    gate_project_trust(name, &entry, source, mode)?;
    TaskAction::from_entry(name, &entry)?;
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
    let config = MachineConfig::load_lenient();
    let check_echo = match mode {
        LoopRunMode::Scheduled => CheckEcho::Capture,
        LoopRunMode::Manual => CheckEcho::Stream {
            announcement: entry.check.as_deref().map(|cmd| {
                format!(
                    "{}\n",
                    ui::paint(ui::palette::MUTED, &format!("  check: {cmd}"))
                )
            }),
            prefix: ui::paint(ui::palette::FAINT, "  │ "),
        },
    };
    let mut fire = rimz::harness::schedule::runner::TaskFire::new(
        name,
        entry.clone(),
        &catalog,
        mode,
        keep,
        Timestamp::now(),
        config,
        check_echo,
        started,
    )?;
    let plan = match fire.prepare() {
        Ok(plan) => plan,
        Err(err) => {
            return Err(record_task_error(&mut fire, name, &entry, err));
        }
    };
    let finished = match plan {
        rimz::harness::schedule::runner::TaskFirePlan::Done(finished) => finished,
        rimz::harness::schedule::runner::TaskFirePlan::Spawn(prepared) => {
            if mode == LoopRunMode::Manual
                && let (Some(check), Some(duration_ms)) =
                    (prepared.check.as_ref(), prepared.check_duration_ms)
                && let Err(source) =
                    write_check_trip_line(&mut ui::out(), &entry, check, duration_ms)
            {
                return Err(record_task_error(&mut fire, name, &entry, source.into()));
            }
            let mut run_globals = globals.clone();
            run_globals.root = Some(prepared.root.clone());
            let effect = crate::cli::supervised::run::run_supervised(
                prepared.request,
                crate::cli::supervised::SupervisedPresentation::text(prepared.stream),
                &run_globals,
            )
            .map(|record| {
                rimz::harness::schedule::runner::TaskFireEffect::Spawn(record.map(Box::new))
            });
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
        rimz::harness::schedule::runner::TaskFirePlan::Deliver(prepared) => {
            if mode == LoopRunMode::Manual
                && let (Some(check), Some(duration_ms)) =
                    (prepared.check.as_ref(), prepared.check_duration_ms)
                && let Err(source) =
                    write_check_trip_line(&mut ui::out(), &entry, check, duration_ms)
            {
                return Err(record_task_error(&mut fire, name, &entry, source.into()));
            }
            let effect = execute_prepared_delivery(prepared, globals);
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
    };
    present_finished(name, &entry, mode, keep, &finished)?;
    if let Some(code) = finished.outcome.exit_code() {
        std::process::exit(code);
    }
    Ok(())
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

fn present_finished(
    name: &str,
    entry: &TaskEntry,
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
                    finished.outcome.result(),
                    &format!("{} — {reason}", finished.outcome.result().label()),
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
    print_run_summary(
        name,
        entry,
        finished.duration_ms,
        mode,
        keep,
        &finished.outcome,
    )
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
    if outcome.result() == LoopRunResult::CheckSkipped {
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

    let result_style = render::loop_result_style(outcome.result());
    let result_label = manual_result_label(entry, outcome);
    write!(
        out,
        "{}",
        ui::paint(
            result_style,
            &format!(
                "{} {result_label}",
                render::loop_result_glyph(outcome.result())
            )
        )
    )?;
    write!(out, " in {}", render::format_duration_ms(duration_ms))?;
    if let Some(spend) = render::spend_segments(
        outcome.cost_usd(),
        outcome.input_tokens(),
        outcome.output_tokens(),
    ) {
        write!(out, " · {spend}")?;
    }
    writeln!(out)?;

    if is_spawn_failure(outcome.result()) && !is_check_only(entry) {
        write_failure_forensics(out, name, outcome)?;
    } else if outcome.result() == LoopRunResult::Completed && outcome.run_id().is_some() {
        write_completion_detail(out, name, outcome)?;
    }
    if !is_spawn_failure(outcome.result()) && !keep && outcome.run_id().is_some() {
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
    let label = match outcome.result() {
        LoopRunResult::Expired => "deadline expired — task left in place".to_owned(),
        LoopRunResult::TargetGone => format!(
            "{} not alive — schedule left in place",
            outcome.target().unwrap_or("target")
        ),
        LoopRunResult::SkippedWindow => {
            let mut label = format!("skipped in {}", render::format_duration_ms(duration_ms));
            if let Some(reason) = outcome.skip_reason() {
                label.push_str(" — ");
                label.push_str(reason);
            }
            label
        }
        _ => return None,
    };
    Some((outcome.result(), label))
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
    if outcome.result() == LoopRunResult::CheckSkipped {
        return write_check_skipped_summary(
            out,
            name,
            entry,
            duration_ms,
            LoopRunMode::Scheduled,
            outcome,
        );
    }
    let result_style = render::loop_result_style(outcome.result());
    let exit_label = outcome_exit_label(outcome);
    if is_spawn_failure(outcome.result()) {
        let mut label = outcome.result().label().to_owned();
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
            outcome.cost_usd(),
            outcome.input_tokens(),
            outcome.output_tokens(),
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
            outcome.cost_usd(),
            outcome.input_tokens(),
            outcome.output_tokens(),
        ) {
            write!(out, " · {spend}")?;
        }
        writeln!(out)?;
        if outcome.result() == LoopRunResult::Completed && outcome.run_id().is_some() {
            write_completion_detail(out, name, outcome)?;
        }
    }
    Ok(())
}

fn success_result_label(outcome: &RunOutcome) -> String {
    match (outcome.result(), outcome.target()) {
        (LoopRunResult::Delivered, Some(target)) => format!("delivered to {target}"),
        _ => outcome.result().label().to_owned(),
    }
}

fn manual_result_label(entry: &TaskEntry, outcome: &RunOutcome) -> String {
    if is_check_only(entry)
        && let Some(check) = outcome.check()
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
        .check()
        .map(check_result_label)
        .unwrap_or_else(|| "check skipped".to_owned());
    let check_duration_ms = outcome.check_duration_ms().unwrap_or(duration_ms);
    let duration = render::format_duration_ms(check_duration_ms);
    let (glyph, style) = render::check_skip_display(outcome.check());
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
    if let Some(run_id) = outcome.run_id() {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::MUTED, &format!("  run: {run_id}"))
        )?;
    }
    if let Some(transcript) = outcome.transcript_path() {
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
    if !outcome.streamed() {
        if let Some(message) = outcome.last_message().filter(|msg| !msg.trim().is_empty()) {
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
    if let Some(exit) = outcome.exit_code() {
        if exit == 0 {
            return None;
        }
        Some(format!("(exit {exit})"))
    } else if let Some(exit) = outcome.check().and_then(|check| check.code) {
        Some(format!("(exit {exit})"))
    } else if outcome.check().is_some_and(|check| check.timed_out) {
        Some("(timeout)".to_owned())
    } else {
        None
    }
}

fn outcome_failure_tail(outcome: &RunOutcome) -> Option<String> {
    if let Some(tail) = outcome
        .failure_tail()
        .filter(|tail| !tail.trim().is_empty())
    {
        return Some(tail.trim_end().to_owned());
    }
    let check = outcome.check()?;
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
        let outcome = RunOutcome::terminal(LoopRunResult::Failed)
            .with_exit_code(Some(1))
            .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
            .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
            .with_failure_tail(Some(
                "error: boom\nUsage: codex [OPTIONS] [PROMPT]".to_owned(),
            ));

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
        let spawn = RunOutcome::check_result(
            LoopRunResult::CheckSkipped,
            CheckRecord {
                code: Some(0),
                timed_out: false,
                output: "ok".to_owned(),
            },
            4_400,
        );
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

        let wake = RunOutcome::check_result(
            LoopRunResult::CheckSkipped,
            CheckRecord {
                code: Some(1),
                timed_out: false,
                output: "no".to_owned(),
            },
            2_000,
        );
        let entry = wake_entry(true, CheckOn::Success);
        assert_eq!(
            summary("nudge", &entry, 8_000, LoopRunMode::Manual, false, &wake,),
            "○ check failed (exit 1) in 2.0s — @planner not woken; fires when the check passes\n"
        );
    }

    #[test]
    fn scheduled_check_skip_keeps_compact_task_prefix() {
        let entry = spawn_entry(true, CheckOn::Fail);
        let outcome = RunOutcome::check_result(
            LoopRunResult::CheckSkipped,
            CheckRecord {
                code: Some(0),
                timed_out: false,
                output: "ok".to_owned(),
            },
            700,
        );

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
        let outcome = RunOutcome::completed(None)
            .with_exit_code(Some(0))
            .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
            .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
            .with_last_message(Some("pong\n".to_owned()))
            .with_cost(Some(0.42), Some(12_000), Some(3_400));

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
        let outcome = RunOutcome::completed(None)
            .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
            .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
            .with_last_message(Some("already streamed".to_owned()))
            .with_streamed(true);

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
        let outcome = RunOutcome::completed(None).with_cost(Some(0.09), Some(14_000), Some(269));

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

        let outcome = RunOutcome::terminal(LoopRunResult::Failed)
            .with_exit_code(Some(1))
            .with_cost(Some(0.09), Some(14_000), Some(269));
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
        let outcome = RunOutcome::completed(None)
            .with_exit_code(Some(0))
            .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
            .with_last_message(Some(" \n".to_owned()));
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
        let outcome = RunOutcome::delivery("@planner", None);

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
            let outcome = RunOutcome::terminal(result).with_check(Some(CheckRecord {
                code,
                timed_out,
                output: "detail".to_owned(),
            }));
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
        let outcome = RunOutcome::completed(None)
            .with_exit_code(Some(0))
            .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
            .with_last_message(Some("done".to_owned()));
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
        let gone = RunOutcome::target_gone("@planner", None);
        assert_eq!(
            summary("nudge", &entry, 100, LoopRunMode::Manual, false, &gone,),
            "○ @planner not alive — schedule left in place\n"
        );

        let expired = RunOutcome::expiry();
        assert_eq!(
            summary("nudge", &entry, 100, LoopRunMode::Manual, false, &expired,),
            "○ deadline expired — task left in place\n"
        );
    }

    #[test]
    fn outcome_record_copies_transcript_path() {
        let outcome = RunOutcome::completed(None)
            .with_transcript_path(Some("/tmp/rimz/session.jsonl".to_owned()));

        let record = outcome.record("wake", LoopRunMode::Manual, 123);

        assert_eq!(
            record.transcript_path.as_deref(),
            Some("/tmp/rimz/session.jsonl")
        );
    }
}
