//! Hook subcommands. Installed hooks `exec` into these — stdout is the
//! agent decision channel; stderr is for diagnostics. The CLI marks the
//! whole subtree `hide = true` because users don't run it by hand.
//!
//! Ask hooks record `Waiting` when the agent has its own prompt surface, then
//! return the agent-native neutral no-op immediately. The agent's UI stays the
//! answer surface.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;
use tracing::{debug, warn};

use super::{GlobalFlags, open_store};
use rimz::Store;
use rimz::agents::lifecycle::{self as agent_lifecycle, LifecycleSignal, TransitionKind};
use rimz::agents::{AgentAdapter, AgentHookClass, AgentLifecycleObservation, adapter_by_kind};
use rimz::ids::{MuxName, PaneId};
use rimz::store::{AgentLifecycleIntent, AgentLifecycleOutcome};
use rimz::workspace::{self, ResolvedWorkspace, WorkspaceResolver};

mod binding;
mod hook_install;
mod install;
mod lifecycle;
mod owner;
mod payload_ids;
mod proctree;

#[cfg(test)]
mod tests;

use binding::{enrich_pane_stamp_from_cache, recover_focused_pane_binding};
pub(in crate::cli) use hook_install::ensure_detected_agent_hooks;
pub(crate) use install::uninstall_managed_hooks;
use install::{run_install, run_uninstall};
pub(crate) use lifecycle::handle_lifecycle_hook;
use owner::{attach_agent_owner, attach_agent_pane, hook_agent_pid, hook_owner_is_daemon};
use payload_ids::{payload_agent_id, payload_context_agent_id, spawn_refresh_detached};
use proctree::sibling_agent_pins;
pub(super) const FOCUSED_PANE_BIND_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(1_000);
#[derive(Debug, Args)]
pub struct HooksArgs {
    #[command(subcommand)]
    command: HooksSubcmd,
}

#[derive(Debug, Subcommand)]
enum HooksSubcmd {
    /// Receive a hook payload on stdin and route it through the agent
    /// adapter. Prints the agent-native stdout payload.
    #[command(hide = true)]
    Feed {
        #[arg(long)]
        source: String,
        /// Optional explicit event name. If absent, parsed from the payload.
        #[arg(long)]
        event: Option<String>,
    },
    /// Install the adapter's hooks into the agent's per-user config file.
    /// Pass --dry-run to preview the config diff without writing files.
    /// Visible top-level command (not hidden) — the help text doubles as the
    /// install instruction.
    Install {
        /// Preview the hook config diff without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Agent name (`claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `droid`, `qwen`). Omit to install every detected agent.
        agent: Option<String>,
    },
    /// Remove the adapter's RimZ-managed hook block.
    Uninstall {
        /// Agent name (`claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `droid`, `kiro`, `qwen`). Omit to remove every RimZ-managed hook set.
        agent: Option<String>,
    },
}

impl HooksArgs {
    /// The low-cardinality command label and the agent it acts on — the hook
    /// `source` for `feed`, the named agent for install/uninstall — for the
    /// Sentry command scope.
    pub(crate) fn scope(&self) -> (&'static str, Option<&str>) {
        match &self.command {
            HooksSubcmd::Feed { source, .. } => ("hooks feed", Some(source.as_str())),
            HooksSubcmd::Install { agent, .. } => ("hooks install", agent.as_deref()),
            HooksSubcmd::Uninstall { agent } => ("hooks uninstall", agent.as_deref()),
        }
    }
}

pub fn run(args: HooksArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        HooksSubcmd::Feed { source, event } => run_feed(source, event, globals),
        HooksSubcmd::Install { agent, dry_run } => run_install(agent, dry_run),
        HooksSubcmd::Uninstall { agent } => run_uninstall(agent),
    }
}

fn run_feed(source: String, event: Option<String>, globals: &GlobalFlags) -> Result<()> {
    // Suppress hooks fired by a RimZ-internal enrichment `codex app-server`.
    // `refresh-context` cold-spawns such a server to read realtime context; it
    // is not a user session, but Codex still fires its configured lifecycle
    // hooks (e.g. `SessionStart`) on startup. Processing one here would call
    // `context_refresh_spawn`, spawn another `refresh-context`, cold-spawn
    // another app-server, and recurse without bound. The marker rides the
    // server's env into this hook child; a neutral no-op (empty stdout) breaks
    // the loop. See `rimz::agents::codex::ENV_INTERNAL_APP_SERVER`.
    if rimz::agents::codex::spawned_as_internal_app_server() {
        debug!(
            source = %source,
            "hooks feed: suppressed — fired by a RimZ-internal codex app-server",
        );
        return Ok(());
    }
    if source == "claude" && rimz::agents::claude::remote_control::spawned_by_remote_control() {
        debug!(
            source = %source,
            "hooks feed: suppressed — fired by a Claude remote-control session",
        );
        return Ok(());
    }
    let raw_agent_pid = hook_agent_pid(&source);
    let normalized_owner_pid = if source == "droid" {
        match raw_agent_pid.map(rimz::agents::droid::hook_process_disposition) {
            Some(rimz::agents::droid::HookProcessDisposition::StockTui) => {
                debug!(source = %source, "hooks feed: suppressed duplicate outer Droid TUI hook");
                return Ok(());
            }
            Some(
                rimz::agents::droid::HookProcessDisposition::InternalWorker { owner_pid }
                | rimz::agents::droid::HookProcessDisposition::Standalone { owner_pid },
            ) => Some(owner_pid),
            None => None,
        }
    } else {
        raw_agent_pid
    };
    let daemon_owned = normalized_owner_pid.is_some_and(|pid| hook_owner_is_daemon(&source, pid));
    let scan = |cwd: &Path| sibling_agent_pins(&source, cwd);
    // A daemon's environment is unattributable: it can carry a valid workspace
    // pin for the unrelated room that launched the shared daemon. Daemon-owned
    // hooks never consult it. Pane-owned hooks keep the env pin first and use
    // sibling recovery when a daemon route cannot be classified.
    let workspace = if daemon_owned {
        WorkspaceResolver::resolve_daemon_participant_with_pin_recovery(
            ".",
            globals.root.clone(),
            &scan,
        )?
    } else {
        WorkspaceResolver::resolve_participant_with_pin_recovery(".", globals.root.clone(), &scan)?
    };
    let store = open_store(&workspace)?;
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading hook stdin")?;
    let payload: Value = if buf.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&buf).context("parsing hook payload")?
    };
    let event_name = event
        .or_else(|| {
            payload
                .get("hook_event_name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());

    let agent = match adapter_by_kind(&source) {
        Ok(agent) => agent,
        Err(err)
            if rimz::agents::plugin::loaded()
                .errors
                .iter()
                .any(|load_error| load_error.kind_hint.as_deref() == Some(source.as_str())) =>
        {
            warn!(source, error = %err, "hooks feed: invalid agent plugin skipped");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    let classified = agent.classify_hook(&event_name, &payload);

    if classified.class != AgentHookClass::AwaitingUser {
        handle_lifecycle_hook(
            &workspace,
            &store,
            agent,
            &event_name,
            &payload,
            normalized_owner_pid,
            globals,
        )?;
        return emit_neutral(agent, &event_name);
    }

    if agent.descriptor().capabilities.native_ask_ui {
        handle_lifecycle_hook(
            &workspace,
            &store,
            agent,
            &event_name,
            &payload,
            normalized_owner_pid,
            globals,
        )?;
    }
    emit_neutral(agent, &event_name)
}

fn emit_neutral(agent: &dyn AgentAdapter, event_name: &str) -> Result<()> {
    if let Some(payload) = agent.render_neutral(event_name)? {
        let rendered = serde_json::to_string(&payload)?;
        #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
        {
            println!("{rendered}");
        }
    }
    Ok(())
}
