use std::io::Write;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use rimz::harness::schedule::arm::{
    ArmOutcome, DeliveryName, DeliveryPrompt, DeliveryProvenance, DeliverySpec, DeliveryTrigger,
    arm_delivery, duration_label,
};

use super::*;

#[derive(Serialize)]
struct WakeReceipt<'a> {
    name: &'a str,
    trigger: &'a str,
    target: &'a str,
    pending: Vec<list::WakeRow>,
}

pub(super) fn run(args: WaitArgs, globals: &GlobalFlags) -> Result<()> {
    validate_shape(&args)?;
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = caller_agent(&snapshot, caller.as_ref())?
        .context("arming a wait is only available to an agent RimZ can identify; run this command from an agent pane")?;
    if agent.agent_id.is_provisional() {
        bail!("the calling agent has not registered a real session yet");
    }
    let target = TaskTarget {
        kind: agent.kind.clone(),
        session: agent.agent_id.clone(),
        handle: rimz::address::agent_handle(
            agent,
            &rimz::address::addressable_agents(&snapshot),
            true,
        ),
    };
    let (trigger, description) = if let Some(delay) = args.in_after {
        (
            DeliveryTrigger::Delay(delay),
            format!("in {}", duration_label(delay)),
        )
    } else if let Some(pid) = args.pid {
        let process = nix::unistd::Pid::from_raw(i32::try_from(pid)?);
        if nix::sys::signal::kill(process, None) == Err(nix::errno::Errno::EPERM) {
            bail!("cannot watch PID {pid}: permission denied; choose a process owned by your user");
        }
        watch_trigger(WatchSpec::Pid { pid }, CheckOn::Any, &args)
    } else if let Some(check) = args.check.clone() {
        let spec = WatchSpec::Check {
            check,
            every: duration_label(args.every.unwrap_or(Duration::from_secs(1))),
            on: match parse_on(args.on.as_deref()) {
                CheckOn::Fail => CheckOn::Fail,
                CheckOn::Success | CheckOn::Any => CheckOn::Success,
            },
        };
        watch_trigger(spec, CheckOn::Any, &args)
    } else {
        let command = command_string(&args.command)?;
        watch_trigger(
            WatchSpec::Command(command),
            parse_on(args.on.as_deref()),
            &args,
        )
    };
    let ArmOutcome::Armed { name, .. } = arm_delivery(
        &ctx.workspace,
        DeliverySpec {
            name: DeliveryName::MintWait,
            target: target.clone(),
            trigger,
            prompt: DeliveryPrompt::None,
            provenance: DeliveryProvenance::SelfWait,
            check: None,
            deadline: None,
            max_strikes: None,
            surplus: None,
        },
    )?
    else {
        unreachable!("timer, process, and command waits are not subscriptions")
    };
    let pending = list::pending_rows(&ctx)?;
    if args.json {
        return super::super::render::json(&WakeReceipt {
            name: &name,
            trigger: &description,
            target: &target.handle,
            pending,
        });
    }
    let mut out = super::super::render::out();
    writeln!(out, "armed {name}: {description} → {}", target.handle)?;
    list::write_rows(&mut out, pending)
}

fn watch_trigger(spec: WatchSpec, on: CheckOn, args: &WaitArgs) -> (DeliveryTrigger, String) {
    let description = spec.describe();
    (
        DeliveryTrigger::Watch {
            spec,
            on,
            timeout: args.timeout.unwrap_or(Duration::from_secs(30 * 60)),
        },
        description,
    )
}

fn validate_shape(args: &WaitArgs) -> Result<()> {
    if usize::from(args.in_after.is_some())
        + usize::from(args.pid.is_some())
        + usize::from(args.check.is_some())
        + usize::from(!args.command.is_empty())
        != 1
    {
        bail!("choose exactly one wait trigger: --in, --pid, --check, or a command after --");
    }
    if args
        .check
        .as_deref()
        .is_some_and(|check| check.trim().is_empty())
    {
        bail!("--check needs a command");
    }
    if args.on.is_some() && args.command.is_empty() && args.check.is_none() {
        bail!("--on requires --check or a command after --");
    }
    if args.check.is_some() && args.on.as_deref() == Some("any") {
        bail!(
            "--on any has no meaning with --check; it polls until the command succeeds (default) or fails"
        );
    }
    if args.every.is_some() && args.check.is_none() {
        bail!("--every requires --check");
    }
    if args.timeout.is_some() && args.in_after.is_some() {
        bail!("--timeout requires --pid, --check, or a command after --");
    }
    for (name, duration) in [
        ("--in", args.in_after),
        ("--timeout", args.timeout),
        ("--every", args.every),
    ] {
        if duration.is_some_and(|duration| duration.is_zero()) {
            bail!("{name} must be greater than zero");
        }
        if duration.is_some_and(|duration| duration >= Duration::from_secs(24 * 60 * 60)) {
            bail!("{name} must be less than 24h");
        }
    }
    Ok(())
}
