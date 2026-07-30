//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::run_report::{
    RunSummary, write_check_trip_line, write_manual_verdict, write_run_summary,
};
use super::*;

// ---- run --------------------------------------------------------------------

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
    let key = task_key(name, &loaded);
    let arm_state = ArmState::resolve(arming::load().get(&key), source, Timestamp::now());
    if mode == LoopRunMode::Scheduled && arm_state != ArmState::Live {
        return Ok(());
    }
    let action = loaded.action().cloned().map_err(Clone::clone)?;
    let started = Instant::now();
    if mode == LoopRunMode::Manual {
        write_manual_header(&mut ui::out(), name, &entry, &action)?;
    }
    if mode == LoopRunMode::Manual {
        let notice = match arm_state {
            ArmState::Disabled(_) => Some("  task is disabled; firing anyway"),
            ArmState::Paused(_) => Some("  task is paused; firing anyway"),
            ArmState::Live => None,
        };
        if let Some(notice) = notice {
            writeln!(ui::out(), "{}", ui::paint(ui::palette::muted(), notice))?;
        }
    }
    let config = MachineConfig::load_lenient();
    if mode == LoopRunMode::Manual {
        crate::cli::report_unknown_config_keys(&config)?;
    }
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
    if let RunTransition::AutoDisabled { strikes } = transition {
        let _ = writeln!(
            ui::out(),
            "loop `{name}`: disabled after {strikes} consecutive failed fires; enable with `rimz loop enable {name}`"
        );
        notify_loop_disabled(name, entry, strikes);
    }
}

fn notify_loop_disabled(name: &str, entry: &TaskEntry, count: u32) {
    let notification = rimz::sidebar::notify::Notification {
        agents: Vec::new(),
        notification_kind: rimz::sidebar::notify::NotificationKind::LoopDisabled,
        title: format!("RimZ: loop {name} disabled"),
        body: format!(
            "{count} consecutive failed fires; inspect with `rimz loop show {name}`, enable with `rimz loop enable {name}`"
        ),
        unread_count: None,
    };
    let prefs = MachineConfig::load_lenient().notifications.clone();
    rimz::sidebar::notify::spawn_notify_handlers(&prefs, &notification);

    let workspace_id = WorkspaceId::from_project_root(&entry.resolved_root());
    let runtime = match RuntimePaths::for_workspace(workspace_id) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::debug!(task = name, error = %err, "loop auto-disable runtime unavailable");
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
        tracing::debug!(task = name, error = %err, "loop auto-disable notification broadcast failed");
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
        TaskFireNotice::TargetGone { handle } if mode == LoopRunMode::Scheduled => {
            writeln!(
                ui::out(),
                "loop `{name}`: target {handle} not alive; removing schedule"
            )?;
        }
        TaskFireNotice::None | TaskFireNotice::TargetGone { .. } => {}
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
            caller: None,
            sender,
            automated: true,
            allow_fanout: false,
            reply: None,
            mux: globals.mux,
            mode: rimz::message::dispatch::DispatchMode::Boundary {
                enter: true,
                gate: DeliveryGate::Done,
                force: false,
                // Domain dispatch resolves this from [harness] smart_compact.
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

#[cfg(test)]
#[path = "run/tests.rs"]
mod tests;
