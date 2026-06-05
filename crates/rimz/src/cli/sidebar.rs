//! `rimz sidebar` — `snapshot` renders the view-model (producer or `--no-produce` consumer read); `serve` runs the terminal renderer loop.
//!
//! The snapshot arm is a thin delegate over the library produce pipeline
//! ([`rimz::sidebar::produce`]): it resolves workspace/session/mux, calls
//! `produce_snapshot` (or the in-process consumer read for `--no-produce`),
//! and emits — the CLI owns argv, fallback intent, and stdout alone.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::paths::env_path;
use rimz::ledger::workspace_record;
use rimz::sidebar::produce::{
    ProduceOptions, pane_fixture_active, produce_rollup_snapshot, produce_snapshot,
};
use rimz::sidebar::snapshot::{
    RollupCursor, enrich_consumer, read_published_snapshot, rollup_snapshot, write_presence_stamp,
};
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths};

#[derive(Debug, Args)]
pub struct SidebarArgs {
    #[command(subcommand)]
    command: SidebarSubcmd,
}

#[derive(Debug, Subcommand)]
enum SidebarSubcmd {
    /// Render the current snapshot. The sidebar process reads this.
    Snapshot {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long)]
        exclude_pane_id: Option<String>,
        /// Require a pane cache produced at or after this Unix millisecond.
        #[arg(long, hide = true)]
        min_pane_cache_ms: Option<u64>,
        #[arg(long)]
        json: bool,
        /// Render read-only from the producer's published cache: never fork
        /// `list-panes` or git. A non-producer renderer (one whose workspace
        /// already has an elder producer) passes this so the per-tab fleet
        /// pays the mux/git round-trip exactly once, on the elder.
        #[arg(long)]
        no_produce: bool,
    },
    /// Run the terminal sidebar renderer.
    Serve {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long, default_value_t = 1)]
        tick_seconds: u64,
    },
    /// Presence poke from the Zellij presence plugin: refresh the liveness
    /// stamp and, on a topology change, datagram the eldest sidebar for a
    /// fresh-panes refetch. Hidden — plugin infrastructure, not a human verb.
    #[command(hide = true)]
    Wake {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long, value_enum)]
        reason: WakeReason,
    },
}

/// Why a presence poke fired. Both reasons refresh the liveness stamp; only a
/// topology change additionally datagrams the eldest sidebar. `alive` is the
/// plugin's keepalive — stamp-only — so an idle-but-healthy channel stays
/// distinguishable from a dead one.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum WakeReason {
    PanesChanged,
    Alive,
}

pub fn run(args: SidebarArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        SidebarSubcmd::Snapshot {
            workspace_id,
            mux,
            session_name,
            exclude_pane_id,
            min_pane_cache_ms,
            json,
            no_produce,
        } => {
            // A producer forks `list-panes`/git and publishes the shared cache;
            // a non-producer renders read-only from that cache. Default is to
            // produce, so bare CLI calls and the plugin rail are unchanged.
            let produce = !no_produce;
            // The serve loop names its session explicitly; a bare CLI/inspection
            // call resolves it from the record. Only the former treats a
            // pane-discovery failure as fatal (see the match below).
            let explicit_session = session_name.is_some();
            let mut resolved_session = None;
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => {
                    let workspace =
                        WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
                    resolved_session = Some(workspace.session_name.clone());
                    workspace.workspace_id
                }
            };
            let state =
                StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
            let runtime =
                RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
            state.ensure_dirs().context("preparing state paths")?;
            runtime.ensure_dirs().context("preparing runtime paths")?;
            let session_name = session_name
                .or(resolved_session)
                .or_else(|| session_name_from_record(&state));
            let exclude = exclude_pane_id
                .as_deref()
                .map(rimz::ids::PaneId::parse)
                .transpose()?;

            let emit = |snapshot: &rimz::SidebarSnapshot| -> Result<()> {
                if json {
                    let rendered = serde_json::to_string_pretty(snapshot)?;
                    #[expect(clippy::print_stdout, reason = "json emitter for sidebar")]
                    {
                        println!("{rendered}");
                    }
                } else {
                    let tally = |status| {
                        snapshot
                            .worktree_groups
                            .iter()
                            .flat_map(|group| &group.status_counts)
                            .filter(|count| count.status == status)
                            .map(|count| count.count)
                            .sum::<usize>()
                    };
                    let waiting = tally(rimz::feed::AgentStatus::Waiting);
                    let failed = tally(rimz::feed::AgentStatus::Failed);
                    #[expect(clippy::print_stdout, reason = "human summary")]
                    {
                        println!("Workspace:       {}", snapshot.display_name);
                        println!("Worktree groups: {}", snapshot.worktree_groups.len());
                        println!("Waiting:         {waiting}");
                        println!("Failed:          {failed}");
                    }
                }
                Ok(())
            };

            // Consumer: render the producer's published frame in process. A
            // cold cache (no publish yet) falls back to the bare rollup with
            // the same read-only enrichments until the next tick. One-shot
            // CLI process, so a fresh cursor (a cold fold) is the only kind.
            // A pane fixture defers to the produce path, which short-circuits
            // on it — deterministic tests neither poison nor read the cache.
            if !produce
                && !pane_fixture_active()
                && let Some(session) = session_name.as_deref()
            {
                let snapshot = match read_published_snapshot(
                    &mut RollupCursor::new(),
                    &state,
                    &runtime,
                    session,
                    exclude.as_ref(),
                ) {
                    Some(snapshot) => snapshot,
                    // Cold start: no published panes yet, so own-view is not
                    // computed — the bare rollup stands until the next tick.
                    None => enrich_consumer(
                        rollup_snapshot(&state, &mut RollupCursor::new())?,
                        None,
                        &runtime,
                        exclude.as_ref(),
                    ),
                };
                return emit(&snapshot);
            }

            // Producer (or a deterministic test fixture, or a bare inspection
            // call): the library pipeline resolves the base — ledger rollup
            // plus live pane list, single-flighted across the fleet — folds
            // the producer enrichments, and publishes the caches consumers
            // read. With no session or no detectable mux there is no pane
            // frame to produce; the rollup-only arm runs the same enrichments
            // over the bare rollup.
            let mux = mux
                .or(globals.mux)
                .or_else(|| rimz::mux::auto_detect_backend(None).ok());
            let rollup_only = |reason: Option<&dyn std::fmt::Display>| -> Result<()> {
                if let Some(error) = reason {
                    tracing::warn!(%error, "sidebar snapshot pane discovery failed; showing ledger rollup");
                }
                emit(&produce_rollup_snapshot(
                    &mut RollupCursor::new(),
                    &state,
                    &runtime,
                    exclude.as_ref(),
                    min_pane_cache_ms,
                )?)
            };
            match (session_name, mux) {
                (Some(session_name), Some(mux)) => {
                    let opts = ProduceOptions {
                        mux,
                        session_name,
                        exclude: exclude.clone(),
                        min_pane_cache_ms,
                    };
                    match produce_snapshot(&mut RollupCursor::new(), &state, &runtime, &opts) {
                        Ok(snapshot) => emit(&snapshot),
                        // The serve loop owns a live session, so a discovery
                        // failure there is real: fail hard and let the loop
                        // hold its last good frame via the degraded path,
                        // rather than flashing the raw ledger rollup (every
                        // agent the log ever saw).
                        Err(err) if explicit_session => {
                            Err(err).context("sidebar snapshot pane discovery")
                        }
                        // A bare inspection call has no live session to
                        // trust; fall back to the ledger rollup.
                        Err(err) => rollup_only(Some(&err)),
                    }
                }
                _ => rollup_only(None),
            }
        }
        SidebarSubcmd::Serve {
            workspace_id,
            mux,
            session_name,
            tick_seconds,
        } => {
            let needs_workspace_resolve = workspace_id.is_none() || session_name.is_none();
            let resolved = if needs_workspace_resolve {
                Some(WorkspaceResolver::resolve_participant(
                    ".",
                    globals.root.clone(),
                )?)
            } else {
                None
            };
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => resolved
                    .as_ref()
                    .ok_or_else(|| anyhow!("workspace_id missing but workspace was not resolved"))?
                    .workspace_id
                    .clone(),
            };
            let session_name = match session_name {
                Some(name) => name,
                None => resolved
                    .as_ref()
                    .ok_or_else(|| anyhow!("session_name missing but workspace was not resolved"))?
                    .session_name
                    .clone(),
            };
            let mux = match mux {
                Some(mux) => mux,
                None => rimz::mux::auto_detect_backend(globals.mux)?,
            };
            let program = sidebar_renderer_program();
            let mut command = Command::new(&program);
            command
                .args([
                    "serve",
                    "--workspace-id",
                    workspace_id.as_str(),
                    "--mux",
                    mux.as_str(),
                    "--session-name",
                    &session_name,
                    "--tick-seconds",
                    &tick_seconds.to_string(),
                ])
                .env("RIMZ_BIN", rimz_cli_program());
            let status = command
                .status()
                .with_context(|| format!("running `{}` serve", program.to_string_lossy()))?;
            if !status.success() {
                bail!("rimz-sidebar serve exited with {status}");
            }
            Ok(())
        }
        SidebarSubcmd::Wake {
            workspace_id,
            reason,
        } => {
            // Feather-weight by design: the poke needs only the workspace
            // runtime dir — one stamp write plus at most one datagram — so it
            // never opens the ledger, never lists panes, never touches the
            // mux. The plugin calls this per topology change.
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => {
                    WorkspaceResolver::resolve_participant(".", globals.root.clone())?.workspace_id
                }
            };
            let runtime =
                RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
            // Both reasons refresh the stamp that flips the producer's pane
            // TTL to event mode; the write is best-effort cache-class — a
            // miss only means the channel reads as dead one poke longer.
            write_presence_stamp(&runtime);
            if let WakeReason::PanesChanged = reason {
                // Topology changed: nudge the eldest sidebar (the elected
                // producer) into a fresh-panes fetch. Eldest-only — the word
                // maps to a force-produce, so a broadcast would fork an N-way
                // produce storm. Best-effort: no live sidebar is fine.
                if let Err(err) = rimz::ledger::wakeup::wake_eldest_sidebar_panes_changed(&runtime)
                {
                    tracing::debug!(error = %err, "presence poke: eldest datagram failed");
                }
            }
            Ok(())
        }
    }
}

fn session_name_from_record(state: &StatePaths) -> Option<String> {
    workspace_record::read(&state.workspace_record)
        .ok()
        .map(|record| record.session_name)
}

pub(crate) fn sidebar_renderer_program() -> PathBuf {
    if let Some(path) = env_path("RIMZ_SIDEBAR_BIN") {
        return path;
    }
    if let Some(path) = sibling_bin("rimz-sidebar").filter(|path| path.is_file()) {
        return path;
    }
    which::which(bin_name("rimz-sidebar"))
        .unwrap_or_else(|_| PathBuf::from(bin_name("rimz-sidebar")))
}

pub(crate) fn sidebar_renderer_present() -> bool {
    if let Some(path) = env_path("RIMZ_SIDEBAR_BIN") {
        return path.is_file();
    }
    sibling_bin("rimz-sidebar").is_some_and(|path| path.is_file())
        || which::which(bin_name("rimz-sidebar")).is_ok()
}

/// A sibling of the running executable, named `stem` with the platform suffix.
fn sibling_bin(stem: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    Some(current.parent()?.join(bin_name(stem)))
}

fn bin_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn rimz_cli_program() -> PathBuf {
    env_path("RIMZ_BIN")
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}
