//! Execute loop tasks and record foreground or scheduled run outcomes.

use super::run_report::{RunSummary, keep_hint, write_manual_verdict, write_run_summary};
use super::*;
use rimz::harness::launch::AgentShellPersistence;

// ---- run --------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct FiredSignal {
    #[serde(flatten)]
    signal: rimz::harness::signal::Signal,
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
    let state = match timing.state() {
        schedule::TaskTimingState::Waiting { .. } => "not met",
        schedule::TaskTimingState::Unknown { .. } => "unknown",
        _ => return Ok(()),
    };
    let count = timing.condition_count().map_or_else(String::new, |count| {
        format!(" ({} of {})", count.met, count.total)
    });
    writeln!(
        out,
        "condition {state}{count}; running anyway, task stays armed"
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
    let mut catalog = task_catalog(globals)?;
    // The task's runs that ended uncollected are collected before the fire decides, so it reads
    // the strikes, the consumed `--until` row and the launch memory they leave.
    if let Some(task) = catalog.for_run(name)
        && rimz::harness::schedule::run_log::collect_task_runs(&task.entry().resolved_root(), name)
    {
        catalog = task_catalog(globals)?;
    }
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
    if keep && entry.workflow.is_some() {
        anyhow::bail!(
            "workflow task `{name}` cannot use --keep; set `keep = true` on the workflow's \
             `[agent.<name>]`"
        );
    }
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
        started,
    )?
    .with_arrived_at(arrived_at)
    .with_condition(condition)
    .with_checkout(checkout);
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
    let plan = fire.prepare(&mut |root| {
        // An agent task's room is born here, so its birth keeps the fire's shell persistence;
        // the run's step launch then finds it open.
        if !entry.stay
            && !matches!(action, TaskAction::Spawn(_))
            && (mode != LoopRunMode::Scheduled
                || !matches!(action, TaskAction::CheckOnly | TaskAction::Workflow(_)))
        {
            if !matches!(action, TaskAction::Workflow(_)) {
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
    let finished = match plan {
        Err(err) => record_task_error(&mut fire, name, &entry, err)?,
        Ok(
            rimz::harness::schedule::runner::TaskFirePlan::AlreadyLaunched
            | rimz::harness::schedule::runner::TaskFirePlan::Waiting,
        ) => return Ok(()),
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Done(finished)) => finished,
        Ok(rimz::harness::schedule::runner::TaskFirePlan::Deliver(prepared)) => {
            let effect = execute_prepared_delivery(prepared, globals);
            finish_task_effect(&mut fire, effect, name, &entry)?
        }
    };
    fire.release_run_lock()?;
    present_finished(name, &entry, &action, mode, &finished)?;
    if mode == LoopRunMode::Manual {
        write_launched_run_command(
            &entry,
            &action,
            keep,
            &finished.record,
            fire.advance(),
            globals,
        )?;
    }
    if let Some(code) = finished.presentation.exit_code {
        std::process::exit(code);
    }
    Ok(())
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

/// The strike-out notice: a desktop notification and a sidebar toast.
pub(super) fn notify_loop_disabled(name: &str, root: &Path, count: u32) {
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
    print_run_summary(name, entry, action, mode, &summary)
}

/// `→ <workflow show command>` under a hand fire's `launched <id>` receipt, then the hold in
/// front of the run's node once its advance has had its first pass, as `workflow run` prints
/// both.
fn write_launched_run_command(
    entry: &TaskEntry,
    action: &TaskAction,
    keep: bool,
    record: &LoopRunRecord,
    advance: Option<&rimz::harness::workflow::Advance>,
    globals: &GlobalFlags,
) -> Result<()> {
    if record.result != LoopRunResult::Launched {
        return Ok(());
    }
    let Some(id) = record
        .run_id
        .as_deref()
        .and_then(|id| id.parse::<rimz::ids::WorkflowRunId>().ok())
    else {
        return Ok(());
    };
    let paths = StatePaths::for_project_root(&entry.resolved_root())?;
    // A finished run keeps its `workflow show` hint; a run with no home left points at the task.
    let run = match rimz::store::workflow::find(&paths, &id)? {
        Some(rimz::store::workflow::FoundRun {
            class: rimz::store::workflow::RunClass::Current(run),
            ..
        }) => *run,
        Some(_) => return Ok(()),
        None => {
            writeln!(ui::out(), "  → rimz loop show {}", record.task)?;
            return Ok(());
        }
    };
    writeln!(
        ui::out(),
        "  → {}",
        crate::cli::workflow_view::resolve::run_command(&run, "show", globals, None, &[], &[])
    )?;
    if let Some(hint) = keep_hint(entry, action, keep, run.state.is_terminal()) {
        writeln!(ui::out(), "{}", ui::paint(ui::palette::muted(), hint))?;
    }
    let paths = paths.workflow(&id);
    match crate::cli::workflow::first_pass(&paths, advance)? {
        Some(run) => crate::cli::workflow::write_held(&paths, &run, "  "),
        None => Ok(()),
    }
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
            about_run: None,
            selected: Vec::new(),
            reply_wanted: false,
            premises: rimz::store::message::Premise::NONE,
            resuming_recipient: None,
            target: format!("@{}", prepared.target.session),
            text: prepared.prompt,
            target_scope: None,
            current_channel: channel.address_context(),
            caller: None,
            sender,
            automated: true,
            allow_fanout: false,
            mux: globals.mux,
            enter: true,
            force: false,
            // Domain dispatch resolves the recipient's seat line, then the harness default.
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
    summary: &RunSummary<'_>,
) -> Result<()> {
    let mut out = ui::out();
    write_run_summary(&mut out, name, entry, action, mode, summary)?;
    // A launch logs no row, so a launch-memory write that failed is said here or nowhere.
    if let (LoopRunResult::Launched, Some(error)) = (summary.record.result, &summary.record.error) {
        writeln!(
            ui::err(),
            "{} {error}",
            ui::paint(ui::palette::warn().bold(), "warning:")
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "run/tests.rs"]
mod tests;
