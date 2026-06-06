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
use serde_json::Value;
use tracing::{debug, warn};

use super::{GlobalFlags, open_ledger};
use rimz::EventEnvelope;
use rimz::Ledger;
use rimz::agents::lifecycle::{self, TransitionKind};
use rimz::agents::{AgentAdapter, AgentHookClass, AgentLifecycleObservation, adapter_by_kind};
use rimz::bridge::{self, BridgeOutcome, ExpectedFrame, SocketGuard};
use rimz::feed::{
    AbandonReason, FeedItem, FeedKind, FeedStatus, ResolverStep, ResolverStepState,
    RuntimeOwnerKind, Surface,
};
use rimz::ids::{MuxName, PaneId};
use rimz::ledger::AskExpiry;
use rimz::ledger::runtime::process_owner;
use rimz::resolver::{Allowlist, AllowlistEntry, fresh_enrolled, is_resolver_fresh, restat};
use rimz::workspace::{self, ResolvedWorkspace, WorkspaceResolver};

/// Hidden env-var override used by integration tests so the cap timeout
/// shape can be exercised in tens of milliseconds. Production callers leave
/// this unset and the adapter's `hook_cap` governs.
const HOOK_CAP_OVERRIDE_ENV: &str = "RIMZ_HOOK_CAP_MILLIS";

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

/// Fold this observation's signal onto the prior rollup state through the shared
/// `lifecycle::step` table and log any anomaly once, under the
/// `rimz::agent::lifecycle` target (stderr — never stdout, the hook decision
/// channel). Best-effort: a missing cached snapshot just skips the check. The
/// reducer re-derives the same state on replay, silently — this call exists only
/// to surface a reconciled or ignored transition while we still have the event
/// in hand to attribute it.
fn log_lifecycle_transition(ledger: &Ledger, kind: &str, observation: &AgentLifecycleObservation) {
    let Some(agent_id) = observation.agent_id.as_deref() else {
        // The reducer quarantines a session-less event (no rollup entry) and
        // stays quiet on replay — this is the once-per-fresh-event warning.
        warn!(
            target: "rimz::agent::lifecycle",
            kind,
            signal = ?observation.signal,
            "session-less agent.lifecycle event — the reducer will quarantine it",
        );
        return;
    };
    // The prior state for this one agent, from the lock-free cached snapshot —
    // the projection of every event before this one, exactly the `prev` the
    // reducer folds this event onto.
    let prev = ledger
        .snapshot_cached()
        .ok()
        .and_then(|snap| {
            snap.agents
                .into_iter()
                .find(|agent| agent.kind == kind && agent.agent_id == agent_id)
        })
        .map(|agent| agent.lifecycle());
    let transition = lifecycle::step(prev.as_ref(), &observation.signal);
    match transition.kind {
        TransitionKind::Reconciled { from, reason } => warn!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id,
            parent_agent_id = observation.parent_agent_id.as_deref().unwrap_or(""),
            from = ?from,
            to = ?transition.next.status,
            signal = ?observation.signal,
            reason,
            "reconciled lifecycle transition",
        ),
        TransitionKind::Ignored { reason } => debug!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id,
            signal = ?observation.signal,
            reason,
            "ignored lifecycle signal",
        ),
        TransitionKind::Normal => {}
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
            model_hint = observation.model.clone();
            // Validate the transition this event drives against the prior rollup
            // and log any anomaly once, here at ingestion — the reducer
            // re-derives the same state silently on every replay.
            log_lifecycle_transition(&ledger, agent.descriptor().kind, &observation);
            let envelope = EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                agent.descriptor().kind,
                &event_name,
                &observation,
            );
            if let Err(err) = ledger.append_event_and_expire(&envelope, expiry) {
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
                let prior = rimz::ledger::agent_context::read_one(
                    ledger.runtime_paths(),
                    agent.descriptor().kind,
                    context_agent_id,
                );
                let refresh = {
                    let refresh_ctx = rimz::agents::LocalContextRefreshCtx {
                        agent_id: context_agent_id,
                        model_hint: model_hint.as_deref(),
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
                        let _ = rimz::ledger::wakeup::wake_sidebars_for_context(
                            ledger.runtime_paths(),
                            &workspace.workspace_id,
                        );
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

/// Best-effort PID of the agent process that spawned this hook helper. Tests
/// and unusual launch chains pin the value via `RIMZ_AGENT_PID`; production
/// hooks fall back to walking the parent chain from `getppid()` looking for an
/// ancestor whose process name matches the agent kind (or its known launcher,
/// e.g. `node` for codex). The walk gracefully returns `None` on non-Linux or
/// if `/proc` lookups fail, preserving the legacy "no PID, no reap" behavior.
fn hook_agent_pid(source: &str) -> Option<u32> {
    if let Some(pid) = std::env::var("RIMZ_AGENT_PID")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|pid| *pid > 1)
    {
        return Some(pid);
    }
    walk_to_agent_ancestor(source)
}

/// Stamp the normalized pane id of the multiplexer pane the hook ran inside.
/// The hook helper is a child of the agent process, which is itself a child of
/// the user's mux pane, so the per-pane env var (`TMUX_PANE` /
/// `ZELLIJ_PANE_ID`) names the right pane unambiguously — the only way to tell
/// two same-kind agents in one worktree apart.
fn attach_agent_pane(observation: &mut AgentLifecycleObservation) {
    if observation.pane_id.is_some() {
        return;
    }
    observation.pane_id = pane_id_from_env();
}

fn pane_id_from_env() -> Option<PaneId> {
    if let Some(raw) = std::env::var("ZELLIJ_PANE_ID")
        .ok()
        .filter(|raw| !raw.is_empty())
    {
        return Some(PaneId::from_parts(
            MuxName::Zellij,
            format!("terminal_{raw}"),
        ));
    }
    if let Some(raw) = std::env::var("TMUX_PANE")
        .ok()
        .filter(|raw| !raw.is_empty())
    {
        return Some(PaneId::from_parts(MuxName::Tmux, raw));
    }
    None
}

fn attach_agent_owner(source: &str, observation: &mut AgentLifecycleObservation) {
    if observation.runtime_owner.is_some() {
        return;
    }
    let Some(agent_id) = observation.agent_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(pid) = observation.agent_pid.or_else(|| hook_agent_pid(source)) else {
        return;
    };
    let owner = process_owner(RuntimeOwnerKind::Agent, agent_id, pid);
    observation.agent_pid = Some(pid);
    observation.agent_process_start = owner.process_start.clone();
    observation.runtime_owner = Some(owner);
}

#[cfg(target_os = "linux")]
fn walk_to_agent_ancestor(source: &str) -> Option<u32> {
    // Cap the walk so a pathologically deep tree (or a /proc parse glitch)
    // cannot loop the hook helper. 32 levels is far beyond any real agent
    // launch chain.
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..32 {
        if pid <= 1 {
            return None;
        }
        let (name, ppid) = read_proc_status(pid)?;
        if matches_agent_kind(&name, source) {
            return Some(pid);
        }
        pid = ppid;
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn walk_to_agent_ancestor(_source: &str) -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn read_proc_status(pid: u32) -> Option<(String, u32)> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut name = None;
    let mut ppid = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("Name:") {
            name = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("PPid:") {
            ppid = rest.trim().parse::<u32>().ok();
        }
        if name.is_some() && ppid.is_some() {
            break;
        }
    }
    Some((name?, ppid?))
}

/// Whether the kernel-reported `comm` is one of the agent's declared process
/// names — its own binary plus any launcher (Codex ships as a JS bundle, so
/// its `comm` is `node`). The set lives on the descriptor; an unregistered
/// kind falls back to the exact-name match.
fn matches_agent_kind(comm: &str, source: &str) -> bool {
    match rimz::agents::descriptor_by_kind(source) {
        Some(descriptor) => descriptor.process_names.contains(&comm),
        None => comm == source,
    }
}

/// Verified pin roots from live processes of this agent kind at the hook's
/// cwd — the recovery source for a daemon-routed hook ([`run_feed`]). The
/// agent spawned its hook with the session cwd, so the in-pane agent process
/// sharing that cwd carries the pane's pin. Every declared process name is a
/// candidate — launchers included (`node` for Codex's JS bundle), so an
/// unrelated same-name process at the same cwd can enter the scan; the
/// per-candidate [`workspace::verify_pin`] and the resolver's all-agree rule
/// keep a stray candidate degrading to the static ladder rather than
/// misrouting. Predicates run cheap-first (`comm`, then the `cwd` readlink,
/// then the two `environ` reads) so only this agent's processes pay the
/// `/proc` reads; non-Linux yields nothing and the resolver falls back to
/// the static ladder.
fn sibling_agent_pins(source: &str, cwd: &Path) -> Vec<PathBuf> {
    rimz::proc::list_processes()
        .into_iter()
        .filter(|info| {
            rimz::proc::comm(info.pid).is_some_and(|comm| matches_agent_kind(&comm, source))
        })
        .filter(|info| rimz::proc::cwd(info.pid).is_some_and(|proc_cwd| proc_cwd == cwd))
        .filter_map(|info| {
            let id = rimz::proc::env_var(info.pid, workspace::ENV_WORKSPACE_ID)?;
            let root = rimz::proc::env_var(info.pid, workspace::ENV_PROJECT_ROOT)?;
            workspace::verify_pin(&id, Path::new(&root))
        })
        .collect()
}

fn run_install(agent: String) -> Result<()> {
    let integration = adapter_by_kind(&agent)?;
    let report = integration.install_hooks()?;
    // User-facing JSON. Report struct derives Serialize so the shape stays in
    // lockstep with `HookInstallReport`.
    let rendered = serde_json::to_string_pretty(&report)?;
    #[expect(clippy::print_stdout, reason = "user-visible install report")]
    {
        println!("{rendered}");
    }
    Ok(())
}

fn run_uninstall(agent: String) -> Result<()> {
    let integration = adapter_by_kind(&agent)?;
    let report = integration.uninstall_hooks()?;
    let rendered = serde_json::to_string_pretty(&report)?;
    #[expect(clippy::print_stdout, reason = "user-visible uninstall report")]
    {
        println!("{rendered}");
    }
    Ok(())
}

fn handle_blocking_feed(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    feed_kind: FeedKind,
    payload: Value,
) -> Result<()> {
    // A fresh ask supersedes any earlier native_ui ask this session left
    // pending — the agent only ever shows one at a time in its own UI. The
    // push below expires the priors inside its own critical section, so the
    // sidebar never stacks two rows for one session and the hook pays one
    // lock cycle. Bridge asks resolve via their socket and are left alone.
    let superseded_session: Option<String> = payload_agent_id(&payload).map(ToOwned::to_owned);
    let supersede = superseded_session
        .as_deref()
        .map(|agent_id| (agent.descriptor().kind, agent_id));

    let allowlist = Allowlist::load().context("loading resolver allowlist")?;
    let fresh = fresh_enrolled(ledger.runtime_paths(), &allowlist)
        .context("checking resolver heartbeat freshness")?;

    if fresh.is_empty() {
        // No fresh enrolled resolver: native_ui path. The hook writes the feed
        // item, wakes sidebars, returns the neutral no-op, and exits — the
        // agent's own UI is the answer surface. An agent with no native ask
        // UI (pi) skips the item: neutral already lets the tool run, and a
        // `native_ui` row would strand waiting on a surface that doesn't
        // exist.
        if agent.descriptor().capabilities.native_ask_ui {
            let item = build_item(workspace, Surface::NativeUi, feed_kind, agent, payload);
            ledger.push_feed_item_superseding(&item, supersede, &workspace.session_name)?;
        }
        emit_neutral(agent, event_name)?;
        return Ok(());
    }

    // Bridge path. Bind before push so a fast resolver can't miss the socket.
    let mut item = build_item(workspace, Surface::Bridge, feed_kind, agent, payload);
    let cap = hook_cap_for(agent);
    item.hook_wait_timeout_seconds = cap.as_secs();
    attach_resolver_chain(&mut item, &fresh);
    let active_resolver = item.chain_active_resolver.clone().ok_or_else(|| {
        anyhow::anyhow!("fresh resolver list did not produce an active chain step")
    })?;
    let (sock, sock_path) =
        bridge::bind(ledger.runtime_paths(), &item.request_id).context("binding bridge socket")?;
    let guard = SocketGuard::new(sock_path);

    // TOCTOU guard — the resolver may have died between the freshness
    // walk and now. If so, downgrade.
    if let Err(err) = restat(
        ledger.runtime_paths(),
        &allowlist,
        &active_resolver,
        &item.request_id,
    ) {
        debug!(
            resolver = active_resolver.as_str(),
            error = %err,
            "bridge: downgrading to native_ui — resolver heartbeat stale"
        );
        drop(guard);
        // Same native-ask gate as the no-resolver branch: with no surface to
        // hand off to, the downgrade is the neutral answer alone.
        if agent.descriptor().capabilities.native_ask_ui {
            let mut downgraded = item;
            downgraded.surface = Surface::NativeUi;
            downgraded.hook_wait_timeout_seconds = 0;
            downgraded.chain.clear();
            downgraded.chain_active_resolver = None;
            downgraded.chain_active_until = None;
            ledger.push_feed_item_superseding(&downgraded, supersede, &workspace.session_name)?;
        }
        emit_neutral(agent, event_name)?;
        return Ok(());
    }

    let request_id = item.request_id.clone();
    let expected = ExpectedFrame {
        workspace_id: item.workspace_id.clone(),
        request_id: request_id.clone(),
        nonce: item.nonce.clone(),
    };

    ledger.push_feed_item_superseding(&item, supersede, &workspace.session_name)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building bridge runtime")?;

    let result: Result<()> = runtime.block_on(async {
        let sock = bridge::adopt(sock).context("adopting bridge socket")?;
        run_bridge_poll(
            agent,
            ledger,
            &allowlist,
            workspace,
            event_name,
            &request_id,
            &expected,
            &sock,
            cap,
        )
        .await
    });
    drop(guard);
    result
}

/// Poll loop driving the bridge from a resolver answer, a per-step budget,
/// or a stale heartbeat to a chain advance or a clean fallback. The socket
/// is reused across iterations so a late wakeup frame from a previously
/// active resolver still lands; each branch reloads the feed item from disk
/// because the ledger is the source of truth and another writer (sidebar
/// dismiss, sibling pane abstain) may have moved the state.
#[expect(
    clippy::too_many_arguments,
    reason = "private helper; the bridge loop borrows every state slice exactly once"
)]
async fn run_bridge_poll(
    agent: &dyn AgentAdapter,
    ledger: &Ledger,
    allowlist: &Allowlist,
    workspace: &ResolvedWorkspace,
    event_name: &str,
    request_id: &rimz::ids::RequestId,
    expected: &ExpectedFrame,
    sock: &tokio::net::UnixDatagram,
    cap: Duration,
) -> Result<()> {
    const POLL_TICK: Duration = Duration::from_secs(1);
    let session_name = workspace.session_name.as_str();
    let started = std::time::Instant::now();

    loop {
        let live = ledger.load_feed_item(request_id)?;
        if let Some(done) = handle_terminal_status(&live, agent, event_name)? {
            return done;
        }

        let elapsed = started.elapsed();
        if elapsed >= cap {
            return finish_on_cap(ledger, agent, event_name, request_id, session_name);
        }
        let hook_remaining = cap - elapsed;

        let Some(active) = live.chain_active_resolver.clone() else {
            let _ = ledger.mark_feed_item_timed_out(
                request_id,
                session_name,
                AbandonReason::ChainExhausted,
            )?;
            emit_neutral(agent, event_name)?;
            return Ok(());
        };

        let inner_cap = hook_remaining
            .min(step_budget_remaining(&live).unwrap_or(hook_remaining))
            .min(POLL_TICK);

        match bridge::wait_for_resolution(sock, expected, Some(inner_cap)).await? {
            BridgeOutcome::Resolved => continue,
            BridgeOutcome::Neutral => {
                if started.elapsed() < cap {
                    advance_chain_if_step_lapsed(
                        ledger,
                        allowlist,
                        request_id,
                        &active,
                        session_name,
                    )?;
                }
            }
        }
    }
}

/// If the reloaded item is in a terminal state, deliver the agent-side
/// answer and tell the loop to exit. `Ok(None)` means "still pending, keep
/// polling".
fn handle_terminal_status(
    live: &FeedItem,
    agent: &dyn AgentAdapter,
    event_name: &str,
) -> Result<Option<Result<()>>> {
    match live.status {
        FeedStatus::Resolved => {
            let resolution = live.resolution.as_ref().ok_or_else(|| {
                anyhow::anyhow!("bridge reload: status=resolved but no resolution on disk")
            })?;
            print_decision(agent, live, resolution)?;
            Ok(Some(Ok(())))
        }
        FeedStatus::TimedOut | FeedStatus::Abandoned => {
            emit_neutral(agent, event_name)?;
            Ok(Some(Ok(())))
        }
        FeedStatus::Pending => Ok(None),
    }
}

/// Hook cap fired. Mark the item timed-out with `BridgeCapElapsed`; if a
/// concurrent resolver landed a decision in the same tick the ledger will
/// have observed it first (CAS) — render that decision instead of neutral.
fn finish_on_cap(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    request_id: &rimz::ids::RequestId,
    session_name: &str,
) -> Result<()> {
    let timeout = ledger.mark_feed_item_timed_out(
        request_id,
        session_name,
        AbandonReason::BridgeCapElapsed,
    )?;
    if timeout.status == FeedStatus::Resolved {
        let resolved = ledger.load_feed_item(request_id)?;
        if let Some(resolution) = resolved.resolution.as_ref() {
            return print_decision(agent, &resolved, resolution);
        }
        warn!(
            request_id = %request_id,
            "bridge: timeout observed Resolved but no resolution on disk"
        );
    }
    emit_neutral(agent, event_name)
}

/// After a Neutral frame inside the cap, decide whether the chain advances.
/// The reload-then-check pattern is required because another writer may have
/// already moved the state; CAS in `elapse_chain_step` will reject if so and
/// the next iteration picks up the new state.
fn advance_chain_if_step_lapsed(
    ledger: &Ledger,
    allowlist: &Allowlist,
    request_id: &rimz::ids::RequestId,
    active: &rimz::ids::ResolverId,
    session_name: &str,
) -> Result<()> {
    let post = ledger.load_feed_item(request_id)?;
    if !post.status.allows_resolution() || post.chain_active_resolver.as_ref() != Some(active) {
        return Ok(());
    }

    let advance_reason = if post
        .chain_active_until
        .is_some_and(|d| d <= jiff::Timestamp::now())
    {
        Some(AbandonReason::BudgetElapsed)
    } else if !is_resolver_fresh(ledger.runtime_paths(), allowlist, active) {
        Some(AbandonReason::HeartbeatStale)
    } else {
        None
    };

    if let Some(reason) = advance_reason
        && let Err(err) = ledger.elapse_chain_step(request_id, active, reason, session_name)
    {
        debug!(error = %err, ?reason, "elapse_chain_step failed; reloading");
    }
    Ok(())
}

fn print_decision(
    agent: &dyn AgentAdapter,
    item: &FeedItem,
    resolution: &rimz::feed::Resolution,
) -> Result<()> {
    let decision = agent.render_decision(item, resolution)?;
    let rendered = serde_json::to_string(&decision)?;
    #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
    {
        println!("{rendered}");
    }
    Ok(())
}

/// Remaining time on the active resolver's per-step budget, or `None` when
/// the feed item carries no deadline (chain not attached, or already moved).
fn step_budget_remaining(item: &FeedItem) -> Option<Duration> {
    let deadline = item.chain_active_until?;
    let span = deadline.duration_since(jiff::Timestamp::now());
    if span.is_negative() {
        return Some(Duration::ZERO);
    }
    // Past the negative guard the span is non-negative, so both components are too.
    Some(Duration::new(
        span.as_secs() as u64,
        span.subsec_nanos() as u32,
    ))
}

fn build_item(
    workspace: &ResolvedWorkspace,
    surface: Surface,
    feed_kind: FeedKind,
    agent: &dyn AgentAdapter,
    payload: Value,
) -> FeedItem {
    let mut item = FeedItem::new(
        workspace.workspace_id.clone(),
        surface,
        feed_kind,
        format!("{} needs attention", agent.descriptor().kind),
        agent.descriptor().kind,
        "agent-hook",
    );
    item.payload = payload;
    item.runtime_owner = agent_runtime_owner(agent.descriptor().kind, &item.payload);
    item.worktree_path = Some(workspace.worktree_root.display().to_string());
    item.worktree_branch = workspace.worktree_branch.clone();
    item
}

/// Spawn an adapter-requested `rimz` helper detached, with all stdio nulled
/// (the fresh-stdio invariant for hook helper children). The hook drops the
/// child without waiting, so it returns before the helper runs and never adds
/// latency to the agent's turn. Best-effort: a spawn failure is logged and
/// ignored — out-of-band enrichment is never correctness.
fn spawn_refresh_detached(spawn: &rimz::agents::RefreshSpawn) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            warn!(error = %err, "lifecycle: cannot locate rimz to spawn the refresh helper");
            return;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = cmd.spawn() {
        warn!(error = %err, "lifecycle: failed to spawn the adapter refresh helper");
    }
}

/// The agent session id from a hook payload, read in the same order as
/// [`rimz::feed::FeedItem::agent_session_id`] (`agent_id`, then `session_id`)
/// so a session resolves to the same key whether read from a lifecycle event
/// or from a stored ask. Empty ids are filtered out.
fn payload_agent_id(payload: &Value) -> Option<&str> {
    ["agent_id", "session_id"].into_iter().find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

/// The sidecar key for local context enrichment. Root sessions file context
/// under `session_id`; child-specific `agent_id`s are lifecycle identities, not
/// Codex rollout files.
fn payload_context_agent_id(payload: &Value) -> Option<&str> {
    ["session_id", "agent_id"].into_iter().find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

fn agent_runtime_owner(source: &str, payload: &Value) -> Option<rimz::RuntimeOwner> {
    let subject_id = payload_agent_id(payload)?;
    let pid = hook_agent_pid(source)?;
    Some(process_owner(RuntimeOwnerKind::Agent, subject_id, pid))
}

fn attach_resolver_chain(item: &mut FeedItem, fresh: &[AllowlistEntry]) {
    let chain = fresh
        .iter()
        .map(|entry| ResolverStep {
            resolver_id: entry.id.clone(),
            display_name: entry.display_name.clone(),
            order: i32::try_from(entry.order).unwrap_or(i32::MAX),
            budget_ms: entry.budget_seconds.saturating_mul(1000),
            state: ResolverStepState::Queued,
            reason: None,
        })
        .collect();
    item.activate_resolver_chain(chain);
}

fn hook_cap_for(agent: &dyn AgentAdapter) -> Duration {
    if let Ok(raw) = std::env::var(HOOK_CAP_OVERRIDE_ENV)
        && let Ok(ms) = raw.parse::<u64>()
    {
        return Duration::from_millis(ms);
    }
    agent.descriptor().hook_cap
}

fn emit_neutral(agent: &dyn AgentAdapter, event_name: &str) -> Result<()> {
    if let Some(payload) = agent.render_neutral(event_name)? {
        let rendered = serde_json::to_string(&payload)?;
        // Hook stdout is the decision channel: legal stdout site.
        #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
        {
            println!("{rendered}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::matches_agent_kind;

    /// Claude's binary is `claude`; codex is shipped as a node bundle, so the
    /// kernel-visible `comm` is `node`. The matcher accepts both so the
    /// ancestor walk pins the right PID under either launch shape.
    #[test]
    fn agent_kind_matches_known_launch_shapes() {
        assert!(matches_agent_kind("claude", "claude"));
        assert!(matches_agent_kind("codex", "codex"));
        assert!(matches_agent_kind("node", "codex"));

        assert!(!matches_agent_kind("node", "claude"));
        assert!(!matches_agent_kind("zsh", "claude"));
        assert!(!matches_agent_kind("bash", "codex"));
    }
}
