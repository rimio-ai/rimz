//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::run_report::{
    RunSummary, write_check_trip_line, write_manual_verdict, write_run_summary,
};
use super::*;
use rimz::harness::schedule::takeover::{self, Takeover};
use std::ops::ControlFlow;

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
    signal: Option<rimz::harness::schedule::signal::Signal>,
    condition: Option<rimz::harness::schedule::when::ConditionEvidence>,
    checkout: Option<PathBuf>,
    globals: &GlobalFlags,
) -> Result<()> {
    let catalog = task_catalog(globals)?;
    let loaded = catalog
        .for_run(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    let entry = loaded.entry().clone();
    let checkout = resident_checkout(&entry, mode, checkout)?;
    let source = loaded.source();
    gate_project_trust(name, &entry, source, mode)?;
    let key = loaded.key(name);
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
        signal,
        check_echo,
        started,
    )?
    .with_condition(condition)
    .with_checkout(checkout)
    .with_hold_notice(move |reason| {
        if mode == LoopRunMode::Manual {
            let _ = writeln!(ui::out(), "held: {reason}");
        }
    });
    let mut plan = fire.prepare(&mut |root| {
        if !entry.stay
            && (mode != LoopRunMode::Scheduled || !matches!(action, TaskAction::CheckOnly))
        {
            return Ok(());
        }
        let state = StatePaths::for_project_root(root)?;
        let runtime = RuntimePaths::for_state(&state)?;
        if fresh_sidebar_present(&runtime) {
            return Ok(());
        }
        let mut room_globals = globals.clone();
        room_globals.root = Some(root.to_path_buf());
        crate::cli::room::ensure_workspace_room_detached(root, &room_globals, true, false)?;
        Ok(())
    });
    if mode == LoopRunMode::Manual
        && let Some(trip) = fire.take_check_trip()
        && let Err(source) = write_check_trip_line(
            &mut ui::out(),
            &action,
            &trip.record,
            trip.watch.as_ref(),
            trip.duration_ms,
        )
    {
        let err = source.into();
        if matches!(
            &plan,
            Ok(rimz::harness::schedule::runner::TaskFirePlan::Done(_))
        ) {
            return Err(err);
        }
        plan = Err(err);
    }
    let finished = match plan {
        Err(err) => record_task_error(&mut fire, name, &entry, err)?,
        Ok(rimz::harness::schedule::runner::TaskFirePlan::AlreadyLaunched) => return Ok(()),
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Resident {
            root,
            cwd,
            spec,
            prompt,
            loop_reminder,
        }) => {
            let effect = (|| {
                let mut launch_globals = globals.clone();
                launch_globals.root = Some(root.clone());
                let ctx = crate::cli::ctx::Ctx::open(&launch_globals)?;
                let stopped = if entry.takeover {
                    match take_over_checkout(&ctx, &launch_globals, &root, &cwd)? {
                        ControlFlow::Continue(stopped) => stopped,
                        ControlFlow::Break(blocked) => return Ok(blocked),
                    }
                } else {
                    Vec::new()
                };
                let mut launch = crate::cli::agents_cmd::AgentLaunchArgs {
                    spec: Some(spec),
                    prompt: Some(prompt),
                    ..Default::default()
                };
                launch.cohort.new_tab = true;
                launch.cohort.bg = true;
                launch.overrides.ask = entry.mode.as_deref() == Some("ask");
                launch.overrides.yolo = entry.mode.as_deref() == Some("yolo");
                launch.overrides.effort = entry.effort.clone();
                let launched = crate::cli::agents_cmd::launch::launch_resolved(
                    launch,
                    &launch_globals,
                    false,
                    &ctx,
                    MachineConfig::load_lenient(),
                    Some(cwd),
                    Some((name, &loop_reminder)),
                )?
                .context("resident launch aborted")?;
                let leader = launched
                    .leader_index
                    .and_then(|index| launched.identities.get(index))
                    .context("resident layout has no prompt leader")?;
                fire.report_launch(&ctx.workspace.workspace_id, &leader.agent_id);
                Ok(rimz::harness::schedule::runner::TaskFireEffect::Resident {
                    leader: leader.name.clone(),
                    handles: launched
                        .identities
                        .iter()
                        .map(|identity| format!("@{}", identity.name))
                        .collect(),
                    stopped,
                })
            })();
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Done(finished)) => finished,
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Spawn(prepared)) => {
            let mut run_globals = globals.clone();
            run_globals.root = Some(prepared.root.clone());
            let effect = crate::cli::supervised::run::run_supervised(
                prepared.request,
                crate::cli::supervised::SupervisedPresentation::text(prepared.stream),
                &run_globals,
            )
            .and_then(|outcome| {
                outcome
                    .map(rimz::harness::schedule::runner::TaskFireEffect::Spawn)
                    .context("scheduled launch aborted despite its explicit root")
            });
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Deliver(prepared)) => {
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

/// Stop every agent occupying the launch checkout and return their handles for
/// the launch to record, or stop none and break with the blocked effect.
fn take_over_checkout(
    ctx: &crate::cli::ctx::Ctx,
    globals: &GlobalFlags,
    root: &Path,
    cwd: &Path,
) -> Result<ControlFlow<rimz::harness::schedule::runner::TaskFireEffect, Vec<String>>> {
    let snapshot = ctx.alive_snapshot()?;
    // A provider hook records the physical cwd, the launch the path as given.
    let path_forms = |path: PathBuf| {
        let physical = std::fs::canonicalize(&path).ok();
        std::iter::once(path).chain(physical)
    };
    let owned: Vec<PathBuf> = match rimz::worktree::discover_owned(root) {
        Ok(owned) => owned
            .into_iter()
            .flat_map(|worktree| path_forms(worktree.marker.worktree_path))
            .collect(),
        Err(rimz::worktree::WorktreeErr::NotRepo) => Vec::new(),
        Err(err) => return Err(err.into()),
    };
    let checkouts: Vec<PathBuf> = path_forms(cwd.to_path_buf()).collect();
    let peers = rimz::address::addressable_agents(&snapshot);
    let handle =
        |agent: &rimz::agents::AgentState| rimz::address::agent_handle(agent, &peers, true);
    let occupants = match takeover::plan(&snapshot.agents, &checkouts, &owned, Timestamp::now()) {
        Takeover::Stop(occupants) => occupants,
        Takeover::Blocked(blockers) => {
            return Ok(ControlFlow::Break(
                rimz::harness::schedule::runner::TaskFireEffect::TakeoverBlocked {
                    blockers: blockers
                        .into_iter()
                        .map(|(agent, turn)| (handle(agent), turn))
                        .collect(),
                },
            ));
        }
    };
    let mut tracker = crate::cli::agents_cmd::StopTracker::default();
    for occupant in &occupants {
        // An occupant its parent's stop already took is skipped; any failure,
        // a pane left open included, fails the fire, so every occupant's pane
        // is closed once the loop ends.
        crate::cli::agents_cmd::stop_resolved(ctx, globals, &snapshot, occupant, &mut tracker)?
            .into_result(&handle(occupant))?;
    }
    Ok(ControlFlow::Continue(
        occupants.into_iter().map(handle).collect(),
    ))
}

fn resident_checkout(
    entry: &TaskEntry,
    mode: LoopRunMode,
    requested: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    if !entry.each_worktree {
        if requested.is_some() {
            bail!("--cwd on loop run requires an --each-worktree task");
        }
        return Ok(None);
    }
    if !entry.stay {
        bail!("--each-worktree requires --stay");
    }
    let owned: Vec<_> = rimz::worktree::discover_owned(&entry.resolved_root())?
        .into_iter()
        .map(|worktree| rimz::utils::path::normalize_path_lexical(&worktree.marker.worktree_path))
        .collect();
    if mode == LoopRunMode::Scheduled {
        let requested = requested.context("--each-worktree needs a checkout from the scheduler; use loop tick or loop fire from an owned worktree")?;
        let requested = rimz::utils::path::normalize_path_lexical(&requested);
        if let Some(checkout) = owned.into_iter().find(|checkout| *checkout == requested) {
            return Ok(Some(checkout));
        }
        bail!(
            "{} is not a RimZ-owned worktree of this task's project",
            requested.display()
        );
    }
    let cwd = std::env::current_dir()?.canonicalize()?;
    let checkout = owned.into_iter().filter_map(|checkout| {
        let canonical = checkout.canonicalize().ok()?;
        cwd.starts_with(&canonical).then_some((canonical.components().count(), checkout))
    }).max_by_key(|(depth, _)| *depth).map(|(_, path)| path)
        .context("fire this --each-worktree task from inside a RimZ-owned worktree, not the project root")?;
    Ok(Some(checkout))
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
            Err(err) => record_task_error(fire, name, entry, err),
        },
        Err(err) => record_task_error(fire, name, entry, err),
    }
}

fn record_task_error(
    fire: &mut rimz::harness::schedule::runner::TaskFire<'_>,
    name: &str,
    entry: &TaskEntry,
    err: anyhow::Error,
) -> Result<rimz::harness::schedule::runner::TaskFireFinished> {
    let finished = fire.finish_error(&err);
    let transition = finished.transition;
    skip_or_error(finished, err).inspect_err(|err| {
        handle_run_transition(name, entry, transition);
        tracing::warn!(task = name, error = %err, "loop task run failed");
    })
}

/// An error the runner recorded as a gate skip ends the fire like any other
/// skip: presented as one, with a zero exit. Every other error propagates.
fn skip_or_error(
    finished: rimz::harness::schedule::runner::TaskFireFinished,
    err: anyhow::Error,
) -> Result<rimz::harness::schedule::runner::TaskFireFinished> {
    match finished.notice {
        rimz::harness::schedule::runner::TaskFireNotice::Gate { .. } => Ok(finished),
        _ => Err(err),
    }
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

    let runtime = match RuntimePaths::for_project_root(&entry.resolved_root()) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::debug!(task = name, error = %err, "loop auto-disable runtime unavailable");
            return;
        }
    };
    let notification_kind = notification.kind_env().to_owned();
    if let Err(err) = rimz::wakeup::broadcast(
        &runtime,
        None,
        rimz::wakeup::events::SidebarEvent::Notify {
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
        prose: ui::prose::Prose::for_stdout(),
    };
    print_run_summary(name, entry, action, mode, keep, &summary)
}

fn execute_prepared_delivery(
    prepared: rimz::harness::schedule::runner::PreparedDelivery,
    globals: &GlobalFlags,
) -> Result<rimz::harness::schedule::runner::TaskFireEffect> {
    let workspace = WorkspaceResolver::resolve_participant(".", Some(prepared.root))?;
    let store = crate::cli::open_store(&workspace)?;
    let channel = crate::cli::current_channel(&workspace, Some(&store));
    let sender = rimz::store::message::MessageSender::Harness {
        notice: match prepared.intent {
            rimz::harness::schedule::runner::DeliveryIntent::Signal => {
                rimz::store::message::HarnessNotice::Signal
            }
            _ => rimz::store::message::HarnessNotice::Wait,
        },
    };
    tracing::debug!(
        kind = %prepared.target.kind,
        session = %prepared.target.session,
        "queueing loop wake-up"
    );
    let mode = if prepared.intent == rimz::harness::schedule::runner::DeliveryIntent::SelfWait {
        rimz::message::dispatch::DispatchMode::Steer
    } else {
        rimz::message::dispatch::DispatchMode::Boundary {
            gate: DeliveryGate::Done,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
        }
    };
    let kind = mode.kind();
    let dispatched = rimz::message::dispatch::dispatch(
        &workspace,
        &store,
        rimz::message::dispatch::DispatchRequest {
            target: format!("@{}", prepared.target.session),
            text: prepared.prompt,
            target_scope: None,
            current_channel: channel.address_context(&workspace),
            caller: None,
            sender,
            automated: true,
            allow_fanout: false,
            reply: None,
            mux: globals.mux,
            enter: true,
            force: false,
            // Domain dispatch resolves this from [harness] smart_compact.
            auto_compact: None,
            mode,
        },
    );
    match dispatched {
        Ok(result) => {
            let message_id = result
                .outcomes
                .first()
                .map(|outcome| match outcome {
                    rimz::message::dispatch::DispatchOutcome::Sent { message_id, .. }
                    | rimz::message::dispatch::DispatchOutcome::Queued { message_id, .. }
                    | rimz::message::dispatch::DispatchOutcome::CompactionPending {
                        message_id,
                        ..
                    }
                    | rimz::message::dispatch::DispatchOutcome::SkippedWaiting {
                        message_id, ..
                    } => message_id.clone(),
                })
                .context("loop wait dispatch returned no outcome")?;
            crate::cli::send::report_dispatch(
                kind,
                &prepared.target.handle,
                &result.outcomes,
                &result.compacted,
            )?;
            Ok(rimz::harness::schedule::runner::TaskFireEffect::Delivered(
                message_id,
            ))
        }
        Err(rimz::message::dispatch::DispatchErr::Recipient(
            rimz::address::TargetErr::NoMatch { .. }
            | rimz::address::TargetErr::NoMatchInChannel { .. },
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
