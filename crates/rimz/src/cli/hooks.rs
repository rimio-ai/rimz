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
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, warn};

use super::{GlobalFlags, open_store};
use rimz::EventEnvelope;
use rimz::Store;
use rimz::agents::lifecycle::{self as agent_lifecycle, LifecycleSignal, TransitionKind};
use rimz::agents::{
    AgentAdapter, AgentHookClass, AgentLifecycleObservation, AgentState, adapter_by_kind,
};
use rimz::ids::{MuxName, PaneId};
use rimz::mux::ClientFocusOptions;
use rimz::pane::{PaneRef, RuntimeOwnerKind};
use rimz::store::runtime::process_owner;
use rimz::store::snapshot::pane_start_allows_bind;
use rimz::workspace::{self, ResolvedWorkspace, WorkspaceResolver};

mod binding;
mod binding_select;
mod install;
mod lifecycle;
mod owner;
mod payload_ids;
mod proctree;

#[cfg(test)]
mod tests;

use binding::{enrich_pane_stamp_from_cache, recover_focused_pane_binding};
pub(crate) use install::uninstall_managed_hooks;
use install::{run_install, run_uninstall};
use lifecycle::handle_lifecycle_hook;
use owner::{attach_agent_owner, attach_agent_pane};
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
        /// Agent name (`claude`, `codex`, `pi`, `opencode`). Omit to install every detected agent.
        agent: Option<String>,
    },
    /// Remove the adapter's Rimz-managed hook block.
    Uninstall {
        /// Agent name (`claude`, `codex`, `pi`, `opencode`). Omit to remove every Rimz-managed hook set.
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
    // Suppress hooks fired by a Rimz-internal enrichment `codex app-server`.
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
            "hooks feed: suppressed — fired by a Rimz-internal codex app-server",
        );
        return Ok(());
    }
    // A daemon-routed hook (Codex's app-server spawns hook children with the
    // daemon's env, not the pane's) misses the session pin in its own
    // environment, so resolution may recover it from the sibling agent
    // process at this cwd.
    let workspace = WorkspaceResolver::resolve_participant_with_pin_recovery(
        ".",
        globals.root.clone(),
        &|cwd: &Path| sibling_agent_pins(&source, cwd),
    )?;
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

    let agent = adapter_by_kind(&source)?;
    let classified = agent.classify_hook(&event_name, &payload);

    if classified.class != AgentHookClass::AwaitingUser {
        return handle_lifecycle_hook(&workspace, &store, agent, &event_name, &payload, globals);
    }

    if agent.descriptor().capabilities.native_ask_ui {
        handle_lifecycle_hook(&workspace, &store, agent, &event_name, &payload, globals)?;
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
