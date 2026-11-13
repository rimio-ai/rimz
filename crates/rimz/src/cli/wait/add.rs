use std::io::Write;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use jiff::Timestamp;
use rimz::config::{MachineConfig, TaskEntry};
use rimz::harness::schedule::arm::{
    ArmOutcome, DeliveryName, DeliveryPrompt, DeliveryProvenance, DeliverySpec, DeliveryTrigger,
    SubscriptionLifetime, arm_delivery, duration_label,
};
use rimz::harness::schedule::runner::{
    AfterReset, WindowRefusal, after_reset, window_condition_provider,
};
use rimz::harness::schedule::{self, ScheduleErr, Trigger};
use rimz::harness::when::WhenExpr;

use super::*;

#[derive(Serialize)]
struct WakeReceipt<'a> {
    name: &'a str,
    trigger: &'a str,
    target: &'a str,
    /// Set when an identical condition wait already existed and nothing was armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    existing: Option<bool>,
    pending: Vec<list::WakeRow>,
}

pub(super) fn run(args: WaitArgs, globals: &GlobalFlags) -> Result<()> {
    validate_shape(&args)?;
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = caller_agent(&snapshot, caller.as_ref(), Some("armed"))?
        .context("arming a wait is only available to an agent RimZ can identify; run this command from an agent pane")?;
    if agent.agent_id.is_provisional() {
        bail!("the calling agent has not registered a real session yet");
    }
    let check_in = || -> Result<Option<Duration>> {
        if args.check_in.is_some() {
            return Ok(args.check_in);
        }
        let config = rimz::config::MachineConfig::load_lenient();
        let published = ctx.published_snapshot()?;
        let ping_agent = published
            .agents
            .iter()
            .find(|row| row.kind == agent.kind && row.agent_id == agent.agent_id)
            .unwrap_or(agent);
        Ok((!(config.harness.cache_keepalive
            && config.harness.ping_lifetime(ping_agent).is_some()))
        .then_some(Duration::from_secs(30 * 60)))
    };
    let target = TaskTarget {
        kind: agent.kind.clone(),
        session: agent.agent_id.clone(),
        handle: rimz::address::agent_handle(
            agent,
            &rimz::address::addressable_agents(&snapshot),
            true,
        ),
    };
    let room_login = || -> Result<(rimz::RuntimePaths, Option<rimz::ids::LoginKey>)> {
        let runtime = rimz::RuntimePaths::for_project_root(&ctx.workspace.project_root)?;
        let login = rimz::harness::task_login::task_login(
            &rimz::store::writer::LaunchLogin::RoomDefault,
            agent.kind.as_str(),
            &runtime,
        )?
        .map(|login| login.key());
        Ok((runtime, login))
    };
    let mut window = None;
    let mut condition = None;
    let (trigger, description) = if let Some(delay) = args.in_after {
        (
            DeliveryTrigger::Delay(delay),
            format!("in {}", duration_label(delay)),
        )
    } else if let Some(at) = args.at.as_deref() {
        let now = Timestamp::now().to_zoned(MachineConfig::load_lenient().time_zone());
        let fire_at = schedule::resolve_once_at("wait", at, &now).map_err(at_refusal)?;
        one_shot_trigger(TaskEntry {
            fire_at: Some(fire_at),
            ..TaskEntry::default()
        })?
    } else if let Some(span) = args.after_reset {
        let (runtime, login) = room_login()?;
        let reset = after_reset(
            span,
            Some(agent.kind.as_str()),
            login.as_ref(),
            false,
            &ctx.workspace.worktree_root,
            &runtime,
            Timestamp::now(),
        )
        .map_err(window_refusal)?;
        let trigger = one_shot_trigger(TaskEntry {
            fire_at: Some(reset.fire_at),
            ..TaskEntry::default()
        })?;
        window = Some(reset);
        trigger
    } else if !args.when.is_empty() {
        let stages = rimz::harness::when::condition::condition_stages(&ctx.workspace.project_root)?;
        let expr = WhenExpr::parse_with_stages(&args.when, Some(&stages))
            .map_err(|error| anyhow!("--when: {error}"))?;
        let (runtime, login) = room_login()?;
        let provider = window_condition_provider(
            &args.when,
            Some(agent.kind.as_str()),
            login.as_ref(),
            &ctx.workspace.worktree_root,
            &runtime,
            Timestamp::now(),
        )
        .map_err(window_refusal)?;
        condition = Some(expr);
        one_shot_trigger(TaskEntry {
            when: Some(args.when.clone()),
            once: Some(true),
            provider,
            ..TaskEntry::default()
        })?
    } else if let Some(pid) = args.pid {
        let process = nix::unistd::Pid::from_raw(i32::try_from(pid)?);
        if nix::sys::signal::kill(process, None) == Err(nix::errno::Errno::EPERM) {
            bail!("cannot watch PID {pid}: permission denied; choose a process owned by your user");
        }
        watch_trigger(WatchSpec::Pid { pid }, CheckOn::Any, check_in()?)
    } else if let Some(check) = args.check.clone() {
        let spec = WatchSpec::Check {
            check,
            every: duration_label(args.every.unwrap_or(Duration::from_secs(1))),
            on: match parse_on(args.on.as_deref()) {
                CheckOn::Fail => CheckOn::Fail,
                CheckOn::Success | CheckOn::Any => CheckOn::Success,
            },
        };
        watch_trigger(spec, CheckOn::Any, check_in()?)
    } else if let Some(file) = &args.file {
        let file = rimz::utils::path::normalize_path_lexical(
            &std::env::current_dir()
                .context("resolving --file against the current directory")?
                .join(file),
        );
        if file.is_dir() {
            bail!("--file {} is a directory; watch a file", file.display());
        }
        let mark = rimz::config::FileMark::read(&file)
            .with_context(|| format!("reading --file {}", file.display()))?;
        let spec = WatchSpec::File {
            file,
            grep: args.grep.clone(),
            mark,
        };
        watch_trigger(spec, CheckOn::Any, check_in()?)
    } else if let Some(command) = args.run.clone() {
        watch_trigger(
            WatchSpec::Command(command),
            parse_on(args.on.as_deref()),
            check_in()?,
        )
    } else {
        unreachable!("validate_shape requires exactly one wait trigger")
    };
    let timeout = match &trigger {
        DeliveryTrigger::Watch {
            timeout: Some(timeout),
            ..
        } => format!(" (check-in {})", duration_label(*timeout)),
        _ => String::new(),
    };
    let outcome = arm_delivery(
        &ctx.workspace,
        DeliverySpec {
            name: DeliveryName::MintWait,
            label: args.label,
            target: target.clone(),
            trigger,
            prompt: DeliveryPrompt::None,
            provenance: DeliveryProvenance::SelfWait {
                reader: agent.name.clone(),
            },
            check: None,
            deadline: None,
            max_strikes: None,
            surplus: None,
        },
    )
    .map_err(|error| home_write_hint(error.into(), Some("armed")))?;
    let (name, armed) = match outcome {
        ArmOutcome::Armed { name, task } => (name, Some(task)),
        ArmOutcome::AlreadySubscribed { name } => (name, None),
    };
    let pending = list::pending_rows(&ctx, Some(agent), &snapshot)?;
    if args.json {
        return super::super::render::json(&WakeReceipt {
            name: &name,
            trigger: &description,
            target: &target.handle,
            existing: armed.is_none().then_some(true),
            pending,
        });
    }
    let mut out = super::super::render::out();
    let Some(task) = armed else {
        writeln!(
            out,
            "already waiting {name}: {description} → {}",
            target.handle
        )?;
        return list::write_rows(&mut out, &ctx, Some(agent), &snapshot, pending);
    };
    writeln!(
        out,
        "armed {name}: {description} → {}{timeout}",
        target.handle
    )?;
    if let Some(reset) = &window {
        write_window(&mut out, reset)?;
    }
    if let Some(expr) = &condition {
        write_condition_now(&mut out, task.entry(), expr)?;
    }
    list::write_rows(&mut out, &ctx, Some(agent), &snapshot, pending)
}

/// The delivery trigger and its listing text for a one-shot clock or condition row.
fn one_shot_trigger(entry: TaskEntry) -> Result<(DeliveryTrigger, String)> {
    let parsed = schedule::parse_trigger("wait", &entry)?;
    let description = parsed.describe();
    let trigger = match parsed.trigger {
        Trigger::Schedule(clock) => DeliveryTrigger::Clock(clock),
        Trigger::Condition { expr, hold } => DeliveryTrigger::Condition {
            expr,
            hold,
            lifetime: SubscriptionLifetime::Once,
        },
        // Callers build entries holding only clock or condition fields.
        Trigger::Signal { .. } | Trigger::Watch(_) => {
            unreachable!("a wait entry holds only clock or condition fields")
        }
    };
    Ok((trigger, description))
}

/// `--at` refusals worded for the flag: a wait has no name before it arms.
fn at_refusal(error: ScheduleErr) -> anyhow::Error {
    match error {
        ScheduleErr::BadOnceAt { value, .. } => anyhow!(
            "--at `{value}` is not a time; use `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`"
        ),
        ScheduleErr::PastOnceAt { value, .. } => {
            anyhow!("--at `{value}` is in the past; choose a later time")
        }
        other => other.into(),
    }
}

/// Window refusals worded for a wait: the loop wording speaks of a task's run.
fn window_refusal(error: WindowRefusal) -> anyhow::Error {
    match error {
        WindowRefusal::ManagedAccount { kind } => anyhow!(
            "{kind} chooses its account per launch, so the room has no {kind} account whose \
             windows a wait can read; --after-reset and window.* terms do not support {kind}"
        ),
        other => other.into(),
    }
}

fn write_window(out: &mut impl Write, reset: &AfterReset) -> Result<()> {
    let left = reset.left.map_or_else(
        || "usage unknown".to_owned(),
        |left| format!("{left}% left"),
    );
    let when = if reset.started {
        format!(
            "{left} · resets {}",
            reset
                .resets_at
                .to_zoned(MachineConfig::load_lenient().time_zone())
                .strftime("%Y-%m-%d %H:%M")
        )
    } else {
        format!("not started · {left} · fires on the next scheduler tick")
    };
    writeln!(
        out,
        "  window: {} {} · {when} · read from the room's current {} account",
        reset.kind, reset.span, reset.kind
    )?;
    Ok(())
}

fn write_condition_now(out: &mut impl Write, entry: &TaskEntry, expr: &WhenExpr) -> Result<()> {
    let now = super::super::wake_view::condition_now(entry, expr, None);
    let sidebar =
        if expr.reads_forge() && !super::super::wake_view::room_open(&entry.resolved_root()) {
            " · pr and ci are read by the room's sidebar, which is not running"
        } else {
            ""
        };
    writeln!(out, "  now: {now}{sidebar}")?;
    Ok(())
}

fn watch_trigger(
    spec: WatchSpec,
    on: CheckOn,
    timeout: Option<Duration>,
) -> (DeliveryTrigger, String) {
    let description = spec.describe();
    (DeliveryTrigger::Watch { spec, on, timeout }, description)
}

fn validate_shape(args: &WaitArgs) -> Result<()> {
    if args.timeout.is_some() {
        bail!(
            "`--timeout` is now `--check-in`: it sends one still-running notice and never stops the command"
        );
    }
    let clock_or_condition =
        args.at.is_some() || args.after_reset.is_some() || !args.when.is_empty();
    if usize::from(args.in_after.is_some())
        + usize::from(args.at.is_some())
        + usize::from(args.after_reset.is_some())
        + usize::from(!args.when.is_empty())
        + usize::from(args.pid.is_some())
        + usize::from(args.check.is_some())
        + usize::from(args.file.is_some())
        + usize::from(args.run.is_some())
        != 1
    {
        bail!(
            "choose exactly one wait trigger: --in, --at, --after-reset, --when, --pid, --check, \
             --file, or --run"
        );
    }
    if args
        .file
        .as_ref()
        .is_some_and(|file| file.as_os_str().is_empty())
    {
        bail!("--file needs a path");
    }
    if args.grep.is_some() && args.file.is_none() {
        bail!("--grep requires --file");
    }
    if args.grep.as_deref().is_some_and(str::is_empty) {
        bail!("--grep needs a pattern");
    }
    if args
        .check
        .as_deref()
        .is_some_and(|check| check.trim().is_empty())
    {
        bail!("--check needs a command");
    }
    if args.run.as_deref().is_some_and(|run| run.trim().is_empty()) {
        bail!("--run needs a command");
    }
    if args.on.is_some() && args.run.is_none() && args.check.is_none() {
        bail!("--on requires --check or --run");
    }
    if args.check.is_some() && args.on.as_deref() == Some("any") {
        bail!(
            "--on any has no meaning with --check; it polls until the command succeeds (default) or fails"
        );
    }
    if args.every.is_some() && args.check.is_none() {
        bail!("--every requires --check");
    }
    if args.check_in.is_some() && (args.in_after.is_some() || clock_or_condition) {
        bail!("--check-in requires --pid, --check, --file, or --run");
    }
    for (name, duration, capped) in [
        ("--in", args.in_after, false),
        ("--check-in", args.check_in, true),
        ("--every", args.every, true),
    ] {
        if duration.is_some_and(|duration| duration.is_zero()) {
            bail!("{name} must be greater than zero");
        }
        if capped && duration.is_some_and(|duration| duration >= Duration::from_secs(24 * 60 * 60))
        {
            bail!("{name} must be less than 24h");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_managed_account_refusal_names_the_wait_flags_not_a_task_run() {
        let message = window_refusal(WindowRefusal::ManagedAccount {
            kind: "qwen".into(),
        })
        .to_string();
        assert_eq!(
            message,
            "qwen chooses its account per launch, so the room has no qwen account whose windows \
             a wait can read; --after-reset and window.* terms do not support qwen"
        );
        for wrong in ["before the run", "window triggers", "task"] {
            assert!(!message.contains(wrong), "{message}");
        }
    }
}
