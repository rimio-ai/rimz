//! `rimz shell` and `rimz watch`: an agent's own waits over the loop scheduler, each with its
//! own `list` and `cancel`.

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Subcommand};

use rimz::config::CheckOn;
use rimz::harness::ancestry::CallerIdentity;
use rimz::harness::schedule::arm::TaskName;
use rimz::harness::schedule::wait::{self, WaitKind, WaitRequest, WaitRoom, WaitTrigger};
use rimz::store::snapshot::SidebarSnapshot;

use super::{Ctx, GlobalFlags};

mod add;
mod cancel;
mod list;
mod watcher;

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Only an agent starts or cancels a shell, and its outcome comes back to that \
agent. With no command, `rimz shell` lists your shells."
)]
pub struct ShellCommand {
    #[command(subcommand)]
    command: Option<ShellSubcmd>,
    #[command(flatten)]
    shell: ShellArgs,
}

#[derive(Debug, Subcommand)]
enum ShellSubcmd {
    /// List your own shells, or the whole room's from a user shell.
    #[command(alias = "ls")]
    List(ListArgs),
    /// Cancel one shell by name, or all your shells; stops its command.
    Cancel(CancelArgs),
    /// Run a shell's command and emit its outcome.
    #[command(hide = true)]
    Watcher { name: String },
}

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    after_help = "Only an agent arms or cancels a watch, and the wake comes back to that agent. \
With no expression, `rimz watch` lists your watches."
)]
pub struct WatchCommand {
    #[command(subcommand)]
    command: Option<WatchSubcmd>,
    #[command(flatten)]
    watch: WatchArgs,
}

#[derive(Debug, Subcommand)]
enum WatchSubcmd {
    /// List your own watches, or the whole room's from a user shell.
    #[command(alias = "ls")]
    List(ListArgs),
    /// Cancel one watch by name, or all your watches.
    Cancel(CancelArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Print the rows as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("target").required(true).args(["name", "all"])))]
struct CancelArgs {
    /// The name, as `list` prints it.
    name: Option<TaskName>,
    /// Cancel every one you armed.
    #[arg(long)]
    all: bool,
    /// Print what was canceled and what remains as JSON.
    #[arg(long)]
    json: bool,
}

/// The options every wait takes but `--check-in`, whose help each verb words for itself.
#[derive(Debug, Default, Args)]
struct WaitOptions {
    /// Short description: one line of 1 to 60 characters after trimming.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true, value_parser = super::parse_task_label)]
    label: Option<String>,
    /// Print the receipt as JSON, or the list when given nothing to wait on.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Default, Args)]
struct ShellArgs {
    /// Deliver for a failed, successful, or any outcome (default: any).
    #[arg(long, value_name = "fail|success|any", value_parser = ["fail", "success", "any"])]
    on: Option<String>,
    #[command(flatten)]
    options: WaitOptions,
    /// Send one still-running message after this long; never stops the command. Default 30m,
    /// or none while cache keepalive pings the agent.
    #[arg(long = "check-in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    check_in: Option<Duration>,
    /// Kill the command, every process of it, once this long has passed; its outcome reads timed
    /// out.
    #[arg(long, value_name = "DURATION", value_parser = rimz::harness::block::timeout)]
    timeout: Option<Duration>,
    /// What the command reads as `input` from the `context.json` that `RIMZ_WORKFLOW_DIR`
    /// names: a JSON object, inline (`{...}`) or a file path.
    #[arg(long, value_name = "JSON|PATH")]
    input: Option<String>,
    /// The command, run once through `sh -c` at the checkout's root: one word as written,
    /// several as that argv, each quoted for sh.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

#[derive(Debug, Default, Args)]
struct WatchArgs {
    #[command(flatten)]
    options: WaitOptions,
    /// Send one still-not-met message after this long; never stops the watch. Default 30m, or
    /// none while cache keepalive pings the agent.
    #[arg(long = "check-in", value_name = "DURATION", value_parser = super::supervised::parse_timeout)]
    check_in: Option<Duration>,
    /// Stop watching once this long has passed with the condition unmet; one wake names the
    /// timeout.
    #[arg(long, value_name = "DURATION", value_parser = rimz::harness::block::timeout)]
    timeout: Option<Duration>,
    /// The condition to wake on, such as `pr=merged` or `ci=passed,failed`.
    #[arg(value_name = "EXPR")]
    when: Option<String>,
}

pub fn run_shell(args: ShellCommand, globals: &GlobalFlags) -> Result<()> {
    let kind = WaitKind::Shell;
    match args.command {
        Some(ShellSubcmd::List(list)) => list::run(kind, list.json, globals),
        Some(ShellSubcmd::Cancel(cancel)) => cancel::run(kind, cancel, globals),
        Some(ShellSubcmd::Watcher { name }) => watcher::run(&name, globals),
        None if args.shell.command.is_empty() && args.shell.is_bare() => {
            list::run(kind, args.shell.options.json, globals)
        }
        None => {
            let ShellArgs {
                on,
                options,
                check_in,
                timeout,
                input,
                command,
            } = args.shell;
            // clap restricts --on to these three values.
            let on = match on.as_deref() {
                Some("fail") => CheckOn::Fail,
                Some("success") => CheckOn::Success,
                _ => CheckOn::Any,
            };
            let command = match command.as_slice() {
                [word] => word.clone(),
                // Process arguments cannot carry the NUL byte, the only input shlex refuses.
                words => shlex::try_join(words.iter().map(String::as_str))
                    .expect("argv words carry no NUL byte"),
            };
            let input = input
                .map(|raw| super::json_object_arg("--input", &raw))
                .transpose()?;
            let trigger = WaitTrigger::Shell { command, on, input };
            add::run(
                options.request(trigger, check_in, timeout),
                options.json,
                globals,
            )
        }
    }
}

pub fn run_watch(args: WatchCommand, globals: &GlobalFlags) -> Result<()> {
    let kind = WaitKind::Watch;
    match args.command {
        Some(WatchSubcmd::List(list)) => list::run(kind, list.json, globals),
        Some(WatchSubcmd::Cancel(cancel)) => cancel::run(kind, cancel, globals),
        None => match args.watch.when {
            None if args.watch.options.is_bare()
                && args.watch.check_in.is_none()
                && args.watch.timeout.is_none() =>
            {
                list::run(kind, args.watch.options.json, globals)
            }
            when => {
                let trigger = WaitTrigger::Watch {
                    when: when.unwrap_or_default(),
                };
                add::run(
                    args.watch
                        .options
                        .request(trigger, args.watch.check_in, args.watch.timeout),
                    args.watch.options.json,
                    globals,
                )
            }
        },
    }
}

impl WaitOptions {
    /// Whether no option but `--json` is set.
    fn is_bare(&self) -> bool {
        self.label.is_none()
    }

    fn request(
        &self,
        trigger: WaitTrigger,
        check_in: Option<Duration>,
        timeout: Option<Duration>,
    ) -> WaitRequest {
        WaitRequest {
            label: self.label.clone(),
            trigger,
            check_in,
            timeout,
        }
    }
}

impl ShellArgs {
    /// Whether no option but `--json` is set.
    fn is_bare(&self) -> bool {
        self.on.is_none()
            && self.check_in.is_none()
            && self.timeout.is_none()
            && self.input.is_none()
            && self.options.is_bare()
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
