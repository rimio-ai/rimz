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
    let action = resolve_add_action(&args, &workspace, action_kind)?;
    let action = match action {
        AddTaskAction::Deliver {
            target,
            selector,
            matches,
        } => {
            return add_delivery(&args, &workspace, target, selector, matches);
        }
        action => action,
    };
    let provider_kind = action.provider_kind().map(ToOwned::to_owned);
    let (mut entry, resolved_for_preflight) = build_task_entry(&args, action, &workspace)?;
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
    let on = args.on.as_deref().map(parse_check_on).transpose()?;
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
            target,
            trigger,
            prompt,
            provenance: DeliveryProvenance::Loop,
            check: args.check.as_ref().map(|command| DeliveryCheck {
                command: command.clone(),
                on,
                timeout: args.timeout.clone(),
            }),
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
                args.check.is_some(),
                "--check",
                "resident launches do not run check commands",
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
    if args.on.is_some() && args.check.is_none() {
        bail!("--on requires --check");
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
            (args.check.is_none(), "--until requires --check"),
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

fn resolve_add_workspace(args: &AddArgs) -> Result<rimz::ResolvedWorkspace> {
    let task_workspace = WorkspaceResolver::resolve(&args.root, None)
        .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    if !args.project {
        return Ok(task_workspace);
    }
    let current =
        WorkspaceResolver::resolve(".", None).context("resolving current project root")?;
    if task_workspace.project_root != current.project_root {
        bail!(
            "--project writes tasks for {}; choose a --root inside that project or run from the target project",
            current.project_root.display()
        );
    }
    Ok(current)
}

fn resolve_add_action(
    args: &AddArgs,
    workspace: &rimz::ResolvedWorkspace,
    kind: TaskActionKind,
) -> Result<AddTaskAction> {
    let mut action = match kind {
        TaskActionKind::Spawn if args.stay => resolve_resident_action(args, workspace)?,
        TaskActionKind::Spawn => {
            let spec = args.agent.as_deref().unwrap_or_default();
            let resolved =
                resolve_single_agent_launch(spec, workspace, &args.account.clone().into())?;
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
                rimz::agents::HookPreflightErr::HooksUntrusted { fix, .. } => fix.clone(),
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
    let on = args.on.as_deref().map(parse_check_on).transpose()?;
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
    let uses_check_timeout = args.check.is_some();
    let mut entry = TaskEntry {
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
        check: args.check.clone(),
        max_strikes: args.max_strikes,
        on,
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
        ..TaskEntry::default()
    };
    let mut resolved_for_preflight = None;
    match action {
        AddTaskAction::Stay { .. } => {
            entry.throttle = throttle_switch(args);
            entry.agent = args.agent.clone();
            entry.mode = args.mode.as_deref().map(parse_mode).transpose()?;
            entry.effort = args.effort.clone();
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
            entry.account.clone_from(&args.account);
            entry.budget = budget;
            entry.budget_per_day = budget_per_day;
            entry.system_prompt_file = args.system_prompt_file.clone();
            entry.timeout = args.timeout.clone();
        }
        AddTaskAction::Deliver { .. } => unreachable!("deliveries use the shared domain builder"),
        AddTaskAction::CheckOnly => {
            entry.timeout = uses_check_timeout.then(|| args.timeout.clone()).flatten();
        }
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
    let name = args.name.unwrap_or_default();
    let task = catalog
        .visible()
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    Ok(vec![(name, task)])
}

fn resolve_delivery_target(
    workspace: &rimz::ResolvedWorkspace,
    args: &AddArgs,
    address: &str,
) -> Result<AddTaskAction> {
    let store = crate::cli::open_store(workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let channel = crate::cli::current_channel(workspace, Some(&store));
    let agent = match crate::cli::resolve_agent_one(
        &store,
        &snapshot,
        address,
        args.worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(agent) => agent,
        Err(err) if address == "@me" => return Err(err),
        Err(_) => {
            bail!("no live agent matches `{address}`; run /schedule from inside the agent pane")
        }
    };
    if agent.agent_id.is_provisional() {
        bail!(
            "`{address}` has not registered a real session yet; run /schedule from inside the agent pane"
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
            vec!["--check", "true"],
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
    }
}
