//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

mod commands;
mod exec;
mod launch;
mod supervised;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, RoomTarget};
use rimz::agents::AgentAdapter;
use rimz::agents_spec::{Cell, LayoutSpec};
use rimz::bridge::{self, ExpectedRunFrame, SocketGuard};
use rimz::feed::AgentState;
use rimz::ids::{AgentKind, AgentSessionId, EventId};
use rimz::ledger::{AgentLaunchAppend, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest};
use rimz::message::{DeliveryGate, gate_open};
use rimz::mux::{LayoutPanes, PaneCmd, TabOptions, own_pane_id};
use rimz::run::{PermissionMode, RunRecord, RunStatus};
use rimz::workspace::WorkspaceResolver;

use commands::*;
use exec::run_exec;
use launch::*;

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
const RUN_MONITOR_POLL: Duration = Duration::from_millis(250);
const RUN_EXIT_TERMINAL_GRACE: Duration = Duration::from_millis(500);
static CLEANUP_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

type LaunchIdentity = AgentLaunchIdentity;

struct LaunchEventParams<'a> {
    cwd: &'a Path,
    worktree_name: Option<&'a str>,
    prompt: Option<&'a str>,
    state: rimz::schema::event::AgentLaunchState,
    pane_id: Option<rimz::ids::PaneId>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AgentsArgs {
    #[command(subcommand)]
    command: Option<AgentsSubcmd>,
    /// Layout spec or named layout (`claude,codex+term`).
    #[arg(value_name = "SPEC")]
    spec: Option<String>,
    /// Prompt broadcast to every launched agent cell.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Use a Rimz-owned worktree. Bare flag creates one fresh worktree; NAME reuses or creates it.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,
    /// Durable name for a single launched agent.
    #[arg(long)]
    name: Option<String>,
    /// Open tabs/windows without moving focus to them.
    #[arg(long)]
    no_focus: bool,
    /// Let the agent ask before tool use where supported.
    #[arg(long, conflicts_with = "yolo")]
    ask: bool,
    /// Skip provider permission prompts where supported.
    #[arg(long)]
    yolo: bool,
    /// Run one supervised agent prompt and print its final answer.
    #[arg(short = 'p', long = "print")]
    print: bool,
    /// Wait cap for `--print` or `wait`.
    #[arg(long, value_parser = crate::cli::agents_cmd::supervised::parse_timeout, requires = "print")]
    timeout: Option<Duration>,
    /// Leave the supervised agent pane open after completion.
    #[arg(long, requires = "print")]
    keep: bool,
    /// Launch the supervised run and print its agent name.
    #[arg(long, requires = "print")]
    detach: bool,
    /// Print JSON output for list or supervised print runs.
    #[arg(long)]
    json: bool,
    /// Stream supervised run progress as NDJSON.
    #[arg(long, requires = "print", conflicts_with_all = ["detach", "json"])]
    stream: bool,
    /// Extra argv appended to every launched agent cell.
    #[arg(last = true)]
    passthrough: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum AgentsSubcmd {
    /// List agent cards in the current room.
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Show one agent card.
    Show {
        reference: String,
        #[arg(long)]
        json: bool,
    },
    /// Focus an agent pane.
    Focus { reference: String },
    /// Wait for a supervised run or for an interactive agent to become idle.
    Wait {
        reference: String,
        #[arg(long, value_parser = crate::cli::agents_cmd::supervised::parse_timeout)]
        timeout: Option<Duration>,
        #[arg(long)]
        stream: bool,
        #[arg(long, requires = "stream")]
        from_start: bool,
        #[arg(long, conflicts_with = "stream")]
        json: bool,
    },
    /// Stop a supervised run or close an agent pane.
    Stop { reference: String },
    /// Hidden wrapper used inside launched agent panes.
    #[command(hide = true)]
    Exec(ExecArgs),
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    /// Resume a prior agent session by id instead of launching fresh — the
    /// argv resume-on-rebirth panes run ([`rimz::resume::plan_resume`]).
    #[arg(long, value_name = "SESSION_ID", conflicts_with = "prompt")]
    resume: Option<String>,
    #[arg(long)]
    run_id: Option<rimz::RunId>,
    #[arg(long)]
    agent_name: Option<String>,
    #[arg(long)]
    launch_id: Option<String>,
    #[arg(long, hide = true)]
    exit_on_run_completion: bool,
    #[arg(long, hide = true)]
    close_pane_on_exit: bool,
    #[arg(long)]
    worktree_path: Option<PathBuf>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(last = true)]
    extra_args: Vec<String>,
}

pub fn run(args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(AgentsSubcmd::Exec(exec)) => return run_exec(exec, globals),
        Some(AgentsSubcmd::List {
            json,
            all,
            worktree,
        }) => return list_agents(json, all, worktree, globals),
        Some(AgentsSubcmd::Show { reference, json }) => {
            return show_agent(reference, json, globals);
        }
        Some(AgentsSubcmd::Focus { reference }) => return focus_agent(reference, globals),
        Some(AgentsSubcmd::Wait {
            reference,
            timeout,
            stream,
            from_start,
            json,
        }) => return wait_agent(reference, timeout, stream, from_start, json, globals),
        Some(AgentsSubcmd::Stop { reference }) => return stop_agent(reference, globals),
        None => {}
    }
    if args.spec.is_none() {
        reject_launch_flags_without_spec(&args)?;
        return list_agents(args.json, false, args.worktree, globals);
    }
    if args.print {
        return match run_print(args, globals) {
            Ok(()) => Ok(()),
            Err(err) => exit_print_usage_error(err),
        };
    }
    if args.json {
        bail!(
            "--json is only supported with `rimz agents`, `rimz agents list`, and `rimz agents -p`"
        );
    }
    launch_layout(args, globals)
}

fn exit_print_usage_error(err: anyhow::Error) -> ! {
    let _ = writeln!(std::io::stderr().lock(), "rimz: {err:#}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests;
