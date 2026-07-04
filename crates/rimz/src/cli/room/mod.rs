//! Room entry: the start/attach pipeline from workspace resolution to the mux attach command.

mod attach_exec;
mod coroner;
mod daemon_view;
mod hook_install;
mod resume;
mod room_recovery;
mod session_record;
mod start_notice;

use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::{
    BackgroundViewOptions, DaemonView, MuxBackend, PresencePluginOptions, SessionOptions,
    SidebarPaneOptions, SidebarWidth,
};
use rimz::{RuntimePaths, StatePaths, WorkspaceRecord};

use crate::cli::{
    AttachArgs, GlobalFlags, StartArgs, confirm_with_default, machine_config, record_workspace,
    render, setup, sidebar,
};

use attach_exec::{
    inside_selected_mux, report_already_inside, run_attach_action, should_report_already_inside,
};
use coroner::{inspect_previous_incarnation, report_previous_session_death};
use daemon_view::{build_daemon_view, maybe_launch_remote_control};
use hook_install::ensure_detected_agent_hooks;
pub(crate) use hook_install::{detected_installable_adapters, render_dry_run};
use resume::{materialize_room_resume, plan_room_resume, record_rebirth_boundary, report_resume};
pub(crate) use room_recovery::gate_room_before_attach;
use session_record::retire_renamed_session;
use start_notice::report_start_notices;

pub(crate) use attach_exec::{attach_action, exec_attach_command};
pub(crate) use resume::session_is_healthy_live;
pub(crate) use room_recovery::{print_reset_report, rebirth_room};
pub(crate) use session_record::{
    ensure_single_backend_room, pick_mux_for_session, workspace_record_for_session,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachMode {
    Auto,
    Attach,
    Print,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachAction {
    Exec,
    Print,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MissingSessionReport {
    Silent,
    Warn,
}

enum RoomEntry<'a> {
    Start {
        workspace: rimz::ResolvedWorkspace,
        args: &'a StartArgs,
        mux: MuxName,
    },
    StartWeb {
        workspace: rimz::ResolvedWorkspace,
        mux: MuxName,
    },
    WebSession {
        record: &'a WorkspaceRecord,
    },
    AttachCwd {
        workspace: rimz::ResolvedWorkspace,
        mode: AttachMode,
        no_resume: bool,
        refresh_ms: Option<u16>,
    },
    AttachSession {
        session: String,
        mode: AttachMode,
        no_resume: bool,
        refresh_ms: Option<u16>,
        record: Result<Option<WorkspaceRecord>>,
    },
}

impl RoomEntry<'_> {
    fn mode(&self) -> AttachMode {
        match self {
            Self::Start { args, .. } => args.attach.mode(),
            Self::StartWeb { .. } | Self::WebSession { .. } => AttachMode::Print,
            Self::AttachCwd { mode, .. } | Self::AttachSession { mode, .. } => *mode,
        }
    }

    fn no_resume(&self) -> bool {
        match self {
            Self::Start { args, .. } => args.no_resume,
            Self::StartWeb { .. } | Self::WebSession { .. } => false,
            Self::AttachCwd { no_resume, .. } | Self::AttachSession { no_resume, .. } => *no_resume,
        }
    }

    fn refresh_ms(&self) -> Option<u16> {
        match self {
            Self::Start { args, .. } => args.refresh_ms,
            Self::StartWeb { .. } | Self::WebSession { .. } => None,
            Self::AttachCwd { refresh_ms, .. } | Self::AttachSession { refresh_ms, .. } => {
                *refresh_ms
            }
        }
    }

    fn requests_web_sharing(&self) -> bool {
        matches!(self, Self::StartWeb { .. } | Self::WebSession { .. })
    }

    fn session_name(&self) -> &str {
        match self {
            Self::Start { workspace, .. }
            | Self::StartWeb { workspace, .. }
            | Self::AttachCwd { workspace, .. } => &workspace.session_name,
            Self::WebSession { record } => &record.session_name,
            Self::AttachSession { session, .. } => session,
        }
    }
}

pub(crate) fn start(args: StartArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = match rimz::WorkspaceResolver::resolve(&args.path, globals.root.clone()) {
        Ok(workspace) => workspace,
        Err(err) => {
            return Err(anyhow::Error::new(err))
                .with_context(|| format!("resolving workspace at {}", args.path.display()));
        }
    };
    // A live room owns this path's session, so attach on its backend rather
    // than the auto-selected default. An explicit rival `--mux` still flows to
    // the birth guard below and refuses the cross-backend split.
    let mux = pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    )?;
    // A same-mux room can't be nested: if we're already inside this backend's
    // session, report the directory's room and stop before any launch side
    // effect — hook install, session birth, sidebar, or the doomed nested
    // `attach --create`.
    if should_report_already_inside(args.attach.mode(), inside_selected_mux(mux)) {
        report_already_inside(mux, &workspace)?;
        return Ok(());
    }
    report_start_notices(&workspace)?;
    if setup::ensure_default_config()? {
        let config_path = rimz::config::MachineConfig::config_path();
        let config_dir = config_path.parent().unwrap_or(config_path.as_path());
        let mut err = std::io::stderr().lock();
        writeln!(
            err,
            "rimz: initialized config under {} — customize files there (`rimz config path`).",
            render::home_relative(&config_dir.display().to_string())
        )?;
    }
    enter_room(
        RoomEntry::Start {
            workspace,
            args: &args,
            mux,
        },
        globals,
    )
}

pub(crate) fn ensure_workspace_room_for_web(path: &Path, globals: &GlobalFlags) -> Result<WebRoom> {
    let workspace = rimz::WorkspaceResolver::resolve(path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", path.display()))?;
    let mux = MuxName::Zellij;
    if setup::ensure_default_config()? {
        let config_path = rimz::config::MachineConfig::config_path();
        let config_dir = config_path.parent().unwrap_or(config_path.as_path());
        let mut err = std::io::stderr().lock();
        writeln!(
            err,
            "rimz: initialized config under {} — customize files there (`rimz config path`).",
            render::home_relative(&config_dir.display().to_string())
        )?;
    }
    let ready = prepare_room(
        RoomEntry::StartWeb {
            workspace: workspace.clone(),
            mux,
        },
        globals,
    )?;
    Ok(WebRoom {
        session_name: workspace.session_name,
        workspace_id: workspace.workspace_id,
        born_web_shared: !ready.was_live,
    })
}

pub(crate) fn ensure_session_room_for_web(session: &str, globals: &GlobalFlags) -> Result<WebRoom> {
    let record = workspace_record_for_session(session).context("checking Rimz workspace record")?;
    let Some(record) = record else {
        bail!(
            "session `{session}` is not a known Rimz workspace session; run `rimz list` or open the workspace with `rimz start` first"
        );
    };
    ensure_single_backend_room(MuxName::Zellij, session)?;
    let ready = prepare_room(RoomEntry::WebSession { record: &record }, globals)?;
    Ok(WebRoom {
        session_name: record.session_name,
        workspace_id: record.workspace_id,
        born_web_shared: !ready.was_live,
    })
}

pub(crate) struct WebRoom {
    pub session_name: String,
    pub workspace_id: WorkspaceId,
    /// Born fresh in this call with `--web-sharing on`, so browser sharing is
    /// already authoritative and the runtime share pipe is redundant.
    pub born_web_shared: bool,
}

pub(crate) fn attach(args: AttachArgs, globals: &GlobalFlags) -> Result<()> {
    let mode = args.attach.mode();
    match args.workspace {
        Some(session) => enter_room(
            RoomEntry::AttachSession {
                record: workspace_record_for_session(&session),
                session,
                mode,
                no_resume: args.no_resume,
                refresh_ms: args.refresh_ms,
            },
            globals,
        ),
        None => {
            let workspace = rimz::WorkspaceResolver::resolve(".", globals.root.clone())?;
            enter_room(
                RoomEntry::AttachCwd {
                    workspace,
                    mode,
                    no_resume: args.no_resume,
                    refresh_ms: args.refresh_ms,
                },
                globals,
            )
        }
    }
}

fn enter_room(entry: RoomEntry<'_>, globals: &GlobalFlags) -> Result<()> {
    let mode = entry.mode();
    let ready = prepare_room(entry, globals)?;
    let backend = rimz::mux::backend_for(ready.mux);
    finish_attach(
        backend.as_ref(),
        &ready.session_name,
        ready.workspace_id.as_ref(),
        &ready.mux_config,
        mode,
        ready.mux,
    )
}

struct ReadyRoom {
    session_name: String,
    workspace_id: Option<WorkspaceId>,
    mux_config: rimz::config::MultiplexerConfig,
    mux: MuxName,
    was_live: bool,
}

fn prepare_room(entry: RoomEntry<'_>, globals: &GlobalFlags) -> Result<ReadyRoom> {
    let machine_config = machine_config();
    let mut mux_config = rimz::config::MultiplexerConfig::from(machine_config.as_ref());
    if entry.requests_web_sharing() {
        mux_config.zellij.web_sharing = true;
    }
    let sidebar_width = SidebarWidth::from_config(&machine_config.theme.display);
    // One terminal probe per command flow: the width picks every sidebar
    // pane's birth size; the pair sizes a detached tmux birth.
    let detected_size = rimz::mux::detect_terminal_size();
    let remote_control = &machine_config.remote_control;
    if matches!(entry, RoomEntry::Start { .. } | RoomEntry::StartWeb { .. }) {
        // Fail-fast precondition for installed agents: fixable host misconfiguration
        // aborts the launch here with the fix, before hook-install or session side
        // effects. An enabled host whose agent is not installed is an inert toggle,
        // skipped here so the room still starts; `rimz doctor` surfaces it.
        rimz::remote_control::preflight(remote_control)?;
    }

    let mux = match &entry {
        RoomEntry::Start { mux, .. } | RoomEntry::StartWeb { mux, .. } => *mux,
        RoomEntry::WebSession { .. } => MuxName::Zellij,
        RoomEntry::AttachCwd { workspace, .. } => pick_mux_for_session(
            &workspace.session_name,
            globals.mux,
            MissingSessionReport::Silent,
        )?,
        RoomEntry::AttachSession {
            session, record, ..
        } => {
            let missing_report = if matches!(record, Ok(Some(_))) {
                MissingSessionReport::Silent
            } else {
                MissingSessionReport::Warn
            };
            pick_mux_for_session(session, globals.mux, missing_report)?
        }
    };

    run_room_preflights(&entry, mux)?;
    if matches!(entry, RoomEntry::Start { .. } | RoomEntry::StartWeb { .. }) {
        ensure_detected_agent_hooks()?;
    }

    let backend = rimz::mux::backend_for(mux);
    if let RoomEntry::Start { workspace, .. }
    | RoomEntry::StartWeb { workspace, .. }
    | RoomEntry::AttachCwd { workspace, .. } = &entry
    {
        retire_renamed_session(backend.as_ref(), workspace);
    }
    // Capture whether this is a plain reattach *before* `ensure_session`, which on
    // tmux would create the session and erase the distinction. A healthy live room
    // re-seeds nothing; only a birth (absent or stuck) resumes prior agents.
    let was_live = session_is_healthy_live(backend.as_ref(), entry.session_name());
    if let RoomEntry::Start { workspace, .. }
    | RoomEntry::StartWeb { workspace, .. }
    | RoomEntry::AttachCwd { workspace, .. } = &entry
    {
        record_workspace(workspace)?;
    }

    let mut attached_workspace_id = None;
    match &entry {
        RoomEntry::Start { workspace, .. } | RoomEntry::StartWeb { workspace, .. } => {
            let room = RoomTarget {
                workspace_id: &workspace.workspace_id,
                project_root: &workspace.project_root,
                session_name: &workspace.session_name,
                cwd: &workspace.worktree_root,
                mux_config: &mux_config,
                width: sidebar_width,
                detected_size,
                refresh_ms: entry.refresh_ms(),
            };
            // The daemon view (`rimzd`) is computed once: its middle column is
            // configurable (default stats), and its daemon hosts depend on config and
            // which agents are on PATH. When present, it leads the session — on Zellij
            // that order is fixed at birth (`open_sidebar` renders the daemon tab
            // first), since Zellij can't reorder tabs afterwards.
            let daemon_view = build_daemon_view(
                remote_control,
                &machine_config.daemon,
                workspace,
                &mux_config,
                &room,
            );
            let daemon = daemon_view.as_ref().map(|view| &view.view);
            birth_room(&RoomBirth {
                backend: backend.as_ref(),
                machine_config: &machine_config,
                room,
                was_live,
                no_resume: entry.no_resume(),
                daemon,
                remote: Some(RemoteControlLaunch {
                    workspace,
                    config: remote_control,
                    daemon_view: daemon_view.as_ref(),
                }),
            })?;
            attached_workspace_id = Some(workspace.workspace_id.clone());
        }
        RoomEntry::AttachCwd { workspace, .. } => {
            let room = RoomTarget {
                workspace_id: &workspace.workspace_id,
                project_root: &workspace.project_root,
                session_name: &workspace.session_name,
                cwd: &workspace.worktree_root,
                mux_config: &mux_config,
                width: sidebar_width,
                detected_size,
                refresh_ms: entry.refresh_ms(),
            };
            birth_room(&RoomBirth {
                backend: backend.as_ref(),
                machine_config: &machine_config,
                room,
                was_live,
                no_resume: entry.no_resume(),
                daemon: None,
                remote: None,
            })?;
            attached_workspace_id = Some(workspace.workspace_id.clone());
        }
        RoomEntry::WebSession { record } => {
            let room = RoomTarget {
                workspace_id: &record.workspace_id,
                project_root: &record.project_root,
                session_name: &record.session_name,
                cwd: &record.project_root,
                mux_config: &mux_config,
                width: sidebar_width,
                detected_size,
                refresh_ms: entry.refresh_ms(),
            };
            birth_room(&RoomBirth {
                backend: backend.as_ref(),
                machine_config: &machine_config,
                room,
                was_live,
                no_resume: entry.no_resume(),
                daemon: None,
                remote: None,
            })?;
            attached_workspace_id = Some(record.workspace_id.clone());
        }
        RoomEntry::AttachSession {
            session, record, ..
        } => match record {
            Ok(Some(record)) => {
                let room = RoomTarget {
                    workspace_id: &record.workspace_id,
                    project_root: &record.project_root,
                    session_name: &record.session_name,
                    cwd: &record.project_root,
                    mux_config: &mux_config,
                    width: sidebar_width,
                    detected_size,
                    refresh_ms: entry.refresh_ms(),
                };
                // Only a session Rimz owns (a matching record) is force-reset; a bare
                // external session by this name is never torn down.
                birth_room(&RoomBirth {
                    backend: backend.as_ref(),
                    machine_config: &machine_config,
                    room,
                    was_live,
                    no_resume: entry.no_resume(),
                    daemon: None,
                    remote: None,
                })?;
                attached_workspace_id = Some(record.workspace_id.clone());
            }
            Ok(None) => {
                tracing::warn!(
                    session = %session,
                    "no workspace record matches session; emitting attach command only",
                );
            }
            Err(err) => {
                tracing::warn!(
                    session = %session,
                    error = %err,
                    "workspace record lookup failed; emitting attach command only",
                );
            }
        },
    }

    Ok(ReadyRoom {
        session_name: entry.session_name().to_owned(),
        workspace_id: attached_workspace_id,
        mux_config,
        mux,
        was_live,
    })
}

fn run_room_preflights(entry: &RoomEntry<'_>, mux: MuxName) -> Result<()> {
    match entry {
        RoomEntry::Start { workspace, .. } | RoomEntry::StartWeb { workspace, .. } => {
            ensure_single_backend_room(mux, &workspace.session_name)?;
            rimz_socket_environment_preflight(&workspace.workspace_id)?;
            mux_environment_preflight(mux, &workspace.session_name)
        }
        RoomEntry::AttachCwd { workspace, .. } => {
            rimz_socket_environment_preflight(&workspace.workspace_id)?;
            mux_environment_preflight(mux, &workspace.session_name)
        }
        RoomEntry::WebSession { record } => {
            rimz_socket_environment_preflight(&record.workspace_id)?;
            mux_environment_preflight(mux, &record.session_name)
        }
        RoomEntry::AttachSession {
            session, record, ..
        } => {
            mux_environment_preflight(mux, session)?;
            if let Ok(Some(record)) = record {
                rimz_socket_environment_preflight(&record.workspace_id)?;
            }
            Ok(())
        }
    }
}

fn mux_environment_preflight(mux: MuxName, session_name: &str) -> Result<()> {
    match mux {
        MuxName::Zellij => rimz::mux::zellij::socket_preflight(session_name)?,
        // tmux sockets live under its own short per-user socket directory; the
        // Rimz session name does not participate in an AF_UNIX path budget.
        MuxName::Tmux => {}
    }
    Ok(())
}

fn rimz_socket_environment_preflight(workspace_id: &WorkspaceId) -> Result<()> {
    RuntimePaths::for_workspace(workspace_id.clone())
        .map(|_| ())
        .context("checking Rimz runtime socket budget")
}

/// Best-effort load of the session's presence plugin — the Zellij push
/// channel that retires the producer's steady-state pane poll (tmux is a
/// no-op; its control-mode watch already pushes). Fired on every attach-shaped
/// flow: the load verb is idempotent and clientless-safe, so a room born
/// detached, a reattach, and a permission granted minutes after the first
/// prompt all converge with no machinery of their own. Failure costs latency
/// only — the producer keeps today's poll — so it never blocks an attach.
pub(crate) fn ensure_presence_plugin(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    zellij_config: &rimz::config::ZellijConfig,
    seed_permissions: bool,
    focus_key: Option<&str>,
) {
    let Some(opts) = presence_plugin_options(
        session_name,
        workspace_id,
        zellij_config,
        seed_permissions,
        focus_key,
    ) else {
        tracing::debug!(
            session = %session_name,
            "presence plugin unavailable; the producer keeps its pane poll",
        );
        return;
    };
    if let Err(err) = backend.ensure_presence_plugin(&opts) {
        tracing::debug!(
            session = %session_name,
            error = %err,
            "presence plugin load failed; the producer keeps its pane poll",
        );
    }
}

pub(crate) fn enable_web_sharing(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    zellij_config: &rimz::config::ZellijConfig,
    seed_permissions: bool,
    focus_key: Option<&str>,
) {
    let Some(opts) = presence_plugin_options(
        session_name,
        workspace_id,
        zellij_config,
        seed_permissions,
        focus_key,
    ) else {
        tracing::debug!(
            session = %session_name,
            "presence plugin unavailable; Zellij web sharing was not requested",
        );
        return;
    };
    if let Err(err) = backend.share_web_session(&opts) {
        tracing::debug!(
            session = %session_name,
            error = %err,
            "Zellij web-sharing pipe failed",
        );
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: could not confirm Zellij web sharing for `{session_name}`; if the browser says \"Web clients are not allowed to attach to this session\", check that Zellij is new enough, Rimz's presence plugin is available, and `[web] enabled = true` in `rimz config path`, then rerun `rimz web open`."
        );
    }
}

fn presence_plugin_options(
    session_name: &str,
    workspace_id: &WorkspaceId,
    zellij_config: &rimz::config::ZellijConfig,
    seed_permissions: bool,
    focus_key: Option<&str>,
) -> Option<PresencePluginOptions> {
    let wasm = rimz::mux::zellij::presence_plugin_path()?;
    Some(PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin: sidebar::rimz_cli_program(),
        converge: false,
        seed_permissions,
        focus_key: focus_key.map(str::to_owned),
        focus_follows_mouse: zellij_config.focus_follows_mouse,
        mouse_click_through: zellij_config.mouse_click_through,
    })
}

/// Register the focus-sidebar chord with the backend. tmux binds it directly;
/// the Zellij backend's default is a no-op (its key routes through the presence
/// plugin). The binding carries no room identity — the tmux command resolves the
/// pressing session at keypress — so this is safe to call per room. A
/// misconfigured chord warns and registers nothing; a backend error is logged —
/// the key is convenience, never a launch precondition.
pub(crate) fn register_focus_key(
    backend: &dyn MuxBackend,
    machine_config: &rimz::config::MachineConfig,
) {
    let Some(label) = machine_config.sidebar.focus_key_label() else {
        return;
    };
    let rimz_bin = sidebar::rimz_cli_program();
    let Some(binding) = rimz::mux::FocusKeyBinding::resolve(label, &rimz_bin) else {
        tracing::warn!(
            focus_key = label,
            "ignoring invalid [sidebar] focus_key; expected e.g. Alt+p"
        );
        return;
    };
    if let Err(err) = backend.register_focus_key(&binding) {
        tracing::debug!(error = %err, "registering the focus-sidebar keybind failed");
    }
}

fn resume_plan_for_birth(
    was_live: bool,
    recover_agents: bool,
    room: &RoomTarget<'_>,
    machine_config: &rimz::config::MachineConfig,
    no_resume: bool,
) -> Result<rimz::harness::resume::ResumePlan> {
    if was_live {
        return Ok(rimz::harness::resume::ResumePlan::default());
    }
    let effective_launch = rimz::config::effective::load(
        &machine_config.agents,
        room.project_root,
        &rimz::ledger::paths::config_home(),
    );
    let (teams, profiles) = match &effective_launch {
        Ok(launch) => (&launch.teams, &launch.profiles),
        Err(err) => {
            tracing::warn!(
                workspace = %room.workspace_id,
                error = %err,
                "effective agent config unavailable; team resume uses machine config only",
            );
            (
                &machine_config.agents.teams,
                &machine_config.agents.profiles,
            )
        }
    };
    let plan = plan_room_resume(
        room.workspace_id,
        &machine_config.resume,
        no_resume,
        recover_agents,
        teams,
        profiles,
        &machine_config.agents.commands,
    );
    let plan = match prompt_recover_or_fresh(plan)? {
        Some(plan) => {
            let paths = StatePaths::for_workspace(room.workspace_id.clone());
            let runtime = RuntimePaths::for_workspace(room.workspace_id.clone());
            match (paths, runtime) {
                (Ok(paths), Ok(runtime)) => {
                    materialize_room_resume(plan, &paths, &runtime, room.session_name, teams)
                }
                (Err(err), _) | (_, Err(err)) => {
                    tracing::warn!(
                        workspace = %room.workspace_id,
                        error = %err,
                        "resume materialization skipped",
                    );
                    rimz::harness::resume::ResumePlan::default()
                }
            }
        }
        None => rimz::harness::resume::ResumePlan::default(),
    };
    record_rebirth_boundary(room.workspace_id, room.session_name);
    Ok(plan)
}

fn prompt_recover_or_fresh(plan: resume::RoomResumePlan) -> Result<Option<resume::RoomResumePlan>> {
    let agents = plan.pane_count();
    if agents == 0 || !std::io::stdin().is_terminal() {
        return Ok(Some(plan));
    }
    let labels = plan.labels().join(", ");
    if confirm_with_default(
        &format!(
            "Recover {agents} agent{} ({labels})?",
            if agents == 1 { "" } else { "s" },
        ),
        true,
    )? {
        Ok(Some(plan))
    } else {
        Ok(None)
    }
}

struct RoomBirth<'a> {
    backend: &'a dyn MuxBackend,
    machine_config: &'a rimz::config::MachineConfig,
    room: RoomTarget<'a>,
    was_live: bool,
    no_resume: bool,
    daemon: Option<&'a DaemonView>,
    remote: Option<RemoteControlLaunch<'a>>,
}

struct RemoteControlLaunch<'a> {
    workspace: &'a rimz::ResolvedWorkspace,
    config: &'a rimz::config::RemoteControlConfig,
    daemon_view: Option<&'a BackgroundViewOptions>,
}

fn birth_room(birth: &RoomBirth<'_>) -> Result<()> {
    let room = &birth.room;
    let machine_config = birth.machine_config;
    let recovery = if birth.was_live {
        coroner::BirthRecovery::default()
    } else {
        inspect_previous_incarnation(birth.backend, room.workspace_id, room.session_name)
    };
    if let Some(death) = &recovery.death {
        report_previous_session_death(
            death,
            recovery.recover_agents && machine_config.resume.on_rebirth && !birth.no_resume,
        );
    }
    let pre_existed = match birth.backend.list_sessions() {
        Ok(sessions) => sessions
            .iter()
            .any(|name| name.as_str() == room.session_name),
        Err(err) => {
            tracing::debug!(
                session = %room.session_name,
                error = %err,
                "could not prove session is absent before birth; using non-destructive sidebar split",
            );
            true
        }
    };
    birth.backend.ensure_session(&SessionOptions {
        session_name: room.session_name.to_owned(),
        workspace_id: room.workspace_id.clone(),
        project_root: room.project_root.to_path_buf(),
        cwd: room.cwd.to_path_buf(),
        config: room.mux_config.clone(),
        detected_size: room.detected_size,
        truecolor: rimz::tui::truecolor(),
    })?;
    // Register the focus-sidebar chord (tmux binds it here; Zellij routes it
    // through the presence plugin). Best-effort: a convenience key never blocks
    // the room from opening.
    register_focus_key(birth.backend, machine_config);
    // Plan which prior agents the reborn room can recover, from the durable
    // rollup. Empty on a healthy reattach (the agents are still alive), when
    // nothing is recoverable, or when the user opted out — then the birth is
    // exactly today's bare working room.
    let resume_plan = resume_plan_for_birth(
        birth.was_live,
        recovery.recover_agents,
        room,
        machine_config,
        birth.no_resume,
    )?;
    launch_sidebar_for_workspace(
        birth.backend,
        room,
        birth.daemon,
        !pre_existed,
        &resume_plan.tabs,
    );
    if let Some(remote) = &birth.remote {
        maybe_launch_remote_control(
            birth.backend,
            remote.workspace,
            remote.config,
            remote.daemon_view,
        );
    }
    // Authoritative gate before the resurrecting `attach --create`: live rooms
    // attach as-is, absent/exited rooms are (re)birthed, and a room that cannot
    // self-heal resets on an attached terminal or fails fast with the fix
    // without one. Accepted recovery seeds the reborn room with resume tabs.
    gate_room_before_attach(birth.backend, room, birth.daemon, &resume_plan.tabs)?;
    report_resume(&resume_plan);
    ensure_presence_plugin(
        birth.backend,
        room.session_name,
        room.workspace_id,
        &room.mux_config.zellij,
        machine_config.web.enabled,
        machine_config.sidebar.focus_key_label(),
    );
    Ok(())
}

fn finish_attach(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: Option<&WorkspaceId>,
    mux_config: &rimz::config::MultiplexerConfig,
    mode: AttachMode,
    mux: MuxName,
) -> Result<()> {
    let spec = backend.attach_command(session_name, mux_config);
    if let Some(workspace_id) = workspace_id {
        tracing::info!(
            workspace = %workspace_id,
            session = %session_name,
            mux = %mux,
            "workspace ready",
        );
    } else {
        tracing::info!(
            session = %session_name,
            mux = %mux,
            "workspace ready",
        );
    }
    run_attach_action(&spec, mode, mux)
}

/// The room a sidebar launch or pre-attach gate targets: workspace identity
/// plus the per-machine knobs every [`SidebarPaneOptions`] build shares.
pub(crate) struct RoomTarget<'a> {
    pub(crate) workspace_id: &'a rimz::WorkspaceId,
    /// The workspace root behind the id — paired into the identity pin a
    /// session birth stamps into the mux environment.
    pub(crate) project_root: &'a Path,
    pub(crate) session_name: &'a str,
    pub(crate) cwd: &'a Path,
    pub(crate) mux_config: &'a rimz::config::MultiplexerConfig,
    pub(crate) width: SidebarWidth,
    /// The launching terminal's `(cols, rows)`, probed once per command that can
    /// birth the session ([`rimz::mux::detect_terminal_size`]): the width picks
    /// the sidebar's birth size, the pair sizes a detached tmux birth. Commands
    /// targeting an already-live session leave this `None`.
    pub(crate) detected_size: Option<(u16, u16)>,
    /// One-shot sidebar render-cadence override for panes born during this
    /// launch. Recovery rebuilt from workspace state falls back to config.
    pub(crate) refresh_ms: Option<u16>,
}

impl RoomTarget<'_> {
    /// The width verdict this command's sidebar panes are born with —
    /// `min(percent × launching terminal, max_cols)`, resolved once here and
    /// constant for the session's life.
    fn birth_size(&self) -> rimz::mux::BirthSize {
        self.width
            .birth_size(self.detected_size.map(|(cols, _)| cols))
    }
}

pub(crate) fn build_sidebar_opts(
    target: &RoomTarget<'_>,
    resume_tabs: Vec<rimz::mux::ResumeTab>,
) -> Result<SidebarPaneOptions> {
    let rimz_bin = rimz::proc::rimz_exe();
    Ok(SidebarPaneOptions {
        session_name: target.session_name.to_owned(),
        workspace_id: target.workspace_id.clone(),
        project_root: target.project_root.to_path_buf(),
        cwd: target.cwd.to_path_buf(),
        birth_size: target.birth_size(),
        rimz_bin,
        replace_existing: false,
        pristine_birth: false,
        config: target.mux_config.clone(),
        resume_tabs,
        refresh_ms: target.refresh_ms,
    })
}

pub(crate) fn launch_sidebar_for_workspace(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    pristine_birth: bool,
    resume_tabs: &[rimz::mux::ResumeTab],
) -> rimz::sidebar::SidebarLaunchOutcome {
    let runtime = match RuntimePaths::for_workspace(target.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(
                workspace = %target.workspace_id,
                error = %err,
                "sidebar launch skipped because runtime paths are unavailable",
            );
            return rimz::sidebar::SidebarLaunchOutcome::Failed;
        }
    };
    let mut opts = match build_sidebar_opts(target, resume_tabs.to_vec()) {
        Ok(opts) => opts,
        Err(err) => {
            tracing::warn!(
                workspace = %target.workspace_id,
                error = %err,
                "sidebar launch skipped because room options are unavailable",
            );
            return rimz::sidebar::SidebarLaunchOutcome::Failed;
        }
    };
    opts.pristine_birth = pristine_birth;
    rimz::sidebar::launch_sidebar_if_needed(backend, &runtime, &opts, daemon)
}
