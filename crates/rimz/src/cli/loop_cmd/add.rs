//! Add, remove, and rename configured loop tasks.

use super::*;
use rimz::harness::schedule::arm::{
    self, ArmOutcome, DeliveryCheck, DeliveryName, DeliveryPrompt, DeliveryProvenance,
    DeliverySpec, DeliverySurplus, DeliveryTrigger, SubscriptionLifetime,
};

struct AddTiming {
    at: Option<String>,
    deadline: Option<Timestamp>,
}

enum AddTaskAction {
    Spawn {
        resolved: ResolvedSingleAgentLaunch,
        mode: Option<String>,
    },
    Stay {
        kind: String,
    },
    Deliver {
        target: TaskTarget,
        selector: Option<schedule::signal::SignalSelector>,
        matches: BTreeMap<String, String>,
    },
    CheckOnly,
}

impl AddTaskAction {
    fn provider_kind(&self) -> Option<&str> {
        match self {
            Self::Spawn { resolved, .. } => Some(resolved.kind()),
            Self::Stay { kind } => Some(kind),
            Self::Deliver { target, .. } => Some(target.kind.as_str()),
            Self::CheckOnly => None,
        }
    }
}

// ---- add / remove -----------------------------------------------------------

pub(super) fn add(args: AddArgs, _globals: &GlobalFlags) -> Result<()> {
    let action_kind = validate_add_args(&args)?;
    let workspace = resolve_add_workspace(&args)?;
    let project_root = workspace.project_root.clone();
    condition::validate(&args.when, &project_root)?;
    let check = resolve_add_check(&args, &workspace)?;
    let action = resolve_add_action(&args, &workspace, action_kind)?;
    let action = match action {
        AddTaskAction::Deliver {
            target,
            selector,
            matches,
        } => {
            return add_delivery(&args, &workspace, target, selector, matches, check);
        }
        action => action,
    };
    let provider_kind = action.provider_kind().map(ToOwned::to_owned);
    let (mut entry, resolved_for_preflight) = build_task_entry(&args, action, &workspace, check)?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    let login = provider_kind
        .as_deref()
        .map(|kind| task_login(&args.account.clone().into(), kind, &runtime))
        .transpose()?
        .flatten();
    if let Some(login) = login.as_ref().filter(|_| args.account.is_some()) {
        login.preflight(&rimz::agents::ambient_env())?;
    }
    let login_key = login.as_ref().map(rimz::agents::ProviderLogin::key);
    entry.provider = window_condition_provider(
        &args.when,
        provider_kind.as_deref(),
        login_key.as_ref(),
        &entry.resolved_root(),
        &runtime,
        Timestamp::now(),
    )?;
    let after_reset = args
        .after_reset
        .map(|span| {
            resolve_after_reset(
                span,
                provider_kind.as_deref(),
                login_key.as_ref(),
                args.surplus.is_some() || args.surplus_after.is_some(),
                &entry.resolved_root(),
                &runtime,
                Timestamp::now(),
            )
        })
        .transpose()?;
    entry.fire_at = after_reset.as_ref().map(|reset| reset.fire_at);
    // Compile once before writing, so validation and feedback share one shape.
    let shape = schedule::TaskShape::compile(&args.name, &entry);
    let parsed = shape.trigger().as_ref().map_err(Clone::clone)?;
    let task_action = shape.action().map_err(Clone::clone)?;
    if !entry.stay {
        preflight_entry(task_action, resolved_for_preflight.as_ref(), login.as_ref())?;
    }
    let catalog = TaskCatalog::load(Some(&project_root))?;
    let project_pre_state = args
        .project
        .then(|| trust::status(&project_root))
        .transpose()?
        .map(|report| report.state);
    let mutation = if args.project {
        catalog.replace_project(&args.name, &project_root, &entry)?
    } else {
        catalog.replace_machine(&args.name, &entry)?
    };

    let mut out = ui::out();
    writeln!(out, "added loop task `{}`", args.name)?;
    if mutation.cleared_overlays() {
        writeln!(out, "arming: reset")?;
    }
    if let Some(pre_state) = project_pre_state {
        finish_project_mutation(&mut out, &project_root, true, pre_state)?;
    }
    write_add_feedback(
        &mut out,
        &entry,
        parsed,
        task_action,
        provider_kind.as_deref(),
        after_reset.as_ref(),
    )?;
    writeln!(
        out,
        "live while a room for {} is open",
        entry.root.display()
    )?;
    if !render::room_open(&entry.root) {
        if timer::active() {
            writeln!(out, "no room is open there; the loop timer will keep time")?;
        } else {
            writeln!(
                out,
                "no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one"
            )?;
        }
        condition::write_no_room_hint(&mut out, parsed)?;
    }
    Ok(())
}

fn add_delivery(
    args: &AddArgs,
    workspace: &rimz::ResolvedWorkspace,
    target: TaskTarget,
    selector: Option<schedule::signal::SignalSelector>,
    matches: BTreeMap<String, String>,
    check: Option<DeliveryCheck>,
) -> Result<()> {
    let timing = resolve_add_timing(args)?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    let login_key = task_login(
        &rimz::store::writer::LaunchLogin::RoomDefault,
        target.kind.as_str(),
        &runtime,
    )?
    .map(|login| login.key());
    let after_reset = args
        .after_reset
        .map(|span| {
            resolve_after_reset(
                span,
                Some(target.kind.as_str()),
                login_key.as_ref(),
                args.surplus.is_some() || args.surplus_after.is_some(),
                &workspace.worktree_root,
                &runtime,
                Timestamp::now(),
            )
        })
        .transpose()?;
    let trigger = if !args.when.is_empty() {
        let parsed = schedule::parse_trigger(
            &args.name,
            &TaskEntry {
                when: Some(args.when.clone()),
                hold: args.hold.clone(),
                once: args.once.then_some(true),
                provider: window_condition_provider(
                    &args.when,
                    Some(target.kind.as_str()),
                    login_key.as_ref(),
                    &workspace.worktree_root,
                    &runtime,
                    Timestamp::now(),
                )?,
                ..TaskEntry::default()
            },
        )?;
        let schedule::Trigger::Condition { expr, hold } = parsed.trigger else {
            unreachable!("the entry contains only condition fields")
        };
        DeliveryTrigger::Condition {
            expr,
            hold,
            lifetime: if args.once {
                SubscriptionLifetime::Once
            } else {
                SubscriptionLifetime::Standing
            },
        }
    } else if let Some(selector) = selector {
        DeliveryTrigger::Signal {
            selector,
            matches,
            lifetime: if args.once {
                SubscriptionLifetime::Once
            } else {
                SubscriptionLifetime::Standing
            },
        }
    } else {
        let parsed = schedule::parse_trigger(
            &args.name,
            &TaskEntry {
                at: timing.at,
                every: args.every.clone(),
                cron: args.cron.clone(),
                fire_at: after_reset.as_ref().map(|reset| reset.fire_at),
                ..TaskEntry::default()
            },
        )?;
        let clock = match parsed.trigger {
            schedule::Trigger::Schedule(clock) => clock,
            // This branch constructs an entry containing only clock fields.
            schedule::Trigger::Condition { .. }
            | schedule::Trigger::Signal { .. }
            | schedule::Trigger::Watch(_) => unreachable!("the entry contains only clock fields"),
        };
        DeliveryTrigger::Clock(clock)
    };
    validate_task_timeout(args.timeout.as_deref())?;
    let prompt = match (&args.prompt, &args.prompt_file) {
        (Some(prompt), _) => DeliveryPrompt::Inline(prompt.clone()),
        (_, Some(path)) => DeliveryPrompt::File(path.clone()),
        _ => DeliveryPrompt::None,
    };
    let surplus = args
        .surplus
        .as_deref()
        .map(schedule::parse_surplus)
        .transpose()
        .map_err(anyhow::Error::msg)?
        .map(|ratio| format!("{ratio}x"));
    if let Some(after) = args.surplus_after.as_deref() {
        schedule::parse_surplus_after(after).map_err(anyhow::Error::msg)?;
    }
    let outcome = arm::arm_delivery(
        workspace,
        DeliverySpec {
            name: DeliveryName::Named(args.name.parse()?),
            label: args.label.clone(),
            target,
            trigger,
            prompt,
            provenance: DeliveryProvenance::Loop,
            check,
            deadline: timing.deadline,
            max_strikes: args.max_strikes,
            surplus: (surplus.is_some() || args.surplus_after.is_some()).then(|| DeliverySurplus {
                ratio: surplus,
                after: args.surplus_after.clone(),
            }),
        },
    )?;
    let mut out = ui::out();
    let (name, task) = match outcome {
        ArmOutcome::Armed { name, task } => (name, task),
        ArmOutcome::AlreadySubscribed { name } => {
            writeln!(out, "already subscribed as {name}")?;
            return Ok(());
        }
    };
    writeln!(out, "added loop task `{name}`")?;
    let entry = task.entry();
    write_add_feedback(
        &mut out,
        entry,
        task.trigger().as_ref().map_err(Clone::clone)?,
        task.action().map_err(Clone::clone)?,
        entry.wait.as_ref().map(|target| target.kind.as_str()),
        after_reset.as_ref(),
    )?;
    writeln!(
        out,
        "live while a room for {} is open",
        entry.root.display()
    )?;
    if entry.when.is_some() && !render::room_open(&entry.root) {
        writeln!(out, "no room is open there; start one with `rimz start`")?;
        condition::write_no_room_hint(&mut out, task.trigger().as_ref().map_err(Clone::clone)?)?;
    }
    Ok(())
}

fn validate_add_args(args: &AddArgs) -> Result<TaskActionKind> {
    schedule::validate_name(&args.name)?;
    if args.check_timeout.is_some() && args.check.is_none() {
        bail!("--check-timeout requires --check");
    }
    for (used, flag) in [
        (args.judge_prompt.is_some(), "--judge-prompt"),
        (args.judge_prompt_file.is_some(), "--judge-prompt-file"),
        (args.judge_recheck.is_some(), "--judge-recheck"),
        (args.judge_timeout.is_some(), "--judge-timeout"),
    ] {
        if used && args.judge.is_none() {
            let hint = if flag == "--judge-timeout" {
                "; for the shell check use --check-timeout"
            } else {
                ""
            };
            bail!("{flag} requires --judge{hint}");
        }
    }
    if args.judge.is_some() {
        if args.judge_prompt.is_some() == args.judge_prompt_file.is_some() {
            bail!("--judge requires exactly one of --judge-prompt and --judge-prompt-file");
        }
        if args.agent.is_none() && args.wait.is_none() {
            bail!("--judge requires --agent or --wait");
        }
        if args.on.is_none() {
            bail!(
                "--judge requires --on: fail acts on a negative verdict, success acts on a positive verdict, any acts whatever the verdict"
            );
        }
    }
    if args.judge_recheck.is_some() && !args.stay {
        bail!("--judge-recheck requires --stay");
    }
    for (raw, flag) in [
        (args.judge_recheck.as_deref(), "--judge-recheck"),
        (args.check_timeout.as_deref(), "--check-timeout"),
        (args.judge_timeout.as_deref(), "--judge-timeout"),
    ] {
        if let Some(raw) = raw {
            if flag == "--judge-recheck" && raw.trim() == "0" {
                continue;
            }
            let duration = parse_task_timeout(raw)
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("invalid {flag}"))?;
            if flag != "--judge-recheck" && duration.is_zero() {
                bail!("{flag} must be greater than zero");
            }
        }
    }
    if args.stay && args.wait.is_some() {
        bail!("--stay cannot use --wait; it launches a new agent layout");
    }
    if args.stay && args.agent.is_none() {
        bail!("--stay requires --agent with a layout to launch");
    }
    for (used, flag) in [
        (args.each_worktree, "--each-worktree"),
        (args.takeover, "--takeover"),
        (!args.subscribe.is_empty(), "--subscribe"),
    ] {
        if used && !args.stay {
            bail!("{flag} requires --stay; it applies to resident layouts");
        }
    }
    if args.each_worktree && args.when.is_empty() {
        bail!("--each-worktree requires --when; fan-out supports conditions only");
    }
    if args.stay {
        for (used, flag, reason) in [
            (
                args.account.is_some(),
                "--account",
                "resident layouts use the room's accounts; omit --account or --stay",
            ),
            (
                args.max_attempts.is_some(),
                "--max-attempts",
                "resident layouts are not supervised retries",
            ),
            (
                args.budget_per_day.is_some(),
                "--budget-per-day",
                "resident layouts have no per-fire spend accounting",
            ),
            (
                args.verify.is_some(),
                "--verify",
                "resident layouts are not supervised turns",
            ),
            (
                args.budget.is_some(),
                "--budget",
                "resident layouts have no supervised run budget",
            ),
            (
                args.surplus.is_some(),
                "--surplus",
                "resident layouts have no bounded run spend",
            ),
            (
                args.surplus_after.is_some(),
                "--surplus-after",
                "resident layouts have no bounded run spend",
            ),
            (
                args.timeout.is_some(),
                "--timeout",
                "resident agents remain open",
            ),
            (
                args.until.is_some(),
                "--until",
                "resident launches are not poll-until actions",
            ),
            (
                args.system_prompt_file.is_some(),
                "--system-prompt-file",
                "resident layouts use their profiles' system prompts",
            ),
            (
                args.worktree.is_some(),
                "--worktree",
                "resident layouts launch in the condition's checkout",
            ),
            (
                args.once,
                "--once",
                "resident launches are deduplicated per checkout, not per task",
            ),
            (
                !args.subscribe.is_empty()
                    && (args.in_after.is_some()
                        || (args.at.is_some() && args.every.is_none())
                        || args.after_reset.is_some()),
                "--subscribe",
                "one-shot triggers remove the task before registration; use --every, --cron, or --when",
            ),
            (
                args.project,
                "--project",
                "resident launch state is machine-local",
            ),
        ] {
            if used {
                bail!("--stay cannot use {flag}; {reason}");
            }
        }
    }
    for signal in &args.subscribe {
        schedule::parse_signal_selector(&args.name, signal, None)?;
        if signal == "agent" || signal.starts_with("agent.") {
            bail!(
                "--subscribe cannot use agent signals without an explicit other-agent match; use a scoped `rimz loop add --wait --signal` instead"
            );
        }
    }
    let project_error = args.project.then(|| {
        [
            (
                args.after_reset.is_some(),
                "--project tasks cannot use --after-reset; its reset instant is machine state",
            ),
            (!args.when.is_empty(), "--project tasks cannot use --when yet; add it without --project"),
            (
                args.wait.is_some(),
                "--project tasks cannot use --wait; project config cannot pin a machine-local session",
            ),
            (
                args.until.is_some(),
                "--project tasks cannot use --until; poll-until deadlines are machine state",
            ),
            (
                args.every.is_none() && args.cron.is_none() && args.signal.is_none(),
                "--project tasks need a trigger; set --every, --cron, or --signal",
            ),
            (
                args.once,
                "--project tasks cannot use --once; one-shot subscriptions are machine state",
            ),
        ]
        .into_iter()
        .find_map(|(invalid, message)| invalid.then_some(message))
    });
    if let Some(message) = project_error.flatten() {
        bail!(message);
    }
    let action_kind = args
        .agent
        .as_ref()
        .map(|_| TaskActionKind::Spawn)
        .or(args.wait.as_ref().map(|_| TaskActionKind::Deliver))
        .or(args.check.as_ref().map(|_| TaskActionKind::CheckOnly))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loop task `{}` needs --agent, --wait, or --check",
                args.name
            )
        })?;
    if args.on.is_some() && args.check.is_none() && args.judge.is_none() {
        bail!("--on requires --check or --judge");
    }
    let matches = parse_matches(&args.matches)?;
    if !args.matches.is_empty() && args.signal.is_none() {
        bail!("--match requires --signal");
    }
    if args.once && args.signal.is_none() && args.when.is_empty() {
        bail!("--once requires --signal or --when");
    }
    if args.wait.is_some()
        && args
            .signal
            .as_deref()
            .is_some_and(|name| name.starts_with("agent."))
        && !matches.contains_key("handle")
        && !matches.contains_key("session")
    {
        bail!(self_wait_guard_message());
    }
    if !action_kind.has_effect() && args.after_reset.is_some() {
        bail!("--after-reset requires --agent or --wait");
    }
    if !action_kind.has_effect() && (args.surplus.is_some() || args.surplus_after.is_some()) {
        bail!("--surplus and --surplus-after require --agent or --wait");
    }
    if args.max_attempts == Some(0) {
        bail!("--max-attempts must be at least 1");
    }
    let until_error = args.until.as_ref().and_then(|_| {
        [
            (
                args.check.is_none() && args.judge.is_none(),
                "--until requires --check or --judge",
            ),
            (args.every.is_none(), "--until requires --every"),
            (
                !action_kind.has_effect(),
                "--until requires --agent or --wait",
            ),
            (args.in_after.is_some(), "--until conflicts with --in"),
        ]
        .into_iter()
        .find_map(|(invalid, message)| invalid.then_some(message))
    });
    if let Some(message) = until_error {
        bail!(message);
    }
    Ok(action_kind)
}

fn resolve_add_check(
    args: &AddArgs,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<Option<DeliveryCheck>> {
    if args.check.is_none() && args.judge.is_none() {
        return Ok(None);
    }
    let mut judge = Vec::new();
    if let Some(profile) = args.judge.as_deref() {
        let resolved = resolve_single_agent_launch(
            profile,
            workspace,
            &rimz::store::writer::LaunchLogin::RoomDefault,
        )?;
        let adapter = rimz::agents::find_definition(resolved.kind())
            .with_context(|| format!("unknown judge kind `{}`", resolved.kind()))?;
        if adapter.spec().launch.headless.is_none() {
            bail!(
                "{}: only claude and codex run headless judges",
                resolved.kind()
            );
        }
        judge.push(Judge {
            agent: profile.to_owned(),
            prompt: args.judge_prompt.clone(),
            prompt_file: args.judge_prompt_file.clone(),
            recheck: args.judge_recheck.clone(),
            timeout: args.judge_timeout.clone(),
        });
    }
    Ok(Some(DeliveryCheck {
        check: args.check.clone().map(TaskCheck::Shell),
        check_timeout: args.check_timeout.clone(),
        judge,
        on: args.on.as_deref().map(parse_check_on).transpose()?,
        timeout: args.timeout.clone(),
    }))
}

fn resolve_add_workspace(args: &AddArgs) -> Result<rimz::ResolvedWorkspace> {
    let task_workspace = if args.root == Path::new(".") {
        resolve_caller_workspace()
    } else {
        WorkspaceResolver::resolve(&args.root, None).map_err(Into::into)
    }
    .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    if !args.project {
        return Ok(task_workspace);
    }
    let current = resolve_caller_workspace().context("resolving current project root")?;
    if task_workspace.project_root != current.project_root {
        bail!(
            "--project writes tasks for {}; choose a --root inside that project or run from the target project",
            current.project_root.display()
        );
    }
    Ok(current)
}

/// The start directory's workspace, or an empty-dir caller's checkout, which holds its lane.
///
/// The caller's record decides, not the start path: a symlinked RimZ home
/// resolves the empty dir to a path outside `empty_dir_root()`.
fn resolve_caller_workspace() -> Result<rimz::ResolvedWorkspace> {
    let workspace = WorkspaceResolver::resolve(".", None)?;
    let Ok(room) = WorkspaceResolver::resolve_participant(".", None) else {
        return Ok(workspace);
    };
    let Ok(Some(store)) = crate::cli::open_existing_store(&room) else {
        return Ok(workspace);
    };
    let Ok(snapshot) = store.snapshot_cached() else {
        return Ok(workspace);
    };
    let Ok(caller) = rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents) else {
        return Ok(workspace);
    };
    let Some(checkout) = caller.checkout.as_deref() else {
        return Ok(workspace);
    };
    WorkspaceResolver::resolve(checkout, None)
        .with_context(|| format!("resolving the caller's checkout `{checkout}`"))
}

fn resolve_add_action(
    args: &AddArgs,
    workspace: &rimz::ResolvedWorkspace,
    kind: TaskActionKind,
) -> Result<AddTaskAction> {
    let preset = rimz::agents::LaunchPreset {
        effort: rimz::harness::plan::normalized_preset_value(args.effort.as_deref()),
        fast: fast_switch(args),
        ..Default::default()
    };
    let mut action = match kind {
        TaskActionKind::Spawn if args.stay => resolve_resident_action(args, workspace, &preset)?,
        TaskActionKind::Spawn => {
            let spec = args.agent.as_deref().unwrap_or_default();
            let resolved =
                resolve_single_agent_launch(spec, workspace, &args.account.clone().into())?;
            rimz::harness::plan::validate_launch_preset(resolved.kind(), &preset)?;
            AddTaskAction::Spawn {
                resolved,
                mode: None,
            }
        }
        TaskActionKind::Deliver => {
            let address = args.wait.as_deref().unwrap_or_default();
            resolve_delivery_target(workspace, args, address)?
        }
        TaskActionKind::CheckOnly => AddTaskAction::CheckOnly,
        TaskActionKind::Workflow => unreachable!("loop add has no workflow flag"),
    };
    reject_unsupported_action_flags(args, kind)?;
    if let AddTaskAction::Spawn { mode, .. } = &mut action {
        *mode = args.mode.as_deref().map(parse_mode).transpose()?;
    }
    Ok(action)
}

fn resolve_resident_action(
    args: &AddArgs,
    workspace: &rimz::ResolvedWorkspace,
    preset: &rimz::agents::LaunchPreset,
) -> Result<AddTaskAction> {
    let machine = MachineConfig::load_lenient();
    let mut effective = rimz::config::effective::load(&machine, &workspace.project_root)?;
    let state = StatePaths::for_project_root(&workspace.project_root)?;
    let runtime = rimz::RuntimePaths::for_state(&state)?;
    let availability =
        rimz::harness::plan::LaunchAvailability::read(&runtime, &state, &machine, Timestamp::now());
    effective.route(
        &machine.tiers,
        rimz::config::effective::ProfileScope::Agents,
        args.agent.as_deref(),
        None,
        None,
        None,
        |kind, model| availability.unavailable(kind, model),
    )?;
    let resolved = rimz::harness::plan::resolve_launch(
        &effective,
        rimz::config::effective::ProfileScope::Agents,
        &machine.agents.commands,
        args.agent.as_deref(),
        None,
    )?;
    for cell in resolved.layout.agent_cells() {
        rimz::harness::plan::validate_launch_preset(&cell.kind, preset)?;
    }
    let team = resolved
        .team_name
        .as_ref()
        .and_then(|name| resolved.teams.0.get(name));
    for signal in &args.subscribe {
        let selector = schedule::parse_signal_selector(&args.name, signal, None)?;
        match arm::default_signal_match_key(&selector, &BTreeMap::new()) {
            Some("path")
                if !args.each_worktree && workspace.worktree_root == workspace.project_root =>
            {
                bail!(
                    "--subscribe {signal} needs a checkout scope; use --each-worktree with --when, or --root <worktree>"
                );
            }
            Some("instance") if team.is_none() => {
                bail!("--subscribe {signal} needs a team layout; choose a team with --agent");
            }
            _ => {}
        }
    }
    let leader = rimz::harness::spec::prompt_leader(&resolved.layout, team)
        .context("--stay and --subscribe need a layout with a prompt leader")?;
    // prompt_leader returns an index into this layout's agent cells.
    let cell = resolved
        .layout
        .agent_cells()
        .nth(leader)
        .expect("validated prompt leader");
    let kind = cell.kind.as_str();
    if !args.subscribe.is_empty() {
        let adapter = rimz::agents::find_definition(kind)
            .with_context(|| format!("unknown agent kind `{kind}`"))?;
        let logins = rimz::agents::RoomLoginSet::for_runtime(&runtime);
        let login = logins.default_login(kind).with_context(|| {
            format!("cannot resolve the room's {kind} account; run `rimz accounts list`")
        })?;
        if let Err(error) = rimz::agents::preflight_hooks(
            adapter,
            &logins.env(&login),
            rimz::agents::TurnLifecycleNeed::NotUnsupported,
        ) {
            let fix = match &error {
                rimz::agents::HookPreflightErr::HooksUntrusted { fix, .. }
                | rimz::agents::HookPreflightErr::HooksDisabled { fix, .. } => fix.clone(),
                rimz::agents::HookPreflightErr::TurnLifecycleUnsupported { .. } => {
                    "choose a prompt leader whose provider supports registration hooks".to_owned()
                }
                _ => format!("install supported hooks with `rimz hooks install {kind}`"),
            };
            bail!(
                "--subscribe needs the prompt leader's registration hooks: {kind}: {error}; {fix}"
            );
        }
    }
    Ok(AddTaskAction::Stay {
        kind: kind.to_owned(),
    })
}

/// A scheduled turn needs a deadline it can reach, so `--timeout 0s` is refused
/// the way `--for`, `--in`, and `--until` are.
fn validate_task_timeout(raw: Option<&str>) -> Result<()> {
    let Some(raw) = raw else { return Ok(()) };
    let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
    if duration.is_zero() {
        bail!("--timeout must be greater than zero");
    }
    Ok(())
}

fn build_task_entry(
    args: &AddArgs,
    action: AddTaskAction,
    workspace: &rimz::ResolvedWorkspace,
    check: Option<DeliveryCheck>,
) -> Result<(TaskEntry, Option<ResolvedSingleAgentLaunch>)> {
    validate_task_timeout(args.timeout.as_deref())?;
    let budget = args
        .budget
        .as_deref()
        .map(str::parse::<rimz::harness::budget::BudgetSpec>)
        .transpose()?
        .map(|spec| spec.to_string());
    let budget_per_day = args
        .budget_per_day
        .as_deref()
        .map(str::parse::<rimz::harness::budget::BudgetSpec>)
        .transpose()?
        .map(|spec| format!("${:.2}", spec.cap_usd));
    let surplus = args
        .surplus
        .as_deref()
        .map(schedule::parse_surplus)
        .transpose()
        .map_err(anyhow::Error::msg)?
        .map(|ratio| format!("{ratio}x"));
    let surplus_after = args
        .surplus_after
        .as_deref()
        .map(schedule::parse_surplus_after)
        .transpose()
        .map_err(anyhow::Error::msg)?
        .map(|_| {
            args.surplus_after
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_owned()
        });
    let matches = parse_matches(&args.matches)?;
    let timing = resolve_add_timing(args)?;
    if matches!(
        action,
        AddTaskAction::Spawn { .. } | AddTaskAction::Stay { .. }
    ) && args.prompt.is_none()
        && args.prompt_file.is_none()
    {
        bail!(
            "loop task `{}` needs a prompt; pass --prompt or --prompt-file",
            args.name
        );
    }
    let mut entry = TaskEntry {
        label: args.label.clone(),
        stay: args.stay,
        each_worktree: args.each_worktree,
        takeover: args.takeover,
        subscribe: args
            .subscribe
            .iter()
            .map(|signal| rimz::config::TeamSignalBinding {
                signal: signal.clone(),
                matches: BTreeMap::new(),
                prompt: None,
            })
            .collect(),
        prompt: args.prompt.clone(),
        prompt_file: args.prompt_file.clone(),
        max_strikes: args.max_strikes,
        root: workspace.project_root.clone(),
        dir: (!args.project && workspace.worktree_root != workspace.project_root)
            .then(|| workspace.worktree_root.clone()),
        at: timing.at,
        every: args.every.clone(),
        cron: args.cron.clone(),
        signal: args.signal.clone(),
        when: (!args.when.is_empty()).then(|| args.when.clone()),
        hold: args.hold.clone(),
        matches: (!matches.is_empty()).then_some(matches),
        once: args.once.then_some(true),
        deadline: timing.deadline,
        surplus,
        surplus_after,
        timeout: args.timeout.clone(),
        ..TaskEntry::default()
    };
    if let Some(check) = check {
        entry.check = check.check;
        entry.check_timeout = check.check_timeout;
        entry.judge = check.judge;
        entry.on = check.on;
    }
    let mut resolved_for_preflight = None;
    match action {
        AddTaskAction::Stay { .. } => {
            entry.throttle = throttle_switch(args);
            entry.agent = args.agent.clone();
            entry.mode = args.mode.as_deref().map(parse_mode).transpose()?;
            entry.effort = args.effort.clone();
            entry.fast = fast_switch(args);
        }
        AddTaskAction::Spawn { resolved, mode, .. } => {
            resolved_for_preflight = Some(resolved);
            entry.throttle = throttle_switch(args);
            entry.agent = args.agent.clone();
            entry.verify = args.verify.clone();
            entry.max_attempts = args.max_attempts;
            entry.worktree = args.worktree.clone();
            entry.mode = mode;
            entry.effort = args.effort.clone();
            entry.fast = fast_switch(args);
            entry.account.clone_from(&args.account);
            entry.budget = budget;
            entry.budget_per_day = budget_per_day;
            entry.system_prompt_file = args.system_prompt_file.clone();
        }
        AddTaskAction::Deliver { .. } => unreachable!("deliveries use the shared domain builder"),
        AddTaskAction::CheckOnly => {}
    }
    Ok((entry, resolved_for_preflight))
}

fn throttle_switch(args: &AddArgs) -> Option<rimz::config::ThrottleSwitch> {
    // clap admits only `on` and `off`.
    args.throttle.as_deref().map(|value| match value {
        "off" => rimz::config::ThrottleSwitch::Off,
        _ => rimz::config::ThrottleSwitch::On,
    })
}

pub(super) fn remove(name: &str, globals: &GlobalFlags) -> Result<()> {
    let catalog = task_catalog(globals)?;
    let project_mutation = project_mutation_pre_state(&catalog, name)?;
    let mutation = catalog.remove(name)?;
    let mut out = ui::out();
    if mutation.changed() {
        writeln!(out, "removed loop task `{name}`")?;
        if let Some((root, pre_state)) = project_mutation {
            finish_project_mutation(&mut out, &root, false, pre_state)?;
        }
    } else {
        writeln!(out, "no loop task named `{name}`")?;
    }
    Ok(())
}

pub(super) fn rename(name: &str, new_name: &str, globals: &GlobalFlags) -> Result<()> {
    schedule::validate_name(new_name)?;
    if name == new_name {
        bail!("new loop task name must differ from `{name}`");
    }
    let catalog = task_catalog(globals)?;
    let project_mutation = project_mutation_pre_state(&catalog, name)?;
    let mutation = catalog.rename(name, new_name)?;
    let mut out = ui::out();
    if mutation.changed() {
        writeln!(out, "renamed loop task `{name}` to `{new_name}`")?;
        if let Some((root, pre_state)) = project_mutation {
            finish_project_mutation(&mut out, &root, false, pre_state)?;
        }
    } else {
        writeln!(out, "no loop task named `{name}`")?;
    }
    Ok(())
}

fn project_mutation_pre_state(
    catalog: &TaskCatalog,
    name: &str,
) -> Result<Option<(PathBuf, TrustState)>> {
    let Some(task) = catalog
        .visible()
        .get(name)
        .filter(|task| matches!(task.source(), TaskSource::Project { .. }))
    else {
        return Ok(None);
    };
    let root = task.entry().root.clone();
    let state = trust::status(&root)?.state;
    Ok(Some((root, state)))
}

pub(super) fn pause(args: PauseArgs, globals: &GlobalFlags) -> Result<()> {
    let task = load_task(&args.name, globals)?.ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let now = Timestamp::now();
    let key = task.key(&args.name);
    let entries = arming::load();
    if matches!(
        ArmState::resolve(entries.get(&key), task.source(), now),
        ArmState::Disabled(_)
    ) {
        bail!(
            "loop task `{}` is disabled; enable it before pausing",
            args.name
        );
    }
    let duration = parse_task_timeout(&args.pause_for).map_err(|err| anyhow::anyhow!(err))?;
    if duration.is_zero() {
        bail!("--for must be greater than zero");
    }
    let until = now
        .checked_add(duration)
        .context("resolving --for against the current clock")?;
    arming::pause(&key, task.source(), until)?;

    let mut out = ui::out();
    writeln!(
        out,
        "loop `{}`: paused; resumes {}",
        args.name,
        pause_until_text(until, now)
    )?;
    Ok(())
}

pub(super) fn enable(args: ScopeArgs, globals: &GlobalFlags) -> Result<()> {
    let tasks = scoped_tasks(args, globals)?;
    let now = Timestamp::now();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let entries = arming::load();
    let mut out = ui::out();
    for (name, task) in tasks {
        let key = task.key(&name);
        let Some(enabled) = task.enable(&name, entries.get(&key), now)? else {
            writeln!(out, "loop `{name}`: already enabled")?;
            continue;
        };
        write!(out, "loop `{name}`: enabled")?;
        if let Some(next) = task_next_fire_text(&name, &task, Some(&enabled), &now_zoned) {
            write!(out, " · next {next}")?;
        }
        writeln!(out)?;
    }
    Ok(())
}

pub(super) fn disable(args: ScopeArgs, globals: &GlobalFlags) -> Result<()> {
    let tasks = scoped_tasks(args, globals)?;
    let mut out = ui::out();
    for (name, task) in tasks {
        arming::disable(&task.key(&name), None)?;
        writeln!(out, "loop `{name}`: disabled")?;
    }
    Ok(())
}

fn scoped_tasks(args: ScopeArgs, globals: &GlobalFlags) -> Result<Vec<(String, LoadedTask)>> {
    let catalog = task_catalog(globals)?;
    if args.all {
        return Ok(catalog
            .visible()
            .iter()
            .map(|(name, task)| (name.clone(), task.clone()))
            .collect());
    }
    args.names
        .into_iter()
        .map(|name| {
            let task = catalog.visible().get(&name).cloned().ok_or_else(|| {
                anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`")
            })?;
            Ok((name, task))
        })
        .collect()
}

fn resolve_delivery_target(
    workspace: &rimz::ResolvedWorkspace,
    args: &AddArgs,
    address: &str,
) -> Result<AddTaskAction> {
    let store = crate::cli::require_existing_store(workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let channel = crate::cli::current_channel(workspace, Some(&store));
    let agent = match crate::cli::resolve_agent_one(
        &store,
        &snapshot,
        address,
        args.worktree.as_deref(),
        &channel.address_context(),
    ) {
        Ok(agent) => agent,
        Err(err) if address == "@me" => return Err(err),
        Err(_) => {
            bail!("no live agent matches `{address}`; see `rimz agents list` for live handles")
        }
    };
    if agent.agent_id.is_provisional() {
        bail!(
            "`{address}` has not registered a real session yet; retry once its first turn has started"
        );
    }
    let peers = rimz::address::addressable_agents(&snapshot);
    let caller = crate::cli::send::resolve_caller(&store)?;
    let scope = caller
        .as_ref()
        .map(|caller| rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller))
        .transpose()?
        .unwrap_or(agent);
    let target = TaskTarget {
        kind: agent.kind.clone(),
        session: agent.agent_id.clone(),
        handle: rimz::address::agent_handle(agent, &peers, true),
    };
    let mut matches = parse_matches(&args.matches)?;
    let selector = if let Some(raw) = args.signal.as_deref() {
        let selector = schedule::parse_signal_selector(&args.name, raw, Some(&matches))?;
        arm::default_signal_matches(workspace, &snapshot.agents, scope, &selector, &mut matches)?;
        arm::validate_self_signal(&selector, &matches, &target)?;
        Some(selector)
    } else {
        None
    };
    Ok(AddTaskAction::Deliver {
        target,
        selector,
        matches,
    })
}

fn parse_matches(raw: &[String]) -> Result<BTreeMap<String, String>> {
    raw.iter()
        .map(|pair| {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid --match `{pair}`; expected KEY=VALUE"))?;
            if key.is_empty() {
                bail!("invalid --match `{pair}`; KEY must not be empty");
            }
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn self_wait_guard_message() -> &'static str {
    "--wait on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"
}

fn fast_switch(args: &AddArgs) -> Option<bool> {
    match (args.fast, args.no_fast) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

fn reject_unsupported_action_flags(args: &AddArgs, kind: TaskActionKind) -> Result<()> {
    if kind.is_spawn() {
        return Ok(());
    }
    let mut flags = Vec::new();
    if kind.is_check_only() && args.worktree.is_some() {
        flags.push("--worktree");
    }
    if args.mode.is_some() {
        flags.push("--mode");
    }
    if args.effort.is_some() {
        flags.push("--effort");
    }
    if args.fast {
        flags.push("--fast");
    }
    if args.no_fast {
        flags.push("--no-fast");
    }
    if args.account.is_some() {
        flags.push("--account");
    }
    if args.budget.is_some() {
        flags.push("--budget");
    }
    if args.budget_per_day.is_some() {
        flags.push("--budget-per-day");
    }
    if args.system_prompt_file.is_some() {
        flags.push("--system-prompt-file");
    }
    if kind == TaskActionKind::Deliver && args.timeout.is_some() && args.check.is_none() {
        flags.push("--timeout");
    }
    if flags.is_empty() {
        return Ok(());
    }
    if kind == TaskActionKind::Deliver {
        bail!(
            "`{}` uses --wait, so {} only apply to --agent tasks",
            args.name,
            flags.join(", ")
        );
    }
    bail!(
        "`{}` uses --check without an agent action, so {} only apply to --agent tasks",
        args.name,
        flags.join(", ")
    )
}

fn resolve_add_timing(args: &AddArgs) -> Result<AddTiming> {
    let deadline = args.until.as_deref().map(resolve_deadline).transpose()?;
    let Some(raw) = args.in_after.as_deref() else {
        return Ok(AddTiming {
            at: args.at.clone(),
            deadline,
        });
    };
    let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
    if duration.is_zero() {
        bail!("--in must be greater than zero");
    }
    if duration >= Duration::from_secs(24 * 60 * 60) {
        bail!("--in must be less than 24h");
    }
    Ok(AddTiming {
        at: Some(schedule::delayed_at(duration)?),
        deadline,
    })
}

fn write_add_feedback(
    out: &mut impl Write,
    entry: &TaskEntry,
    parsed: &schedule::ParsedTrigger,
    action: &TaskAction,
    action_kind: Option<&str>,
    after_reset: Option<&AfterReset>,
) -> Result<()> {
    match action {
        TaskAction::Spawn(agent) if entry.stay => {
            if entry.each_worktree {
                writeln!(
                    out,
                    "action: leaves resident layout {agent} in each matching owned worktree"
                )?;
            } else {
                writeln!(
                    out,
                    "action: leaves resident layout {agent} in {}",
                    entry.run_dir().display()
                )?;
            }
            if entry.takeover {
                writeln!(
                    out,
                    "before launch: takes over the checkout (stops its idle agents, waits while any is busy)"
                )?;
            }
            for binding in &entry.subscribe {
                writeln!(
                    out,
                    "subscription: {} for the prompt leader",
                    binding.signal
                )?;
            }
        }
        TaskAction::Spawn(agent) => {
            let account = entry
                .account
                .as_ref()
                .map(|account| format!(" on account `{account}`"))
                .unwrap_or_default();
            writeln!(
                out,
                "action: launches a fresh {agent} pane{account} in {}",
                entry.root.display()
            )?;
        }
        TaskAction::Deliver(target) => {
            writeln!(
                out,
                "action: waits {} — pinned to {} session `{}` now; skipped and removed if that session exits",
                target.handle, target.kind, target.session
            )?;
        }
        TaskAction::CheckOnly => {
            writeln!(out, "action: runs check in {}", entry.run_dir().display())?;
        }
        TaskAction::Workflow(name) => writeln!(out, "action: starts workflow {name}")?,
    }
    if action.kind().has_effect()
        && (entry.guard().shell.is_some() || !entry.guard().judges.is_empty())
    {
        writeln!(out, "guard: {}", render::guard_value(entry))?;
    }
    let suffix = if parsed.once { "; then removed" } else { "" };
    if let Some(reset) = after_reset {
        writeln!(
            out,
            "trigger: fires once after the next {} {} reset",
            reset.kind, reset.span
        )?;
    } else if matches!(parsed.trigger, schedule::Trigger::Condition { .. }) {
        writeln!(out, "trigger: {}{suffix}", parsed.describe())?;
        condition::write_receipt(out, entry, parsed)?;
    } else {
        writeln!(out, "trigger: fires {}{suffix}", parsed.describe())?;
    }
    if entry.surplus.is_some() || entry.surplus_after.is_some() {
        let kind = action_kind.unwrap_or("provider");
        let threshold = entry
            .surplus
            .as_deref()
            .and_then(|raw| schedule::parse_surplus(raw).ok())
            .unwrap_or(1.0);
        let mut segments = Vec::new();
        if let Some(after) = entry.surplus_after.as_deref() {
            segments.push(format!("after {after} into the {kind} longest window"));
        }
        segments.push(format!("surplus ≥ {threshold:.1}x"));
        writeln!(out, "gate: {}", segments.join(" · "))?;
    }
    let zone = MachineConfig::load_lenient().time_zone();
    if let Some(reset) = after_reset {
        let left = reset.left.map_or_else(
            || "usage unknown".to_owned(),
            |left| format!("{left}% left"),
        );
        if !reset.started {
            writeln!(
                out,
                "window: {} {} · not started · {left}",
                reset.kind, reset.span
            )?;
            writeln!(
                out,
                "next fire: the next scheduler tick, since the window has not started"
            )?;
            return Ok(());
        }
        writeln!(
            out,
            "window: {} {} · {left} · resets {}",
            reset.kind,
            reset.span,
            reset
                .resets_at
                .to_zoned(zone.clone())
                .strftime("%Y-%m-%d %H:%M")
        )?;
    }
    if let Some(next) = first_next_fire(parsed) {
        let local = next.to_zoned(zone);
        writeln!(
            out,
            "next fire: {} ({})",
            local.strftime("%Y-%m-%d %H:%M"),
            ui::rel_until(next, Timestamp::now())
        )?;
    }
    Ok(())
}

fn first_next_fire(parsed: &schedule::ParsedTrigger) -> Option<Timestamp> {
    let now = Timestamp::now();
    let zone = MachineConfig::load_lenient().time_zone();
    parsed.next_after(&now.to_zoned(zone))
}

fn resolve_deadline(raw: &str) -> Result<Timestamp> {
    let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
    if duration.is_zero() {
        bail!("--until must be greater than zero");
    }
    Ok(Timestamp::now()
        .to_zoned(MachineConfig::load_lenient().time_zone())
        .checked_add(duration)
        .context("resolving --until against the configured clock")?
        .timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    fn judge_args(extra: &[&str]) -> AddArgs {
        let matches = AddArgs::augment_args(clap::Command::new("add"))
            .try_get_matches_from(["add", "resident"].into_iter().chain(extra.iter().copied()));
        assert!(
            matches.is_ok(),
            "judge flags must be recognized: {matches:?}"
        );
        AddArgs::from_arg_matches(&matches.unwrap()).unwrap()
    }

    #[test]
    fn shell_and_judge_flags_validate_caps_and_polarity() {
        for action in [vec!["--agent", "codex"], vec!["--wait"]] {
            let flags = action
                .into_iter()
                .chain([
                    "--check",
                    "cargo test",
                    "--check-timeout",
                    "10m",
                    "--judge",
                    "claude",
                    "--judge-prompt",
                    "q",
                    "--judge-timeout",
                    "2m",
                    "--on",
                    "fail",
                    "--every",
                    "15m",
                ])
                .collect::<Vec<_>>();
            assert!(validate_add_args(&judge_args(&flags)).is_ok(), "{flags:?}");
        }
        assert!(
            validate_add_args(&judge_args(&[
                "--agent",
                "codex",
                "--stay",
                "--check",
                "true",
                "--check-timeout",
                "10m"
            ]))
            .is_ok()
        );
    }

    #[test]
    fn judge_modifiers_require_judge_action_and_explicit_polarity() {
        for (flags, expected) in [
            (
                vec![
                    "--agent",
                    "codex",
                    "--judge",
                    "claude",
                    "--judge-prompt",
                    "q",
                ],
                "--on",
            ),
            (
                vec!["--judge", "claude", "--judge-prompt", "q", "--on", "fail"],
                "--agent or --wait",
            ),
            (
                vec!["--agent", "codex", "--judge-prompt", "q"],
                "requires --judge",
            ),
            (
                vec!["--agent", "codex", "--judge-prompt-file", "q.md"],
                "requires --judge",
            ),
            (
                vec!["--agent", "codex", "--judge-recheck", "1h"],
                "requires --judge",
            ),
            (
                vec!["--agent", "codex", "--judge-timeout", "1m"],
                "--check-timeout",
            ),
            (
                vec!["--agent", "codex", "--check-timeout", "1m"],
                "requires --check",
            ),
            (
                vec![
                    "--agent",
                    "codex",
                    "--judge",
                    "claude",
                    "--judge-prompt",
                    "q",
                    "--judge-recheck",
                    "1h",
                    "--on",
                    "fail",
                ],
                "requires --stay",
            ),
        ] {
            let result = validate_add_args(&judge_args(&flags));
            assert!(result.is_err(), "{flags:?} must be refused");
            assert!(
                result.unwrap_err().to_string().contains(expected),
                "{flags:?}: expected {expected}"
            );
        }
    }

    #[test]
    fn both_entry_builders_preserve_the_same_guard() {
        if std::env::var_os("RIMZ_TEST_GUARD_BUILDERS").is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cli::loop_cmd::add::tests::both_entry_builders_preserve_the_same_guard",
                    "--nocapture",
                ])
                .env("RIMZ_TEST_GUARD_BUILDERS", "1")
                .env("RIMZ_HOME", home.path())
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = rimz::WorkspaceResolver::resolve(root.path(), None).unwrap();
        let state = StatePaths::for_project_root(root.path()).unwrap();
        let runtime = RuntimePaths::for_state(&state).unwrap();
        rimz::Store::open(state, runtime)
            .unwrap()
            .record_workspace(&workspace)
            .unwrap();
        let flags = [
            "--check",
            "cargo test",
            "--check-timeout",
            "10m",
            "--judge",
            "claude",
            "--judge-prompt",
            "q",
            "--judge-timeout",
            "2m",
            "--on",
            "fail",
            "--every",
            "15m",
            "--prompt",
            "fix",
        ];
        let spawn_args = judge_args(&[vec!["--agent", "codex"], flags.to_vec()].concat());
        let action = resolve_add_action(&spawn_args, &workspace, TaskActionKind::Spawn).unwrap();
        let (spawn, _) = build_task_entry(
            &spawn_args,
            action,
            &workspace,
            resolve_add_check(&spawn_args, &workspace).unwrap(),
        )
        .unwrap();
        let wait_args = judge_args(&[vec!["--wait"], flags.to_vec()].concat());
        add_delivery(
            &wait_args,
            &workspace,
            TaskTarget {
                kind: rimz::ids::AgentKind::new_unchecked("codex"),
                session: "session".into(),
                handle: "@coder".into(),
            },
            None,
            BTreeMap::new(),
            resolve_add_check(&wait_args, &workspace).unwrap(),
        )
        .unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let delivered = catalog.visible()["resident"].entry().clone();
        assert_eq!(spawn.guard().shell_timeout, Some("10m"));
        assert_eq!(spawn.guard().judges.len(), 1);
        assert_eq!(delivered.guard().shell, spawn.guard().shell);
        assert_eq!(delivered.guard().shell_timeout, spawn.guard().shell_timeout);
        assert_eq!(delivered.guard().judges, spawn.guard().judges);
        assert_eq!(delivered.on, spawn.on);
    }

    #[test]
    fn approved_watchdog_receipt_names_the_whole_guard() {
        let entry: TaskEntry = toml::from_str("agent='codex'\nprompt='fix the failing test'\nevery='15m'\ncheck='cargo test'\ncheck-timeout='10m'\non='fail'\n[judge]\nagent='haiku'\nprompt='Are these failures flakes or already fixed on main?'\n").unwrap();
        let shape = schedule::TaskShape::compile("watchdog", &entry);
        let mut out = Vec::new();
        write_add_feedback(
            &mut out,
            &entry,
            shape.trigger().as_ref().unwrap(),
            shape.action().unwrap(),
            Some("codex"),
            None,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.lines().any(|line| line == "guard: runs cargo test (cap 10m), then asks haiku (cap 5m); acts when both fail"), "{text}");
    }

    fn args(extra: &[&str]) -> AddArgs {
        let matches = AddArgs::augment_args(clap::Command::new("add"))
            .try_get_matches_from(["add", "resident"].into_iter().chain(extra.iter().copied()))
            .unwrap();
        AddArgs::from_arg_matches(&matches).unwrap()
    }

    #[test]
    fn resident_subscriptions_reject_one_shot_triggers() {
        for trigger in [["--in", "5m"], ["--at", "12:00"], ["--after-reset", "5h"]] {
            let argv = ["--agent", "claude", "--stay"]
                .into_iter()
                .chain(trigger)
                .collect::<Vec<_>>();
            assert!(validate_add_args(&args(&argv)).is_ok());
            let argv = argv
                .into_iter()
                .chain(["--subscribe", "ci.failed"])
                .collect::<Vec<_>>();
            let error =
                validate_add_args(&args(&argv)).expect_err("one-shot cannot retain subscriptions");
            assert!(error.to_string().contains("--subscribe"), "{error}");
        }
    }

    #[test]
    fn resident_flags_require_their_launch_and_trigger() {
        for (argv, flag) in [
            (vec!["--stay", "--wait"], "--wait"),
            (vec!["--stay", "--check", "true"], "--agent"),
            (
                vec!["--agent", "claude", "--each-worktree", "--when", "pr=open"],
                "--stay",
            ),
            (
                vec![
                    "--agent",
                    "claude",
                    "--stay",
                    "--each-worktree",
                    "--signal",
                    "pr.merged",
                ],
                "--when",
            ),
            (
                vec!["--agent", "claude", "--subscribe", "ci.failed"],
                "--stay",
            ),
            (vec!["--agent", "claude", "--takeover"], "--stay"),
        ] {
            let error = validate_add_args(&args(&argv)).expect_err("invalid resident flags");
            assert!(error.to_string().contains(flag), "{error}");
        }
        let removed = AddArgs::augment_args(clap::Command::new("add"))
            .try_get_matches_from([
                "add",
                "resident",
                "--agent",
                "claude",
                "--stay",
                "--stop-team",
            ])
            .expect_err("--stop-team is gone");
        assert_eq!(removed.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn throttle_flag_takes_on_or_off_and_needs_an_agent() {
        let parse = |flags: &[&str]| {
            AddArgs::augment_args(clap::Command::new("add")).try_get_matches_from(
                ["add", "nightly", "--every", "1h"]
                    .into_iter()
                    .chain(flags.iter().copied()),
            )
        };
        assert_eq!(
            throttle_switch(&args(&["--agent", "claude", "--throttle", "off"])),
            Some(rimz::config::ThrottleSwitch::Off)
        );
        assert_eq!(
            throttle_switch(&args(&["--agent", "claude", "--throttle", "on"])),
            Some(rimz::config::ThrottleSwitch::On)
        );
        assert_eq!(throttle_switch(&args(&["--agent", "claude"])), None);
        parse(&["--agent", "claude", "--throttle", "maybe"]).expect_err("only on and off");
        parse(&["--check", "true", "--throttle", "off"]).expect_err("needs an agent task");
    }

    #[test]
    fn resident_launch_rejects_supervised_and_one_shot_options() {
        for flags in [
            vec!["--verify", "true"],
            vec!["--max-attempts", "2", "--verify", "true"],
            vec!["--budget", "$2"],
            vec!["--budget-per-day", "$4", "--budget", "$2"],
            vec!["--surplus", "1.5x"],
            vec!["--surplus-after", "3d"],
            vec!["--timeout", "1h"],
            vec!["--system-prompt-file", "/prompt"],
            vec!["--worktree", "lane"],
            vec!["--once"],
            vec!["--project"],
        ] {
            let argv = ["--agent", "claude", "--stay", "--when", "pr=open"]
                .into_iter()
                .chain(flags.iter().copied())
                .collect::<Vec<_>>();
            let error = validate_add_args(&args(&argv)).expect_err("unsupported resident option");
            assert!(error.to_string().contains(flags[0]), "{error}");
            assert!(error.to_string().contains("--stay"), "{error}");
        }
        let error = validate_add_args(&args(&[
            "--agent", "claude", "--stay", "--every", "1m", "--until", "1h", "--check", "true",
        ]))
        .expect_err("resident poll-until must be refused");
        assert!(
            error.to_string().contains("--stay") && error.to_string().contains("--until"),
            "{error}"
        );
    }

    #[test]
    fn agent_check_flags_require_a_profile_prompt_and_action() {
        for (flags, message) in [
            (vec!["--judge", "haiku"], "--judge-prompt"),
            (
                vec![
                    "--judge",
                    "haiku",
                    "--judge-prompt",
                    "q",
                    "--judge-prompt-file",
                    "q.md",
                ],
                "exactly one",
            ),
            (vec!["--judge-prompt", "q"], "--judge"),
            (vec!["--judge-prompt-file", "q.md"], "--judge"),
            (vec!["--judge-recheck", "6h"], "--judge"),
            (vec!["--judge-timeout", "5m"], "--judge"),
            (
                vec![
                    "--judge",
                    "haiku",
                    "--judge-prompt",
                    "q",
                    "--judge-recheck",
                    "6h",
                    "--on",
                    "success",
                ],
                "--stay",
            ),
        ] {
            let argv = ["--agent", "claude"]
                .into_iter()
                .chain(flags)
                .collect::<Vec<_>>();
            let result = validate_add_args(&args(&argv));
            assert!(
                result.is_err(),
                "invalid judge flags must be rejected: {argv:?}"
            );
            assert!(
                result.unwrap_err().to_string().contains(message),
                "{argv:?}"
            );
        }
        let result = validate_add_args(&args(&["--judge", "haiku", "--judge-prompt", "q"]));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("--agent or --wait")
        );
    }

    #[test]
    fn resident_checks_and_agent_check_polarity_are_accepted() {
        for flags in [
            vec![
                "--until",
                "1h",
                "--every",
                "1m",
                "--judge",
                "haiku",
                "--judge-prompt",
                "q",
                "--on",
                "fail",
            ],
            vec!["--stay", "--check", "true", "--on", "success"],
            vec![
                "--stay",
                "--judge",
                "haiku",
                "--judge-prompt",
                "q",
                "--judge-recheck",
                "6h",
                "--judge-timeout",
                "5m",
                "--on",
                "success",
            ],
        ] {
            let argv = ["--agent", "claude"]
                .into_iter()
                .chain(flags)
                .collect::<Vec<_>>();
            assert!(validate_add_args(&args(&argv)).is_ok(), "{argv:?}");
        }
        assert!(
            validate_add_args(&args(&[
                "--wait",
                "--judge",
                "haiku",
                "--judge-prompt-file",
                "q.md",
                "--on",
                "any"
            ]))
            .is_ok()
        );
    }

    #[test]
    fn zero_recheck_reasks_without_accepting_a_zero_timeout() {
        let flags = [
            "--agent",
            "claude",
            "--stay",
            "--judge",
            "haiku",
            "--judge-prompt",
            "q",
            "--judge-recheck",
            "0",
            "--on",
            "success",
        ];
        assert!(
            validate_add_args(&args(&flags)).is_ok(),
            "zero recheck must be accepted"
        );
        let timeout = [
            "--agent",
            "claude",
            "--stay",
            "--judge",
            "haiku",
            "--judge-prompt",
            "q",
            "--judge-timeout",
            "0",
            "--on",
            "success",
        ];
        assert!(
            validate_add_args(&args(&timeout)).is_err(),
            "zero timeout must remain refused"
        );
    }
}
