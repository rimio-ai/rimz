//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod codex;
mod doctor;
mod event;
mod feed;
mod gc;
mod hooks;
mod list;
mod pane;
mod parse;
mod reload;
mod resolver;
mod sidebar;
mod statusline;
mod trust;
mod workspace;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use rimz::agents::{HookInstallPreview, StatusLineChange};
use rimz::ids::MuxName;
use rimz::ledger::paths::workspaces_dir;
use rimz::ledger::workspace_record;
use rimz::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, BackgroundViewPane, MuxBackend, SessionOptions,
    SidebarPaneOptions,
};
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths, WorkspaceRecord};

pub(crate) const DEFAULT_SIDEBAR_WIDTH_PERCENT: u16 = 30;

/// Entry point used by `main.rs`.
pub fn dispatch() -> Result<()> {
    let cli = Cli::parse();
    let globals = cli.global;
    match cli.subcommand {
        Some(Subcmd::Workspace(args)) => workspace::run(args, &globals),
        Some(Subcmd::List(args)) => list::run(args, &globals),
        Some(Subcmd::Event(args)) => event::run(args, &globals),
        Some(Subcmd::Feed(args)) => feed::run(args, &globals),
        Some(Subcmd::Gc(args)) => gc::run(args, &globals),
        Some(Subcmd::Reload(args)) => reload::run(args, &globals),
        Some(Subcmd::Pane(args)) => pane::run(args, &globals),
        Some(Subcmd::Resolver(args)) => resolver::run(args, &globals),
        Some(Subcmd::Sidebar(args)) => sidebar::run(args, &globals),
        Some(Subcmd::Statusline(args)) => statusline::run(args, &globals),
        Some(Subcmd::Hooks(args)) => hooks::run(args, &globals),
        Some(Subcmd::Codex(args)) => codex::run(args, &globals),
        Some(Subcmd::Trust(args)) => trust::run(args, &globals),
        Some(Subcmd::Doctor(args)) => doctor::run(args, &globals),
        Some(Subcmd::Ping) => doctor::ping(),
        Some(Subcmd::Start(args)) => start(args, &globals),
        Some(Subcmd::Attach(args)) => attach(args, &globals),
        None => start(
            StartArgs {
                path: cli.path.unwrap_or_else(|| PathBuf::from(".")),
                attach: cli.attach,
            },
            &globals,
        ),
    }
}

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    bin_name = "rimz",
    about = "One room per project for agents, scripts, and humans.",
    subcommand_negates_reqs = true
)]
struct Cli {
    #[clap(flatten)]
    global: GlobalFlags,

    /// Optional path; equivalent to `rimz start <path>`.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    #[clap(flatten)]
    attach: AttachFlags,

    #[command(subcommand)]
    subcommand: Option<Subcmd>,
}

/// Flags accepted at every level. Shared so per-command code stays terse.
#[derive(Debug, Args, Clone)]
pub struct GlobalFlags {
    /// Override multiplexer backend selection.
    #[arg(long, value_parser = parse_mux, global = true)]
    pub mux: Option<MuxName>,
    /// Override project-root resolution (monorepo escape hatch).
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Subcmd {
    /// Start or attach to a workspace session (default action).
    Start(StartArgs),
    /// Attach to a workspace session by name.
    Attach(AttachArgs),
    /// Workspace identity helpers.
    Workspace(workspace::WorkspaceArgs),
    /// Show known workspaces and which mux is currently running them.
    List(list::ListArgs),
    /// Emit generic events into the workspace ledger.
    Event(event::EventArgs),
    /// Feed primitives: ask, push, list, show, resolve, dismiss.
    Feed(feed::FeedArgs),
    /// Remove stale runtime liveness hints.
    Gc(gc::GcArgs),
    /// Reload running sidebars in place (pick up a freshly-installed build).
    Reload(reload::ReloadArgs),
    /// Pane primitives backed by the selected mux backend.
    Pane(pane::PaneArgs),
    /// Manage the per-machine resolver allowlist.
    Resolver(resolver::ResolverArgs),
    /// Sidebar helper API. The sidebar calls these; humans usually do not.
    #[command(hide = true)]
    Sidebar(sidebar::SidebarArgs),
    /// Statusline datasource. The installed `statusLine` command calls this;
    /// humans do not.
    #[command(hide = true)]
    Statusline(statusline::StatuslineArgs),
    /// Install/uninstall agent hooks. Internal hook entrypoints live here too.
    Hooks(hooks::HooksArgs),
    /// Codex helper API. The Codex hook calls these; humans usually do not.
    #[command(hide = true)]
    Codex(codex::CodexArgs),
    /// Manage the project's executable-surface trust grant.
    Trust(trust::TrustArgs),
    /// Environment + backend report.
    Doctor(doctor::DoctorArgs),
    /// Machine-readable liveness check (prints `ok`).
    Ping,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    #[command(flatten)]
    attach: AttachFlags,
    /// Path to use as the workspace cwd.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, Args, Default)]
#[group(required = false, multiple = false)]
pub struct AttachFlags {
    /// Attach to the mux session instead of printing the attach command.
    #[arg(long)]
    attach: bool,
    /// Print the attach command instead of entering the mux session.
    #[arg(long)]
    no_attach: bool,
    /// Alias for `--no-attach`.
    #[arg(long)]
    print: bool,
}

impl AttachFlags {
    fn mode(&self) -> AttachMode {
        if self.attach {
            AttachMode::Attach
        } else if self.no_attach || self.print {
            AttachMode::Print
        } else {
            AttachMode::Auto
        }
    }
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    #[command(flatten)]
    attach: AttachFlags,
    /// Workspace session name (omit to use the cwd's workspace).
    #[arg(value_name = "SESSION")]
    workspace: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachMode {
    Auto,
    Attach,
    Print,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachAction {
    Exec,
    Print,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingSessionReport {
    Silent,
    Warn,
}

fn parse_mux(value: &str) -> std::result::Result<MuxName, String> {
    value.parse::<MuxName>().map_err(|err| err.to_string())
}

fn ensure_detected_agent_hooks() -> Result<()> {
    let mut missing = Vec::new();

    for name in rimz::agents::KNOWN_AGENTS {
        let agent = rimz::agents::integration_by_name(name)?;
        if which::which(agent.name()).is_err() {
            continue;
        }

        if !agent.supports_hook_install() {
            let reason = agent
                .hook_install_unavailable_reason()
                .unwrap_or("hook install is not supported for this adapter");
            tracing::warn!(
                agent = agent.name(),
                reason,
                "detected agent cannot be wired automatically",
            );
            continue;
        }

        if !agent.hooks_installed() {
            missing.push(agent.preview_hook_install()?);
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        print_hook_consent_gate(&missing, false)?;
        return Ok(());
    }

    for name in approve_hook_install(&missing)? {
        let agent = rimz::agents::integration_by_name(name)?;
        let report = agent.install_hooks()?;
        let mut stderr = std::io::stderr().lock();
        writeln!(
            stderr,
            "Installed {} hooks at {}",
            report.agent,
            report.config_path.display(),
        )?;
    }

    Ok(())
}

fn approve_hook_install(previews: &[HookInstallPreview]) -> Result<Vec<&'static str>> {
    print_hook_consent_gate(previews, true)?;
    loop {
        let mut stderr = std::io::stderr().lock();
        write!(stderr, "Choose [Enter/d/c/s]: ")?;
        stderr.flush()?;
        drop(stderr);

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        match answer.trim() {
            "" | "y" | "Y" | "yes" | "YES" | "Yes" => {
                return Ok(previews.iter().map(|preview| preview.agent).collect());
            }
            "d" | "D" => {
                print_hook_diffs(previews)?;
            }
            "c" | "C" => {
                return choose_hook_agents(previews);
            }
            "s" | "S" | "n" | "N" | "no" | "NO" | "No" => return Ok(Vec::new()),
            _ => {
                writeln!(
                    std::io::stderr().lock(),
                    "Enter installs all, d shows the diff, c chooses per agent, s skips."
                )?;
            }
        }
    }
}

fn print_hook_consent_gate(previews: &[HookInstallPreview], interactive: bool) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Rimz first run on this machine: detected agent hooks are not installed for {}.",
        join_agent_names(previews.iter().map(|preview| preview.agent)),
    )?;
    writeln!(
        stderr,
        "Rimz will make an additive, reversible per-user config change so runs appear in the sidebar.",
    )?;
    writeln!(
        stderr,
        "These hooks only report events to Rimz. They never answer a prompt for you.",
    )?;
    for preview in previews {
        writeln!(
            stderr,
            "  + {}: {} events at {}",
            preview.agent,
            preview.planned_events.len(),
            preview.config_path.display(),
        )?;
        match &preview.status_line_change {
            Some(StatusLineChange::Added) => writeln!(
                stderr,
                "      also sets your statusLine to report context to Rimz (removed on uninstall)",
            )?,
            Some(StatusLineChange::Wrapping { original }) => writeln!(
                stderr,
                "      also wraps your statusLine command ({original}) — restored on uninstall",
            )?,
            Some(StatusLineChange::Unchanged) | None => {}
        }
    }
    writeln!(
        stderr,
        "Reversible any time with `rimz hooks uninstall <agent>`."
    )?;
    writeln!(
        stderr,
        "[Enter] install all    [d] show full diff    [c] choose per agent    [s] skip",
    )?;
    if !interactive {
        writeln!(
            stderr,
            "No terminal input is available, so Rimz installs nothing and continues into the room.",
        )?;
    }
    Ok(())
}

fn print_hook_diffs(previews: &[HookInstallPreview]) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    for preview in previews {
        writeln!(stderr, "{}", preview_diff(preview))?;
    }
    Ok(())
}

fn choose_hook_agents(previews: &[HookInstallPreview]) -> Result<Vec<&'static str>> {
    let mut selected = Vec::new();
    for preview in previews {
        let mut stderr = std::io::stderr().lock();
        write!(stderr, "Install {} hooks? [y/N] ", preview.agent)?;
        stderr.flush()?;
        drop(stderr);
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes") {
            selected.push(preview.agent);
        }
    }
    Ok(selected)
}

fn preview_diff(preview: &HookInstallPreview) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "--- {}\n+++ {}\n",
        preview.config_path.display(),
        preview.config_path.display()
    ));
    let original = preview.original_config.as_deref().unwrap_or("");
    if original.is_empty() {
        out.push_str("@@ new file @@\n");
    } else {
        out.push_str("@@ original @@\n");
        for line in original.lines() {
            out.push('-');
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("@@ candidate @@\n");
    }
    for line in preview.candidate_config.lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn join_agent_names(names: impl IntoIterator<Item = &'static str>) -> String {
    names.into_iter().collect::<Vec<_>>().join(", ")
}

fn start(args: StartArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(&args.path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", args.path.display()))?;
    ensure_detected_agent_hooks()?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    retire_renamed_session(backend.as_ref(), &workspace);
    record_workspace(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        cwd: workspace.worktree_root.clone(),
    })?;
    launch_sidebar_for_workspace(
        backend.as_ref(),
        &workspace.workspace_id,
        &workspace.session_name,
        &workspace.worktree_root,
    );
    maybe_launch_remote_control(backend.as_ref(), &workspace);
    let spec = backend.attach_command(&workspace.session_name);
    tracing::info!(
        workspace = %workspace.workspace_id,
        session = %workspace.session_name,
        mux = %mux,
        "workspace ready",
    );
    run_attach_action(&spec, args.attach.mode(), mux)
}

fn attach(args: AttachArgs, globals: &GlobalFlags) -> Result<()> {
    let mode = args.attach.mode();
    match args.workspace {
        Some(session) => attach_named(&session, mode, globals),
        None => attach_cwd(mode, globals),
    }
}

fn attach_cwd(mode: AttachMode, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    retire_renamed_session(backend.as_ref(), &workspace);
    record_workspace(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        cwd: workspace.worktree_root.clone(),
    })?;
    launch_sidebar_for_workspace(
        backend.as_ref(),
        &workspace.workspace_id,
        &workspace.session_name,
        &workspace.worktree_root,
    );
    let spec = backend.attach_command(&workspace.session_name);
    run_attach_action(&spec, mode, mux)
}

fn attach_named(session: &str, mode: AttachMode, globals: &GlobalFlags) -> Result<()> {
    let record = workspace_record_for_session(session);
    let missing_report = if matches!(record, Ok(Some(_))) {
        MissingSessionReport::Silent
    } else {
        MissingSessionReport::Warn
    };
    let mux = pick_mux_for_session(session, globals.mux, missing_report)?;
    let backend = rimz::mux::backend_for(mux);
    match record {
        Ok(Some(record)) => {
            backend.ensure_session(&SessionOptions {
                session_name: record.session_name.clone(),
                cwd: record.project_root.clone(),
            })?;
            launch_sidebar_for_workspace(
                backend.as_ref(),
                &record.workspace_id,
                &record.session_name,
                &record.project_root,
            );
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
    }
    let spec = backend.attach_command(session);
    run_attach_action(&spec, mode, mux)
}

/// Prefer the mux currently hosting `session`. Falls back to auto-detect when
/// the session isn't on any backend; warns to stderr so reattach failures are
/// visible before the user runs the emitted command.
fn pick_mux_for_session(
    session: &str,
    explicit: Option<MuxName>,
    missing_report: MissingSessionReport,
) -> Result<MuxName> {
    if let Some(mux) = explicit {
        return Ok(mux);
    }
    for candidate in [MuxName::Zellij, MuxName::Tmux] {
        match rimz::mux::backend_for(candidate).list_sessions() {
            Ok(sessions) if sessions.iter().any(|s| s == session) => return Ok(candidate),
            Ok(_) => {}
            Err(rimz::mux::MuxErr::NotInstalled { .. }) => {}
            Err(err) => tracing::warn!(mux = %candidate, error = %err, "list_sessions failed"),
        }
    }
    let detected = rimz::mux::auto_detect_backend(None)?;
    if missing_report == MissingSessionReport::Warn {
        tracing::warn!(
            session = %session,
            mux = %detected,
            "no live session matches; emitting attach command for auto-detected mux",
        );
    }
    Ok(detected)
}

/// Decide whether a workspace's live mux session is stranded by a session-name
/// change. The session name is derived from the project root, so changing the
/// derivation (or the path) leaves the previously-born session answering to the
/// recorded name while every new lookup, wakeup, and sidebar launch keys on the
/// derived one. Returns the recorded name to retire when it diverges from the
/// derived name and a session under it is still live.
fn renamed_session_to_retire<'a>(
    recorded: Option<&'a str>,
    derived: &str,
    live: &[String],
) -> Option<&'a str> {
    let recorded = recorded?;
    if recorded == derived {
        return None;
    }
    live.iter().any(|name| name == recorded).then_some(recorded)
}

/// Retire a live session left behind by a session-name change so the upcoming
/// `ensure_session` rebirths the workspace under the derived name (with a fresh
/// sidebar) instead of orphaning the old one. Must run before `record_workspace`
/// overwrites the stored name — that record is the only breadcrumb to the old
/// session. Best-effort: any lookup failure leaves the launch to proceed.
fn retire_renamed_session(backend: &dyn MuxBackend, workspace: &rimz::ResolvedWorkspace) {
    let Ok(paths) = StatePaths::for_workspace(workspace.workspace_id.clone()) else {
        return;
    };
    let recorded = match workspace_record::read(&paths.workspace_record) {
        Ok(record) => record.session_name,
        Err(_) => return, // No prior record: first birth, nothing to retire.
    };
    let live = backend.list_sessions().unwrap_or_default();
    if let Some(stale) = renamed_session_to_retire(Some(&recorded), &workspace.session_name, &live)
    {
        match backend.kill_session(stale) {
            Ok(()) => tracing::info!(
                old = %stale,
                new = %workspace.session_name,
                "retired session left by a session-name change; rebirthing under the new name",
            ),
            Err(err) => tracing::warn!(
                old = %stale,
                error = %err,
                "could not retire renamed session; launch will create the new session alongside it",
            ),
        }
    }
}

fn workspace_record_for_session(session: &str) -> Result<Option<WorkspaceRecord>> {
    let root = workspaces_dir();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", root.display())),
    };
    let mut record_paths = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            record_paths.push(path.join("workspace.json"));
        }
    }
    record_paths.sort();
    for path in record_paths {
        let record = match workspace_record::read(&path) {
            Ok(record) => record,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping unreadable workspace record");
                continue;
            }
        };
        if record.session_name == session {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn launch_sidebar_for_workspace(
    backend: &dyn MuxBackend,
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    cwd: &Path,
) -> rimz::sidebar::SidebarLaunchOutcome {
    let runtime = match RuntimePaths::for_workspace(workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(
                workspace = %workspace_id,
                error = %err,
                "sidebar launch skipped because runtime paths are unavailable",
            );
            return rimz::sidebar::SidebarLaunchOutcome::Failed;
        }
    };
    let rimz_bin = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                workspace = %workspace_id,
                error = %err,
                "sidebar launch skipped because current executable is unavailable",
            );
            return rimz::sidebar::SidebarLaunchOutcome::Failed;
        }
    };
    let opts = SidebarPaneOptions {
        session_name: session_name.to_owned(),
        workspace_id: workspace_id.clone(),
        cwd: cwd.to_path_buf(),
        width_percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
        rimz_bin,
        replace_existing: false,
    };
    rimz::sidebar::launch_sidebar_if_needed(backend, &runtime, &opts)
}

/// Auto-launch the enabled remote-control hosts (Claude, Codex) in one managed
/// background view when the per-machine config opts in and that agent is on
/// PATH. Best-effort: every failure is logged and the room still opens. The view
/// runs from the project root (the main checkout), so Claude's `--spawn=worktree`
/// carves new on-demand sessions off the canonical repo rather than the current
/// worktree.
fn maybe_launch_remote_control(backend: &dyn MuxBackend, workspace: &rimz::ResolvedWorkspace) {
    let config = match rimz::config::MachineConfig::load() {
        Ok(config) => config.remote_control,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "reading per-machine config; skipping remote-control auto-launch",
            );
            return;
        }
    };
    let panes = remote_control_panes(
        &config,
        which::which("claude").is_ok(),
        which::which("codex").is_ok(),
    );
    if panes.is_empty() {
        return;
    }
    let host_count = panes.len();
    let opts = BackgroundViewOptions {
        session_name: workspace.session_name.clone(),
        cwd: workspace.project_root.clone(),
        name: rimz::remote_control::VIEW_NAME.to_owned(),
        panes,
    };
    match backend.open_background_view(&opts) {
        Ok(BackgroundViewLaunch::Launched) => tracing::info!(
            session = %workspace.session_name,
            view = rimz::remote_control::VIEW_NAME,
            hosts = host_count,
            "launched remote-control hosts in a background view",
        ),
        Ok(BackgroundViewLaunch::AlreadyRunning) => tracing::debug!(
            session = %workspace.session_name,
            "remote-control view already present; skipping",
        ),
        Err(err) => tracing::warn!(
            session = %workspace.session_name,
            error = %err,
            "remote-control auto-launch failed; continuing without it",
        ),
    }
}

/// The remote-control panes to launch, from the per-machine toggles and which
/// agents are on PATH — split out pure for testing. An agent contributes a pane
/// only when its toggle is on *and* it is installed. Claude (a long-lived
/// foreground host) leads and never `keep_open`; Codex (`remote-control start`
/// returns once the daemon is up) follows and is `keep_open` so its receipt
/// stays on screen. Claude-first ordering keeps the long-lived host as the
/// view's primary pane.
fn remote_control_panes(
    config: &rimz::config::RemoteControlConfig,
    claude_present: bool,
    codex_present: bool,
) -> Vec<BackgroundViewPane> {
    let mut panes = Vec::new();
    if config.claude && claude_present {
        panes.push(BackgroundViewPane {
            command: rimz::remote_control::claude_command(),
            keep_open: false,
        });
    }
    if config.codex && codex_present {
        panes.push(BackgroundViewPane {
            command: rimz::remote_control::codex_command(),
            keep_open: true,
        });
    }
    panes
}

fn run_attach_action(spec: &rimz::mux::CommandSpec, mode: AttachMode, mux: MuxName) -> Result<()> {
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        inside_selected_mux(mux),
    ) {
        AttachAction::Print => {
            print_attach_command(spec);
            Ok(())
        }
        AttachAction::Exec => exec_attach_command(spec),
    }
}

fn attach_action(
    mode: AttachMode,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    inside_target_mux: bool,
) -> AttachAction {
    match mode {
        AttachMode::Attach => AttachAction::Exec,
        AttachMode::Print => AttachAction::Print,
        AttachMode::Auto if stdin_is_tty && stdout_is_tty && !inside_target_mux => {
            AttachAction::Exec
        }
        AttachMode::Auto => AttachAction::Print,
    }
}

fn inside_selected_mux(mux: MuxName) -> bool {
    match mux {
        MuxName::Zellij => {
            std::env::var_os("ZELLIJ").is_some() || std::env::var_os("ZELLIJ_PANE_ID").is_some()
        }
        MuxName::Tmux => {
            std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some()
        }
    }
}

#[cfg(unix)]
fn exec_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let mut command = spec.to_command();
    let err = command.exec();
    Err::<(), _>(err).with_context(|| format!("execing `{}`", command_display(spec)))
}

#[cfg(not(unix))]
fn exec_attach_command(spec: &rimz::mux::CommandSpec) -> Result<()> {
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", command_display(spec)))?;
    if !status.success() {
        anyhow::bail!(
            "attach command `{}` exited with {status}",
            command_display(spec)
        );
    }
    Ok(())
}

fn print_attach_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", command_display(spec));
    }
}

fn command_display(spec: &rimz::mux::CommandSpec) -> String {
    if spec.args.is_empty() {
        spec.program.clone()
    } else {
        format!("{} {}", spec.program, spec.args.join(" "))
    }
}

/// Open a ledger from a resolved workspace; convenience for command bodies.
pub(crate) fn open_ledger(workspace: &rimz::ResolvedWorkspace) -> Result<Ledger> {
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let ledger = Ledger::open(paths, runtime).context("opening ledger")?;
    ledger
        .record_workspace(workspace)
        .context("recording workspace metadata")?;
    Ok(ledger)
}

pub(crate) fn record_workspace(workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let ledger = Ledger::open(paths, runtime).context("opening ledger")?;
    ledger
        .record_workspace(workspace)
        .context("recording workspace metadata")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_action_matrix() {
        assert_eq!(
            attach_action(AttachMode::Auto, true, true, false),
            AttachAction::Exec,
        );
        assert_eq!(
            attach_action(AttachMode::Auto, false, true, false),
            AttachAction::Print,
        );
        assert_eq!(
            attach_action(AttachMode::Auto, true, false, false),
            AttachAction::Print,
        );
        assert_eq!(
            attach_action(AttachMode::Auto, true, true, true),
            AttachAction::Print,
        );
        assert_eq!(
            attach_action(AttachMode::Attach, false, false, true),
            AttachAction::Exec,
        );
        assert_eq!(
            attach_action(AttachMode::Print, true, true, false),
            AttachAction::Print,
        );
    }

    #[test]
    fn remote_control_panes_need_opt_in_and_detection_per_agent() {
        use rimz::config::RemoteControlConfig;
        let cmds = |panes: &[BackgroundViewPane]| {
            panes
                .iter()
                .map(|p| p.command.first().cloned().unwrap_or_default())
                .collect::<Vec<_>>()
        };

        // Both off → nothing, regardless of what is installed.
        let off = RemoteControlConfig::default();
        assert!(remote_control_panes(&off, true, true).is_empty());

        // A toggle without the binary contributes nothing; with it, one pane.
        let claude_only = RemoteControlConfig {
            claude: true,
            codex: false,
        };
        assert!(remote_control_panes(&claude_only, false, true).is_empty());
        assert_eq!(
            cmds(&remote_control_panes(&claude_only, true, true)),
            vec!["claude"]
        );

        let codex_only = RemoteControlConfig {
            claude: false,
            codex: true,
        };
        assert!(remote_control_panes(&codex_only, true, false).is_empty());
        assert_eq!(
            cmds(&remote_control_panes(&codex_only, true, true)),
            vec!["codex"]
        );

        // Both on + both present → Claude first (the long-lived primary), then
        // Codex which is kept open on its start receipt.
        let both = RemoteControlConfig {
            claude: true,
            codex: true,
        };
        let panes = remote_control_panes(&both, true, true);
        assert_eq!(cmds(&panes), vec!["claude", "codex"]);
        assert!(!panes[0].keep_open, "claude host closes with its process");
        assert!(panes[1].keep_open, "codex pane lingers on its receipt");
    }

    #[test]
    fn renamed_session_retires_only_a_live_diverged_name() {
        let live = vec!["rimz-old".to_owned(), "unrelated".to_owned()];

        // Name changed and the old session is still live: retire it.
        assert_eq!(
            renamed_session_to_retire(Some("rimz-old"), "rimz-new", &live),
            Some("rimz-old"),
        );
        // Name unchanged: nothing to retire even if it is live.
        assert_eq!(
            renamed_session_to_retire(Some("rimz-old"), "rimz-old", &live),
            None,
        );
        // Name changed but no live session under the old name: nothing to kill.
        assert_eq!(
            renamed_session_to_retire(Some("rimz-gone"), "rimz-new", &live),
            None,
        );
        // No prior record (first birth): nothing to retire.
        assert_eq!(renamed_session_to_retire(None, "rimz-new", &live), None);
    }
}
