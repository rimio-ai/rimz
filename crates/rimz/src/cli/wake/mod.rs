//! `rimz wake` — caller-pinned wakeups and signal subscriptions for agents.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use rimz::config::{CheckOn, TaskTarget};
use rimz::harness::ancestry::CallerIdentity;
use rimz::ids::AgentSessionId;
use rimz::store::snapshot::SidebarSnapshot;

use super::{Ctx, GlobalFlags};

mod add;
mod cancel;
mod list;
mod wait;
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
    /// List pending wakeups.
    #[command(alias = "ls")]
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a pending wakeup.
    Cancel {
        /// Wake name.
        name: String,
    },
    /// Run a watched command and emit its completion signal.
    #[command(hide = true)]
    Watch {
        /// Wake name.
        name: String,
    },
}

#[derive(Debug, Default, Args)]
struct WakeArgs {
    /// Agent to wake. Omit from an agent pane to wake yourself.
    #[arg(value_name = "@TARGET")]
    target: Option<String>,
    /// Message delivered with the wake evidence.
    #[arg(long, conflicts_with = "prompt_file")]
    prompt: Option<String>,
    /// File whose contents become the delivered message.
    #[arg(long = "prompt-file", value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// Wake once after this duration.
    #[arg(long = "in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    in_after: Option<Duration>,
    /// Listen for this signal or family selector (for example, ci.failed or ci.*).
    #[arg(long, value_name = "NAME")]
    signal: Option<String>,
    /// Require a top-level signal payload field to equal this value.
    #[arg(long = "match", value_name = "KEY=VALUE", requires = "signal")]
    matches: Vec<String>,
    /// Deliver for a failed, successful, or any command outcome (default: any).
    #[arg(long, value_name = "fail|success|any", value_parser = ["fail", "success", "any"])]
    on: Option<String>,
    /// Signal quiet window or command watch timeout (default: 59m).
    #[arg(long, value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    timeout: Option<Duration>,
    /// Wait inline for the command outcome; use `--wait=5m` for a deadline.
    #[arg(
        long,
        value_name = "DURATION",
        num_args = 0..=1,
        require_equals = true,
        value_parser = super::supervised::parse_timeout
    )]
    wait: Option<Option<Duration>>,
    /// Emit JSON.
    #[arg(long)]
    json: bool,
    /// Command to watch.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

pub fn run(args: WakeCommand, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(WakeSubcmd::List { json }) => list::run(json, globals),
        Some(WakeSubcmd::Cancel { name }) => cancel::run(&name, globals),
        Some(WakeSubcmd::Watch { name }) => watch::run(&name, globals),
        None if args.wake.is_empty() => list::run(args.wake.json, globals),
        None => add::run(args.wake, globals),
    }
}

impl WakeArgs {
    fn is_empty(&self) -> bool {
        self.target.is_none()
            && self.prompt.is_none()
            && self.prompt_file.is_none()
            && self.in_after.is_none()
            && self.signal.is_none()
            && self.matches.is_empty()
            && self.on.is_none()
            && self.timeout.is_none()
            && self.wait.is_none()
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

fn caller_session(ctx: &Ctx) -> Result<Option<AgentSessionId>> {
    let caller = caller(ctx)?;
    let snapshot = ctx.resolution_snapshot()?;
    Ok(caller_agent(&snapshot, caller.as_ref())?.map(|agent| agent.agent_id.clone()))
}

fn delivery_target(
    ctx: &Ctx,
    caller: Option<&CallerIdentity>,
    address: Option<&str>,
) -> Result<TaskTarget> {
    let snapshot = ctx.resolution_snapshot()?;
    if let Some(address) = address {
        if !address.starts_with('@') {
            bail!("wake target must start with `@`");
        }
        let agent = super::resolve_agent_one(&snapshot, address, None, ctx.channel())
            .map_err(|_| anyhow::anyhow!("no live agent matches `{address}`"))?;
        if agent.agent_id.is_provisional() {
            bail!("`{address}` has not registered a real session yet");
        }
        let peers = rimz::harness::target::addressable_agents(&snapshot);
        return Ok(TaskTarget {
            kind: agent.kind.as_str().to_owned(),
            session: agent.agent_id.as_str().to_owned(),
            handle: rimz::harness::target::agent_handle(agent, &peers, true),
        });
    }

    let agent = caller_agent(&snapshot, caller)?.ok_or_else(|| {
        anyhow::anyhow!(
            "arming a wake without an explicit @target is only available to an agent RimZ can identify; from a user shell, pass the live agent address"
        )
    })?;
    if agent.agent_id.is_provisional() {
        bail!("the calling agent has not registered a real session yet");
    }
    let handle = agent
        .name
        .as_deref()
        .map(|name| format!("@{name}"))
        .unwrap_or_else(|| format!("@{}", agent.kind));
    Ok(TaskTarget {
        kind: agent.kind.as_str().to_owned(),
        session: agent.agent_id.as_str().to_owned(),
        handle,
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

fn self_wake_guard(
    signal: Option<&str>,
    matches: &BTreeMap<String, String>,
    target: &TaskTarget,
) -> Result<()> {
    if !signal.is_some_and(|name| name.starts_with("agent.")) {
        return Ok(());
    }
    fn unqualified_handle(handle: &str) -> &str {
        handle.split_once('#').map_or(handle, |(name, _)| name)
    }
    let names_other_agent = matches
        .get("handle")
        .is_some_and(|handle| unqualified_handle(handle) != unqualified_handle(&target.handle))
        || matches
            .get("session")
            .is_some_and(|session| session != &target.session);
    if names_other_agent {
        return Ok(());
    }
    bail!(
        "wake on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"
    )
}
