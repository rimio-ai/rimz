//! Add, remove, and rename configured loop tasks.

use super::*;

struct AddTiming {
    at: Option<String>,
    fire_at: Option<Timestamp>,
    deadline: Option<Timestamp>,
}

// ---- add / remove -----------------------------------------------------------

pub(super) fn add(args: AddArgs) -> Result<()> {
    validate_add_args(&args)?;
    let workspace = resolve_add_workspace(&args)?;
    let project_root = workspace.project_root.clone();
    condition::validate(&args.when, &project_root)?;
    let catalog = TaskCatalog::load(Some(&project_root))?;
    let name = args.name.clone().unwrap_or_else(|| catalog.mint_name());
    let mut entry = build_task_entry(&args, &name, &workspace)?;
    check_launch(&args, &name, &workspace.project_root)?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    let provider = args.provider.clone();
    let login = provider
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
        provider.as_deref(),
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
                provider.as_deref(),
                login_key.as_ref(),
                &entry.resolved_root(),
                &runtime,
                Timestamp::now(),
            )
        })
        .transpose()?;
    if let Some(reset) = &after_reset {
        entry.fire_at = Some(reset.fire_at);
        entry.provider = Some(reset.kind.clone());
    }
    if entry.provider.is_some() {
        entry.account.clone_from(&args.account);
    }
    // Compile once before writing, so validation and feedback share one shape.
    let shape = schedule::TaskShape::compile(&name, &entry);
    let parsed = shape.trigger().as_ref().map_err(Clone::clone)?;
    let action = shape.action().map_err(Clone::clone)?;
    crate::cli::wake_view::refuse_unreadable_forge_task(&entry)?;
    let project_pre_state = args
        .project
        .then(|| trust::status(&project_root))
        .transpose()?
        .map(|report| report.state);
    let mutation = if args.project {
        catalog.replace_project(&name, &project_root, &entry)?
    } else {
        entry.booked_by = booking_chain(&workspace);
        catalog.replace_machine(&name, &entry)?
    };

    let mut out = ui::out();
    writeln!(out, "added loop task `{name}`")?;
    if mutation.cleared_overlays() {
        writeln!(out, "arming: reset")?;
    }
    if let Some(pre_state) = project_pre_state {
        finish_project_mutation(&mut out, &project_root, true, pre_state)?;
    }
    write_add_feedback(&mut out, &entry, action, parsed, after_reset.as_ref())?;
    write_linked_add_note(&mut out, &workspace)?;
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

/// The booking chain of an agent calling from the task's room. A shell, a caller this room's
/// rows do not resolve, and any read failure book without one: the add never fails on it.
fn booking_chain(workspace: &rimz::ResolvedWorkspace) -> Vec<rimz::agents::LaunchedBy> {
    let Ok(Some(store)) = super::super::open_existing_store(workspace) else {
        return Vec::new();
    };
    let Ok(rows) = store.runtime_projection(rimz::RuntimeScope::Audit) else {
        return Vec::new();
    };
    let Ok(booker) = rimz::harness::ancestry::resolve_calling_agent(&rows.agents) else {
        return Vec::new();
    };
    rimz::harness::ancestry::booking_chain(&rows.agents, booker)
}

/// The declared kind and its target: `--workflow`, `--agent`, `--team`, else shell.
fn launch_kind(args: &AddArgs) -> (rimz::config::LaunchKind, Option<&str>) {
    use rimz::config::LaunchKind;
    [
        (LaunchKind::Workflow, args.workflow.as_deref()),
        (LaunchKind::Agent, args.agent.as_deref()),
        (LaunchKind::Team, args.team.as_deref()),
    ]
    .into_iter()
    .find_map(|(kind, target)| target.map(|target| (kind, Some(target))))
    .unwrap_or((LaunchKind::Shell, None))
}

/// The tail checked as the fire's launch will parse it, each refusal naming the task and the
/// kind flag.
fn check_launch(args: &AddArgs, name: &str, project_root: &Path) -> Result<()> {
    let (kind, target) = launch_kind(args);
    let place = crate::cli::loop_launch::LoopLaunchPlace {
        project_root,
        each_worktree: args.each_worktree,
    };
    check_kind_flags(args, kind)
        .and_then(|()| crate::cli::loop_launch::check(kind, target, &args.args, &place))
        .map_err(|err| match target {
            Some(target) => {
                anyhow::anyhow!(
                    "loop task `{name}`: --{} `{target}`: {err:#}",
                    kind.as_str()
                )
            }
            None => anyhow::anyhow!("loop task `{name}`: {err:#}"),
        })
}

/// The add flags a launch kind refuses: a launch bounds itself.
fn check_kind_flags(args: &AddArgs, kind: rimz::config::LaunchKind) -> Result<()> {
    if kind != rimz::config::LaunchKind::Shell && args.timeout.is_some() {
        bail!(
            "--timeout bounds a shell task's command; remove it: {}",
            kind.timeout_bound()
        );
    }
    Ok(())
}

fn validate_add_args(args: &AddArgs) -> Result<()> {
    if let Some(name) = &args.name {
        rimz::harness::name::validate_name(name)?;
    }
    let (kind, _) = launch_kind(args);
    if kind == rimz::config::LaunchKind::Shell && args.args.is_empty() {
        bail!("a shell task needs its command after `--`: `rimz loop add <name> ... -- <command>`");
    }
    if args.until.is_some() {
        bail!("--until is no longer accepted; fire on a condition with --when");
    }
    let repeats_by_clock = args.every.is_some() || args.cron.is_some();
    if args.repeat {
        if repeats_by_clock {
            bail!("--repeat cannot be used with --every or --cron; they repeat already");
        }
        if args.signal.is_none() && args.when.is_empty() {
            bail!("--repeat requires --signal or --when");
        }
    }
    let repeats = repeats_by_clock || args.repeat;
    if let Some(limit) = args.overlap_limit {
        anyhow::ensure!(
            args.overlap.as_deref() == Some("queue"),
            "--overlap-limit requires --overlap queue"
        );
        anyhow::ensure!(limit > 0, "--overlap-limit must be at least 1");
    }
    // `TaskShape::compile` refuses `allow` below, with the stored row's reasons.
    if args
        .overlap
        .as_deref()
        .is_some_and(|policy| policy != "allow")
    {
        for (used, reason) in [
            (
                args.each_worktree,
                "waiting triggers are kept per task, and an each-worktree task fires per checkout",
            ),
            (!repeats, "a one-shot has no task left to drain"),
        ] {
            if used {
                bail!("--overlap cannot be used here; {reason}");
            }
        }
    }
    if args.each_worktree {
        let used = [
            (args.signal.is_some(), "--signal"),
            (args.in_after.is_some(), "--in"),
            (
                args.at.is_some() && args.every.is_none(),
                "--at without --every",
            ),
            (args.after_reset.is_some(), "--after-reset"),
        ]
        .into_iter()
        .filter_map(|(used, flag)| used.then_some(flag))
        .collect::<Vec<_>>();
        if !used.is_empty() {
            bail!(
                "--each-worktree cannot use {}; fan-out needs --when, --every, or --cron",
                used.join(", ")
            );
        }
        if !repeats {
            bail!(
                "--each-worktree with --when needs --repeat; a one-shot would remove the task \
                 after its first checkout"
            );
        }
    }
    let project_error = args.project.then(|| {
        [
            (
                args.after_reset.is_some(),
                "--project tasks cannot use --after-reset; its reset instant is machine state",
            ),
            (
                !args.when.is_empty(),
                "--project tasks cannot use --when yet; add it without --project",
            ),
            (
                args.each_worktree,
                "--project tasks cannot use --each-worktree; per-worktree fan-out is machine state",
            ),
            (
                !repeats_by_clock && !(args.signal.is_some() && args.repeat),
                "--project tasks must repeat; set --every, --cron, or --signal with --repeat",
            ),
        ]
        .into_iter()
        .find_map(|(invalid, message)| invalid.then_some(message))
    });
    if let Some(message) = project_error.flatten() {
        bail!(message);
    }
    parse_matches(&args.matches)?;
    if !args.matches.is_empty() && args.signal.is_none() {
        bail!("--match requires --signal");
    }
    if let Some(kind) = &args.provider
        && rimz::agents::find_definition(kind).is_none()
    {
        bail!("--provider `{kind}` is not an agent kind; see `rimz agents profiles`");
    }
    if args.account.is_some() && args.provider.is_none() {
        bail!(
            "--account requires --provider; it picks the account whose windows a condition reads"
        );
    }
    Ok(())
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

/// The stored `timeout`: `--timeout`, else `loop.default-timeout`, else the scheduled default.
/// A command needs a deadline it can reach, so `--timeout 0s` is refused the way `--for` and
/// `--in` are.
fn task_timeout(raw: Option<&str>) -> Result<String> {
    let Some(raw) = raw else {
        let configured = MachineConfig::load_lenient().r#loop.default_timeout.clone();
        return Ok(configured.unwrap_or_else(|| SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL.to_owned()));
    };
    let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
    if duration.is_zero() {
        bail!("--timeout must be greater than zero");
    }
    Ok(raw.trim().to_owned())
}

fn build_task_entry(
    args: &AddArgs,
    name: &str,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<TaskEntry> {
    let matches = parse_matches(&args.matches)?;
    let timing = resolve_add_timing(args, name)?;
    let (kind, target) = launch_kind(args);
    let shell = kind == rimz::config::LaunchKind::Shell;
    let entry = TaskEntry {
        label: args.label.clone(),
        kind: Some(kind),
        target: target.map(str::to_owned),
        args: args.args.clone(),
        timeout: shell
            .then(|| task_timeout(args.timeout.as_deref()))
            .transpose()?,
        each_worktree: args.each_worktree,
        max_strikes: args.max_strikes,
        overlap: args.overlap.as_deref().map(|value| match value {
            "coalesce" => rimz::config::OverlapPolicy::Coalesce,
            "queue" => rimz::config::OverlapPolicy::Queue,
            "replace" => rimz::config::OverlapPolicy::Replace,
            "allow" => rimz::config::OverlapPolicy::Allow,
            _ => rimz::config::OverlapPolicy::Skip,
        }),
        overlap_limit: args.overlap_limit,
        root: workspace.project_root.clone(),
        at: timing.at,
        fire_at: timing.fire_at,
        every: args.every.clone(),
        cron: args.cron.clone(),
        signal: args.signal.clone(),
        when: (!args.when.is_empty()).then(|| args.when.clone()),
        hold: args.hold.clone(),
        matches: (!matches.is_empty()).then_some(matches),
        repeat: args.repeat,
        once_per_checkout: args.once_per_checkout.then_some(true),
        deadline: timing.deadline,
        ..TaskEntry::default()
    };
    if args.once_per_checkout && schedule::TaskShape::compile(name, &entry).is_ephemeral() {
        bail!(
            "--once-per-checkout cannot be used on a one-shot: its first fire removes the task, \
             so launch memory never acts; make it repeat with --every, --cron, or --repeat beside \
             --signal or --when"
        );
    }
    Ok(entry)
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
    rimz::harness::name::validate_name(new_name)?;
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
    let entries = arming::load()?;
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
    arming::pause(&task.binding(&args.name), task.source(), until)?;

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
    let entries = arming::load()?;
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
        arming::disable(&task.binding(&name), None)?;
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

fn resolve_add_timing(args: &AddArgs, name: &str) -> Result<AddTiming> {
    let now = Timestamp::now().to_zoned(MachineConfig::load_lenient().time_zone());
    let mut timing = AddTiming {
        at: None,
        fire_at: None,
        deadline: None,
    };
    if let Some(raw) = args.in_after.as_deref() {
        let duration = parse_task_timeout(raw).map_err(|err| anyhow::anyhow!("{err}"))?;
        if duration.is_zero() {
            bail!("--in must be greater than zero");
        }
        timing.fire_at = Some(schedule::delayed_instant(duration, now.timestamp())?);
    } else if let Some(at) = args.at.as_deref() {
        if args.every.is_none() {
            timing.fire_at = Some(schedule::resolve_once_at(name, at, &now)?);
        } else if at.trim().contains(char::is_whitespace) {
            bail!(
                "--at with --every takes HH:MM; for a weekday time use --every <days> --at HH:MM, such as --every fri --at 09:30"
            );
        } else {
            timing.at = Some(at.to_owned());
        }
    }
    Ok(timing)
}

/// Where each fire's command runs, as its receipt names it.
fn fire_place(entry: &TaskEntry) -> String {
    if entry.each_worktree {
        "each RimZ-owned worktree where the trigger holds".to_owned()
    } else {
        entry.run_dir().display().to_string()
    }
}

/// The receipt line for a task added from a linked worktree: it runs at the project root, not
/// there.
fn write_linked_add_note(out: &mut impl Write, workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    if workspace.worktree_root == workspace.project_root {
        return Ok(());
    }
    writeln!(
        out,
        "note: runs at the project root {}, not {}",
        workspace.project_root.display(),
        workspace.worktree_root.display()
    )?;
    Ok(())
}

fn write_add_feedback(
    out: &mut impl Write,
    entry: &TaskEntry,
    action: &TaskAction,
    parsed: &schedule::ParsedTrigger,
    after_reset: Option<&AfterReset>,
) -> Result<()> {
    match entry.timeout.as_deref() {
        Some(timeout) => writeln!(
            out,
            "action: runs `{}` in {} · timeout {timeout}",
            action.subject(),
            fire_place(entry),
        )?,
        None => writeln!(
            out,
            "action: launches `{}` in {}",
            action.subject(),
            fire_place(entry),
        )?,
    }
    let suffix = if parsed.once {
        "; then removed"
    } else if entry.repeat {
        "; repeats after each fire"
    } else {
        ""
    };
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

#[cfg(test)]
#[path = "add_tests.rs"]
mod tests;
