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

pub(super) fn run(args: WakeArgs, globals: &GlobalFlags) -> Result<()> {
    validate_shape(&args)?;
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    let agent = caller_agent(&snapshot, caller.as_ref())?
        .context("arming a wake is only available to an agent RimZ can identify; run this command from an agent pane")?;
    if agent.agent_id.is_provisional() {
        bail!("the calling agent has not registered a real session yet");
    }
    let target = TaskTarget {
        kind: agent.kind.clone(),
        session: agent.agent_id.clone(),
        handle: rimz::harness::target::agent_handle(
            agent,
            &rimz::harness::target::addressable_agents(&snapshot),
            true,
        ),
    };
    let (trigger, description) = if let Some(delay) = args.in_after {
        (
            DeliveryTrigger::Delay(delay),
            format!("in {}", duration_label(delay)),
        )
    } else {
        let command = if let Some(pid) = args.pid {
            let process = nix::unistd::Pid::from_raw(i32::try_from(pid)?);
            if nix::sys::signal::kill(process, None) == Err(nix::errno::Errno::EPERM) {
                bail!(
                    "cannot watch PID {pid}: permission denied; choose a process owned by your user"
                );
            }
            format!("while kill -0 {pid} 2>/dev/null; do sleep 1; done")
        } else {
            command_string(&args.command)?
        };
        let description = format!("watch: {}", rimz::theme::fmt::command_preview(&command));
        (
            DeliveryTrigger::Watch {
                command,
                on: parse_on(args.on.as_deref()),
                timeout: args.timeout.unwrap_or(Duration::from_secs(30 * 60)),
            },
            description,
        )
    };
    let ArmOutcome::Armed { name, .. } = arm_delivery(
        &ctx.workspace,
        DeliverySpec {
            name: DeliveryName::MintWake,
            target: target.clone(),
            trigger,
            prompt: DeliveryPrompt::None,
            provenance: DeliveryProvenance::SelfWake,
            check: None,
            deadline: None,
            max_strikes: None,
            surplus: None,
        },
    )?
    else {
        unreachable!("timer, process, and command wakes are not subscriptions")
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

fn validate_shape(args: &WakeArgs) -> Result<()> {
    if usize::from(args.in_after.is_some())
        + usize::from(args.pid.is_some())
        + usize::from(!args.command.is_empty())
        != 1
    {
        bail!("choose exactly one wake trigger: --in, --pid, or a command after --");
    }
    if args.on.is_some() && args.command.is_empty() {
        bail!("--on requires a command after --");
    }
    if args.timeout.is_some() && args.command.is_empty() && args.pid.is_none() {
        bail!("--timeout requires --pid or a command after --");
    }
    for (name, duration) in [("--in", args.in_after), ("--timeout", args.timeout)] {
        if duration.is_some_and(|duration| duration.is_zero()) {
            bail!("{name} must be greater than zero");
        }
        if duration.is_some_and(|duration| duration >= Duration::from_secs(24 * 60 * 60)) {
            bail!("{name} must be less than 24h");
        }
    }
    Ok(())
}
