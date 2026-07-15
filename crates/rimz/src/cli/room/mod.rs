//! Room entry: the start/attach pipeline from workspace resolution to the mux attach command.

mod attach_exec;
mod resume;
mod start_notice;
#[cfg(test)]
mod tests;

use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use rimz::ids::{MuxName, WorkspaceId};
use rimz::room::session::{
    MissingSessionReport, ensure_single_backend_room, pick_mux_for_session, retire_renamed_session,
    session_probe_retry_timeout, session_probe_timeout, workspace_record_for_session,
};
use rimz::room::{
    AttendedRecovery, BackgroundViewBirth, NormalBirth, NormalRebirth, RoomBirth, RoomContext,
    RoomSizing,
};
use rimz::{RuntimePaths, WorkspaceRecord};

use crate::cli::hooks::ensure_detected_agent_hooks;
use crate::cli::{
    AttachArgs, GlobalFlags, StartArgs, confirm_with_default, first_run, machine_config, render,
    setup,
};

use attach_exec::{
    inside_selected_mux, report_already_inside, run_attach_action, should_report_already_inside,
};
use resume::{report_previous_session_death, report_resume};
use rimz::harness::rebirth::RebirthChoice;
use start_notice::report_start_notices;

pub(crate) use attach_exec::{attach_action, exec_attach_command};

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
        resume_prompt_mode(confirm_resume, start_attended())
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

/// A start driven by the remote link supervisor's retry loop is unattended:
/// consent gates fall back to their non-interactive behavior.
fn start_attended() -> bool {
    std::io::stdin().is_terminal() && !rimz::remote::reconnect_marked()
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
    let mux = render::room::present_mux_pick(pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    ))?;
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
    let mux = render::room::present_mux_pick(pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    ))?;
    render::room::print_notices(ensure_single_backend_room(mux, &workspace.session_name)?)?;
    preflight_web_engine(mux)?;
    setup::ensure_default_config()?;
    let ready = prepare_room(
        RoomEntry::StartWeb {
            workspace,
            mux,
            no_resume,
            confirm_resume,
        },
        globals,
    )?;
    web_room_from_ready(ready)
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
    let mux = render::room::present_mux_pick(pick_mux_for_session(
        session,
        globals.mux,
        MissingSessionReport::Silent,
    ))?;
    let record = workspace_record_for_web_session(session, mux)?;
    preflight_web_engine(mux)?;
    let ready = prepare_room(
        RoomEntry::WebSession {
            record: &record,
            no_resume,
            confirm_resume,
        },
        globals,
    )?;
    web_room_from_ready(ready)
}

pub(crate) fn web_room_for_session(session: &str, globals: &GlobalFlags) -> Result<WebRoom> {
    let mux = render::room::present_mux_pick(pick_mux_for_session(
        session,
        globals.mux,
        MissingSessionReport::Silent,
    ))?;
    let record = workspace_record_for_web_session(session, mux)?;
    Ok(WebRoom {
        context: RoomContext::from_record(&record, machine_config(), mux, RoomSizing::OrdinaryTab)?,
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
    let mux = render::room::present_mux_pick(pick_mux_for_session(
        &record.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    ))?;
    render::room::print_notices(ensure_single_backend_room(mux, &record.session_name)?)?;
    Ok(WebRoom {
        context: RoomContext::from_record(&record, machine_config(), mux, RoomSizing::OrdinaryTab)?,
    })
}

fn web_room_from_ready(ready: ReadyRoom) -> Result<WebRoom> {
    match ready {
        ReadyRoom::Managed(context) => Ok(WebRoom { context: *context }),
        ReadyRoom::External { session_name, .. } => {
            bail!("session `{session_name}` is not a managed Rimz room")
        }
    }
}

fn workspace_record_for_web_session(session: &str, mux: MuxName) -> Result<WorkspaceRecord> {
    let record = workspace_record_for_session(session).context("checking Rimz workspace record")?;
    let Some(record) = record else {
        bail!(
            "session `{session}` is not a known Rimz workspace session; run `rimz list` or open the workspace with `rimz start` first"
        );
    };
    render::room::print_notices(ensure_single_backend_room(mux, session)?)?;
    Ok(record)
}

fn preflight_web_engine(mux: MuxName) -> Result<()> {
    if mux == MuxName::Tmux {
        rimz::web::ttyd::ttyd_program()?;
    }
    Ok(())
}

fn ensure_default_config_for_start() -> Result<bool> {
    let config_was_missing = !rimz::config::MachineConfig::config_path().exists();
    let initialized_config = setup::ensure_default_config()?;
    let first_run = config_was_missing && rimz::config::MachineConfig::config_path().exists();
    if initialized_config && !(first_run && start_attended()) {
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
    pub context: RoomContext,
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
    finish_attach(ready, mode)
}

enum ReadyRoom {
    Managed(Box<RoomContext>),
    External {
        session_name: String,
        mux_config: rimz::config::MultiplexerConfig,
        mux: MuxName,
    },
}

fn prepare_room(entry: RoomEntry<'_>, globals: &GlobalFlags) -> Result<ReadyRoom> {
    let mut machine_config = machine_config();
    if matches!(entry, RoomEntry::Start { .. } | RoomEntry::StartWeb { .. }) {
        preflight_account_budgets(rimz::config::MachineConfig::load())?;
        // Fail-fast precondition for installed agents: fixable host misconfiguration
        // aborts the launch here with the fix, before hook-install or session side
        // effects. An enabled host whose agent is not installed is an inert toggle,
        // skipped here so the room still starts; `rimz doctor` surfaces it.
        rimz::remote_control::preflight(&machine_config.remote_control)?;
    }

    let mux = match &entry {
        RoomEntry::Start { mux, .. } | RoomEntry::StartWeb { mux, .. } => *mux,
        RoomEntry::WebSession { record, .. } => {
            render::room::present_mux_pick(pick_mux_for_session(
                &record.session_name,
                globals.mux,
                MissingSessionReport::Silent,
            ))?
        }
        RoomEntry::AttachCwd { workspace, .. } => {
            render::room::present_mux_pick(pick_mux_for_session(
                &workspace.session_name,
                globals.mux,
                MissingSessionReport::Silent,
            ))?
        }
        RoomEntry::AttachSession {
            session, record, ..
        } => {
            let missing_report = if matches!(record, Ok(Some(_))) {
                MissingSessionReport::Silent
            } else {
                MissingSessionReport::Warn
            };
            render::room::present_mux_pick(pick_mux_for_session(
                session,
                globals.mux,
                missing_report,
            ))?
        }
    };

    run_room_preflights(&entry, mux)?;

    let backend = rimz::mux::backend_for(mux);
    // Capture whether this is a plain reattach *before* `ensure_session`, which on
    // tmux would create the session and erase the distinction. A healthy live room
    // re-seeds nothing; only a birth (absent or stuck) resumes prior agents.
    let was_live = RoomContext::session_is_healthy_live(mux, entry.session_name());

    let hook_intro_rendered = if matches!(entry, RoomEntry::Start { .. }) && !was_live {
        ensure_detected_agent_hooks(start_attended())?
    } else {
        false
    };
    if let RoomEntry::Start {
        first_run: true, ..
    } = &entry
        && start_attended()
    {
        let defaults = first_run::Defaults::from_config(&machine_config, rimz::tui::truecolor());
        first_run::run(
            defaults,
            machine_config.theme.pets.clone(),
            hook_intro_rendered,
        )?;
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

    if let RoomEntry::Start { workspace, .. }
    | RoomEntry::StartWeb { workspace, .. }
    | RoomEntry::AttachCwd { workspace, .. } = &entry
    {
        retire_renamed_session(backend.as_ref(), workspace);
    }
    if let RoomEntry::Start { workspace, .. } = &entry
        && !was_live
        && start_attended()
    {
        prompt_project_trust(&workspace.project_root);
    }

    let ready = match &entry {
        RoomEntry::Start { workspace, .. } | RoomEntry::StartWeb { workspace, .. } => {
            let mut context = RoomContext::from_resolved(
                workspace,
                machine_config.clone(),
                mux,
                RoomSizing::Birth,
            )?;
            context.claim_owner()?;
            birth_managed_room(
                &mut context,
                was_live,
                entry.no_resume(),
                entry.resume_prompt_mode(),
                entry.refresh_ms(),
                true,
                workspace.worktree_root.clone(),
            )?;
            ReadyRoom::Managed(Box::new(context))
        }
        RoomEntry::AttachCwd { workspace, .. } => {
            let mut context = RoomContext::from_resolved(
                workspace,
                machine_config.clone(),
                mux,
                RoomSizing::Birth,
            )?;
            context.claim_owner()?;
            birth_managed_room(
                &mut context,
                was_live,
                entry.no_resume(),
                entry.resume_prompt_mode(),
                entry.refresh_ms(),
                false,
                workspace.worktree_root.clone(),
            )?;
            ReadyRoom::Managed(Box::new(context))
        }
        RoomEntry::WebSession { record, .. } => {
            let mut context =
                RoomContext::from_record(record, machine_config.clone(), mux, RoomSizing::Birth)?;
            birth_managed_room(
                &mut context,
                was_live,
                entry.no_resume(),
                entry.resume_prompt_mode(),
                entry.refresh_ms(),
                false,
                record.project_root.clone(),
            )?;
            ReadyRoom::Managed(Box::new(context))
        }
        RoomEntry::AttachSession {
            session, record, ..
        } => match record {
            Ok(Some(record)) => {
                // Only a session Rimz owns (a matching record) is force-reset; a bare
                // external session by this name is never torn down.
                let mut context = RoomContext::from_record(
                    record,
                    machine_config.clone(),
                    mux,
                    RoomSizing::Birth,
                )?;
                birth_managed_room(
                    &mut context,
                    was_live,
                    entry.no_resume(),
                    entry.resume_prompt_mode(),
                    entry.refresh_ms(),
                    false,
                    record.project_root.clone(),
                )?;
                ReadyRoom::Managed(Box::new(context))
            }
            Ok(None) => {
                tracing::warn!(
                    session = %session,
                    "no workspace record matches session; emitting attach command only",
                );
                ReadyRoom::External {
                    session_name: session.clone(),
                    mux_config: rimz::config::MultiplexerConfig::from(machine_config.as_ref()),
                    mux,
                }
            }
            Err(err) => {
                tracing::warn!(
                    session = %session,
                    error = %err,
                    "workspace record lookup failed; emitting attach command only",
                );
                ReadyRoom::External {
                    session_name: session.clone(),
                    mux_config: rimz::config::MultiplexerConfig::from(machine_config.as_ref()),
                    mux,
                }
            }
        },
    };

    Ok(ready)
}

fn birth_managed_room(
    context: &mut RoomContext,
    was_live: bool,
    no_resume: bool,
    resume_prompt: ResumePromptMode,
    refresh_ms: Option<u16>,
    launch_background_view: bool,
    cwd: std::path::PathBuf,
) -> Result<()> {
    let rebirth = if was_live {
        NormalRebirth::Live
    } else {
        match context.inspect_rebirth(no_resume) {
            Ok(plan) => {
                let preview = plan.preview();
                if preview.pane_count() > 0
                    && let Some(death) = preview.death()
                {
                    report_previous_session_death(death);
                }
                let choice = prompt_recover_or_fresh(&preview, resume_prompt)?;
                NormalRebirth::Selected {
                    plan: Box::new(plan),
                    choice,
                }
            }
            Err(err) => {
                tracing::warn!(workspace = %context.workspace_id(), error = %err, "rebirth inspection skipped");
                NormalRebirth::Fresh
            }
        }
    };
    let outcome = render::room::present_birth_outcome(
        context.birth(RoomBirth::Normal(NormalBirth {
            cwd,
            rebirth,
            background_view: if launch_background_view {
                BackgroundViewBirth::Launch
            } else {
                BackgroundViewBirth::Skip
            },
            refresh_ms,
            recovery: if std::io::stdin().is_terminal() {
                AttendedRecovery::Reset
            } else {
                AttendedRecovery::RequireExplicitReset
            },
        })),
        context.session_name(),
    )?;
    report_resume(&outcome.resume);
    Ok(())
}

fn preflight_account_budgets(
    config: rimz::config::Result<rimz::config::MachineConfig>,
) -> Result<()> {
    match config {
        Err(error @ rimz::config::ConfigErr::AccountBudget { .. }) => Err(error.into()),
        Ok(_) | Err(_) => Ok(()),
    }
}

fn run_room_preflights(entry: &RoomEntry<'_>, mux: MuxName) -> Result<()> {
    match entry {
        RoomEntry::Start { workspace, .. } | RoomEntry::StartWeb { workspace, .. } => {
            render::room::print_notices(ensure_single_backend_room(mux, &workspace.session_name)?)?;
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

fn prompt_recover_or_fresh(
    plan: &rimz::harness::rebirth::RebirthPreview,
    mode: ResumePromptMode,
) -> Result<RebirthChoice> {
    let agents = plan.pane_count();
    if agents == 0 || mode == ResumePromptMode::Silent {
        return Ok(RebirthChoice::Recover);
    }
    let labels = plan.labels().join(", ");
    if confirm_with_default(
        &format!(
            "Recover {agents} agent{} ({labels})?",
            if agents == 1 { "" } else { "s" },
        ),
        true,
    )? {
        Ok(RebirthChoice::Recover)
    } else {
        Ok(RebirthChoice::Fresh)
    }
}

fn finish_attach(ready: ReadyRoom, mode: AttachMode) -> Result<()> {
    match ready {
        ReadyRoom::Managed(context) => {
            let spec = context.prepare_attach();
            tracing::info!(
                workspace = %context.workspace_id(),
                session = %context.session_name(),
                mux = %context.mux_name(),
                "workspace ready",
            );
            run_attach_action(&spec, mode, context.mux_name())
        }
        ReadyRoom::External {
            session_name,
            mux_config,
            mux,
        } => {
            let backend = rimz::mux::backend_for(mux);
            let spec = backend.attach_command(&session_name, &mux_config);
            tracing::info!(session = %session_name, mux = %mux, "workspace ready");
            run_attach_action(&spec, mode, mux)
        }
    }
}
