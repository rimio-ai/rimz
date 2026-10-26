//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::run_report::{
    RunSummary, write_check_trip_line, write_manual_verdict, write_run_summary,
};
use super::*;
use rimz::harness::launch::AgentShellPersistence;
use rimz::harness::schedule::takeover::{self, Takeover};
use std::ops::ControlFlow;

// ---- run --------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct FiredSignal {
    #[serde(flatten)]
    signal: schedule::signal::Signal,
    #[serde(default)]
    arrived_at: Option<Timestamp>,
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

fn write_condition_notice(
    out: &mut impl Write,
    timing: &schedule::TaskTiming,
) -> std::io::Result<()> {
    if !matches!(timing.state(), schedule::TaskTimingState::Waiting { .. }) {
        return Ok(());
    }
    let count = timing.condition_count().map_or_else(String::new, |count| {
        format!(" ({} of {})", count.met, count.total)
    });
    writeln!(
        out,
        "condition not met{count}; running anyway, task stays armed"
    )
}

pub(super) fn run_one(
    args: RunArgs,
    mode: LoopRunMode,
    keep: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let name = args.name.as_str();
    let signal = args
        .signal_json
        .as_deref()
        .map(serde_json::from_str::<FiredSignal>)
        .transpose()
        .context("decoding loop trigger signal")?;
    let arrived_at = signal.as_ref().and_then(|signal| signal.arrived_at);
    let signal = signal.map(|signal| signal.signal);
    let condition = args
        .condition_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("decoding loop condition evidence")?;
    let catalog = task_catalog(globals)?;
    let loaded = catalog.for_run(name).cloned();
    if args.drain
        && loaded.as_ref().is_none_or(|task| {
            task.action().is_err()
                || task.entry().each_worktree
                || matches!(
                    task.entry().overlap,
                    None | Some(rimz::config::OverlapPolicy::Skip)
                )
        })
    {
        if let Some(root) = project_root_for_globals(globals)
            .or_else(|| loaded.as_ref().map(|task| task.entry().resolved_root()))
        {
            rimz::harness::schedule::runner::discard_backlog(name, &root)?;
        }
        return Ok(());
    }
    let loaded = loaded
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    let entry = loaded.entry().clone();
    let checkout = resident_checkout(&entry, mode, args.cwd)?;
    let source = loaded.source();
    if !args.drain {
        gate_project_trust(name, &entry, source, mode)?;
    }
    let key = loaded.key(name);
    let arm_state = ArmState::resolve(arming::load().get(&key), source, Timestamp::now());
    if !args.drain && mode == LoopRunMode::Scheduled && arm_state != ArmState::Live {
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
            announcement: entry.guard().shell.map(|command| {
                let label = if matches!(action, TaskAction::CheckOnly) {
                    format!("  check: {command}")
                } else {
                    format!("  guard: {}", render::guard_value(&entry))
                };
                format!("{}\n", ui::paint(ui::palette::muted(), &label))
            }),
            prefix: ui::paint(ui::palette::faint(), "  │ "),
        },
    };
    if mode == LoopRunMode::Manual {
        let mut observed_entry = entry.clone();
        if let Some(checkout) = &checkout {
            observed_entry.dir = Some(checkout.clone());
        }
        let now = Timestamp::now();
        let timing = schedule::TaskTiming::evaluate(
            loaded.trigger(),
            TaskSource::Config,
            Some(now),
            None,
            &now.to_zoned(config.time_zone()),
        );
        let timing = condition::observe(name, &observed_entry, timing, now);
        write_condition_notice(&mut ui::out(), &timing)?;
    }
    let mut fire = rimz::harness::schedule::runner::TaskFire::new(
        name,
        loaded,
        &catalog,
        mode,
        keep,
        Timestamp::now(),
        config.clone(),
        signal,
        check_echo,
        started,
    )?
    .with_arrived_at(arrived_at)
    .with_condition(condition)
    .with_checkout(checkout)
    .with_hold_notice(move |reason| {
        if mode == LoopRunMode::Manual {
            let _ = writeln!(ui::out(), "held: {reason}");
        }
    });
    if args.drain {
        if !fire.take_backlog()? {
            return Ok(());
        }
        if ArmState::resolve(arming::load().get(&key), source, Timestamp::now()) != ArmState::Live {
            return Ok(());
        }
        if let Err(error) = gate_project_trust(name, &entry, source, mode) {
            return record_task_error(&mut fire, name, &entry, error).map(|_| ());
        }
    }
    let mut plan = fire.prepare(&mut |root| {
        if !entry.stay
            && (mode != LoopRunMode::Scheduled
                || !matches!(action, TaskAction::CheckOnly | TaskAction::Workflow(_)))
        {
            if !matches!(action, TaskAction::Spawn(_) | TaskAction::Workflow(_)) {
                return Ok(());
            }
            let workspace = WorkspaceResolver::resolve_participant(root, Some(root.to_path_buf()))?;
            if crate::cli::open_existing_store(&workspace)?.is_some() {
                return Ok(());
            }
        }
        let state = StatePaths::for_project_root(root)?;
        let runtime = RuntimePaths::for_state(&state)?;
        if fresh_sidebar_present(&runtime)
            && rimz::Store::open_existing(state, runtime.clone()).is_some()
        {
            return Ok(());
        }
        let mut room_globals = globals.clone();
        room_globals.root = Some(root.to_path_buf());
        crate::cli::room::ensure_workspace_room_detached(
            root,
            &room_globals,
            true,
            false,
            match mode {
                LoopRunMode::Scheduled => AgentShellPersistence::PerLaunch,
                LoopRunMode::Manual => AgentShellPersistence::Room,
            },
        )?;
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
        Ok(
            rimz::harness::schedule::runner::TaskFirePlan::AlreadyLaunched
            | rimz::harness::schedule::runner::TaskFirePlan::DeclineHeld,
        ) => return Ok(()),
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Resident {
            root,
            cwd,
            worktree,
            spec,
            prompt,
            loop_reminder,
        }) => {
            let effect = (|| {
                let mut launch_globals = globals.clone();
                launch_globals.root = Some(root.clone());
                let ctx = crate::cli::ctx::Ctx::open(&launch_globals)?;
                let request = rimz::launcher::LayoutRequest {
                    context: rimz::launcher::LaunchContext {
                        workspace: ctx.workspace.clone(),
                        store: ctx.store.clone(),
                        machine: config.clone(),
                        mux: launch_globals.mux,
                        root: launch_globals.root.clone(),
                        channel: ctx.channel().map(str::to_owned),
                    },
                    launch: rimz::launcher::LayoutLaunch {
                        spec: Some(spec),
                        prompt: Some(prompt),
                        cohort: rimz::launcher::CohortRequest {
                            new_tab: true,
                            bg: true,
                            worktree: worktree.clone(),
                            fresh: worktree.is_some() && entry.takeover,
                            ..Default::default()
                        },
                        overrides: rimz::launcher::LaunchOverrides {
                            mode: match entry.mode.as_deref() {
                                Some("ask") => Some(rimz::agents::PermissionMode::Ask),
                                Some("yolo") => Some(rimz::agents::PermissionMode::Yolo),
                                _ => None,
                            },
                            preset: rimz::agents::LaunchPreset {
                                effort: rimz::harness::plan::normalized_preset_value(
                                    entry.effort.as_deref(),
                                ),
                                fast: entry.fast,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    cwd: worktree.is_none().then(|| cwd.clone()),
                    allow_in_place: false,
                    entry: rimz::launcher::RoomEntry::Live,
                    starter: rimz::launcher::Starter::Loop,
                    resident_loop: Some(rimz::launcher::ResidentLoop {
                        task: name.to_owned(),
                        reminder: loop_reminder,
                    }),
                    workflow: None,
                    leader_run: None,
                };
                let mut interaction = crate::cli::launch_interaction::CliInteraction;
                let prepared = match rimz::launcher::prepare_layout(request, &mut interaction)? {
                    rimz::launcher::Preparation::Prepared(prepared) => prepared,
                    rimz::launcher::Preparation::Handled(_) => {
                        anyhow::bail!("resident launch aborted")
                    }
                };
                let stopped = if entry.takeover {
                    let destination = worktree
                        .as_deref()
                        .map(|name| {
                            let requested = rimz::worktree::parse_requested_name(name)?;
                            rimz::worktree::worktree_path(
                                ctx.workspace.launch_repo_root(),
                                &config.agents.worktree,
                                &requested.name,
                            )
                        })
                        .transpose()?;
                    match take_over_checkout(
                        &ctx,
                        &launch_globals,
                        &root,
                        destination.as_deref().unwrap_or(&cwd),
                    )? {
                        ControlFlow::Continue(stopped) => stopped,
                        ControlFlow::Break(blocked) => return Ok(blocked),
                    }
                } else {
                    Vec::new()
                };
                let rimz::launcher::LaunchOutcome::Opened(launched) =
                    rimz::launcher::execute(*prepared, &mut interaction)?
                else {
                    anyhow::bail!("resident launch aborted");
                };
                let leader = launched
                    .leader_index
                    .and_then(|index| launched.identities.get(index))
                    .context("resident layout has no prompt leader")?;
                fire.report_launch(&ctx.workspace.workspace_id, &leader.agent_id);
                Ok(rimz::harness::schedule::runner::TaskFireEffect::Resident {
                    leader: leader.name.clone(),
                    checkout: launched.cwd,
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
                fire.record_signal_guard(),
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
    if let Some(holder) = finished.replace_holder.as_deref() {
        match stop_active_run(
            name,
            &entry.resolved_root(),
            schedule::backlog::StopCause::Replace,
            Some(*holder),
            globals,
        ) {
            Ok((_, None)) => {}
            Ok((_, Some(open))) => tracing::warn!(task = name, "{open}"),
            Err(error) => {
                tracing::warn!(task = name, %error, "replacement could not stop the active run; trigger remains waiting")
            }
        }
    }
    fire.release_run_lock()?;
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
    let takeover = takeover::read(&snapshot.agents, cwd, root)?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let handle =
        |agent: &rimz::agents::AgentState| rimz::address::agent_handle(agent, &peers, true);
    let occupants = match takeover {
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
    let stops = crate::cli::agents_cmd::stop_resolved(ctx, globals, &snapshot, &occupants)?;
    for (occupant, stop) in occupants.iter().zip(stops) {
        stop.into_result(&handle(occupant))?;
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
    if !entry.stay && entry.workflow.is_none() {
        bail!("--each-worktree requires --stay or workflow");
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
        handle_run_transition(name, &entry.resolved_root(), transition);
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

pub(super) fn handle_run_transition(name: &str, root: &Path, transition: RunTransition) {
    if let RunTransition::AutoDisabled { strikes } = transition {
        let _ = writeln!(
            ui::out(),
            "loop `{name}`: disabled after {strikes} consecutive failed fires; enable with `rimz loop enable {name}`"
        );
        notify_loop_disabled(name, root, strikes);
    }
}

fn notify_loop_disabled(name: &str, root: &Path, count: u32) {
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

    let runtime = match RuntimePaths::for_project_root(root) {
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

    handle_run_transition(name, &entry.resolved_root(), finished.transition);
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
    let store = crate::cli::require_existing_store(&workspace)?;
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
    for message_id in &prepared.supersedes {
        store.cancel_message(
            message_id,
            &workspace.session_name,
            "superseded by the wait's final outcome",
        )?;
    }
    let dispatched = rimz::message::dispatch::dispatch(
        &workspace,
        &store,
        rimz::message::dispatch::DispatchRequest {
            resuming_recipient: None,
            target: format!("@{}", prepared.target.session),
            text: prepared.prompt,
            target_scope: None,
            current_channel: channel.address_context(),
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
