//! Hook subcommands. Installed hooks `exec` into these — stdout is the
//! agent decision channel; stderr is for diagnostics. The CLI marks the
//! whole subtree `hide = true` because users don't run it by hand.
//!
//! Bridge wiring: when the per-machine allowlist contains a resolver whose
//! heartbeat is fresh under the workspace runtime dir, the hook engages the
//! bridge — binds a per-request socket, re-stats the resolver (TOCTOU
//! guard), pushes a `Surface::Bridge` feed item, and blocks on the socket
//! for up to the agent descriptor's `hook_cap`. On resolver answer
//! the hook prints the agent-native decision JSON; on cap or resolver loss
//! it downgrades to `native_ui` and returns the agent-native no-op. See
//! `docs/internals/sidebar/ledger.md` for the wire-level contract.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, warn};

use super::{GlobalFlags, open_ledger};
use rimz::EventEnvelope;
use rimz::Ledger;
use rimz::agents::lifecycle::{self as agent_lifecycle, LifecycleSignal, TransitionKind};
use rimz::agents::{AgentAdapter, AgentHookClass, AgentLifecycleObservation, adapter_by_kind};
use rimz::bridge::{self as bridge_api, BridgeOutcome, ExpectedFrame, SocketGuard};
use rimz::feed::{
    AbandonReason, AgentState, FeedItem, FeedKind, FeedStatus, PaneRef, ResolverStep,
    ResolverStepState, RuntimeOwnerKind, Surface,
};
use rimz::ids::{MuxName, PaneId};
use rimz::ledger::AskExpiry;
use rimz::ledger::runtime::process_owner;
use rimz::ledger::snapshot::pane_start_allows_bind;
use rimz::mux::ClientFocusOptions;
use rimz::resolver::{Allowlist, AllowlistEntry, fresh_enrolled, is_resolver_fresh, restat};
use rimz::workspace::{self, ResolvedWorkspace, WorkspaceResolver};

mod binding;
mod binding_select;
mod bridge;
mod feed_item;
mod install;
mod lifecycle;
mod owner;
mod proctree;

#[cfg(test)]
mod tests;

use binding::recover_focused_pane_binding;
use bridge::handle_blocking_feed;
use feed_item::{payload_agent_id, payload_context_agent_id, spawn_refresh_detached};
use install::{run_install, run_uninstall};
use lifecycle::handle_lifecycle_hook;
use owner::{attach_agent_owner, attach_agent_pane};
use proctree::sibling_agent_pins;
/// Hidden env-var override used by integration tests so the cap timeout
/// shape can be exercised in tens of milliseconds. Production callers leave
/// this unset and the adapter's `hook_cap` governs.
pub(super) const HOOK_CAP_OVERRIDE_ENV: &str = "RIMZ_HOOK_CAP_MILLIS";
pub(super) const FOCUSED_PANE_BIND_TIMEOUT: Duration = Duration::from_millis(1_000);
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
    /// Visible top-level command (not hidden) — the help text doubles as the
    /// install instruction.
    Install {
        /// Agent name (`claude`, `codex`, `pi`). Omit to install every detected agent.
        agent: Option<String>,
    },
    /// Remove the adapter's Rimz-managed hook block.
    Uninstall {
        /// Agent name (`claude`, `codex`, `pi`). Omit to remove every Rimz-managed hook set.
        agent: Option<String>,
    },
}

pub fn run(args: HooksArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        HooksSubcmd::Feed { source, event } => run_feed(source, event, globals),
        HooksSubcmd::Install { agent } => run_install(agent),
        HooksSubcmd::Uninstall { agent } => run_uninstall(agent),
    }
}

fn run_feed(source: String, event: Option<String>, globals: &GlobalFlags) -> Result<()> {
    // A daemon-routed hook (Codex's app-server spawns hook children with the
    // daemon's env, not the pane's) misses the session pin in its own
    // environment, so resolution may recover it from the sibling agent
    // process at this cwd.
    let workspace = WorkspaceResolver::resolve_participant_with_pin_recovery(
        ".",
        globals.root.clone(),
        &|cwd: &Path| sibling_agent_pins(&source, cwd),
    )?;
    let ledger = open_ledger(&workspace)?;
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

    if classified.class != AgentHookClass::BlockingFeed {
        return handle_lifecycle_hook(&workspace, &ledger, agent, &event_name, &payload, globals);
    }

    let feed_kind = classified.feed_kind.unwrap_or(FeedKind::Generic);
    handle_blocking_feed(&workspace, &ledger, agent, &event_name, feed_kind, payload)
}
