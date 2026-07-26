//! `rimz sidebar` — inspect, serve, and structurally repair the sidebar fleet.
//!
//! Snapshot inspection and one-shot frame rendering are thin delegates over the
//! library data plane. Snapshot resolves workspace/session/mux and calls
//! `produce_snapshot_with_refresh` (or the in-process consumer read for
//! `--no-produce`); frame prefers an already-published consumer frame and falls
//! back to the same producer path. The CLI owns argv, fallback intent, and
//! stdout alone. The elder renderer produces in process on its fetch worker, so
//! these arms serve inspection and scripting.

#[cfg(feature = "testkit")]
use std::io;
use std::io::Write;

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgAction, Args, Subcommand};

use super::{GlobalFlags, current_channel, open_store};
use crate::cli::render;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::sidebar::consumer::{PublishedSnapshotReader, RollupCursor, published_frame_exists};
use rimz::sidebar::events::SidebarEvent;
use rimz::sidebar::notify::{Notification, NotificationAgent, NotificationKind};
use rimz::sidebar::presence::{
    ZellijPluginTelemetry, ZellijWake, ZellijWakeOutcome, ZellijWakeReason, ingest_zellij_wake,
};
use rimz::sidebar::produce::{
    ProduceOptions, pane_fixture_active, produce_rollup_snapshot_with_refresh,
    produce_snapshot_with_refresh,
};
use rimz::store::workspace_record;
use rimz::workspace::WorkspaceResolver;
use rimz::{PaneAgent, RuntimePaths, SidebarRow, SidebarSnapshot, StatePaths};

#[cfg(feature = "testkit")]
mod fixture;

#[cfg(feature = "testkit")]
use fixture::sidebar_fixture_snapshot;
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
    /// Render the live sidebar snapshot once without capturing a mux pane.
    Frame {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        width: Option<u16>,
        #[arg(
            long,
            conflicts_with = "expand",
            value_parser = clap::value_parser!(u16).range(1..)
        )]
        height: Option<u16>,
        /// Every card expanded, every group un-truncated; the frame grows to fit.
        #[arg(long)]
        expand: bool,
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
    /// Repair missing, duplicate, wedged, or mis-docked sidebar panes without
    /// publishing a new build.
    Repair,
    /// Render a deterministic sidebar fixture frame. Hidden — contributor
    /// screenshot infrastructure, not a user-facing sidebar verb.
    #[cfg(feature = "testkit")]
    #[command(hide = true)]
    Fixture {
        #[arg(value_enum)]
        state: SidebarFixtureState,
        #[arg(long, default_value_t = 54)]
        width: u16,
        #[arg(long, default_value_t = 34)]
        height: u16,
        #[arg(long)]
        watch: bool,
        #[arg(long)]
        theme_mode: Option<String>,
        #[arg(long)]
        theme_scheme: Option<String>,
    },
    /// Open a live sidebar feature gallery in a room tab or inline outside a
    /// mux session. Hidden — contributor visual review tool.
    #[cfg(feature = "testkit")]
    #[command(hide = true)]
    Gallery {
        #[arg(long)]
        pets: bool,
    },
    /// Render the live sidebar gallery compositor. Hidden — launched by
    /// `sidebar gallery`, not a user-facing sidebar verb.
    #[cfg(feature = "testkit")]
    #[command(hide = true)]
    GalleryRender {
        #[arg(long)]
        pets: bool,
    },
    /// Presence poke from the Zellij presence plugin: refresh the liveness
    /// stamp and wake the sidebar fleet through either an exact-cache shortcut
    /// or a producer refetch. Hidden — plugin infrastructure, not a human verb.
    #[command(hide = true)]
    Wake(Box<WakeArgs>),
    /// Write a read receipt for a sidebar row. Hidden — test/API machinery.
    #[command(hide = true)]
    MarkRead {
        target: String,
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Open an unread episode for a sidebar row. Hidden — test/API machinery.
    #[command(hide = true)]
    MarkUnread {
        target: String,
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Focus the session's sidebar pane — the global focus-key target. With
    /// `--toggle`, return to the last working pane when already on the sidebar.
    /// The tmux keybind passes `--session-name` (resolved per keypress); a bare
    /// invocation resolves the room from the cwd. Hidden — the keybind and
    /// scripts call it.
    #[command(hide = true)]
    Focus {
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long)]
        toggle: bool,
    },
    /// Append renderer-owned focus-repair evidence. Hidden — detached helper
    /// infrastructure, not a human verb.
    #[command(hide = true)]
    RecordFocusRepair {
        #[arg(long)]
        record_json: String,
    },
    /// Exercise sidebar notification delivery. Hidden — test/API machinery.
    #[command(hide = true)]
    NotifyTest {
        target: String,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, default_value = "waiting")]
        kind: String,
        #[arg(long)]
        force_bell: bool,
        #[arg(long)]
        no_command: bool,
    },
}

#[derive(Debug, Args)]
struct WakeArgs {
    #[arg(long)]
    workspace_id: Option<String>,
    #[arg(long, value_enum)]
    reason: WakeReason,
    #[arg(long)]
    session_name: Option<String>,
    #[arg(long)]
    pane_id: Option<String>,
    #[arg(long, hide = true)]
    active_tab: Option<u64>,
    #[arg(long, hide = true)]
    focus_generation: Option<u64>,
    #[arg(long, hide = true)]
    focus_clients: Option<String>,
    #[arg(long = "command-arg", hide = true)]
    _command_args: Vec<String>,
    #[arg(long = "focused-pane-id", hide = true)]
    _focused_pane_ids: Vec<String>,
    #[arg(long = "unfocused-pane-id", hide = true)]
    _unfocused_pane_ids: Vec<String>,
    #[arg(
        long = "topology",
        hide = true,
        action = ArgAction::Append,
        allow_hyphen_values = true
    )]
    topology: Vec<String>,
    #[arg(long, hide = true, value_parser = parse_plugin_telemetry)]
    plugin_telemetry: Option<ZellijPluginTelemetry>,
    // Compatibility bridge: remove after no supported vendored or shared
    // presence-plugin artifact emits the split telemetry flags.
    #[arg(long, hide = true)]
    plugin_mem_pages: Option<u64>,
    #[arg(long, hide = true)]
    plugin_id: Option<u32>,
    #[arg(long, hide = true)]
    plugin_build: Option<String>,
    #[arg(long, hide = true)]
    plugin_loaded_at_ms: Option<u64>,
    #[arg(long, hide = true)]
    plugin_uptime_ms: Option<u64>,
    #[arg(long, hide = true)]
    plugin_commands: Option<u64>,
    #[arg(long, hide = true)]
    plugin_commands_succeeded: Option<u64>,
    /// Accepted so a lagging plugin's wake still parses; the value is
    /// `completed - succeeded`, which the host derives when it needs it.
    #[arg(long = "plugin-commands-failed", hide = true)]
    _plugin_commands_failed: Option<u64>,
    #[arg(long, hide = true)]
    plugin_stale_writer_rejections: Option<u64>,
    #[arg(long, hide = true)]
    plugin_topology_failures: Option<u64>,
    #[arg(long, hide = true)]
    plugin_other_failures: Option<u64>,
    #[arg(long, hide = true)]
    plugin_zellij_version: Option<String>,
}

fn parse_plugin_telemetry(raw: &str) -> std::result::Result<ZellijPluginTelemetry, String> {
    serde_json::from_str(raw).map_err(|err| format!("invalid plugin telemetry JSON: {err}"))
}

impl WakeArgs {
    fn telemetry(&self) -> Option<ZellijPluginTelemetry> {
        if let Some(telemetry) = &self.plugin_telemetry {
            return Some(telemetry.clone());
        }
        self.plugin_mem_pages.map(|pages| ZellijPluginTelemetry {
            plugin_id: self.plugin_id,
            build: self.plugin_build.clone(),
            loaded_at_ms: self.plugin_loaded_at_ms.unwrap_or_default(),
            pages,
            uptime_ms: self.plugin_uptime_ms.unwrap_or_default(),
            commands: self.plugin_commands.unwrap_or_default(),
            commands_succeeded: self.plugin_commands_succeeded,
            stale_writer_rejections: self.plugin_stale_writer_rejections,
            topology_failures: self.plugin_topology_failures,
            other_failures: self.plugin_other_failures,
            zellij_version: self.plugin_zellij_version.clone(),
            // Split-flag builds predate failure evidence and never send one.
            last_failure: None,
        })
    }
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
    SwitchSettled,
    CommandChanged,
    FocusChanged,
    Alive,
}

impl From<WakeReason> for ZellijWakeReason {
    fn from(reason: WakeReason) -> Self {
        match reason {
            WakeReason::PanesChanged
            | WakeReason::PaneOpened
            | WakeReason::PaneClosed
            | WakeReason::CommandChanged
            | WakeReason::FocusChanged => Self::Announced,
            WakeReason::FocusStranded => Self::FocusStranded,
            WakeReason::SwitchSettled => Self::SwitchSettled,
            WakeReason::Alive => Self::Alive,
        }
    }
}

#[derive(serde::Deserialize)]
struct ZellijFocusClientWire {
    client_id: u32,
    pane_id: ZellijFocusPaneWire,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum ZellijFocusPaneWire {
    Terminal(u64),
    Plugin(u64),
}

fn parse_zellij_focus_clients(raw: &str) -> Vec<rimz::mux::ClientPaneView> {
    let Ok(mut clients) = serde_json::from_str::<Vec<ZellijFocusClientWire>>(raw) else {
        tracing::debug!("presence poke: focus client evidence parse failed");
        return Vec::new();
    };
    let mut projected = clients
        .drain(..)
        .map(|client| rimz::mux::ClientPaneView {
            client_id: rimz::mux::MuxClientId::Zellij(client.client_id),
            pane_id: match client.pane_id {
                ZellijFocusPaneWire::Terminal(id) => {
                    PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}"))
                }
                ZellijFocusPaneWire::Plugin(id) => {
                    PaneId::from_parts(MuxName::Zellij, format!("plugin_{id}"))
                }
            },
        })
        .collect::<Vec<_>>();
    projected.sort();
    projected.dedup();
    projected
}

#[cfg(feature = "testkit")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum SidebarFixtureState {
    Empty,
    Fleet,
    Provider,
    Cockpit,
    Focus,
    Economy,
    Reach,
}

impl SidebarArgs {
    /// The low-cardinality command label for the Sentry command scope.
    pub(crate) fn command_label(&self) -> &'static str {
        match &self.command {
            SidebarSubcmd::Snapshot { .. } => "sidebar snapshot",
            SidebarSubcmd::Frame { .. } => "sidebar frame",
            SidebarSubcmd::Serve { .. } => "sidebar serve",
            SidebarSubcmd::Repair => "sidebar repair",
            #[cfg(feature = "testkit")]
            SidebarSubcmd::Fixture { .. } => "sidebar fixture",
            #[cfg(feature = "testkit")]
            SidebarSubcmd::Gallery { .. } => "sidebar gallery",
            #[cfg(feature = "testkit")]
            SidebarSubcmd::GalleryRender { .. } => "sidebar gallery-render",
            SidebarSubcmd::Wake { .. } => "sidebar wake",
            SidebarSubcmd::MarkRead { .. } => "sidebar mark-read",
            SidebarSubcmd::MarkUnread { .. } => "sidebar mark-unread",
            SidebarSubcmd::Focus { .. } => "sidebar focus",
            SidebarSubcmd::RecordFocusRepair { .. } => "sidebar record-focus-repair",
            SidebarSubcmd::NotifyTest { .. } => "sidebar notify-test",
        }
    }
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
        SidebarSubcmd::Frame {
            workspace_id,
            mux,
            session_name,
            width,
            height,
            expand,
        } => frame(
            globals,
            FrameCommand {
                workspace_id,
                mux,
                session_name,
                width,
                height,
                expand,
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
        SidebarSubcmd::Repair => repair(globals),
        #[cfg(feature = "testkit")]
        SidebarSubcmd::Fixture {
            state,
            width,
            height,
            watch,
            theme_mode,
            theme_scheme,
        } => fixture(state, width, height, watch, theme_mode, theme_scheme),
        #[cfg(feature = "testkit")]
        SidebarSubcmd::Gallery { pets } => gallery(globals, pets),
        #[cfg(feature = "testkit")]
        SidebarSubcmd::GalleryRender { pets } => gallery_render(pets),
        SidebarSubcmd::Wake(args) => {
            let telemetry = args.telemetry();
            let WakeArgs {
                workspace_id,
                reason,
                session_name,
                pane_id,
                active_tab,
                focus_generation,
                focus_clients,
                _command_args: _,
                _focused_pane_ids: _,
                _unfocused_pane_ids: _,
                topology,
                plugin_telemetry: _,
                plugin_mem_pages: _,
                plugin_id: _,
                plugin_build: _,
                plugin_loaded_at_ms: _,
                plugin_uptime_ms: _,
                plugin_commands: _,
                plugin_commands_succeeded: _,
                _plugin_commands_failed: _,
                plugin_stale_writer_rejections: _,
                plugin_topology_failures: _,
                plugin_other_failures: _,
                plugin_zellij_version: _,
            } = *args;
            wake(
                globals,
                workspace_id,
                ZellijWake {
                    reason: reason.into(),
                    session_name,
                    pane_id: pane_id.map(|raw| PaneId::from_parts(MuxName::Zellij, raw)),
                    active_tab,
                    focus_generation,
                    focus_clients: focus_clients
                        .as_deref()
                        .map(parse_zellij_focus_clients)
                        .unwrap_or_default(),
                    topology: (!topology.is_empty()).then(|| topology.concat()).and_then(|raw| {
                        match serde_json::from_str(&raw) {
                            Ok(cache) => Some(cache),
                            Err(err) => {
                                tracing::debug!(error = %err, "presence poke: topology payload parse failed");
                                None
                            }
                        }
                    }),
                    telemetry,
                },
            )
        }
        SidebarSubcmd::MarkRead { target, worktree } => mark_read(globals, target, worktree),
        SidebarSubcmd::MarkUnread { target, worktree } => mark_unread(globals, target, worktree),
        SidebarSubcmd::Focus {
            session_name,
            toggle,
        } => focus(globals, session_name, toggle),
        SidebarSubcmd::RecordFocusRepair { record_json } => {
            let record = rimz::diag::focus_repair::parse(&record_json)
                .context("parsing focus-repair diagnostic record")?;
            rimz::diag::focus_repair::append(&record);
            Ok(())
        }
        SidebarSubcmd::NotifyTest {
            target,
            worktree,
            title,
            body,
            kind,
            force_bell,
            no_command,
        } => notify_test(
            globals,
            NotifyTestCommand {
                target,
                worktree,
                title,
                body,
                kind,
                force_bell,
                no_command,
            },
        ),
    }
}

/// Run the structural pass directly; `rimz reload --repair` composes this
/// entry after its independent upgrade transaction.
pub(crate) fn repair(_globals: &GlobalFlags) -> Result<()> {
    let outcome = rimz::reload::repair_user_sidebars();
    let mut out = render::out();
    let n = |count: usize, noun: &str| {
        let value = if count == 1 {
            format!("1 {noun}")
        } else if noun.ends_with("process") {
            format!("{count} {noun}es")
        } else {
            format!("{count} {noun}s")
        };
        render::paint(render::palette::accent(), &value)
    };
    if outcome.sessions == 0 {
        writeln!(out, "No running sidebars to repair.")?;
        return Ok(());
    }
    if outcome.presence_dead > 0 {
        writeln!(
            out,
            "No live presence channel for {}; repair skipped. Reattach or restart the session.",
            n(outcome.presence_dead, "session"),
        )?;
    }
    if outcome.recovered > 0 {
        writeln!(
            out,
            "Recovered {} in place.",
            n(outcome.recovered, "sidebar")
        )?;
    }
    if outcome.closed > 0 {
        writeln!(
            out,
            "Closed {}.",
            n(outcome.closed, "duplicate or unresponsive sidebar"),
        )?;
    }
    if outcome.redocked > 0 {
        writeln!(out, "Repaired {} geometry.", n(outcome.redocked, "sidebar"))?;
    }
    if outcome.reaped > 0 {
        writeln!(
            out,
            "Reaped {}.",
            n(outcome.reaped, "orphaned sidebar process")
        )?;
    }
    if outcome.misdocked > 0 {
        writeln!(
            out,
            "{} still working but not docked.",
            n(outcome.misdocked, "sidebar"),
        )?;
    }
    if outcome.deferred > 0 {
        writeln!(
            out,
            "Deferred {} (no attached client); attach and re-run `rimz sidebar repair`.",
            n(outcome.deferred, "sidebar repair"),
        )?;
    }
    if outcome.failed > 0 {
        writeln!(
            out,
            "{} could not be repaired; attach and re-run `rimz sidebar repair`.",
            n(outcome.failed, "sidebar"),
        )?;
    }
    if outcome.presence_dead
        + outcome.recovered
        + outcome.closed
        + outcome.redocked
        + outcome.reaped
        + outcome.misdocked
        + outcome.deferred
        + outcome.failed
        == 0
    {
        writeln!(
            out,
            "Sidebar structure is healthy across {}.",
            n(outcome.sessions, "session")
        )?;
    }
    Ok(())
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

struct FrameCommand {
    workspace_id: Option<String>,
    mux: Option<MuxName>,
    session_name: Option<String>,
    width: Option<u16>,
    height: Option<u16>,
    expand: bool,
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
    let snapshot = producer_snapshot(&context, command.mux, globals)?;
    emit_snapshot(&snapshot, command.json)
}

fn frame(globals: &GlobalFlags, command: FrameCommand) -> Result<()> {
    let context = resolve_snapshot_context(
        globals,
        &SnapshotCommand {
            workspace_id: command.workspace_id,
            mux: command.mux,
            session_name: command.session_name,
            exclude_pane_id: None,
            min_pane_cache_ms: None,
            json: false,
            no_produce: false,
        },
    )?;
    let snapshot = if !pane_fixture_active()
        && let Some(session) = context.session_name.as_deref()
        && published_frame_exists(&context.runtime, session)
    {
        PublishedSnapshotReader::new(context.runtime.clone(), session, None)
            .read(&context.state)
            .context("reading the consumer snapshot")?
    } else {
        producer_snapshot(&context, command.mux, globals)?
    };

    let sidebar_width = rimz::mux::SidebarWidth::from_config(&snapshot.theme);
    let terminal_size = rimz::mux::detect_terminal_size();
    let width = command.width.unwrap_or_else(|| {
        terminal_size.map_or_else(
            || sidebar_width.max_cols.get(),
            |(cols, _)| {
                u16::try_from(sidebar_width.target_cols(u64::from(cols))).unwrap_or(u16::MAX)
            },
        )
    });
    let height = command
        .height
        .unwrap_or_else(|| terminal_size.map_or(24, |(_, rows)| rows));

    let mut out = render::out();
    let write = if command.expand {
        rimz::sidebar_pane::render::render_expanded_line_ansi(&mut out, &snapshot, width)
    } else {
        rimz::sidebar_pane::render::render_fixed_line_ansi(&mut out, &snapshot, None, width, height)
    };
    render::finish(write.and_then(|()| out.flush()))
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
    let runtime = super::runtime_paths_for(workspace_id)?;
    let session_name = command
        .session_name
        .clone()
        .or(resolved_session)
        .or_else(|| {
            workspace_record::read(&state.workspace_record)
                .ok()
                .map(|record| record.session_name)
        });
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
    let snapshot =
        PublishedSnapshotReader::new(context.runtime.clone(), session, context.exclude.clone())
            .read(&context.state)
            .context("reading the consumer snapshot")?;
    emit_snapshot(&snapshot, json)?;
    Ok(true)
}

fn producer_snapshot(
    context: &SnapshotContext,
    mux: Option<MuxName>,
    globals: &GlobalFlags,
) -> Result<SidebarSnapshot> {
    let mux = mux
        .or(globals.mux)
        .or_else(|| rimz::mux::auto_detect_backend(None).ok());
    let (Some(session_name), Some(mux)) = (context.session_name.clone(), mux) else {
        return rollup_snapshot_fallback(context, None);
    };
    let opts = ProduceOptions {
        mux,
        session_name,
        exclude: context.exclude.clone(),
        min_pane_cache_ms: context.min_pane_cache_ms,
        diag: rimz::diag::DiagSink::disabled(),
    };
    match produce_snapshot_with_refresh(
        &mut RollupCursor::new(),
        &context.state,
        &context.runtime,
        &opts,
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(err) => rollup_snapshot_fallback(context, Some(&err)),
    }
}

fn rollup_snapshot_fallback(
    context: &SnapshotContext,
    reason: Option<&dyn std::fmt::Display>,
) -> Result<SidebarSnapshot> {
    if let Some(error) = reason {
        tracing::warn!(%error, "pane discovery failed; falling back to the frameless rollup");
    }
    produce_rollup_snapshot_with_refresh(
        &mut RollupCursor::new(),
        &context.state,
        &context.runtime,
        context.exclude.as_ref(),
        context.min_pane_cache_ms,
    )
    .map_err(Into::into)
}

fn emit_snapshot(snapshot: &rimz::SidebarSnapshot, json: bool) -> Result<()> {
    if json {
        render::json_pretty(snapshot)
    } else {
        let waiting = status_tally(snapshot, rimz::agents::AgentStatus::Waiting);
        let failed = status_tally(snapshot, rimz::agents::AgentStatus::Failed);
        let waiting_style = if waiting > 0 {
            render::palette::warn()
        } else {
            render::palette::muted()
        };
        let failed_style = if failed > 0 {
            render::palette::alarm()
        } else {
            render::palette::muted()
        };
        let mut kv = render::KeyVals::new();
        kv.push(
            "Workspace",
            render::cell(snapshot.display_name.to_string()).fg(render::palette::accent()),
        );
        kv.push(
            "Worktree groups",
            render::cell(snapshot.worktree_groups.len().to_string()),
        );
        kv.push(
            "Waiting",
            render::cell(waiting.to_string()).fg(waiting_style),
        );
        kv.push("Failed", render::cell(failed.to_string()).fg(failed_style));
        render::finish(kv.render(&mut render::out()))
    }
}

fn status_tally(snapshot: &rimz::SidebarSnapshot, status: rimz::agents::AgentStatus) -> usize {
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
    let machine_config = rimz::config::MachineConfig::load_lenient();
    let config = rimz::sidebar_pane::app::ServeConfig {
        workspace_id,
        mux,
        session_name,
        instance_id: rimz::sidebar_pane::supervise::instance_id(),
        tick_seconds,
        refresh_ms_override: refresh_ms,
        timezone: machine_config.time_zone(),
        notification_prefs: machine_config.notifications.clone(),
        nav_keys: rimz::sidebar_pane::app::NavKeymap::from_config(&machine_config.sidebar.keys),
        own_pane: rimz::mux::own_pane_id(mux),
    };
    if rimz::sidebar_pane::supervise::is_worker() {
        match rimz::sidebar_pane::supervise::run_worker(config).context("serving sidebar")? {
            rimz::sidebar_pane::app::ServeOutcome::Stopped => Ok(()),
            rimz::sidebar_pane::app::ServeOutcome::SelfCloseRequested => {
                std::process::exit(rimz::sidebar_pane::supervise::SELF_CLOSE_EXIT_CODE)
            }
        }
    } else {
        rimz::sidebar_pane::supervise::run(config).context("supervising sidebar")
    }
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

#[cfg(feature = "testkit")]
fn fixture(
    state: SidebarFixtureState,
    width: u16,
    height: u16,
    watch: bool,
    theme_mode: Option<String>,
    theme_scheme: Option<String>,
) -> Result<()> {
    let mut snapshot = sidebar_fixture_snapshot(state)?;
    if let Some(mode) = theme_mode.as_deref() {
        snapshot.theme.mode = parse_fixture_theme_mode(mode)?;
    }
    if let Some(scheme) = theme_scheme {
        snapshot.theme.scheme = Some(scheme);
    }
    if watch {
        let refresh_ms = snapshot.theme.display.resolved_refresh_ms();
        return rimz::sidebar_pane::app::serve_fixture(snapshot, refresh_ms)
            .context("serving sidebar fixture");
    }
    rimz::sidebar_pane::render::render_fixed_line_ansi(io::stdout(), &snapshot, None, width, height)
        .context("rendering sidebar fixture")
}

#[cfg(feature = "testkit")]
fn gallery(globals: &GlobalFlags, pets: bool) -> Result<()> {
    if rimz::mux::ambient_pane_id().is_none() {
        // No room can host a tab; the compositor owns this terminal.
        return gallery_render(pets);
    }
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let machine_config = super::machine_config();
    let room = rimz::room::RoomContext::from_resolved(
        &workspace,
        machine_config,
        mux,
        rimz::room::RoomSizing::Birth,
    )?;
    let rimz_bin = rimz::proc::rimz_exe().to_string_lossy().into_owned();
    let mut argv = vec![rimz_bin, "sidebar".to_owned(), "gallery-render".to_owned()];
    if pets {
        argv.push("--pets".to_owned());
    }
    let gallery_pane = rimz::mux::PaneCmd { argv };
    room.backend()
        .open_tab(&rimz::mux::TabOptions {
            title: "gallery".to_owned(),
            panes: rimz::mux::LayoutPanes {
                columns: vec![rimz::mux::LayoutColumn {
                    panes: vec![gallery_pane],
                    stacked: false,
                }],
            },
            focus: true,
            dock_sidebar: false,
            sidebar: room.sidebar_options(&workspace.worktree_root, Vec::new(), None),
        })
        .context("opening sidebar gallery")
}

#[cfg(feature = "testkit")]
const GALLERY_PETS: [&str; 4] = ["rocky", "seedy", "fireball", "bsod"];

#[cfg(feature = "testkit")]
fn gallery_render(pets: bool) -> Result<()> {
    let machine_config = super::machine_config();
    let refresh_ms = machine_config.theme.display.resolved_refresh_ms();
    let columns = gallery_render_columns(pets, &machine_config.theme)?;
    rimz::sidebar_pane::app::serve_gallery(columns, refresh_ms).context("serving sidebar gallery")
}

#[cfg(feature = "testkit")]
fn gallery_render_columns(
    pets: bool,
    theme: &rimz::config::ThemeConfig,
) -> Result<Vec<(SidebarSnapshot, usize)>> {
    let columns = gallery_fixture_columns()
        .into_iter()
        .enumerate()
        .map(|(index, (state, selector))| {
            let mut snapshot = sidebar_fixture_snapshot(state)?;
            snapshot.theme.mode = theme.mode;
            snapshot.theme.display.pixel = theme.display.pixel;
            snapshot.theme.glyphs = theme.glyphs.clone();
            snapshot.theme.pets.glyphs = theme.pets.glyphs;
            snapshot.theme.pets.enabled = pets;
            snapshot.theme.pets.pet = GALLERY_PETS[index].to_owned();
            let selected_index = gallery_selected_index(&snapshot, selector);
            Ok((snapshot, selected_index))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(columns)
}

#[cfg(feature = "testkit")]
fn gallery_selected_index(
    snapshot: &rimz::SidebarSnapshot,
    selector: fn(&rimz::SidebarRow) -> bool,
) -> usize {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .position(selector)
        .unwrap_or(0)
}

#[cfg(feature = "testkit")]
type GallerySelector = fn(&rimz::SidebarRow) -> bool;

#[cfg(feature = "testkit")]
fn gallery_fixture_columns() -> [(SidebarFixtureState, GallerySelector); 4] {
    [
        (
            SidebarFixtureState::Focus,
            (|row: &rimz::SidebarRow| row.id == "agent:claude:planner") as GallerySelector,
        ),
        (
            SidebarFixtureState::Cockpit,
            (|row: &rimz::SidebarRow| row.id == "agent:codex:pricing") as GallerySelector,
        ),
        (SidebarFixtureState::Reach, |row: &rimz::SidebarRow| {
            row.id == "agent:pi:reach"
        }),
        (SidebarFixtureState::Economy, |row: &rimz::SidebarRow| {
            row.id == "agent:opencode:credits"
        }),
    ]
}

#[cfg(feature = "testkit")]
fn parse_fixture_theme_mode(value: &str) -> Result<rimz::config::ThemeMode> {
    match value {
        "auto" => Ok(rimz::config::ThemeMode::Auto),
        "truecolor" => Ok(rimz::config::ThemeMode::Truecolor),
        "256" => Ok(rimz::config::ThemeMode::Indexed),
        other => anyhow::bail!("unknown theme mode `{other}`; expected auto, truecolor, or 256"),
    }
}

fn wake(globals: &GlobalFlags, workspace_id: Option<String>, wake: ZellijWake) -> Result<()> {
    let workspace_id = match workspace_id.as_deref() {
        Some(raw) => raw.parse::<WorkspaceId>()?,
        None => WorkspaceResolver::resolve_participant(".", globals.root.clone())?.workspace_id,
    };
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    let state = StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
    match ingest_zellij_wake(&state, &runtime, &wake).context("ingesting Zellij presence wake")? {
        ZellijWakeOutcome::RejectedStaleWriter => {
            std::process::exit(rimz::sidebar::presence::STALE_WRITER_EXIT_CODE)
        }
        ZellijWakeOutcome::Accepted(events) => {
            for event in events {
                broadcast_wake_event(&runtime, wake.session_name.as_deref(), event);
            }
        }
    }
    Ok(())
}

fn broadcast_wake_event(runtime: &RuntimePaths, session_name: Option<&str>, event: SidebarEvent) {
    if let Err(err) = rimz::store::wakeup::broadcast_sidebar_event(runtime, session_name, event) {
        tracing::debug!(error = %err, "presence poke: event datagram failed");
    }
}

fn mark_read(globals: &GlobalFlags, target: String, worktree: Option<String>) -> Result<()> {
    let resolved = resolve_sidebar_targets(globals, &target, worktree.as_deref())?;
    let ids = resolved
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let now_ms = unix_now_ms_i64();
    rimz::sidebar::read_marks::write_manual_read_marks(&resolved.runtime, ids, now_ms)
        .context("writing manual read marks")?;
    let mut episodes = rimz::sidebar::unread::UnreadEpisodes::load(&resolved.runtime);
    let mut cleared = Vec::new();
    for row in &resolved.rows {
        if episodes.remove_reached_for_row(row, now_ms) {
            cleared.push(row);
        }
    }
    if !cleared.is_empty() {
        episodes
            .persist(&resolved.runtime)
            .context("writing unread episodes")?;
    }
    let diag = diag_for_workspace(&resolved.workspace);
    for row in cleared {
        diag.trace_notify(rimz::diag::notify::NotifyTraceEvent::UnreadCleared {
            row_id: row.id.clone(),
            label: Some(rimz::sidebar::unread::row_label(row)),
            agent_kind: Some(AgentKind::new_unchecked(row.name.clone())),
            agent_id: Some(AgentSessionId::from(row.id.clone())),
            worktree: row
                .worktree_branch
                .clone()
                .or_else(|| row.worktree_path.clone()),
            pane_id: row.pane.as_ref().map(|pane| pane.pane_id.clone()),
            cause: rimz::sidebar::unread::UnreadClearCause::MarkRead
                .as_str()
                .to_owned(),
            cleared_at_ms: Some(now_ms),
        });
    }
    wake_sidebars(&resolved.runtime);
    emit_hidden_count("Marked read", resolved.rows.len())
}

fn mark_unread(globals: &GlobalFlags, target: String, worktree: Option<String>) -> Result<()> {
    let resolved = resolve_sidebar_targets(globals, &target, worktree.as_deref())?;
    let now_ms = unix_now_ms_i64();
    let opened = rimz::sidebar::unread::mark_rows_unread(&resolved.runtime, &resolved.rows, now_ms)
        .context("writing unread episodes")?;
    let diag = diag_for_workspace(&resolved.workspace);
    for item in &opened {
        diag.trace_notify(rimz::diag::notify::NotifyTraceEvent::UnreadMarked {
            row_id: item.row_id.clone(),
            label: Some(item.label.clone()),
            agent_kind: Some(item.agent_kind.clone()),
            agent_id: Some(item.agent_id.clone()),
            worktree: item.worktree.clone(),
            pane_id: item.pane_id.clone(),
            status: item.status.as_str().to_owned(),
            episode_ms: item.episode_ms,
        });
    }
    wake_sidebars(&resolved.runtime);
    emit_hidden_count("Marked unread", resolved.rows.len())
}

/// Focus the session's sidebar pane, the global focus-key target. `--toggle`
/// returns to a working pane in the sidebar's tab when already on the sidebar,
/// so one key reaches the sidebar and goes back. The session is the keypress's
/// `--session-name` (the tmux binding resolves it per room); a bare invocation
/// resolves the room from the cwd. Focus takes only the session; the Zellij
/// roster resolves the workspace from RimZ's known-session registry when the
/// off-server `run-shell` child has no room cwd.
fn focus(globals: &GlobalFlags, session_name: Option<String>, toggle: bool) -> Result<()> {
    let session_name = match session_name {
        Some(name) => name,
        None => WorkspaceResolver::resolve_participant(".", globals.root.clone())?.session_name,
    };
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let listing = backend
        .list_panes(rimz::mux::PaneListOptions {
            session_name: Some(session_name.clone()),
            ..Default::default()
        })
        .context("listing panes")?;
    let focused_pane = backend
        .client_view(rimz::mux::ClientFocusOptions {
            session_name: Some(session_name.clone()),
            ..Default::default()
        })
        .map(|view| {
            let mut viewed = view.viewed_panes;
            viewed.sort_by_key(ToString::to_string);
            viewed.dedup();
            viewed
        });
    let focused_pane = match focused_pane {
        Ok(viewed) => match viewed.as_slice() {
            [pane] => Some(pane.clone()),
            _ if toggle => bail!(
                "sidebar toggle requires one attached client view; found {} distinct views",
                viewed.len()
            ),
            _ => None,
        },
        Err(err) if toggle => {
            return Err(err).context("sampling attached client focus for sidebar toggle");
        }
        Err(_) => None,
    };
    let focused_tab = focused_pane.as_ref().and_then(|pane| {
        listing
            .panes
            .iter()
            .find(|candidate| &candidate.pane_id == pane)
            .and_then(|candidate| candidate.view_id.clone())
    });
    let Some(sidebar) = rimz::pane::select_sidebar_pane(&listing.panes, &[focused_tab])
        .map(|pane| pane.pane_id.clone())
    else {
        bail!("session {session_name} has no sidebar pane to focus");
    };
    let on_sidebar = focused_pane.as_ref() == Some(&sidebar);
    let target = if toggle && on_sidebar {
        let sidebar_tab = listing
            .panes
            .iter()
            .find(|pane| pane.pane_id == sidebar)
            .and_then(|pane| pane.view_id.clone());
        // The toggle-back target: a working pane in the sidebar's own tab. This
        // is stateless; the Zellij plugin path tracks the precise prior pane.
        listing
            .panes
            .iter()
            .filter(|pane| pane.pane_id != sidebar && !pane.is_rimz_sidebar())
            .find(|pane| sidebar_tab.is_none() || pane.view_id == sidebar_tab)
            .map(|pane| pane.pane_id.clone())
    } else {
        Some(sidebar)
    };
    if let Some(target) = target {
        let workspace = rimz::room::session::workspace_record_for_session(&session_name)?
            .ok_or_else(|| anyhow::anyhow!("session {session_name} is not a managed RimZ room"))?;
        let runtime = RuntimePaths::for_workspace(workspace.workspace_id)?;
        rimz::sidebar::focus_anchor::execute_action(
            backend.as_ref(),
            &runtime,
            &session_name,
            target,
            rimz::sidebar::focus_anchor::FocusOrigin::User,
            None,
        )
        .context("focusing pane")?;
    }
    Ok(())
}

struct NotifyTestCommand {
    target: String,
    worktree: Option<String>,
    title: Option<String>,
    body: Option<String>,
    kind: String,
    force_bell: bool,
    no_command: bool,
}

fn notify_test(globals: &GlobalFlags, command: NotifyTestCommand) -> Result<()> {
    let resolved = resolve_sidebar_targets(globals, &command.target, command.worktree.as_deref())?;
    let notification_kind = notification_kind_from_cli(&command.kind)?;
    let labels = resolved
        .rows
        .iter()
        .map(rimz::sidebar::unread::row_label)
        .collect::<Vec<_>>();
    let title = command
        .title
        .unwrap_or_else(|| "RimZ: notification test".to_owned());
    let body = command
        .body
        .unwrap_or_else(|| format!("Testing notification delivery for {}.", labels.join(", ")));
    let agents = resolved
        .rows
        .iter()
        .map(|row| NotificationAgent {
            kind: AgentKind::new_unchecked(row.name.clone()),
            agent_id: AgentSessionId::from(row.id.clone()),
            label: rimz::sidebar::unread::row_label(row),
            handle: row.display_name().to_owned(),
            worktree: row
                .worktree_branch
                .clone()
                .or_else(|| row.worktree_path.clone()),
            task: row.task().map(str::to_owned),
            pane_id: row.pane.as_ref().map(|pane| pane.pane_id.clone()),
            root: row.worktree_path.clone(),
            ask_id: None,
            new_status: row.status(),
        })
        .collect::<Vec<_>>();
    let notification = Notification {
        agents,
        notification_kind,
        title,
        body,
        unread_count: None,
    };
    if !command.no_command {
        let prefs = rimz::config::MachineConfig::load_lenient()
            .notifications
            .clone();
        rimz::sidebar::notify::spawn_notify_handlers(&prefs, &notification);
    }
    let panes = resolved
        .rows
        .iter()
        .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.clone()))
        .collect::<Vec<_>>();
    rimz::store::wakeup::broadcast_sidebar_event(
        &resolved.runtime,
        Some(&resolved.workspace.session_name),
        SidebarEvent::Notify {
            title: notification.title.clone(),
            body: notification.body.clone(),
            panes,
            recheck_unread: !command.force_bell,
            notification_kind: Some(notification.kind_env().to_owned()),
        },
    )
    .context("broadcasting notify-test event")?;
    emit_hidden_count("Notification test sent", resolved.rows.len())
}

struct ResolvedSidebarTargets {
    workspace: rimz::ResolvedWorkspace,
    runtime: RuntimePaths,
    rows: Vec<SidebarRow>,
}

fn resolve_sidebar_targets(
    globals: &GlobalFlags,
    target: &str,
    worktree: Option<&str>,
) -> Result<ResolvedSidebarTargets> {
    rimz::harness::target::require_mention(target)?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let state = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime paths")?;
    let channel = current_channel(&workspace);
    if let Ok(snapshot) =
        PublishedSnapshotReader::new(runtime.clone(), workspace.session_name.clone(), None)
            .read(&state)
        && let Ok(rows) = resolve_rows(&snapshot, target, worktree, channel.as_deref())
        && !rows.is_empty()
    {
        return Ok(ResolvedSidebarTargets {
            workspace,
            runtime,
            rows,
        });
    }

    let store = open_store(&workspace)?;
    let snapshot = rimz::sidebar::produce::resolution_snapshot(&workspace, &store, globals.mux)?;
    let rows = resolve_rows(&snapshot, target, worktree, channel.as_deref())?;
    Ok(ResolvedSidebarTargets {
        workspace,
        runtime,
        rows,
    })
}

fn resolve_rows(
    snapshot: &SidebarSnapshot,
    target: &str,
    worktree: Option<&str>,
    channel: Option<&str>,
) -> Result<Vec<SidebarRow>> {
    let targets = super::resolve_pane_targets(snapshot, target, worktree, channel)?;
    rows_for_targets(snapshot, &targets)
}

fn rows_for_targets(snapshot: &SidebarSnapshot, targets: &[&PaneAgent]) -> Result<Vec<SidebarRow>> {
    let mut rows = Vec::new();
    for target in targets {
        let Some(row) = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| {
                row.pane
                    .as_ref()
                    .is_some_and(|pane| pane.pane_id == target.pane_id)
            })
        else {
            bail!("target {} has no rendered sidebar row", target.label());
        };
        rows.push(row.clone());
    }
    Ok(rows)
}

fn notification_kind_from_cli(value: &str) -> Result<NotificationKind> {
    match value {
        "waiting" => Ok(NotificationKind::Waiting),
        "failed" => Ok(NotificationKind::Failed),
        "paused" => Ok(NotificationKind::Paused),
        "loop_disabled" => Ok(NotificationKind::LoopDisabled),
        "success" => Ok(NotificationKind::Success),
        "coalesced" => Ok(NotificationKind::Coalesced),
        "reminder" => Ok(NotificationKind::Reminder),
        other => bail!("unknown notification kind `{other}`"),
    }
}

fn diag_for_workspace(workspace: &rimz::ResolvedWorkspace) -> rimz::diag::DiagSink {
    rimz::diag::DiagSink::for_workspace(
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        None,
    )
}

fn wake_sidebars(runtime: &RuntimePaths) {
    if let Err(err) = rimz::store::wakeup::wake_sidebars(runtime) {
        tracing::debug!(error = %err, "sidebar unread wake failed");
    }
}

fn unix_now_ms_i64() -> i64 {
    i64::try_from(rimz::sidebar::timing::unix_now_ms()).unwrap_or(i64::MAX)
}

fn emit_hidden_count(label: &str, count: usize) -> Result<()> {
    let mut kv = render::KeyVals::new();
    kv.push(
        label,
        render::cell(count.to_string()).fg(render::palette::accent()),
    );
    kv.render(&mut render::out())?;
    Ok(())
}

#[cfg(test)]
mod tests;
