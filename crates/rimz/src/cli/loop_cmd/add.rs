//! Add, remove, and rename configured loop tasks.

use super::*;

struct AddTiming {
    at: Option<String>,
    days: Option<String>,
    once: bool,
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

pub(super) fn add(args: AddArgs) -> Result<()> {
    schedule::validate_name(&args.name)?;
    let agent_action_requested = args.spec.is_some() || args.bind.is_some();
    if !agent_action_requested && args.check.is_none() {
        bail!("loop task `{}` needs --spec, --bind, or --check", args.name);
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
            bail!("--until requires --spec or --bind");
        }
        if args.once {
            bail!("--until conflicts with --once");
        }
        if args.in_after.is_some() {
            bail!("--until conflicts with --in");
        }
    }
    let workspace = WorkspaceResolver::resolve(&args.root, None)
        .with_context(|| format!("resolving project root at {}", args.root.display()))?;
    let target = match args.bind.as_deref() {
        Some(address) => Some(resolve_delivery_target(&workspace, &args, address)?),
        None => None,
    };
    let resolved = match args.spec.as_deref() {
        Some(spec) => Some(resolve_task_spec(spec, &workspace)?),
        None => None,
    };
    let action = match (target, resolved) {
        (Some(target), resolved) => AddTaskAction::Deliver { target, resolved },
        (None, Some(resolved)) => {
            let is_ping = args
                .spec
                .as_deref()
                .is_some_and(agents_spec::virtual_ping_shape);
            if is_ping {
                ping_kind_supported(&resolved.kind)?;
            }
            AddTaskAction::Spawn { resolved, is_ping }
        }
        (None, None) => AddTaskAction::CheckOnly,
    };
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
    let on = args.on.as_deref().map(parse_check_on).transpose()?;
    let timing = resolve_add_timing(&args)?;
    let prompt = match &action {
        AddTaskAction::Spawn { is_ping: true, .. }
            if args.prompt.is_none() && args.prompt_file.is_none() =>
        {
            Some("ping".to_owned())
        }
        _ => args.prompt,
    };
    if !matches!(action, AddTaskAction::CheckOnly)
        && prompt.is_none()
        && args.prompt_file.is_none()
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
                spec: args.spec,
                bind: None,
                prompt,
                prompt_file: args.prompt_file,
                check,
                on,
                root: workspace.project_root,
                worktree: args.worktree,
                mode,
                effort: args.effort,
                system_prompt_file: args.system_prompt_file,
                timeout: args.timeout,
                at: timing.at,
                days: timing.days,
                every: args.every,
                cron: args.cron,
                deadline: timing.deadline,
                once: timing.once,
            }
        }
        AddTaskAction::Deliver { target, resolved } => {
            resolved_for_preflight = resolved;
            TaskEntry {
                spec: args.spec,
                bind: Some(target),
                prompt,
                prompt_file: args.prompt_file,
                check,
                on,
                root: workspace.project_root,
                worktree: None,
                mode: None,
                effort: None,
                system_prompt_file: None,
                timeout: uses_check_timeout.then_some(args.timeout).flatten(),
                at: timing.at,
                days: timing.days,
                every: args.every,
                cron: args.cron,
                deadline: timing.deadline,
                once: timing.once,
            }
        }
        AddTaskAction::CheckOnly => TaskEntry {
            spec: None,
            bind: None,
            prompt,
            prompt_file: args.prompt_file,
            check,
            on,
            root: workspace.project_root,
            worktree: None,
            mode: None,
            effort: None,
            system_prompt_file: None,
            timeout: uses_check_timeout.then_some(args.timeout).flatten(),
            at: timing.at,
            days: timing.days,
            every: args.every,
            cron: args.cron,
            deadline: timing.deadline,
            once: timing.once,
        },
    };
    // Validate the firing time before writing, so a bad `--at`/`--days` fails here.
    let parsed = schedule::parse_schedule(&args.name, &entry)?;
    if entry.spec.is_some() || entry.bind.is_some() {
        preflight_entry(&args.name, &entry, resolved_for_preflight.as_ref())?;
    }
    if instances::is_ephemeral(&entry) {
        config_remove(&args.name)?;
        instances::insert(&args.name, &entry)?;
    } else {
        instances::remove(&args.name)?;
        config_set_entry(&args.name, &entry)?;
    }

    let mut out = ui::out();
    writeln!(
        out,
        "added loop task `{}`: {} {} in {}",
        args.name,
        task_subject(&entry),
        parsed.describe(),
        entry.root.display()
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

pub(super) fn remove(name: &str) -> Result<()> {
    let removed = instances::remove(name)? | config_remove(name)?;
    let mut out = ui::out();
    if removed {
        writeln!(out, "removed loop task `{name}`")?;
    } else {
        writeln!(out, "no loop task named `{name}`")?;
    }
    Ok(())
}

pub(super) fn rename(name: &str, new_name: &str) -> Result<()> {
    schedule::validate_name(new_name)?;
    if name == new_name {
        bail!("new loop task name must differ from `{name}`");
    }
    if instances::load_all().contains_key(new_name) {
        bail!("loop task `{new_name}` already exists");
    }

    let renamed = instances::rename(name, new_name)? | config_rename(name, new_name)?;
    let mut out = ui::out();
    if renamed {
        writeln!(out, "renamed loop task `{name}` to `{new_name}`")?;
    } else {
        writeln!(out, "no loop task named `{name}`")?;
    }
    Ok(())
}

fn resolve_delivery_target(
    workspace: &rimz::ResolvedWorkspace,
    args: &AddArgs,
    address: &str,
) -> Result<TaskTarget> {
    let ledger = crate::cli::open_ledger(workspace)?;
    let snapshot = ledger.snapshot_cached().context("reading agent snapshot")?;
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
        "`{}` uses --bind, so {} only apply to --spec tasks",
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
    if args.system_prompt_file.is_some() {
        flags.push("--system-prompt-file");
    }
    if flags.is_empty() {
        return Ok(());
    }
    bail!(
        "`{}` uses --check without an agent action, so {} only apply to --spec tasks",
        args.name,
        flags.join(", ")
    )
}


fn resolve_add_timing(args: &AddArgs) -> Result<AddTiming> {
    let deadline = args.until.as_deref().map(resolve_deadline).transpose()?;
    let Some(raw) = args.in_after.as_deref() else {
        return Ok(AddTiming {
            at: args.at.clone(),
            days: args.days.clone(),
            once: args.once,
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
        days: Some(weekday_name(target.weekday()).to_owned()),
        once: true,
        deadline,
    })
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

fn weekday_name(day: jiff::civil::Weekday) -> &'static str {
    match day {
        jiff::civil::Weekday::Monday => "mon",
        jiff::civil::Weekday::Tuesday => "tue",
        jiff::civil::Weekday::Wednesday => "wed",
        jiff::civil::Weekday::Thursday => "thu",
        jiff::civil::Weekday::Friday => "fri",
        jiff::civil::Weekday::Saturday => "sat",
        jiff::civil::Weekday::Sunday => "sun",
    }
}
