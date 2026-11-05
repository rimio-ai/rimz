//! `rimz wait` — self waits, room wake-ups, and scoped cancellation over the loop scheduler.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};

use rimz::config::{CheckOn, TaskTarget, WatchSpec};
use rimz::harness::ancestry::CallerIdentity;
use rimz::harness::schedule::arm::TaskName;
use rimz::store::snapshot::SidebarSnapshot;

use super::{Ctx, GlobalFlags};

mod add;
mod cancel;
mod list;
mod watch;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct WaitCommand {
    #[command(subcommand)]
    command: Option<WaitSubcmd>,
    #[command(flatten)]
    wait: WaitArgs,
}

#[derive(Debug, Subcommand)]
enum WaitSubcmd {
    /// List your own waits, or the whole room from a user shell.
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Cancel any of your waits or subscriptions by name, including its watched command.
    Cancel {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        name: Option<TaskName>,
        /// Cancel only waits you armed with `rimz wait`, leaving subscriptions intact.
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
struct WaitArgs {
    /// Short description, one line of 1–60 characters after trimming.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true, value_parser = super::parse_task_label)]
    label: Option<String>,
    /// Wait out this duration once (less than 24h).
    #[arg(long = "in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    in_after: Option<Duration>,
    /// Wait for this existing process to disappear; its exit status is not available.
    #[arg(long, value_name = "PID", value_parser = clap::value_parser!(u32).range(1..=i32::MAX as i64))]
    pid: Option<u32>,
    /// Run this shell command repeatedly until it succeeds (or fails, with --on fail).
    #[arg(long, value_name = "COMMAND")]
    check: Option<String>,
    /// Run this shell command once through sh -c and wake with its outcome.
    #[arg(long, value_name = "COMMAND")]
    run: Option<String>,
    /// Sleep this long between --check runs (default: 1s).
    #[arg(long, value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    every: Option<Duration>,
    /// Wait for this file to change: created, deleted, or written.
    #[arg(long, value_name = "PATH")]
    file: Option<std::path::PathBuf>,
    /// With --file, wait for a new line containing this literal text.
    #[arg(long, value_name = "PATTERN")]
    grep: Option<String>,
    /// Deliver for a failed, successful, or any command outcome (default: any; --check: success).
    #[arg(long, value_name = "fail|success|any", value_parser = ["fail", "success", "any"])]
    on: Option<String>,
    /// Send one still-running notice; default 30m only without cache keepalive. Never stop the command.
    #[arg(long = "check-in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    check_in: Option<Duration>,
    #[arg(long, hide = true, value_name = "DURATION")]
    timeout: Option<String>,
    #[arg(long)]
    json: bool,
}

pub fn run(args: WaitCommand, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(WaitSubcmd::List { json }) => list::run(json, globals),
        Some(WaitSubcmd::Cancel { name, all, json }) => cancel::run(name, all, json, globals),
        Some(WaitSubcmd::Watch { name }) => watch::run(&name, globals),
        None if args.wait.is_empty() => list::run(args.wait.json, globals),
        None => add::run(args.wait, globals),
    }
}

impl WaitArgs {
    fn is_empty(&self) -> bool {
        self.in_after.is_none()
            && self.pid.is_none()
            && self.check.is_none()
            && self.run.is_none()
            && self.every.is_none()
            && self.file.is_none()
            && self.grep.is_none()
            && self.on.is_none()
            && self.check_in.is_none()
            && self.timeout.is_none()
            && self.label.is_none()
    }
}

fn caller(ctx: &Ctx) -> Result<Option<CallerIdentity>> {
    super::send::resolve_caller(&ctx.store)
}

fn caller_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    caller: Option<&CallerIdentity>,
    action: Option<&str>,
) -> Result<Option<&'a rimz::agents::AgentState>> {
    let Some(caller) = caller else {
        return Ok(None);
    };
    let agent =
        rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller).map_err(|_| {
            let identity = match (&caller.launch_id, &caller.pane_id) {
                (Some(launch_id), _) => format!("launch id `{launch_id}`"),
                (None, Some(pane_id)) => format!("pane `{pane_id}`"),
                (None, None) => "no launch id or pane".into(),
            };
            let unchanged = action
                .map(|action| format!("nothing was {action}: "))
                .unwrap_or_default();
            anyhow!(
                "{unchanged}RimZ could not find the calling agent's live session from this \
                 process (looked for kind `{}`, {identity}). If this ran inside a provider's \
                 command sandbox: {SANDBOX_REMEDY}",
                caller.kind
            )
        })?;
    if agent.ended_at.is_some() {
        bail!("RimZ identified the calling agent but its live session is unavailable");
    }
    Ok(Some(agent))
}

fn parse_on(raw: Option<&str>) -> CheckOn {
    match raw {
        Some("fail") => CheckOn::Fail,
        Some("success") => CheckOn::Success,
        Some("any") | None => CheckOn::Any,
        Some(_) => unreachable!("clap restricts --on values"),
    }
}

const SANDBOX_REMEDY: &str = "Run `rimz wait …` as its own command, without a chain, \
    pipe, redirect, or `VAR=` prefix, so its routine rule runs outside the sandbox; if \
    `allow-routine-rimz` is off, approve the sandbox's escalation. If Codex's routine rules \
    predate this RimZ, refresh them with `rimz hooks install codex` from a user shell and \
    restart the agent";

fn home_write_hint(error: anyhow::Error, action: &str) -> anyhow::Error {
    if !error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ReadOnlyFilesystem)
    }) {
        return error;
    }
    error.context(format!(
        "nothing was {action}: this command cannot write the RimZ home, for example inside a \
         provider's command sandbox. {SANDBOX_REMEDY}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_write_hint_only_for_read_only_filesystem() {
        for action in ["armed", "canceled"] {
            for kind in [
                std::io::ErrorKind::ReadOnlyFilesystem,
                std::io::ErrorKind::PermissionDenied,
            ] {
                let error = anyhow::Error::from(rimz::harness::schedule::arm::ArmFailure::State(
                    Box::new(rimz::disk::lock::LockErr::Open {
                        path: "rimz-home/loops/loop-arming.lock".into(),
                        source: std::io::Error::from(kind),
                    }),
                ))
                .context("updating wait");
                let original = format!("{error:#}");
                let report = format!("{:#}", home_write_hint(error, action));
                if kind != std::io::ErrorKind::ReadOnlyFilesystem {
                    assert_eq!(report, original);
                    continue;
                }
                assert!(
                    report.contains(&format!("nothing was {action}")),
                    "{report}"
                );
                assert!(report.contains(&original), "{report}");
                for text in [
                    "provider's command sandbox",
                    "own command",
                    "chain, pipe, redirect, or `VAR=` prefix",
                    "allow-routine-rimz",
                    "escalation",
                    "`rimz hooks install codex`",
                ] {
                    assert!(report.contains(text), "{report}");
                }
            }
        }
    }
}
