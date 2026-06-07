//! `rimz sidebar` — `snapshot` renders the view-model (producer or `--no-produce` consumer read); `serve` runs the terminal renderer loop.
//!
//! The snapshot arm is a thin delegate over the library produce pipeline
//! ([`rimz::sidebar::produce`]): it resolves workspace/session/mux, calls
//! `produce_snapshot` (or the in-process consumer read for `--no-produce`),
//! and emits — the CLI owns argv, fallback intent, and stdout alone. The
//! elder renderer produces in process on its fetch worker, so this arm serves
//! inspection, scripting, and the plugin rail's `--no-produce` read.

use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::paths::env_path;
use rimz::ledger::workspace_record;
use rimz::schema::sidebar_event::SidebarEvent;
use rimz::sidebar::consumer::read_published_snapshot;
use rimz::sidebar::produce::{
    ProduceOptions, pane_fixture_active, produce_rollup_snapshot, produce_snapshot,
};
use rimz::sidebar::{cache::write_presence_stamp, consumer::RollupCursor};
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
    /// Read a snapshot JSON from stdin and render one fixed frame.
    Render {
        #[arg(long, default_value_t = 80)]
        width: u16,
        #[arg(long, default_value_t = 24)]
        height: u16,
    },
    /// Presence poke from the Zellij presence plugin: refresh the liveness
    /// stamp and wake the sidebar fleet through either an exact-cache shortcut
    /// or a producer refetch. Hidden — plugin infrastructure, not a human verb.
    #[command(hide = true)]
    Wake {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long, value_enum)]
        reason: WakeReason,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long)]
        pane_id: Option<String>,
        #[arg(long = "command-arg")]
        command_args: Vec<String>,
        #[arg(long = "focused-pane-id")]
        focused_pane_ids: Vec<String>,
        #[arg(long = "unfocused-pane-id")]
        unfocused_pane_ids: Vec<String>,
    },
}

/// Why a presence poke fired. Every reason refreshes the liveness stamp;
/// `alive` is the plugin's keepalive — stamp-only — so an idle-but-healthy
/// channel stays distinguishable from a dead one.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum WakeReason {
    PanesChanged,
    PaneOpened,
    PaneClosed,
    FocusStranded,
    CommandChanged,
    FocusChanged,
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
            // A producer reads `list-panes`/git and publishes the shared cache;
            // a non-producer renders read-only from that cache. Default is to
            // produce, so bare CLI calls and the plugin rail are unchanged.
            let produce = !no_produce;
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
            let runtime = RuntimePaths::for_workspace(workspace_id.clone())
                .context("preparing runtime paths")?;
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
            // cold cache (no publish yet) returns the bare rollup with the
            // same read-only enrichments until the next tick. One-shot CLI
            // process, so a fresh cursor (a cold fold) is the only kind.
            // A pane fixture defers to the produce path, which short-circuits
            // on it — deterministic tests neither poison nor read the cache.
            if !produce
                && !pane_fixture_active()
                && let Some(session) = session_name.as_deref()
            {
                let snapshot = read_published_snapshot(
                    &mut RollupCursor::new(),
                    &state,
                    &runtime,
                    session,
                    exclude.as_ref(),
                )
                .context("reading the consumer snapshot")?;
                return emit(&snapshot);
            }

            // Producer (or a deterministic test fixture, or a bare inspection
            // call): the library pipeline resolves the base — ledger rollup
            // plus live pane list, single-flighted across the fleet — folds
            // the producer enrichments, and publishes the caches consumers
            // read. With no session or no detectable mux there is no pane
            // frame to produce; the frameless arm runs the same metadata
            // enrichments over the bare rollup and emits no groups.
            let mux = mux
                .or(globals.mux)
                .or_else(|| rimz::mux::auto_detect_backend(None).ok());
            let rollup_only = |reason: Option<&dyn std::fmt::Display>| -> Result<()> {
                if let Some(error) = reason {
                    tracing::warn!(%error, "sidebar snapshot pane discovery failed; emitting frameless rollup metadata");
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
                        // An inspection call has no live frame to hold (the
                        // serve loop produces in process and owns its own
                        // degraded path); fall back to the ledger rollup.
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
            rimz::sidebar_renderer::app::serve(rimz::sidebar_renderer::app::ServeConfig {
                workspace_id,
                mux,
                session_name,
                instance_id: SidebarInstanceId::new(),
                tick_seconds,
            })
            .context("serving sidebar")
        }
        SidebarSubcmd::Render { width, height } => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("reading stdin")?;
            let snapshot = serde_json::from_str(&buf).context("parsing snapshot from stdin")?;
            rimz::sidebar_renderer::render::render_fixed(
                io::stdout(),
                &snapshot,
                None,
                width,
                height,
            )
            .context("rendering snapshot")
        }
        SidebarSubcmd::Wake {
            workspace_id,
            reason,
            session_name,
            pane_id,
            command_args,
            focused_pane_ids,
            unfocused_pane_ids,
        } => {
            // Feather-weight by design: the poke needs only the workspace
            // runtime dir — one stamp write plus at most one datagram — so it
            // never opens the ledger, never lists panes, never touches the
            // mux. The plugin calls this per presence event.
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => {
                    WorkspaceResolver::resolve_participant(".", globals.root.clone())?.workspace_id
                }
            };
            let runtime =
                RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
            // Every reason refreshes the stamp that flips the producer's pane
            // TTL to event mode; the write is best-effort cache-class — a miss
            // only means the channel reads as dead one poke longer.
            write_presence_stamp(&runtime);
            let Some(event) = wake_event(
                reason,
                pane_id.as_deref(),
                &command_args,
                &focused_pane_ids,
                &unfocused_pane_ids,
            ) else {
                return Ok(());
            };
            if let Err(err) = rimz::ledger::wakeup::broadcast_sidebar_event(
                &runtime,
                session_name.as_deref(),
                event,
            ) {
                tracing::debug!(error = %err, "presence poke: event datagram failed");
            }
            Ok(())
        }
    }
}

/// Map a poke reason onto its typed event. `None` means the poke carries no
/// event of its own (`alive` is stamp-only). Producer-verifying pane reasons
/// missing their pane data degrade to the identity-free `PanesChanged` nudge,
/// so a sparse poke still triggers the producer's verifying pull.
fn wake_event(
    reason: WakeReason,
    pane_id: Option<&str>,
    command_args: &[String],
    focused_pane_ids: &[String],
    unfocused_pane_ids: &[String],
) -> Option<SidebarEvent> {
    let zellij_pane = |raw: &str| rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw);
    match reason {
        WakeReason::Alive => None,
        WakeReason::PanesChanged => Some(SidebarEvent::PanesChanged),
        WakeReason::PaneOpened => Some(match pane_id {
            Some(pane_id) => SidebarEvent::PaneOpened {
                pane_id: zellij_pane(pane_id),
                command: command_from_args(command_args),
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::PaneClosed => Some(match pane_id {
            Some(pane_id) => SidebarEvent::PaneClosed {
                pane_id: zellij_pane(pane_id),
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::FocusStranded => pane_id.map(|pane_id| SidebarEvent::FocusStranded {
            pane_id: zellij_pane(pane_id),
        }),
        WakeReason::CommandChanged => Some(match pane_id.zip(command_from_args(command_args)) {
            Some((pane_id, command)) => SidebarEvent::CommandChanged {
                pane_id: zellij_pane(pane_id),
                command,
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::FocusChanged => Some(SidebarEvent::FocusChanged {
            focused: zellij_pane_ids(focused_pane_ids),
            unfocused: zellij_pane_ids(unfocused_pane_ids),
        }),
    }
}

fn zellij_pane_ids(raws: &[String]) -> Vec<rimz::ids::PaneId> {
    raws.iter()
        .filter(|raw| !raw.is_empty())
        .map(|raw| rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw))
        .collect()
}

fn command_from_args(args: &[String]) -> Option<String> {
    let command = args
        .iter()
        .filter(|arg| !arg.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

fn session_name_from_record(state: &StatePaths) -> Option<String> {
    workspace_record::read(&state.workspace_record)
        .ok()
        .map(|record| record.session_name)
}

fn bin_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn rimz_cli_program() -> PathBuf {
    env_path("RIMZ_BIN")
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}
