//! `rimz wait` — self waits, room wake-ups, and scoped cancellation over the loop scheduler.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use rimz::config::CheckOn;
use rimz::harness::ancestry::CallerIdentity;
use rimz::harness::schedule::arm::TaskName;
use rimz::harness::schedule::wait::{self, WaitRequest, WaitRoom};
use rimz::store::snapshot::SidebarSnapshot;

use super::{Ctx, GlobalFlags};

mod add;
mod cancel;
mod list;
mod watch;

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Only an agent arms or cancels a wait, and the wake comes back to that agent. \
With no flag but `--json`, `rimz wait` lists your waits."
)]
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
        /// Print the rows as JSON, subscriptions included.
        #[arg(long)]
        json: bool,
    },
    /// Cancel one wait or subscription by name, or all your waits; stops a watched command.
    #[command(group(clap::ArgGroup::new("target").required(true).args(["name", "all"])))]
    Cancel {
        /// The wait or subscription name, as `rimz wait list` prints it.
        name: Option<TaskName>,
        /// Cancel only waits you armed with `rimz wait`, leaving subscriptions intact.
        #[arg(long)]
        all: bool,
        /// Print what was canceled and what remains as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run a watched command and emit its outcome.
    #[command(hide = true)]
    Watch { name: String },
}

#[derive(Debug, Default, Args)]
struct WaitArgs {
    /// Short description: one line of 1 to 60 characters after trimming.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true, value_parser = super::parse_task_label)]
    label: Option<String>,
    /// Wake once after this delay, rounded up to the next minute.
    #[arg(long = "in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    in_after: Option<Duration>,
    /// Wake once at this time: `HH:MM` (today, else tomorrow), `<weekday> HH:MM`, or
    /// `YYYY-MM-DD HH:MM`.
    #[arg(long, value_name = "TIME")]
    at: Option<String>,
    /// Wake once a state expression such as `pr=merged` reads true; repeated clauses are ANDed.
    #[arg(long, value_name = "EXPR")]
    when: Vec<String>,
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
    /// Wait for this file to change: created, deleted, written, or replaced.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = clap::builder::TypedValueParser::map(
            clap::builder::OsStringValueParser::new(),
            std::path::PathBuf::from,
        )
    )]
    file: Option<std::path::PathBuf>,
    /// With --file, wait for a new line containing this literal text.
    #[arg(long, value_name = "PATTERN")]
    grep: Option<String>,
    /// Deliver for a failed, successful, or any command outcome (default: any; --check: success).
    #[arg(long, value_name = "fail|success|any", value_parser = ["fail", "success", "any"])]
    on: Option<String>,
    /// Send one still-running message after this long; never stops the watch. Default 30m, or
    /// none while cache keepalive pings the agent.
    #[arg(long = "check-in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    check_in: Option<Duration>,
    #[arg(long, hide = true, value_name = "DURATION")]
    timeout: Option<String>,
    /// Print the receipt as JSON, or the list when no other flag is given.
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
            && self.at.is_none()
            && self.when.is_empty()
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

fn room<'a>(ctx: &'a Ctx, snapshot: &'a SidebarSnapshot) -> WaitRoom<'a> {
    WaitRoom {
        workspace: &ctx.workspace,
        store: &ctx.store,
        snapshot,
        sidebar_fresh: rimz::sidebar::fresh_sidebar_present,
    }
}

impl WaitArgs {
    fn into_request(self) -> WaitRequest {
        WaitRequest {
            label: self.label,
            in_after: self.in_after,
            at: self.at,
            when: self.when,
            pid: self.pid,
            check: self.check,
            run: self.run,
            every: self.every,
            file: self.file,
            grep: self.grep,
            // clap restricts --on to these three values.
            on: self.on.as_deref().map(|on| match on {
                "fail" => CheckOn::Fail,
                "success" => CheckOn::Success,
                _ => CheckOn::Any,
            }),
            check_in: self.check_in,
        }
    }
}
