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
//! `docs/internals/ledger.md` for the wire-level contract.

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
use lifecycle::{log_lifecycle_transition, proof_of_work_pre_tool};
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
        /// Agent name (`claude`, `codex`, `pi`).
        agent: String,
    },
    /// Remove the adapter's Rimz-managed hook block.
    Uninstall {
        /// Agent name (`claude`, `codex`, `pi`).
        agent: String,
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
        // A non-blocking event records its observation on the ledger (status,
        // mode, agent_id, task) and exits with no stdout.
        // `observe_lifecycle` returns `Some` only for transition-bearing
        // events, so high-frequency tool hooks stay silent. The neutral
        // Lifecycle stdout is a model-visible/context surface in some agents,
        // so it stays empty unless this path is rendering a real decision.
        //
        // Captured for the out-of-band Codex context refresh below: the model id
        // the observation resolved, used to look up the model's display name.
        let mut model_hint: Option<String> = None;
        // A lifecycle boundary can strand the session's pending native_ui asks:
        // the agent answers those in its own UI and never reports back, so they
        // pile up as duplicate attention. When the session *ends*, expire every
        // surface it left pending; when it merely *moves on* (a new prompt or
        // turn end), expire only its native_ui asks so an in-flight bridge ask
        // keeps resolving. The sidebar's read-side guard self-heals races. The
        // expiry rides the lifecycle append's own lock cycle below.
        let expiry_scope = if agent.ends_session(&event_name) {
            Some(AskExpiry::SessionEnded)
        } else if agent.moves_on(&event_name) {
            Some(AskExpiry::MovedOn)
        } else {
            None
        };
        let agent_id = payload_agent_id(&payload);
        let expiry = match (agent_id, expiry_scope) {
            (Some(agent_id), Some(scope)) => Some((agent.descriptor().kind, agent_id, scope)),
            _ => None,
        };
        if let Some(mut observation) = agent.observe_lifecycle(&event_name, &payload) {
            attach_agent_owner(agent.descriptor().kind, &mut observation);
            attach_agent_pane(&mut observation);
            if observation.worktree_path.is_none() {
                observation.worktree_path = Some(workspace.worktree_root.display().to_string());
            }
            if observation.worktree_branch.is_none() {
                observation.worktree_branch = workspace.worktree_branch.clone();
            }
            recover_focused_pane_binding(
                agent.descriptor().kind,
                agent.descriptor().capabilities.registers_lazily,
                globals.mux,
                &workspace,
                &ledger,
                &mut observation,
            );
            model_hint = observation.model.clone();
            // Validate the transition this event drives against the prior rollup
            // and log any anomaly once, here at ingestion — the reducer
            // re-derives the same state silently on every replay.
            let transition =
                log_lifecycle_transition(&ledger, agent.descriptor().kind, &observation);
            let envelope = EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                agent.descriptor().kind,
                &event_name,
                &observation,
            );
            // `ToolUsed { false, false }` is reserved for non-blocking
            // PreToolUse proof-of-work. PostToolUse observations are emitted
            // only from the `tool_mutates` arm, so they always carry
            // `mutates: true`; this gate keeps PreToolUse out of the durable log
            // unless it actually reconciles a resting row to running.
            let append_lifecycle = !proof_of_work_pre_tool(&observation.signal)
                || transition.is_some_and(|transition| {
                    matches!(transition.kind, TransitionKind::Reconciled { .. })
                });
            if append_lifecycle && let Err(err) = ledger.append_event_and_expire(&envelope, expiry)
            {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    error = %err,
                    "lifecycle: failed to record the agent.lifecycle event",
                );
            }
        } else if let Some((source, agent_id, scope)) = expiry {
            // A boundary event the adapter doesn't observe still expires the
            // session's superseded asks through the standalone path.
            let result = match scope {
                AskExpiry::SessionEnded => {
                    ledger.expire_agent_session(source, agent_id, &workspace.session_name)
                }
                AskExpiry::MovedOn => {
                    ledger.expire_agent_native_ui_asks(source, agent_id, &workspace.session_name)
                }
            };
            if let Err(err) = result {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    error = %err,
                    "lifecycle: failed to expire the session's pending asks",
                );
            }
        }
        if let Some(agent_id) = agent_id {
            // Tombstone the session's statusline context sidecar so it can't
            // pin stale enrichment to a session the rollup has dropped.
            if agent.ends_session(&event_name)
                && let Err(err) = rimz::ledger::agent_context::remove(
                    ledger.runtime_paths(),
                    agent.descriptor().kind,
                    agent_id,
                )
            {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    error = %err,
                    "lifecycle: failed to remove the session's context sidecar",
                );
            }
            // Refresh the agent's activity heartbeat on progress-proving events
            // (the descriptor's `activity_events`, in each agent's own wire
            // vocabulary) so the sidebar's `last_activity` advances per tool
            // call, not just per turn. A latency hint, never correctness —
            // log and continue on failure.
            if agent.descriptor().records_activity(&event_name)
                && let Err(err) = rimz::agent_activity::touch(
                    ledger.runtime_paths(),
                    agent.descriptor().kind,
                    agent_id,
                )
            {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    error = %err,
                    "lifecycle: failed to touch the agent activity heartbeat",
                );
            }
            if let Some(context_agent_id) = payload_context_agent_id(&payload) {
                if let Some(marker) = agent.observe_turn_error_from_hook(&event_name, &payload) {
                    if let Err(err) = rimz::ledger::agent_context::merge_turn_error(
                        ledger.runtime_paths(),
                        agent.descriptor().kind,
                        context_agent_id,
                        marker,
                    ) {
                        warn!(
                            agent = agent.descriptor().kind,
                            event = %event_name,
                            error = %err,
                            "lifecycle: failed to merge turn-error marker",
                        );
                    } else {
                        let _ = rimz::ledger::wakeup::wake_sidebars(ledger.runtime_paths());
                    }
                }
                let prior = rimz::ledger::agent_context::read_one(
                    ledger.runtime_paths(),
                    agent.descriptor().kind,
                    context_agent_id,
                );
                let refresh = {
                    let local_model_hint = model_hint.as_deref().or_else(|| {
                        prior
                            .as_ref()
                            .and_then(|record| record.context.model_id.as_deref())
                    });
                    let refresh_ctx = rimz::agents::LocalContextRefreshCtx {
                        agent_id: context_agent_id,
                        model_hint: local_model_hint,
                        prior_effort: prior
                            .as_ref()
                            .and_then(|record| record.context.effort.as_deref()),
                        prior_transcript_path: prior
                            .as_ref()
                            .and_then(|record| record.transcript_path.as_deref()),
                        prior_transcript_stat: prior
                            .as_ref()
                            .and_then(|record| record.transcript_stat.as_ref()),
                    };
                    agent.local_context_refresh(&event_name, &refresh_ctx)
                };
                if let Some(refresh) = refresh {
                    if let Err(err) = rimz::ledger::agent_context::merge_local_context(
                        ledger.runtime_paths(),
                        agent.descriptor().kind,
                        context_agent_id,
                        prior,
                        refresh,
                        jiff::Timestamp::now(),
                    ) {
                        warn!(
                            agent = agent.descriptor().kind,
                            event = %event_name,
                            error = %err,
                            "lifecycle: failed to merge local context sidecar",
                        );
                    } else {
                        let _ = rimz::ledger::wakeup::wake_sidebars(ledger.runtime_paths());
                    }
                }
            }
            // An adapter can request a detached `rimz` helper after a
            // lifecycle event — Codex refreshes its app-server context on turn
            // boundaries. Spawned with fresh stdio and never awaited, so it
            // adds no latency to the agent's turn; the sidebar's next wakeup
            // folds the fresh sidecar in.
            let refresh_ctx = rimz::agents::LifecycleRefreshCtx {
                agent_id,
                workspace_id: workspace.workspace_id.as_str(),
                model_hint: model_hint.as_deref(),
            };
            if let Some(spawn) = agent.post_lifecycle_refresh(&event_name, &refresh_ctx) {
                spawn_refresh_detached(&spawn);
            }
        }
        return Ok(());
    }

    let feed_kind = classified.feed_kind.unwrap_or(FeedKind::Generic);
    handle_blocking_feed(&workspace, &ledger, agent, &event_name, feed_kind, payload)
}
