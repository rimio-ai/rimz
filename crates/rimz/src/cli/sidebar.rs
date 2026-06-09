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
use clap::{Args, Subcommand, ValueEnum};

use super::GlobalFlags;
use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
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

mod fixture;
mod wake;

pub(crate) use wake::rimz_cli_program;

use fixture::sidebar_fixture_snapshot;
use wake::{session_name_from_record, wake_event, write_topology_cache};
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
        #[arg(long)]
        refresh_ms: Option<u16>,
    },
    /// Read a snapshot JSON from stdin and render one fixed frame.
    Render {
        #[arg(long, default_value_t = 80)]
        width: u16,
        #[arg(long, default_value_t = 24)]
        height: u16,
    },
    /// Render a deterministic sidebar fixture frame. Hidden — contributor
    /// screenshot infrastructure, not a user-facing sidebar verb.
    #[command(hide = true)]
    Fixture {
        #[arg(value_enum)]
        state: SidebarFixtureState,
        #[arg(long, default_value_t = 54)]
        width: u16,
        #[arg(long, default_value_t = 34)]
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
        #[arg(long = "topology", hide = true)]
        topology: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SidebarFixtureState {
    Empty,
    Fleet,
    Provider,
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
        } => snapshot(
            globals,
            SnapshotCommand {
                workspace_id,
                mux,
                session_name,
                exclude_pane_id,
                min_pane_cache_ms,
                json,
                no_produce,
            },
        ),
        SidebarSubcmd::Serve {
            workspace_id,
            mux,
            session_name,
            tick_seconds,
            refresh_ms,
        } => serve(
            globals,
            workspace_id,
            mux,
            session_name,
            tick_seconds,
            refresh_ms,
        ),
        SidebarSubcmd::Render { width, height } => render(width, height),
        SidebarSubcmd::Fixture {
            state,
            width,
            height,
        } => fixture(state, width, height),
        SidebarSubcmd::Wake {
            workspace_id,
            reason,
            session_name,
            pane_id,
            command_args,
            focused_pane_ids,
            unfocused_pane_ids,
            topology,
        } => wake(
            globals,
            WakeCommand {
                workspace_id,
                reason,
                session_name,
                pane_id,
                command_args,
                focused_pane_ids,
                unfocused_pane_ids,
                topology,
            },
        ),
    }
}

struct SnapshotCommand {
    workspace_id: Option<String>,
    mux: Option<MuxName>,
    session_name: Option<String>,
    exclude_pane_id: Option<String>,
    min_pane_cache_ms: Option<u64>,
    json: bool,
    no_produce: bool,
}

struct SnapshotContext {
    state: StatePaths,
    runtime: RuntimePaths,
    session_name: Option<String>,
    exclude: Option<PaneId>,
    min_pane_cache_ms: Option<u64>,
}

fn snapshot(globals: &GlobalFlags, command: SnapshotCommand) -> Result<()> {
    let context = resolve_snapshot_context(globals, &command)?;
    if try_emit_consumer_snapshot(&context, !command.no_produce, command.json)? {
        return Ok(());
    }
    emit_producer_snapshot(&context, command.mux, globals, command.json)
}

fn resolve_snapshot_context(
    globals: &GlobalFlags,
    command: &SnapshotCommand,
) -> Result<SnapshotContext> {
    let mut resolved_session = None;
    let workspace_id = match command.workspace_id.as_deref() {
        Some(raw) => raw.parse::<WorkspaceId>()?,
        None => {
            let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
            resolved_session = Some(workspace.session_name.clone());
            workspace.workspace_id
        }
    };
    let state = StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    state.ensure_dirs().context("preparing state paths")?;
    runtime.ensure_dirs().context("preparing runtime paths")?;
    let session_name = command
        .session_name
        .clone()
        .or(resolved_session)
        .or_else(|| session_name_from_record(&state));
    let exclude = command
        .exclude_pane_id
        .as_deref()
        .map(PaneId::parse)
        .transpose()?;
    Ok(SnapshotContext {
        state,
        runtime,
        session_name,
        exclude,
        min_pane_cache_ms: command.min_pane_cache_ms,
    })
}

fn try_emit_consumer_snapshot(
    context: &SnapshotContext,
    produce: bool,
    json: bool,
) -> Result<bool> {
    if produce || pane_fixture_active() {
        return Ok(false);
    }
    let Some(session) = context.session_name.as_deref() else {
        return Ok(false);
    };
    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &context.state,
        &context.runtime,
        session,
        context.exclude.as_ref(),
    )
    .context("reading the consumer snapshot")?;
    emit_snapshot(&snapshot, json)?;
    Ok(true)
}

fn emit_producer_snapshot(
    context: &SnapshotContext,
    mux: Option<MuxName>,
    globals: &GlobalFlags,
    json: bool,
) -> Result<()> {
    let mux = mux
        .or(globals.mux)
        .or_else(|| rimz::mux::auto_detect_backend(None).ok());
    let (Some(session_name), Some(mux)) = (context.session_name.clone(), mux) else {
        return emit_rollup_snapshot(context, json, None);
    };
    let opts = ProduceOptions {
        mux,
        session_name,
        exclude: context.exclude.clone(),
        min_pane_cache_ms: context.min_pane_cache_ms,
    };
    match produce_snapshot(
        &mut RollupCursor::new(),
        &context.state,
        &context.runtime,
        &opts,
    ) {
        Ok(snapshot) => emit_snapshot(&snapshot, json),
        Err(err) => emit_rollup_snapshot(context, json, Some(&err)),
    }
}

fn emit_rollup_snapshot(
    context: &SnapshotContext,
    json: bool,
    reason: Option<&dyn std::fmt::Display>,
) -> Result<()> {
    if let Some(error) = reason {
        tracing::warn!(%error, "sidebar snapshot pane discovery failed; emitting frameless rollup metadata");
    }
    let snapshot = produce_rollup_snapshot(
        &mut RollupCursor::new(),
        &context.state,
        &context.runtime,
        context.exclude.as_ref(),
        context.min_pane_cache_ms,
    )?;
    emit_snapshot(&snapshot, json)
}

fn emit_snapshot(snapshot: &rimz::SidebarSnapshot, json: bool) -> Result<()> {
    if json {
        let rendered = serde_json::to_string_pretty(snapshot)?;
        #[expect(clippy::print_stdout, reason = "json emitter for sidebar")]
        {
            println!("{rendered}");
        }
    } else {
        let waiting = status_tally(snapshot, rimz::feed::AgentStatus::Waiting);
        let failed = status_tally(snapshot, rimz::feed::AgentStatus::Failed);
        #[expect(clippy::print_stdout, reason = "human summary")]
        {
            println!("Workspace:       {}", snapshot.display_name);
            println!("Worktree groups: {}", snapshot.worktree_groups.len());
            println!("Waiting:         {waiting}");
            println!("Failed:          {failed}");
        }
    }
    Ok(())
}

fn status_tally(snapshot: &rimz::SidebarSnapshot, status: rimz::feed::AgentStatus) -> usize {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .filter(|count| count.status == status)
        .map(|count| count.count)
        .sum()
}

fn serve(
    globals: &GlobalFlags,
    workspace_id: Option<String>,
    mux: Option<MuxName>,
    session_name: Option<String>,
    tick_seconds: u64,
    refresh_ms: Option<u16>,
) -> Result<()> {
    let (workspace_id, session_name) = resolve_serve_identity(globals, workspace_id, session_name)?;
    let mux = match mux {
        Some(mux) => mux,
        None => rimz::mux::auto_detect_backend(globals.mux)?,
    };
    rimz::sidebar_pane::app::serve(rimz::sidebar_pane::app::ServeConfig {
        workspace_id,
        mux,
        session_name,
        instance_id: SidebarInstanceId::new(),
        tick_seconds,
        refresh_ms_override: refresh_ms,
        notification_prefs: rimz::config::MachineConfig::load()
            .unwrap_or_default()
            .notifications,
        own_pane: rimz::mux::own_pane_id(mux),
    })
    .context("serving sidebar")
}

fn resolve_serve_identity(
    globals: &GlobalFlags,
    workspace_id: Option<String>,
    session_name: Option<String>,
) -> Result<(WorkspaceId, String)> {
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
    Ok((workspace_id, session_name))
}

fn render(width: u16, height: u16) -> Result<()> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    let snapshot = serde_json::from_str(&buf).context("parsing snapshot from stdin")?;
    rimz::sidebar_pane::render::render_fixed(io::stdout(), &snapshot, None, width, height)
        .context("rendering snapshot")
}

fn fixture(state: SidebarFixtureState, width: u16, height: u16) -> Result<()> {
    let snapshot = sidebar_fixture_snapshot(state)?;
    rimz::sidebar_pane::render::render_fixed_line_ansi(io::stdout(), &snapshot, None, width, height)
        .context("rendering sidebar fixture")
}

struct WakeCommand {
    workspace_id: Option<String>,
    reason: WakeReason,
    session_name: Option<String>,
    pane_id: Option<String>,
    command_args: Vec<String>,
    focused_pane_ids: Vec<String>,
    unfocused_pane_ids: Vec<String>,
    topology: Option<String>,
}

fn wake(globals: &GlobalFlags, command: WakeCommand) -> Result<()> {
    let workspace_id = match command.workspace_id.as_deref() {
        Some(raw) => raw.parse::<WorkspaceId>()?,
        None => WorkspaceResolver::resolve_participant(".", globals.root.clone())?.workspace_id,
    };
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    write_presence_stamp(&runtime);
    write_topology_cache(&runtime, command.topology.as_deref());
    let Some(event) = wake_event(
        command.reason,
        command.pane_id.as_deref(),
        &command.command_args,
        &command.focused_pane_ids,
        &command.unfocused_pane_ids,
    ) else {
        return Ok(());
    };
    broadcast_wake_event(&runtime, command.session_name.as_deref(), event);
    Ok(())
}

fn broadcast_wake_event(runtime: &RuntimePaths, session_name: Option<&str>, event: SidebarEvent) {
    if let Err(err) = rimz::ledger::wakeup::broadcast_sidebar_event(runtime, session_name, event) {
        tracing::debug!(error = %err, "presence poke: event datagram failed");
    }
}

#[cfg(test)]
mod tests;
