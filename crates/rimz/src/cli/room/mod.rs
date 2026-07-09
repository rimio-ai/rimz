//! Room entry: the start/attach pipeline from workspace resolution to the mux attach command.

mod attach_exec;
mod coroner;
mod daemon_view;
mod hook_install;
mod resume;
mod room_recovery;
mod session_record;
mod start_notice;
#[cfg(test)]
mod tests;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::{
    BackgroundViewOptions, DaemonView, MuxBackend, PresencePluginOptions, SessionOptions,
    SidebarPaneOptions, SidebarWidth,
};
use rimz::{RuntimePaths, StatePaths, WorkspaceRecord};

use crate::cli::{
    AttachArgs, GlobalFlags, StartArgs, confirm_with_default, first_run, machine_config,
    open_store, render, setup, sidebar,
};

use attach_exec::{
    inside_selected_mux, report_already_inside, run_attach_action, should_report_already_inside,
};
use coroner::{BirthRecovery, inspect_previous_incarnation, report_previous_session_death};
use daemon_view::{build_daemon_view, maybe_launch_remote_control};
pub(crate) use hook_install::{
    detected_installable_adapters, ensure_detected_agent_hooks, render_dry_run,
};
use resume::{AgentRecovery, materialize_room_resume, plan_room_resume, report_resume};
pub(crate) use room_recovery::gate_room_before_attach;
use session_record::{retire_renamed_session, session_probe_retry_timeout, session_probe_timeout};
use start_notice::report_start_notices;

pub(crate) use attach_exec::{attach_action, exec_attach_command};
pub(crate) use resume::record_rebirth_boundary;
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
enum ResumePromptMode {
    Interactive,
    Silent,
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
        first_run: bool,
    },
    StartWeb {
        workspace: rimz::ResolvedWorkspace,
        mux: MuxName,
        no_resume: bool,
        confirm_resume: bool,
    },
    WebSession {
        record: &'a WorkspaceRecord,
        no_resume: bool,
        confirm_resume: bool,
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
            Self::StartWeb { no_resume, .. } | Self::WebSession { no_resume, .. } => *no_resume,
            Self::AttachCwd { no_resume, .. } | Self::AttachSession { no_resume, .. } => *no_resume,
        }
    }

    fn resume_prompt_mode(&self) -> ResumePromptMode {
        let confirm_resume = match self {
            Self::StartWeb { confirm_resume, .. } | Self::WebSession { confirm_resume, .. } => {
                *confirm_resume
            }
            Self::Start { .. } | Self::AttachCwd { .. } | Self::AttachSession { .. } => false,
        };
        resume_prompt_mode(confirm_resume, std::io::stdin().is_terminal())
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

    fn session_name(&self) -> &str {
        match self {
            Self::Start { workspace, .. }
            | Self::StartWeb { workspace, .. }
            | Self::AttachCwd { workspace, .. } => &workspace.session_name,
            Self::WebSession { record, .. } => &record.session_name,
            Self::AttachSession { session, .. } => session,
        }
    }
}

fn resume_prompt_mode(confirm_resume: bool, stdin_is_terminal: bool) -> ResumePromptMode {
    if confirm_resume || stdin_is_terminal {
        ResumePromptMode::Interactive
    } else {
        ResumePromptMode::Silent
    }
}

pub(crate) fn start(args: StartArgs, globals: &GlobalFlags) -> Result<()> {
    validate_agent_plugins()?;
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
    let first_run = ensure_default_config_for_start()?;
    enter_room(
        RoomEntry::Start {
            workspace,
            args: &args,
            mux,
            first_run,
        },
        globals,
    )
}

pub(crate) fn ensure_workspace_room_for_web(
    path: &Path,
    globals: &GlobalFlags,
    no_resume: bool,
    confirm_resume: bool,
) -> Result<WebRoom> {
    validate_agent_plugins()?;
    let workspace = rimz::WorkspaceResolver::resolve(path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", path.display()))?;
    let mux = MuxName::Zellij;
    setup::ensure_default_config()?;
    prepare_room(
        RoomEntry::StartWeb {
            workspace: workspace.clone(),
            mux,
            no_resume,
            confirm_resume,
        },
        globals,
    )?;
    Ok(WebRoom {
        session_name: workspace.session_name,
        workspace_id: workspace.workspace_id,
    })
}

fn validate_agent_plugins() -> Result<()> {
    let loaded = rimz::agents::plugin::loaded();
    if loaded.errors.is_empty() {
        return Ok(());
    }
    let details = loaded
        .errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    bail!(
        "agent plugin validation failed before room start:\n{details}\nfix or remove the manifest, then run `rimz agents register --check`"
    )
}

pub(crate) fn ensure_session_room_for_web(
    session: &str,
    globals: &GlobalFlags,
    no_resume: bool,
    confirm_resume: bool,
) -> Result<WebRoom> {
    let record = workspace_record_for_web_session(session)?;
    prepare_room(
        RoomEntry::WebSession {
            record: &record,
            no_resume,
            confirm_resume,
        },
        globals,
    )?;
    Ok(WebRoom {
        session_name: record.session_name,
        workspace_id: record.workspace_id,
    })
}

pub(crate) fn web_room_for_session(session: &str) -> Result<WebRoom> {
    let record = workspace_record_for_web_session(session)?;
    Ok(WebRoom {
        session_name: record.session_name,
        workspace_id: record.workspace_id,
    })
}

pub(crate) fn existing_web_room_for_path(path: &Path, globals: &GlobalFlags) -> Result<WebRoom> {
    let workspace = rimz::WorkspaceResolver::resolve(path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", path.display()))?;
    let record = workspace_record_for_session(&workspace.session_name)
        .context("checking Rimz workspace record")?;
    let Some(record) = record else {
        bail!(
            "workspace session `{}` has not been born by Rimz; run `rimz web open {}` or `rimz start {}` first",
            workspace.session_name,
            path.display(),
            path.display(),
        );
    };
    ensure_single_backend_room(MuxName::Zellij, &record.session_name)?;
    Ok(WebRoom {
        session_name: record.session_name,
        workspace_id: record.workspace_id,
    })
}

fn workspace_record_for_web_session(session: &str) -> Result<WorkspaceRecord> {
    let record = workspace_record_for_session(session).context("checking Rimz workspace record")?;
    let Some(record) = record else {
        bail!(
            "session `{session}` is not a known Rimz workspace session; run `rimz list` or open the workspace with `rimz start` first"
        );
    };
    ensure_single_backend_room(MuxName::Zellij, session)?;
    Ok(record)
}

fn ensure_default_config_for_start() -> Result<bool> {
    let config_was_missing = !rimz::config::MachineConfig::config_path().exists();
    let initialized_config = setup::ensure_default_config()?;
    let first_run = config_was_missing && rimz::config::MachineConfig::config_path().exists();
    if initialized_config && !(first_run && std::io::stdin().is_terminal()) {
        report_initialized_config()?;
    }
    Ok(first_run)
}

fn report_initialized_config() -> Result<()> {
    let config_path = rimz::config::MachineConfig::config_path();
    let config_dir = config_path.parent().unwrap_or(config_path.as_path());
    let mut err = std::io::stderr().lock();
    writeln!(
        err,
        "rimz: initialized config under {} — customize files there (`rimz config path`).",
        render::home_relative(&config_dir.display().to_string())
    )?;
    Ok(())
}

pub(crate) struct WebRoom {
    pub session_name: String,
    pub workspace_id: WorkspaceId,
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
}

fn prepare_room(entry: RoomEntry<'_>, globals: &GlobalFlags) -> Result<ReadyRoom> {
    let mut machine_config = machine_config();
    if matches!(entry, RoomEntry::Start { .. } | RoomEntry::StartWeb { .. }) {
        // Fail-fast precondition for installed agents: fixable host misconfiguration
        // aborts the launch here with the fix, before hook-install or session side
        // effects. An enabled host whose agent is not installed is an inert toggle,
        // skipped here so the room still starts; `rimz doctor` surfaces it.
        rimz::remote_control::preflight(&machine_config.remote_control)?;
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
    let hook_intro_rendered = if matches!(entry, RoomEntry::Start { .. }) {
        ensure_detected_agent_hooks()?
    } else {
        false
    };
    if let RoomEntry::Start {
        first_run: true, ..
    } = &entry
        && std::io::stdin().is_terminal()
    {
        let defaults = first_run::Defaults::from_config(&machine_config);
        first_run::run(defaults, hook_intro_rendered)?;
        let mut out = render::err();
        writeln!(out, "Opening the room...")?;
        match rimz::config::MachineConfig::load() {
            Ok(config) => machine_config = std::sync::Arc::new(config),
            Err(err) => tracing::warn!(
                error = %err,
                "first-run config reload failed; using startup config for this room"
            ),
        }
    }

    let mux_config = rimz::config::MultiplexerConfig::from(machine_config.as_ref());
    let sidebar_width = SidebarWidth::from_config(&machine_config.theme.display);
    // One terminal probe per command flow: the width picks every sidebar
    // pane's birth size; the pair sizes a detached tmux birth.
    let detected_size = rimz::mux::detect_terminal_size();
    let remote_control = &machine_config.remote_control;
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
        let rimz_bin = room_owner_bin();
        record_room_bin(workspace, rimz_bin.as_path())?;
    }
    if let RoomEntry::Start { workspace, .. } = &entry
        && !was_live
        && std::io::stdin().is_terminal()
    {
        prompt_project_trust(&workspace.project_root);
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
                resume_prompt: entry.resume_prompt_mode(),
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
                resume_prompt: entry.resume_prompt_mode(),
                daemon: None,
                remote: None,
            })?;
            attached_workspace_id = Some(workspace.workspace_id.clone());
        }
        RoomEntry::WebSession { record, .. } => {
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
                resume_prompt: entry.resume_prompt_mode(),
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
                    resume_prompt: entry.resume_prompt_mode(),
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
        RoomEntry::WebSession { record, .. } => {
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

fn prompt_project_trust(project_root: &Path) {
    let offer = match rimz::trust::birth_prompt(project_root) {
        Ok(Some(offer)) => offer,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(error = %err, "trust birth prompt skipped");
            return;
        }
    };
    if let Err(err) = write_project_trust_offer(&offer) {
        tracing::warn!(error = %err, "trust birth prompt render failed");
        return;
    }
    match confirm_with_default("Trust this project's config on this machine?", true) {
        Ok(true) => match rimz::trust::grant(project_root) {
            Ok(_) => {
                if let Err(err) = write_project_trust_notice(&[
                    "rimz: trusted — scheduled loops and project config are now active.",
                ]) {
                    tracing::warn!(error = %err, "trust grant notice failed");
                }
            }
            Err(err) => tracing::warn!(error = %err, "trust grant from birth prompt failed"),
        },
        Ok(false) => {
            if let Err(err) = rimz::trust::dismiss_birth_prompt_offer(project_root, &offer) {
                tracing::warn!(error = %err, "recording trust decline failed");
            }
            if let Err(err) = write_project_trust_notice(&[
                "rimz: left untrusted; run `rimz trust grant` when ready.",
                "Rimz won't ask again until .rimz/config.toml changes.",
            ]) {
                tracing::warn!(error = %err, "trust decline notice failed");
            }
        }
        Err(err) => tracing::warn!(error = %err, "trust prompt read failed"),
    }
}

fn write_project_trust_offer(offer: &rimz::trust::BirthPromptOffer) -> std::io::Result<()> {
    let mut out = render::err();
    write_project_trust_offer_to(&mut out, offer)
}

fn write_project_trust_offer_to(
    out: &mut impl Write,
    offer: &rimz::trust::BirthPromptOffer,
) -> std::io::Result<()> {
    let summary = &offer.summary;
    writeln!(
        out,
        "This project ships .rimz/config.toml with config that stays inert"
    )?;
    writeln!(out, "until you trust it on this machine:")?;
    write_project_trust_list(&mut *out, "loop tasks", &summary.task_names)?;
    write_project_trust_list(&mut *out, "profiles", &summary.profiles)?;
    write_project_trust_list(&mut *out, "teams", &summary.teams)?;
    write_project_trust_list(&mut *out, "env for", &summary.env_agents)?;
    if summary.hooks > 0 {
        writeln!(out, "  hooks: {}", summary.hooks)?;
    }
    Ok(())
}

fn write_project_trust_list(
    out: &mut impl Write,
    label: &str,
    values: &[String],
) -> std::io::Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    writeln!(out, "  {label}: {}", values.join(", "))
}

fn write_project_trust_notice(lines: &[&str]) -> std::io::Result<()> {
    let mut out = render::err();
    for line in lines {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

fn mux_environment_preflight(mux: MuxName, session_name: &str) -> Result<()> {
    match mux {
        MuxName::Zellij => {
            rimz::mux::zellij::socket_preflight(session_name)?;
            mux_responsive_preflight(mux)?;
            zellij_version_preflight()?;
        }
        // tmux sockets live under its own short per-user socket directory; the
        // Rimz session name does not participate in an AF_UNIX path budget.
        MuxName::Tmux => mux_responsive_preflight(mux)?,
    }
    Ok(())
}

fn mux_responsive_preflight(mux: MuxName) -> Result<()> {
    let backend = rimz::mux::backend_for(mux);
    if let Err(err @ rimz::mux::MuxErr::Timeout { .. }) =
        backend.list_sessions_within(session_probe_timeout())
    {
        let retry = session_probe_retry_timeout();
        {
            let mut out = render::err();
            writeln!(
                out,
                "note: {err}; retrying once ({}).",
                duration_label(retry)
            )?;
        }
        if let Err(err @ rimz::mux::MuxErr::Timeout { .. }) = backend.list_sessions_within(retry) {
            bail!("{}", mux_not_responding_message(mux, retry, &err));
        }
    }
    Ok(())
}

fn mux_not_responding_message(
    mux: MuxName,
    timeout: std::time::Duration,
    err: &rimz::mux::MuxErr,
) -> String {
    let (reset, fallback) = match mux {
        MuxName::Zellij => ("zellij kill-all-sessions", "rimz --tmux"),
        MuxName::Tmux => ("tmux kill-server", "rimz --zellij"),
    };
    format!(
        "{mux} is not responding: `{mux} list-sessions` hung for {} and was killed.\n\
         Recover with:\n    {reset}\n\
         Or run this room under {}:\n    {fallback}\n\n\
         detail: {err}",
        duration_label(timeout),
        mux.other(),
    )
}

fn duration_label(duration: std::time::Duration) -> String {
    let millis = duration.as_millis();
    if millis.is_multiple_of(1000) {
        format!("{}s", millis / 1000)
    } else {
        format!("{millis}ms")
    }
}

fn zellij_version_preflight() -> Result<()> {
    let caps = rimz::mux::zellij::capabilities().context("checking Zellij version")?;
    if caps.meets_min_version {
        return Ok(());
    }
    let (maj, min, patch) = rimz::mux::zellij::MIN_ZELLIJ_VERSION;
    let found = caps.binary_version.trim();
    anyhow::bail!(
        "Zellij {found} is below Rimz's floor; upgrade Zellij to >= {maj}.{min}.{patch}, or run this room with `--mux tmux`."
    );
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
        false,
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
) -> bool {
    let Some(opts) = presence_plugin_options(
        session_name,
        workspace_id,
        zellij_config,
        seed_permissions,
        focus_key,
        true,
    ) else {
        tracing::debug!(
            session = %session_name,
            "presence plugin unavailable; Zellij web sharing was not requested",
        );
        warn_web_sharing_unconfirmed(session_name);
        return false;
    };
    if let Err(err) = backend.share_web_session(&opts) {
        tracing::debug!(
            session = %session_name,
            error = %err,
            "Zellij web-sharing pipe failed",
        );
        warn_web_sharing_unconfirmed(session_name);
        return false;
    }
    true
}

pub(crate) fn warn_web_sharing_unconfirmed(session_name: &str) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: could not confirm Zellij web sharing for `{session_name}`; if the browser says \"Web clients are not allowed to attach to this session\", check that Zellij is new enough, Rimz's presence plugin is available, and `[web] enabled = true` in `rimz config path`, then rerun `rimz web open`."
    );
}

fn presence_plugin_options(
    session_name: &str,
    workspace_id: &WorkspaceId,
    zellij_config: &rimz::config::ZellijConfig,
    seed_permissions: bool,
    focus_key: Option<&str>,
    materialize_artifact: bool,
) -> Option<PresencePluginOptions> {
    let wasm = if materialize_artifact {
        rimz::mux::zellij::ensure_presence_plugin_artifact()?
    } else {
        rimz::mux::zellij::presence_plugin_path()?
    };
    let rimz_bin = room_bin_for_workspace(workspace_id);
    Some(PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin,
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
    recovery: &BirthRecovery,
    room: &RoomTarget<'_>,
    machine_config: &rimz::config::MachineConfig,
    no_resume: bool,
    prompt_mode: ResumePromptMode,
) -> Result<rimz::harness::resume::ResumePlan> {
    if was_live {
        return Ok(rimz::harness::resume::ResumePlan::default());
    }
    let effective_launch = rimz::config::effective::load(
        &machine_config.agents,
        room.project_root,
        &rimz::store::paths::config_home(),
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
        AgentRecovery {
            enabled: recovery.recover_agents,
            roster: &recovery.roster,
        },
        teams,
        profiles,
        &machine_config.agents.commands,
    );
    let plan = if plan.pane_count() > 0 {
        if let Some(death) = recovery.death.as_ref() {
            report_previous_session_death(death);
        }
        prompt_recover_or_fresh(plan, prompt_mode)?
    } else {
        Some(plan)
    };
    let plan = match plan {
        Some(plan) => {
            let paths = StatePaths::for_workspace(room.workspace_id.clone());
            let runtime = RuntimePaths::for_workspace(room.workspace_id.clone());
            match (paths, runtime) {
                (Ok(paths), Ok(runtime)) => materialize_room_resume(
                    plan,
                    &paths,
                    &runtime,
                    room.session_name,
                    teams,
                    &recovery.roster,
                ),
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
    if let Some(death) = recovery.death.as_ref() {
        let recovered = plan.tabs.iter().map(rimz::mux::ResumeTab::pane_count).sum();
        coroner::record_recovery_outcome(room.workspace_id, death, recovered);
    }
    record_rebirth_boundary(room.workspace_id, room.session_name);
    Ok(plan)
}

fn prompt_recover_or_fresh(
    plan: resume::RoomResumePlan,
    mode: ResumePromptMode,
) -> Result<Option<resume::RoomResumePlan>> {
    let agents = plan.pane_count();
    if agents == 0 || mode == ResumePromptMode::Silent {
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
    resume_prompt: ResumePromptMode,
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
    if !pre_existed {
        purge_rebirth_heartbeats_for_workspace(room.workspace_id);
    }
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
        &recovery,
        room,
        machine_config,
        birth.no_resume,
        birth.resume_prompt,
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

pub(crate) fn purge_rebirth_heartbeats_for_workspace(workspace_id: &WorkspaceId) {
    match RuntimePaths::for_workspace(workspace_id.clone()) {
        Ok(runtime) => rimz::sidebar::purge_rebirth_heartbeats(&runtime),
        Err(err) => tracing::debug!(
            workspace = %workspace_id,
            error = %err,
            "sidebar rebirth heartbeat purge skipped because runtime paths are unavailable",
        ),
    }
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
    let rimz_bin = room_bin_for_workspace(target.workspace_id);
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

fn room_owner_bin() -> PathBuf {
    rimz::reload::current_reexec_target().unwrap_or_else(rimz::proc::rimz_exe)
}

fn record_room_bin(workspace: &rimz::ResolvedWorkspace, rimz_bin: &Path) -> Result<()> {
    open_store(workspace)?
        .record_room_bin(workspace, rimz_bin.to_path_buf())
        .context("recording room binary")
}

fn room_bin_for_workspace(workspace_id: &WorkspaceId) -> PathBuf {
    let recorded = StatePaths::for_workspace(workspace_id.clone())
        .ok()
        .and_then(|paths| rimz::store::workspace_record::read(&paths.workspace_record).ok())
        .and_then(|record| record.rimz_bin);
    rimz::workspace::resolve_recorded_rimz_bin(recorded.as_deref())
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
