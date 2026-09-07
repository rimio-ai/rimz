//! `rimz wake` — self-only timer and command waits over the loop scheduler.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use rimz::config::{CheckOn, TaskTarget};
use rimz::harness::ancestry::CallerIdentity;
use rimz::harness::schedule::arm::TaskName;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::snapshot::SidebarSnapshot;

use super::{Ctx, GlobalFlags};

mod add;
mod cancel;
mod list;
mod watch;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct WakeCommand {
    #[command(subcommand)]
    command: Option<WakeSubcmd>,
    #[command(flatten)]
    wake: WakeArgs,
}

#[derive(Debug, Subcommand)]
enum WakeSubcmd {
    /// List pending wakes and subscriptions aimed at you.
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Cancel your pending wake or subscription, including its watched command.
    Cancel {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        name: Option<TaskName>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run a watched command and emit its outcome.
    #[command(hide = true)]
    Watch { name: String },
}

#[derive(Debug, Default, Args)]
struct WakeArgs {
    /// Wake yourself once after this duration (less than 24h).
    #[arg(long = "in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    in_after: Option<Duration>,
    /// Deliver for a failed, successful, or any command outcome (default: any).
    #[arg(long, value_name = "fail|success|any", value_parser = ["fail", "success", "any"])]
    on: Option<String>,
    /// Send one still-running notice after this duration; do not stop the command (default: 30m).
    #[arg(long, value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    timeout: Option<Duration>,
    #[arg(long)]
    json: bool,
    /// Command to watch.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

pub fn run(args: WakeCommand, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(WakeSubcmd::List { json }) => list::run(json, globals),
        Some(WakeSubcmd::Cancel { name, all, json }) => cancel::run(name, all, json, globals),
        Some(WakeSubcmd::Watch { name }) => watch::run(&name, globals),
        None if args.wake.is_empty() => list::run(args.wake.json, globals),
        None => add::run(args.wake, globals),
    }
}

impl WakeArgs {
    fn is_empty(&self) -> bool {
        self.in_after.is_none()
            && self.on.is_none()
            && self.timeout.is_none()
            && self.command.is_empty()
    }
}

fn caller(ctx: &Ctx) -> Result<Option<CallerIdentity>> {
    super::send::resolve_caller(&ctx.store)
}

fn caller_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    caller: Option<&CallerIdentity>,
) -> Result<Option<&'a rimz::agents::AgentState>> {
    let Some(caller) = caller else {
        return Ok(None);
    };
    let agent = rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller)?;
    if agent.ended_at.is_some() {
        bail!("RimZ identified the calling agent but its live session is unavailable");
    }
    Ok(Some(agent))
}

fn caller_session(ctx: &Ctx) -> Result<Option<(AgentKind, AgentSessionId)>> {
    let caller = caller(ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    Ok(caller_agent(&snapshot, caller.as_ref())?
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone())))
}

fn parse_on(raw: Option<&str>) -> CheckOn {
    match raw {
        Some("fail") => CheckOn::Fail,
        Some("success") => CheckOn::Success,
        Some("any") | None => CheckOn::Any,
        Some(_) => unreachable!("clap restricts --on values"),
    }
}

fn command_string(argv: &[String]) -> Result<String> {
    argv.iter()
        .map(|arg| shlex::try_quote(arg).map(|arg| arg.into_owned()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(|argv| argv.join(" "))
        .context("quoting watched command")
}
