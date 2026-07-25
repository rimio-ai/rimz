//! Add, remove, and rename configured loop tasks.

use super::*;

struct AddTiming {
    at: Option<String>,
    deadline: Option<Timestamp>,
}

enum AddTaskAction {
    Spawn {
        resolved: ResolvedTaskSpec,
        mode: Option<String>,
    },
    Deliver {
        target: TaskTarget,
    },
    CheckOnly,
}

impl AddTaskAction {
    fn provider_kind(&self) -> Option<&str> {
        match self {
            Self::Spawn { resolved, .. } => Some(resolved.kind()),
            Self::Deliver { target } => Some(&target.kind),
            Self::CheckOnly => None,
        }
    }
}

// ---- add / remove -----------------------------------------------------------

pub(super) fn add(args: AddArgs, _globals: &GlobalFlags) -> Result<()> {
    let action_kind = validate_add_args(&args)?;
    let workspace = resolve_add_workspace(&args)?;
    let project_root = workspace.project_root.clone();
    let action = resolve_add_action(&args, &workspace, action_kind)?;
    let provider_kind = action.provider_kind().map(ToOwned::to_owned);
    let (entry, resolved_for_preflight) = build_task_entry(&args, action, &project_root)?;
    // Compile once before writing, so validation and feedback share one shape.
    let shape = schedule::TaskShape::compile(&args.name, &entry);
    let parsed = shape.schedule().as_ref().map_err(Clone::clone)?;
    let task_action = shape.action().map_err(Clone::clone)?;
    if action_kind.has_effect() {
        preflight_entry(task_action, resolved_for_preflight.as_ref())?;
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
    )?;
    writeln!(
        out,
        "live while a room for {} is open",
        entry.root.display()
    )?;
    if !render::room_open(&entry.root) {
        writeln!(out, "no room is open there; start one with `rimz start`")?;
    }
    Ok(())
}

fn validate_add_args(args: &AddArgs) -> Result<TaskActionKind> {
    schedule::validate_name(&args.name)?;
    let project_error = args.project.then(|| {
        [
            (
                args.wake.is_some(),
                "--project tasks cannot use --wake; project config cannot pin a machine-local session",
            ),
            (
                args.until.is_some(),
                "--project tasks cannot use --until; poll-until deadlines are machine state",
            ),
            (
                args.every.is_none() && args.cron.is_none(),
                "--project tasks must repeat; set --every or --cron",
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
        .or(args.wake.as_ref().map(|_| TaskActionKind::Deliver))
        .or(args.check.as_ref().map(|_| TaskActionKind::CheckOnly))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loop task `{}` needs --agent, --wake, or --check",
                args.name
            )
        })?;
    if args.on.is_some() && args.check.is_none() {
        bail!("--on requires --check");
    }
    if !action_kind.has_effect() && (args.surplus.is_some() || args.surplus_after.is_some()) {
        bail!("--surplus and --surplus-after require --agent or --wake");
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
                "--until requires --agent or --wake",
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
        TaskActionKind::Spawn => {
            let spec = args.agent.as_deref().unwrap_or_default();
            let resolved = resolve_task_spec(spec, workspace)?;
            AddTaskAction::Spawn {
                resolved,
                mode: None,
            }
        }
        TaskActionKind::Deliver => {
            let address = args.wake.as_deref().unwrap_or_default();
            AddTaskAction::Deliver {
                target: resolve_delivery_target(workspace, args, address)?,
            }
        }
        TaskActionKind::CheckOnly => AddTaskAction::CheckOnly,
    };
    reject_unsupported_action_flags(args, kind)?;
    if let AddTaskAction::Spawn { mode, .. } = &mut action {
        *mode = args.mode.as_deref().map(parse_mode).transpose()?;
    }
    Ok(action)
}

fn build_task_entry(
    args: &AddArgs,
    action: AddTaskAction,
    project_root: &Path,
) -> Result<(TaskEntry, Option<ResolvedTaskSpec>)> {
    if let Some(timeout) = args.timeout.as_deref() {
        parse_task_timeout(timeout).map_err(|err| anyhow::anyhow!("{err}"))?;
    }
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
    let timing = resolve_add_timing(args)?;
    if !matches!(action, AddTaskAction::CheckOnly)
        && args.prompt.is_none()
        && args.prompt_file.is_none()
    {
        bail!(
            "loop task `{}` needs a prompt; pass --prompt or --prompt-file",
            args.name
        );
    }
    let uses_check_timeout = args.check.is_some();
    let mut entry = TaskEntry {
        prompt: args.prompt.clone(),
        prompt_file: args.prompt_file.clone(),
        check: args.check.clone(),
        max_strikes: args.max_strikes,
        on,
        root: project_root.to_path_buf(),
        at: timing.at,
        every: args.every.clone(),
        cron: args.cron.clone(),
        deadline: timing.deadline,
        surplus,
        surplus_after,
        ..TaskEntry::default()
    };
    let mut resolved_for_preflight = None;
    match action {
        AddTaskAction::Spawn { resolved, mode, .. } => {
            resolved_for_preflight = Some(resolved);
            entry.agent = args.agent.clone();
            entry.verify = args.verify.clone();
            entry.max_attempts = args.max_attempts;
            entry.worktree = args.worktree.clone();
            entry.mode = mode;
            entry.effort = args.effort.clone();
            entry.budget = budget;
            entry.budget_per_day = budget_per_day;
            entry.system_prompt_file = args.system_prompt_file.clone();
            entry.timeout = args.timeout.clone();
        }
        AddTaskAction::Deliver { target } => {
            entry.wake = Some(target);
            entry.timeout = uses_check_timeout.then(|| args.timeout.clone()).flatten();
        }
        AddTaskAction::CheckOnly => {
            entry.timeout = uses_check_timeout.then(|| args.timeout.clone()).flatten();
        }
    }
    Ok((entry, resolved_for_preflight))
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
    let key = task_key(&args.name, &task);
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
        let key = task_key(&name, &task);
        let record = entries.get(&key);
        let already_enabled = ArmState::resolve(record, task.source(), now) == ArmState::Live
            && record.is_none_or(|record| record.enabled && record.strikes.is_none());
        if already_enabled {
            strikes::clear(&key)?;
            writeln!(out, "loop `{name}`: already enabled")?;
            continue;
        }
        let enabled = arming::enable(&key)?;
        strikes::clear(&key)?;
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
        arming::disable(&task_key(&name, &task), None)?;
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
) -> Result<TaskTarget> {
    let store = crate::cli::open_store(workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let channel = crate::cli::current_channel(workspace);
    let agent = match crate::cli::resolve_agent_one(
        &snapshot,
        address,
        args.worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(agent) => agent,
        Err(_) => {
            bail!("no live agent matches `{address}`; run /schedule from inside the agent pane")
        }
    };
    if agent.agent_id.is_provisional() {
        bail!(
            "`{address}` has not registered a real session yet; run /schedule from inside the agent pane"
        );
    }
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    Ok(TaskTarget {
        kind: agent.kind.as_str().to_owned(),
        session: agent.agent_id.as_str().to_owned(),
        handle: rimz::harness::target::agent_handle(agent, &peers, true),
    })
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
            "`{}` uses --wake, so {} only apply to --agent tasks",
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
    let target = Timestamp::now()
        .to_zoned(MachineConfig::load_lenient().time_zone())
        .checked_add(duration)
        .context("resolving --in against the configured clock")?;
    Ok(AddTiming {
        at: Some(format!("{:02}:{:02}", target.hour(), target.minute())),
        deadline,
    })
}

fn write_add_feedback(
    out: &mut impl Write,
    entry: &TaskEntry,
    parsed: &schedule::ParsedSchedule,
    action: &TaskAction,
    action_kind: Option<&str>,
) -> Result<()> {
    match action {
        TaskAction::Spawn(agent) => {
            writeln!(
                out,
                "action: launches a fresh {agent} pane in {}",
                entry.root.display()
            )?;
        }
        TaskAction::Deliver(target) => {
            writeln!(
                out,
                "action: wakes {} — pinned to {} session `{}` now; skipped and removed if that session exits",
                target.handle, target.kind, target.session
            )?;
        }
        TaskAction::CheckOnly => {
            writeln!(out, "action: runs check in {}", entry.root.display())?;
        }
    }
    let suffix = if parsed.once { "; then removed" } else { "" };
    writeln!(out, "schedule: {}{suffix}", parsed.describe())?;
    if entry.surplus.is_some() || entry.surplus_after.is_some() {
        let kind = action_kind.unwrap_or("provider");
        let threshold = entry
            .surplus
            .as_deref()
            .and_then(|raw| schedule::parse_surplus(raw).ok())
            .unwrap_or(1.0);
        let mut segments = Vec::new();
        if let Some(after) = entry.surplus_after.as_deref() {
            segments.push(format!("after {after} into the {kind} 7d window"));
        }
        segments.push(format!("surplus ≥ {threshold:.1}x"));
        writeln!(out, "gate: {}", segments.join(" · "))?;
    }
    if let Some(next) = first_next_fire(parsed) {
        let zone = MachineConfig::load_lenient().time_zone();
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

fn first_next_fire(parsed: &schedule::ParsedSchedule) -> Option<Timestamp> {
    let now = Timestamp::now();
    let zone = MachineConfig::load_lenient().time_zone();
    parsed.schedule.next_after(now, &now.to_zoned(zone))
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
