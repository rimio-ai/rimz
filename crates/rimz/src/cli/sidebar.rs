//! `rimz sidebar` — `snapshot` renders the view-model (producer or `--no-produce` consumer read); `serve` runs the terminal renderer loop.
//!
//! The snapshot arm is a thin delegate over the library produce pipeline
//! ([`rimz::sidebar::produce`]): it resolves workspace/session/mux, calls
//! `produce_snapshot_with_refresh` (or the in-process consumer read for
//! `--no-produce`), and emits — the CLI owns argv, fallback intent, and stdout
//! alone. The elder renderer produces in process on its fetch worker, so this
//! arm serves inspection and scripting.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};

use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::ledger::workspace_record;
use rimz::sidebar::consumer::read_published_snapshot;
use rimz::sidebar::events::SidebarEvent;
use rimz::sidebar::notify::{Notification, NotificationAgent, NotificationKind};
use rimz::sidebar::produce::{
    ProduceOptions, pane_fixture_active, produce_rollup_snapshot_with_refresh,
    produce_snapshot_with_refresh,
};
use rimz::sidebar::{cache::write_presence_stamp, consumer::RollupCursor};
use rimz::workspace::WorkspaceResolver;
use rimz::{PaneAgent, RuntimePaths, SidebarRow, SidebarSnapshot, StatePaths};

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
        #[arg(long)]
        watch: bool,
        #[arg(long)]
        theme_mode: Option<String>,
        #[arg(long)]
        theme_scheme: Option<String>,
    },
    /// Open a live sidebar feature gallery. Hidden — contributor visual review
    /// tool, not a user-facing sidebar verb.
    #[command(hide = true)]
    Gallery,
    /// Render the live sidebar gallery compositor. Hidden — launched by
    /// `sidebar gallery`, not a user-facing sidebar verb.
    #[command(hide = true)]
    GalleryRender,
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
        #[arg(long, hide = true)]
        plugin_mem_pages: Option<u64>,
        #[arg(long, hide = true)]
        plugin_uptime_ms: Option<u64>,
        #[arg(long, hide = true)]
        plugin_commands: Option<u64>,
        #[arg(long, hide = true)]
        plugin_zellij_version: Option<String>,
    },
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
            SidebarSubcmd::Serve { .. } => "sidebar serve",
            SidebarSubcmd::Render { .. } => "sidebar render",
            SidebarSubcmd::Fixture { .. } => "sidebar fixture",
            SidebarSubcmd::Gallery => "sidebar gallery",
            SidebarSubcmd::GalleryRender => "sidebar gallery-render",
            SidebarSubcmd::Wake { .. } => "sidebar wake",
            SidebarSubcmd::MarkRead { .. } => "sidebar mark-read",
            SidebarSubcmd::MarkUnread { .. } => "sidebar mark-unread",
            SidebarSubcmd::Focus { .. } => "sidebar focus",
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
            watch,
            theme_mode,
            theme_scheme,
        } => fixture(
            globals,
            state,
            width,
            height,
            watch,
            theme_mode,
            theme_scheme,
        ),
        SidebarSubcmd::Gallery => gallery(globals),
        SidebarSubcmd::GalleryRender => gallery_render(),
        SidebarSubcmd::Wake {
            workspace_id,
            reason,
            session_name,
            pane_id,
            command_args,
            focused_pane_ids,
            unfocused_pane_ids,
            topology,
            plugin_mem_pages,
            plugin_uptime_ms,
            plugin_commands,
            plugin_zellij_version,
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
                plugin_mem_pages,
                plugin_uptime_ms,
                plugin_commands,
                plugin_zellij_version,
            },
        ),
        SidebarSubcmd::MarkRead { target, worktree } => mark_read(globals, target, worktree),
        SidebarSubcmd::MarkUnread { target, worktree } => mark_unread(globals, target, worktree),
        SidebarSubcmd::Focus {
            session_name,
            toggle,
        } => focus(globals, session_name, toggle),
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
        diag: rimz::diag::DiagSink::disabled(),
    };
    match produce_snapshot_with_refresh(
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
    let snapshot = produce_rollup_snapshot_with_refresh(
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
        let mut stdout = io::stdout().lock();
        render::finish(writeln!(stdout, "{rendered}"))
    } else {
        let waiting = status_tally(snapshot, rimz::agents::AgentStatus::Waiting);
        let failed = status_tally(snapshot, rimz::agents::AgentStatus::Failed);
        let waiting_style = if waiting > 0 {
            render::palette::WARN
        } else {
            render::palette::MUTED
        };
        let failed_style = if failed > 0 {
            render::palette::ALARM
        } else {
            render::palette::MUTED
        };
        let mut kv = render::KeyVals::new();
        kv.push(
            "Workspace",
            render::cell(snapshot.display_name.to_string()).fg(render::palette::ACCENT),
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
        own_pane: rimz::mux::own_pane_id(mux),
    };
    if rimz::sidebar_pane::supervise::is_worker() {
        rimz::sidebar_pane::supervise::run_worker(config).context("serving sidebar")
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

fn render(width: u16, height: u16) -> Result<()> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    let snapshot = serde_json::from_str(&buf).context("parsing snapshot from stdin")?;
    rimz::sidebar_pane::render::render_fixed(io::stdout(), &snapshot, None, width, height)
        .context("rendering snapshot")
}

fn fixture(
    globals: &GlobalFlags,
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
        let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
            .context("resolving current workspace")?;
        let mux = rimz::mux::auto_detect_backend(globals.mux)?;
        return rimz::sidebar_pane::app::serve_fixture(
            snapshot,
            refresh_ms,
            mux,
            &workspace.session_name,
        )
        .context("serving sidebar fixture");
    }
    rimz::sidebar_pane::render::render_fixed_line_ansi(io::stdout(), &snapshot, None, width, height)
        .context("rendering sidebar fixture")
}

fn gallery(globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let machine_config = super::machine_config();
    let mux_config = rimz::config::MultiplexerConfig::from(machine_config.as_ref());
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.theme.display);
    let detected_size = rimz::mux::detect_terminal_size();
    let room = crate::cli::room::RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &workspace.worktree_root,
        mux_config: &mux_config,
        width,
        detected_size,
        refresh_ms: None,
    };
    let rimz_bin = rimz_cli_program().to_string_lossy().into_owned();
    let gallery_pane = rimz::mux::PaneCmd {
        argv: vec![rimz_bin, "sidebar".to_owned(), "gallery-render".to_owned()],
    };
    backend
        .open_tab(&rimz::mux::TabOptions {
            session_name: workspace.session_name.clone(),
            title: "gallery".to_owned(),
            cwd: workspace.worktree_root.clone(),
            panes: rimz::mux::LayoutPanes {
                columns: vec![rimz::mux::LayoutColumn {
                    panes: vec![gallery_pane],
                    stacked: false,
                }],
            },
            focus: true,
            dock_sidebar: false,
            sidebar: crate::cli::room::build_sidebar_opts(&room, Vec::new())?,
        })
        .context("opening sidebar gallery")
}

fn gallery_render() -> Result<()> {
    let machine_config = super::machine_config();
    let refresh_ms = machine_config.theme.display.resolved_refresh_ms();
    let columns = gallery_fixture_columns()
        .into_iter()
        .map(|(state, selector)| {
            let mut snapshot = sidebar_fixture_snapshot(state)?;
            snapshot.theme.mode = machine_config.theme.mode;
            snapshot.theme.glyphs = machine_config.theme.glyphs.clone();
            snapshot.theme.pets.glyphs = machine_config.theme.pets.glyphs;
            let selected_index = gallery_selected_index(&snapshot, selector);
            Ok((snapshot, selected_index))
        })
        .collect::<Result<Vec<_>>>()?;
    let workspace =
        WorkspaceResolver::resolve_participant(".", None).context("resolving gallery workspace")?;
    let mux = rimz::mux::auto_detect_backend(None)?;
    rimz::sidebar_pane::app::serve_gallery(columns, refresh_ms, mux, &workspace.session_name)
        .context("serving sidebar gallery")
}

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

type GallerySelector = fn(&rimz::SidebarRow) -> bool;

fn gallery_fixture_columns() -> [(SidebarFixtureState, GallerySelector); 4] {
    [
        (
            SidebarFixtureState::Cockpit,
            (|row: &rimz::SidebarRow| row.id == "agent:claude:compacting") as GallerySelector,
        ),
        (SidebarFixtureState::Focus, |row: &rimz::SidebarRow| {
            row.as_agent().and_then(|card| card.handle.as_deref()) == Some("planner")
        }),
        (SidebarFixtureState::Reach, |row: &rimz::SidebarRow| {
            row.id == "agent:pi:reach"
        }),
        (SidebarFixtureState::Economy, |row: &rimz::SidebarRow| {
            row.id == "agent:opencode:credits"
        }),
    ]
}

fn parse_fixture_theme_mode(value: &str) -> Result<rimz::config::ThemeMode> {
    match value {
        "auto" => Ok(rimz::config::ThemeMode::Auto),
        "truecolor" => Ok(rimz::config::ThemeMode::Truecolor),
        "256" => Ok(rimz::config::ThemeMode::Indexed),
        other => anyhow::bail!("unknown theme mode `{other}`; expected auto, truecolor, or 256"),
    }
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
    plugin_mem_pages: Option<u64>,
    plugin_uptime_ms: Option<u64>,
    plugin_commands: Option<u64>,
    plugin_zellij_version: Option<String>,
}

fn wake(globals: &GlobalFlags, command: WakeCommand) -> Result<()> {
    let workspace_id = match command.workspace_id.as_deref() {
        Some(raw) => raw.parse::<WorkspaceId>()?,
        None => WorkspaceResolver::resolve_participant(".", globals.root.clone())?.workspace_id,
    };
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    write_presence_stamp(&runtime);
    write_plugin_presence_sample(&workspace_id, &command)?;
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

fn write_plugin_presence_sample(workspace_id: &WorkspaceId, command: &WakeCommand) -> Result<()> {
    let Some(pages) = command.plugin_mem_pages else {
        return Ok(());
    };
    let state = StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
    let _ = state.ensure_dirs();
    rimz::diag::plugin_presence::log(&state.root).append(
        &rimz::diag::plugin_presence::PluginPresenceSample::new(
            rimz::sidebar::timing::unix_now_ms(),
            command.session_name.clone(),
            pages,
            command.plugin_uptime_ms.unwrap_or_default(),
            command.plugin_commands.unwrap_or_default(),
            command.plugin_zellij_version.clone(),
        ),
    );
    Ok(())
}

fn broadcast_wake_event(runtime: &RuntimePaths, session_name: Option<&str>, event: SidebarEvent) {
    if let Err(err) = rimz::ledger::wakeup::broadcast_sidebar_event(runtime, session_name, event) {
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
/// resolves the room from the cwd. Focus needs only the session — never the
/// workspace id — so it skips the participant resolve when the session is given,
/// which also lets the off-server `run-shell` child work without a room cwd.
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
        .map(|view| view.viewed_panes)
        .unwrap_or_default()
        .into_iter()
        .next()
        .or_else(|| {
            listing
                .panes
                .iter()
                .find(|pane| pane.is_focused)
                .map(|pane| pane.pane_id.clone())
        });
    let focused_tab = focused_pane.as_ref().and_then(|pane| {
        listing
            .panes
            .iter()
            .find(|candidate| &candidate.pane_id == pane)
            .and_then(|candidate| candidate.view_id.clone())
    });
    // Focus the sidebar of the tab the user is on, falling back to any sidebar
    // in the session.
    let Some(sidebar) = listing
        .panes
        .iter()
        .filter(|pane| pane.is_rimz_sidebar())
        .find(|pane| focused_tab.is_some() && pane.view_id == focused_tab)
        .or_else(|| listing.panes.iter().find(|pane| pane.is_rimz_sidebar()))
        .map(|pane| pane.pane_id.clone())
    else {
        bail!("session {session_name} has no sidebar pane to focus");
    };
    // Zellij's `list-clients` keeps the client terminal id after an external
    // `focus-pane-id`, while `list-panes` marks the sidebar focused.
    let on_sidebar = focused_pane.as_ref() == Some(&sidebar)
        || listing
            .panes
            .iter()
            .any(|pane| pane.pane_id == sidebar && pane.is_focused);
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
        backend
            .focus_pane(&target, Some(&session_name))
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
        .unwrap_or_else(|| "Rimz: notification test".to_owned());
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
    rimz::ledger::wakeup::broadcast_sidebar_event(
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
    state.ensure_dirs().context("preparing state paths")?;
    runtime.ensure_dirs().context("preparing runtime paths")?;
    let channel = current_channel(&workspace);
    if let Ok(snapshot) = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        &workspace.session_name,
        None,
    ) && let Ok(rows) = resolve_rows(&snapshot, target, worktree, channel.as_deref())
        && !rows.is_empty()
    {
        return Ok(ResolvedSidebarTargets {
            workspace,
            runtime,
            rows,
        });
    }

    let ledger = open_ledger(&workspace)?;
    let snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
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
    if let Err(err) = rimz::ledger::wakeup::wake_sidebars(runtime) {
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
        render::cell(count.to_string()).fg(render::palette::ACCENT),
    );
    kv.render(&mut render::out())?;
    Ok(())
}

#[cfg(test)]
mod tests;
