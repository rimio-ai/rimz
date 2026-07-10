//! Add, remove, and rename configured loop tasks.

use super::*;

struct AddTiming {
    at: Option<String>,
    deadline: Option<Timestamp>,
}

enum AddTaskAction {
    Spawn {
        resolved: ResolvedTaskSpec,
        is_ping: bool,
    },
    Deliver {
        target: TaskTarget,
        resolved: Option<ResolvedTaskSpec>,
    },
    CheckOnly,
}

// ---- add / remove -----------------------------------------------------------

pub(super) fn add(args: AddArgs, _globals: &GlobalFlags) -> Result<()> {
    schedule::validate_name(&args.name)?;
    if args.project && args.wake.is_some() {
        bail!(
            "--project tasks cannot use --wake; project config cannot pin a machine-local session"
        );
    }
    if args.project && args.until.is_some() {
        bail!("--project tasks cannot use --until; poll-until deadlines are machine state");
    }
    if args.project && (args.every.is_none() && args.cron.is_none()) {
        bail!("--project tasks must repeat; set --every or --cron");
    }
    let agent_action_requested = args.agent.is_some() || args.wake.is_some();
    if !agent_action_requested && args.check.is_none() {
        bail!(
            "loop task `{}` needs --agent, --wake, or --check",
            args.name
        );
    }
    if args.on.is_some() && args.check.is_none() {
        bail!("--on requires --check");
    }
    if args.until.is_some() {
        if args.check.is_none() {
            bail!("--until requires --check");
        }
        if args.every.is_none() {
            bail!("--until requires --every");
        }
        if !agent_action_requested {
            bail!("--until requires --agent or --wake");
        }
        if args.in_after.is_some() {
            bail!("--until conflicts with --in");
        }
    }
    let task_workspace = WorkspaceResolver::resolve(&args.root, None)
        .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    let workspace = if args.project {
        let current =
            WorkspaceResolver::resolve(".", None).context("resolving current project root")?;
        if task_workspace.project_root != current.project_root {
            bail!(
                "--project writes tasks for {}; choose a --root inside that project or run from the target project",
                current.project_root.display()
            );
        }
        current
    } else {
        task_workspace
    };
    let project_root = workspace.project_root.clone();
    let target = match args.wake.as_deref() {
        Some(address) => Some(resolve_delivery_target(&workspace, &args, address)?),
        None => None,
    };
    let resolved = match args.agent.as_deref() {
        Some(spec) => Some(resolve_task_spec(spec, &workspace)?),
        None => None,
    };
    let action = match (target, resolved) {
        (Some(target), resolved) => AddTaskAction::Deliver { target, resolved },
        (None, Some(resolved)) => {
            let is_ping = args
                .agent
                .as_deref()
                .is_some_and(agents_spec::virtual_ping_shape);
            if is_ping {
                ping_kind_supported(&resolved.kind)?;
            }
            AddTaskAction::Spawn { resolved, is_ping }
        }
        (None, None) => AddTaskAction::CheckOnly,
    };
    if args.every.as_deref() == Some("reset")
        && !matches!(action, AddTaskAction::Spawn { is_ping: true, .. })
    {
        bail!("--every reset only applies to a `<kind>-ping` agent task");
    }
    let mode = match &action {
        AddTaskAction::Spawn { .. } => args.mode.as_deref().map(parse_mode).transpose()?,
        AddTaskAction::Deliver { .. } => {
            reject_delivery_spawn_flags(&args)?;
            None
        }
        AddTaskAction::CheckOnly => {
            reject_check_only_agent_flags(&args)?;
            None
        }
    };
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
    let on = args.on.as_deref().map(parse_check_on).transpose()?;
    let timing = resolve_add_timing(&args)?;
    let prompt = args.prompt;
    if !matches!(action, AddTaskAction::CheckOnly) && prompt.is_none() && args.prompt_file.is_none()
    {
        bail!(
            "loop task `{}` needs a prompt; pass --prompt or --prompt-file",
            args.name
        );
    }
    let check = args.check;
    let uses_check_timeout = check.is_some();
    let mut resolved_for_preflight = None;
    let entry = match action {
        AddTaskAction::Spawn { resolved, .. } => {
            resolved_for_preflight = Some(resolved);
            TaskEntry {
                agent: args.agent,
                wake: None,
                prompt,
                prompt_file: args.prompt_file,
                check,
                on,
                root: project_root.clone(),
                worktree: args.worktree,
                mode,
                effort: args.effort,
                budget,
                budget_per_day,
                system_prompt_file: args.system_prompt_file,
                timeout: args.timeout,
                at: timing.at,
                every: args.every,
                cron: args.cron,
                deadline: timing.deadline,
            }
        }
        AddTaskAction::Deliver { target, resolved } => {
            resolved_for_preflight = resolved;
            TaskEntry {
                agent: args.agent,
                wake: Some(target),
                prompt,
                prompt_file: args.prompt_file,
                check,
                on,
                root: project_root.clone(),
                worktree: None,
                mode: None,
                effort: None,
                budget: None,
                budget_per_day: None,
                system_prompt_file: None,
                timeout: uses_check_timeout.then_some(args.timeout).flatten(),
                at: timing.at,
                every: args.every,
                cron: args.cron,
                deadline: timing.deadline,
            }
        }
        AddTaskAction::CheckOnly => TaskEntry {
            agent: None,
            wake: None,
            prompt,
            prompt_file: args.prompt_file,
            check,
            on,
            root: project_root.clone(),
            worktree: None,
            mode: None,
            effort: None,
            budget: None,
            budget_per_day: None,
            system_prompt_file: None,
            timeout: uses_check_timeout.then_some(args.timeout).flatten(),
            at: timing.at,
            every: args.every,
            cron: args.cron,
            deadline: timing.deadline,
        },
    };
    // Validate the firing time before writing, so a bad `--at`/`--every` fails here.
    let parsed = schedule::parse_schedule(&args.name, &entry)?;
    if entry.agent.is_some() || entry.wake.is_some() {
        preflight_entry(&args.name, &entry, resolved_for_preflight.as_ref())?;
    }
    if args.project {
        project_config_set_entry(&project_root, &args.name, &entry)?;
    } else if matches!(
        instances::load_entry_visible_with_project(
            &args.name,
            project_visible_merge(&project_tasks_for_root(&project_root)?)
        ),
        Some((_, TaskSource::Project { .. }))
    ) {
        bail!(
            "loop task `{}` is project-owned in {}; use `rimz loop add --project` or choose another name",
            args.name,
            project_config_path(&project_root).display()
        );
    } else if instances::is_ephemeral(&entry) {
        config_remove(&args.name)?;
        instances::insert(&args.name, &entry)?;
    } else {
        instances::remove(&args.name)?;
        config_set_entry(&args.name, &entry)?;
    }
    let cleared_pause = pauses::remove(&args.name)?;

    let mut out = ui::out();
    writeln!(out, "added loop task `{}`", args.name)?;
    if cleared_pause {
        writeln!(out, "pause: cleared")?;
    }
    if args.project {
        finish_project_mutation(&mut out, &project_root, true)?;
    }
    write_add_feedback(&mut out, &args.name, &entry, &parsed)?;
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

pub(super) fn remove(name: &str, globals: &GlobalFlags) -> Result<()> {
    let loaded = load_task(name, globals)?;
    let removed = match loaded {
        Some((entry, source)) => {
            let removed = remove_loaded_task(name, &entry, source)?;
            if removed && matches!(source, TaskSource::Project { .. }) {
                pauses::remove(name)?;
                let mut out = ui::out();
                writeln!(out, "removed loop task `{name}`")?;
                finish_project_mutation(&mut out, &entry.root, false)?;
                return Ok(());
            }
            removed
        }
        None => false,
    };
    let mut out = ui::out();
    if removed {
        pauses::remove(name)?;
        writeln!(out, "removed loop task `{name}`")?;
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
    if load_all_tasks(globals)?.contains_key(new_name) {
        bail!("loop task `{new_name}` already exists");
    }

    let loaded = load_task(name, globals)?;
    let renamed = match loaded {
        Some((entry, source)) => match source {
            TaskSource::Config => config_rename(name, new_name)?,
            TaskSource::Instance => instances::rename(name, new_name)?,
            TaskSource::Project { .. } => {
                let renamed = project_config_rename(&entry.root, name, new_name)?;
                if renamed {
                    pauses::rename(name, new_name)?;
                    let mut out = ui::out();
                    writeln!(out, "renamed loop task `{name}` to `{new_name}`")?;
                    finish_project_mutation(&mut out, &entry.root, false)?;
                    return Ok(());
                }
                false
            }
        },
        None => false,
    };
    let mut out = ui::out();
    if renamed {
        pauses::rename(name, new_name)?;
        writeln!(out, "renamed loop task `{name}` to `{new_name}`")?;
    } else {
        writeln!(out, "no loop task named `{name}`")?;
    }
    Ok(())
}

pub(super) fn pause(args: PauseArgs, globals: &GlobalFlags) -> Result<()> {
    load_task(&args.name, globals)?.ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let now = Timestamp::now();
    let until = args
        .pause_for
        .as_deref()
        .map(|raw| {
            let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!(err))?;
            if duration.is_zero() {
                bail!("--for must be greater than zero");
            }
            now.checked_add(duration)
                .context("resolving --for against the current clock")
        })
        .transpose()?;
    pauses::set(&args.name, PauseEntry { until })?;

    let mut out = ui::out();
    match until {
        Some(until) => writeln!(
            out,
            "loop `{}`: paused; resumes {}",
            args.name,
            pause_until_text(until, now)
        )?,
        None => writeln!(
            out,
            "loop `{}`: paused; resume with `rimz loop resume {}`",
            args.name, args.name
        )?,
    }
    Ok(())
}

pub(super) fn resume(name: &str, globals: &GlobalFlags) -> Result<()> {
    let (entry, _) = load_task(name, globals)?
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    let now = Timestamp::now();
    let pause = pauses::load().remove(name);
    if !pause
        .as_ref()
        .is_some_and(|entry| pauses::is_active(entry, now))
    {
        writeln!(ui::out(), "loop `{name}`: not paused")?;
        return Ok(());
    }

    let resumed = PauseEntry { until: Some(now) };
    pauses::set(name, resumed)?;
    let mut out = ui::out();
    write!(out, "loop `{name}`: resumed")?;
    if let Some(next) = render::task_next_fire_text(name, &entry, Some(&resumed), now) {
        write!(out, " · next {next}")?;
    }
    writeln!(out)?;
    Ok(())
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
    let peers: Vec<_> = snapshot
        .agents
        .iter()
        .filter(|peer| peer.parent_agent_id.is_none())
        .collect();
    Ok(TaskTarget {
        kind: agent.kind.as_str().to_owned(),
        session: agent.agent_id.as_str().to_owned(),
        handle: rimz::harness::target::agent_handle(agent, &peers, true),
    })
}

fn reject_delivery_spawn_flags(args: &AddArgs) -> Result<()> {
    let mut flags = Vec::new();
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
    if args.timeout.is_some() && args.check.is_none() {
        flags.push("--timeout");
    }
    if flags.is_empty() {
        return Ok(());
    }
    bail!(
        "`{}` uses --wake, so {} only apply to --agent tasks",
        args.name,
        flags.join(", ")
    )
}

fn reject_check_only_agent_flags(args: &AddArgs) -> Result<()> {
    let mut flags = Vec::new();
    if args.worktree.is_some() {
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
    if flags.is_empty() {
        return Ok(());
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
    name: &str,
    entry: &TaskEntry,
    parsed: &schedule::ParsedSchedule,
) -> Result<()> {
    match task_action(name, entry)? {
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
    if let Some(next) = first_next_fire(entry, parsed) {
        let zone = MachineConfig::load_lenient().time_zone();
        let local = next.to_zoned(zone);
        writeln!(
            out,
            "next fire: {} ({})",
            local.strftime("%Y-%m-%d %H:%M"),
            ui::rel_until(next, Timestamp::now())
        )?;
    } else {
        writeln!(out, "next fire: waiting for a provider reset reading")?;
    }
    Ok(())
}

fn first_next_fire(entry: &TaskEntry, parsed: &schedule::ParsedSchedule) -> Option<Timestamp> {
    let now = Timestamp::now();
    let zone = MachineConfig::load_lenient().time_zone();
    let window_reset = match &parsed.schedule {
        schedule::Schedule::WindowReset => entry
            .agent
            .as_deref()
            .and_then(rimz::harness::spec::ping_kind)
            .and_then(|kind| window_reset_at(entry, kind).ok().flatten()),
        _ => None,
    };
    parsed
        .schedule
        .next_after(now, &now.to_zoned(zone), window_reset)
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
