//! CLI parsing surface. Each subcommand has its own file under `cli/` and
//! exposes a single `run(...)` entry called from `dispatch`.

mod agents_cmd;
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
mod reset;
mod resolver;
mod sidebar;
mod statusline;
mod tab;
mod trust;
mod workspace;
mod worktree;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use rimz::agents::{HookInstallPreview, StatusLineChange};
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::paths::workspaces_dir;
use rimz::ledger::workspace_record;
use rimz::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, DaemonView, HostPane, MuxBackend,
    PresencePluginOptions, SessionHealth, SessionOptions, SidebarPaneOptions, SidebarWidth,
};
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths, WorkspaceRecord};

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
        Some(Subcmd::Worktree(args)) => worktree::run(args, &globals),
        Some(Subcmd::Tab(args)) => tab::run(args, &globals),
        Some(Subcmd::Agents(args)) => agents_cmd::run(args, &globals),
        Some(Subcmd::Reload(args)) => reload::run(args, &globals),
        Some(Subcmd::Reset(args)) => reset::run(args, &globals),
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
                no_resume: cli.no_resume,
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

    /// Come up empty: skip re-seeding prior agents when the session is reborn.
    #[arg(long)]
    no_resume: bool,

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
    /// Feed primitives: ask, push, list, show, resolve, dismiss, abstain.
    Feed(feed::FeedArgs),
    /// Remove stale runtime liveness hints.
    Gc(gc::GcArgs),
    /// Create, list, and remove Rimz-owned git worktrees.
    Worktree(worktree::WorktreeArgs),
    /// Open one laid-out tab/window in the current Rimz room.
    Tab(tab::TabArgs),
    /// Launch agent tabs, optionally in Rimz-owned worktrees.
    Agents(agents_cmd::AgentsArgs),
    /// Reload running sidebars in place (pick up a freshly-installed build).
    Reload(reload::ReloadArgs),
    /// Force a clean rebirth of this workspace's room, destroying a stuck or
    /// resurrected Zellij session and sweeping its orphaned processes.
    Reset(reset::ResetArgs),
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
    /// Come up empty: skip re-seeding prior agents when the session is reborn.
    #[arg(long)]
    pub no_resume: bool,
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
    #[arg(value_name = "SESSION", conflicts_with = "remote")]
    workspace: Option<String>,
    /// SSH remote attach: `[user@]host:<session-or-path>`. The host's own
    /// rimz starts or reattaches the room; it renders here over `ssh -t`.
    #[arg(long, value_name = "TARGET")]
    remote: Option<String>,
    /// Hand the link to a single ssh run instead of supervising it and
    /// reconnecting when it drops.
    #[arg(long, requires = "remote")]
    no_reconnect: bool,
    /// Come up empty: skip re-seeding prior agents when the session is reborn.
    #[arg(long)]
    pub no_resume: bool,
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

    for agent in rimz::agents::ADAPTERS {
        let descriptor = agent.descriptor();
        if which::which(descriptor.kind).is_err() {
            continue;
        }

        if !descriptor.capabilities.hook_install {
            let reason = descriptor
                .hook_install_unavailable
                .unwrap_or("hook install is not supported for this adapter");
            tracing::warn!(
                agent = descriptor.kind,
                reason,
                "detected agent cannot be wired automatically",
            );
            continue;
        }

        if !agent.hooks_installed() {
            missing.push(agent.preview_hook_install()?);
            continue;
        }

        warn_untrusted_hooks(descriptor.kind, &agent.untrusted_installed_hooks())?;
    }

    if missing.is_empty() {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        print_hook_consent_gate(&missing, false)?;
        return Ok(());
    }

    for name in approve_hook_install(&missing)? {
        let agent = rimz::agents::adapter_by_kind(name)?;
        let report = agent.install_hooks()?;
        {
            let mut stderr = std::io::stderr().lock();
            writeln!(
                stderr,
                "Installed {} hooks at {}",
                report.agent,
                report.config_path.display(),
            )?;
        }
        // A fresh install lands untrusted, so the notice must follow it here
        // — the user is one `/hooks` away from a live channel, not done.
        warn_untrusted_hooks(name, &agent.untrusted_installed_hooks())?;
    }

    Ok(())
}

/// Stderr notice for hooks the agent's own trust gate still skips: the gate
/// silences every untrusted hook with no signal of its own, so the start
/// notice is where the dead channel becomes visible. Rimz cannot trust on
/// the user's behalf — only the agent's own UI can — so this warns with the
/// fix rather than gating the start. No-op when `untrusted` is empty.
fn warn_untrusted_hooks(kind: &str, untrusted: &[String]) -> Result<()> {
    if untrusted.is_empty() {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "{kind} hooks are installed but untrusted ({}) — {kind} silently skips them; {}",
        untrusted.join(", "),
        rimz::agents::hook_trust_fix(kind),
    )?;
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

/// One consent line for a statusline-style wrap (`statusLine` or
/// `subagentStatusLine`), keeping the change a visible security surface. An
/// unchanged re-install or an agent that manages no such command prints nothing.
fn write_status_line_consent(
    w: &mut impl std::io::Write,
    key: &str,
    purpose: &str,
    change: &Option<StatusLineChange>,
) -> Result<()> {
    match change {
        Some(StatusLineChange::Added) => writeln!(
            w,
            "      also sets your {key} to {purpose} (removed on uninstall)",
        )?,
        Some(StatusLineChange::Wrapping { original }) => writeln!(
            w,
            "      also wraps your {key} command ({original}) — restored on uninstall",
        )?,
        Some(StatusLineChange::Unchanged) | None => {}
    }
    Ok(())
}

fn print_hook_consent_gate(previews: &[HookInstallPreview], interactive: bool) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Rimz: agent hooks are not currently installed for {}.",
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
        write_status_line_consent(
            &mut stderr,
            "statusLine",
            "report context to Rimz",
            &preview.status_line_change,
        )?;
        write_status_line_consent(
            &mut stderr,
            "subagentStatusLine",
            "report subagent activity to Rimz",
            &preview.subagent_status_line_change,
        )?;
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
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    // A same-mux room can't be nested: if we're already inside this backend's
    // session, report the directory's room and stop before any launch side
    // effect — hook install, session birth, sidebar, or the doomed nested
    // `attach --create`.
    if should_report_already_inside(args.attach.mode(), inside_selected_mux(mux)) {
        report_already_inside(mux, &workspace)?;
        return Ok(());
    }
    report_start_notices(&workspace)?;
    let machine_config = machine_config();
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let sidebar_width = SidebarWidth::from_config(&machine_config.sidebar);
    // One terminal probe per command flow: the width picks every sidebar
    // pane's birth size; the pair sizes a detached tmux birth.
    let detected_size = rimz::mux::detect_terminal_size();
    let remote_control = &machine_config.remote_control;
    // Fail-fast precondition: an enabled host that cannot start aborts the
    // launch here, with the fix, before any hook-install or session side
    // effects — never bring a workspace up around a doomed host.
    rimz::remote_control::preflight(remote_control)?;
    ensure_detected_agent_hooks()?;
    let backend = rimz::mux::backend_for(mux);
    retire_renamed_session(backend.as_ref(), &workspace);
    // Capture whether this is a plain reattach *before* `ensure_session`, which on
    // tmux would create the session and erase the distinction. A healthy live room
    // re-seeds nothing; only a birth (absent or stuck) resumes prior agents.
    let was_live = session_is_healthy_live(backend.as_ref(), &workspace.session_name);
    record_workspace(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: workspace.worktree_root.clone(),
        config: mux_config.clone(),
        detected_size,
    })?;
    // The daemon view (`rimzd`) is computed once, before the session is born: its
    // hosts depend on config and which agents are on PATH. When present, it leads
    // the session — on Zellij that order is fixed at birth (`open_sidebar` renders
    // the daemon tab first), since Zellij can't reorder tabs afterwards.
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &workspace.worktree_root,
        mux_config: &mux_config,
        width: sidebar_width,
        detected_size,
    };
    let daemon_view = build_daemon_view(remote_control, &workspace, &mux_config, &room);
    let daemon = daemon_view.as_ref().map(|view| DaemonView {
        name: view.name.clone(),
        hosts: view.hosts.clone(),
    });
    // Plan which prior agents the reborn room re-seeds, from the durable rollup.
    // Empty on a healthy reattach (the agents are still alive), when nothing is
    // recoverable, or when the user opted out — then the birth is exactly today's
    // bare working room.
    let resume_plan = if was_live {
        rimz::resume::ResumePlan::default()
    } else {
        plan_room_resume(
            &workspace.workspace_id,
            &workspace.session_name,
            &machine_config.resume,
            args.no_resume,
        )
    };
    launch_sidebar_for_workspace(backend.as_ref(), &room, daemon.as_ref(), &resume_plan.panes);
    maybe_launch_remote_control(
        backend.as_ref(),
        &workspace,
        remote_control,
        daemon_view.as_ref(),
    );
    // Authoritative gate before the resurrecting `attach --create`: rebirth an
    // inspected stale/serialized room, and on one that cannot self-heal or
    // cannot be inspected, offer a reset (interactive) or fail fast with the fix
    // (non-interactive). The reborn room is seeded with the resume panes.
    gate_room_before_attach(backend.as_ref(), &room, daemon.as_ref(), &resume_plan.panes)?;
    report_resume(&resume_plan);
    ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
    );
    let spec = backend.attach_command(&workspace.session_name, &mux_config);
    tracing::info!(
        workspace = %workspace.workspace_id,
        session = %workspace.session_name,
        mux = %mux,
        "workspace ready",
    );
    run_attach_action(&spec, args.attach.mode(), mux)
}

/// Best-effort load of the session's presence plugin — the Zellij push
/// channel that retires the producer's steady-state pane poll (tmux is a
/// no-op; its control-mode watch already pushes). Fired on every attach-shaped
/// flow: the load verb is idempotent and clientless-safe, so a room born
/// detached, a reattach, and a permission granted minutes after the first
/// prompt all converge with no machinery of their own. Failure costs latency
/// only — the producer keeps today's poll — so it never blocks an attach.
fn ensure_presence_plugin(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
) {
    let Some(wasm) = rimz::mux::zellij::presence_plugin_path() else {
        tracing::debug!(
            session = %session_name,
            "presence plugin artifact not installed; the producer keeps its pane poll",
        );
        return;
    };
    let opts = PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin: sidebar::rimz_cli_program(),
        converge: false,
    };
    if let Err(err) = backend.ensure_presence_plugin(&opts) {
        tracing::debug!(
            session = %session_name,
            error = %err,
            "presence plugin load failed; the producer keeps its pane poll",
        );
    }
}

fn attach(args: AttachArgs, globals: &GlobalFlags) -> Result<()> {
    let mode = args.attach.mode();
    if let Some(target) = args.remote.as_deref() {
        return attach_remote(target, mode, args.no_resume, args.no_reconnect, globals);
    }
    match args.workspace {
        Some(session) => attach_named(&session, mode, args.no_resume, globals),
        None => attach_cwd(mode, args.no_resume, globals),
    }
}

/// SSH remote attach: the local rimz is a launcher and link supervisor only.
/// Workspace resolution, session birth, the sidebar, and the health gate all
/// run on the remote host's own `rimz`; the room renders here over `ssh -t`.
fn attach_remote(
    target: &str,
    mode: AttachMode,
    no_resume: bool,
    no_reconnect: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let target = rimz::remote::RemoteTarget::parse(target)?;
    let spec = rimz::remote::ssh_attach_spec(&target, no_resume, globals.mux);

    // The local nesting block does not apply: a remote room inside a local
    // pane is a legitimate shape (the remote rimz checks its own env).
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        false,
    ) {
        AttachAction::Print => {
            print_remote_command(&spec);
            Ok(())
        }
        AttachAction::Exec => {
            let program = rimz::remote::ssh_program();
            which::which(&program).map_err(|_| {
                anyhow::anyhow!(
                    "`{program}` is not on PATH; install an OpenSSH client to attach \
                     remotely, or run with --print to emit the command"
                )
            })?;
            if no_reconnect {
                report_remote_connect(target.host_display(), false);
                exec_attach_command(&spec)
            } else {
                supervise_remote(&spec, &target)
            }
        }
    }
}

/// Run ssh and keep the link alive, autossh-style: a clean detach exits, a
/// dropped link on an established session reconnects with capped backoff, and
/// anything else fails with the remote's own error. The remote mux session
/// survives the drop by design, so reattaching is idempotent.
fn supervise_remote(
    spec: &rimz::mux::CommandSpec,
    target: &rimz::remote::RemoteTarget,
) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, Verdict};

    let policy = ReconnectPolicy::from_env();
    let host = target.host_display();
    let mut established = false;
    let mut consecutive_failures: u32 = 0;
    report_remote_connect(host, true);
    loop {
        let started = std::time::Instant::now();
        let status = spec
            .to_command()
            .status()
            .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
        if started.elapsed() >= policy.gatetime {
            established = true;
            consecutive_failures = 0;
        }
        match rimz::remote::verdict(status.code(), established, consecutive_failures, &policy) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => anyhow::bail!(
                "ssh to {host} exited with status {code}; not reconnecting \
                 (only a dropped link on an established session is retried)"
            ),
            Verdict::Retry { delay } => {
                // Numbered per outage: failures reset once a session lives
                // past the gatetime, so this never reads "attempt 47" after a
                // week of clean reconnects.
                consecutive_failures = consecutive_failures.saturating_add(1);
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "rimz: link to {host} lost — reconnecting in {}s (attempt {consecutive_failures}); Ctrl-C stops",
                    delay.as_secs(),
                );
                drop(stderr);
                std::thread::sleep(delay);
            }
        }
    }
}

/// One stderr line before the terminal belongs to ssh, so the user knows the
/// room they are about to see is remote.
fn report_remote_connect(host: &str, reconnect: bool) {
    let mut stderr = std::io::stderr().lock();
    let tail = if reconnect {
        " (auto-reconnect on; Ctrl-C stops)"
    } else {
        ""
    };
    let _ = writeln!(stderr, "rimz: attaching to {host} over ssh…{tail}");
}

fn print_remote_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", rimz::remote::display_ssh_command(spec));
    }
}

fn attach_cwd(mode: AttachMode, no_resume: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let machine_config = machine_config();
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let sidebar_width = SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    retire_renamed_session(backend.as_ref(), &workspace);
    let was_live = session_is_healthy_live(backend.as_ref(), &workspace.session_name);
    record_workspace(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: workspace.worktree_root.clone(),
        config: mux_config.clone(),
        detected_size,
    })?;
    let resume_plan = if was_live {
        rimz::resume::ResumePlan::default()
    } else {
        plan_room_resume(
            &workspace.workspace_id,
            &workspace.session_name,
            &machine_config.resume,
            no_resume,
        )
    };
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &workspace.worktree_root,
        mux_config: &mux_config,
        width: sidebar_width,
        detected_size,
    };
    launch_sidebar_for_workspace(backend.as_ref(), &room, None, &resume_plan.panes);
    gate_room_before_attach(backend.as_ref(), &room, None, &resume_plan.panes)?;
    report_resume(&resume_plan);
    ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
    );
    let spec = backend.attach_command(&workspace.session_name, &mux_config);
    run_attach_action(&spec, mode, mux)
}

fn attach_named(
    session: &str,
    mode: AttachMode,
    no_resume: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let record = workspace_record_for_session(session);
    let missing_report = if matches!(record, Ok(Some(_))) {
        MissingSessionReport::Silent
    } else {
        MissingSessionReport::Warn
    };
    let machine_config = machine_config();
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let sidebar_width = SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let mux = pick_mux_for_session(session, globals.mux, missing_report)?;
    let backend = rimz::mux::backend_for(mux);
    // Captured before `ensure_session` so a tmux create never masks a reattach.
    let was_live = session_is_healthy_live(backend.as_ref(), session);
    match record {
        Ok(Some(record)) => {
            backend.ensure_session(&SessionOptions {
                session_name: record.session_name.clone(),
                workspace_id: record.workspace_id.clone(),
                project_root: record.project_root.clone(),
                cwd: record.project_root.clone(),
                config: mux_config.clone(),
                detected_size,
            })?;
            let resume_plan = if was_live {
                rimz::resume::ResumePlan::default()
            } else {
                plan_room_resume(
                    &record.workspace_id,
                    &record.session_name,
                    &machine_config.resume,
                    no_resume,
                )
            };
            let room = RoomTarget {
                workspace_id: &record.workspace_id,
                project_root: &record.project_root,
                session_name: &record.session_name,
                cwd: &record.project_root,
                mux_config: &mux_config,
                width: sidebar_width,
                detected_size,
            };
            launch_sidebar_for_workspace(backend.as_ref(), &room, None, &resume_plan.panes);
            // Only a session Rimz owns (a matching record) is force-reset; a bare
            // external session by this name is never torn down.
            gate_room_before_attach(backend.as_ref(), &room, None, &resume_plan.panes)?;
            report_resume(&resume_plan);
            ensure_presence_plugin(backend.as_ref(), &record.session_name, &record.workspace_id);
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
    let spec = backend.attach_command(session, &mux_config);
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

/// Build the sidebar/room options shared by the best-effort sidebar launch and
/// the authoritative pre-attach health gate, so the room shape is defined once.
/// Whether the session is already a healthy, live room we will simply reattach
/// to — so there is no birth to re-seed prior agents into. Probed before any
/// launch side effect (tmux's `ensure_session` would otherwise create it). An
/// absent or stuck/unhealthy room returns `false`: a birth is coming, and that
/// is what resume seeds. Best-effort: an unprobeable backend reads as not-live,
/// so resume errs toward seeding rather than silently coming up empty.
fn session_is_healthy_live(backend: &dyn MuxBackend, session_name: &str) -> bool {
    let exists = backend
        .list_sessions()
        .map(|sessions| sessions.iter().any(|name| name == session_name))
        .unwrap_or(false);
    exists
        && matches!(
            backend.probe_session_health(session_name),
            Ok(SessionHealth::Healthy)
        )
}

/// Plan the agents a reborn session re-seeds, reading the durable *audit*
/// rollup — the one that keeps the dead-process agents a runtime read would
/// expel, which is exactly the set a rebirth must bring back. Best-effort: a
/// disabled feature, the `--no-resume` override, or any ledger read error yields
/// an empty plan (the birth comes up bare) and never blocks the launch.
fn plan_room_resume(
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    resume_cfg: &rimz::config::ResumeConfig,
    disabled: bool,
) -> rimz::resume::ResumePlan {
    if disabled || !resume_cfg.on_rebirth {
        return rimz::resume::ResumePlan::default();
    }
    let planned = (|| -> Result<rimz::resume::ResumePlan> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        let ledger = Ledger::open(paths, runtime)?;
        let projection = ledger.runtime_projection(rimz::RuntimeScope::Audit)?;
        let ended = rimz::ledger::snapshot::agent_tombstones_for_events(&projection.events);
        Ok(rimz::resume::plan_resume(
            &projection.agents,
            session_name,
            &ended,
            resume_cfg.max,
            |path| path.is_dir(),
        ))
    })();
    planned.unwrap_or_else(|err| {
        tracing::warn!(workspace = %workspace_id, error = %err, "resume planning skipped");
        rimz::resume::ResumePlan::default()
    })
}

/// Tell the user which prior agents the reborn room brought back, and which it
/// could not — to stderr, so the attach command on stdout stays clean for
/// scripting. Silent when there is nothing to resume.
fn report_resume(plan: &rimz::resume::ResumePlan) {
    if !plan.panes.is_empty() {
        let labels = plan
            .panes
            .iter()
            .map(|pane| pane.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            std::io::stderr(),
            "resumed {} agent{}: {labels}",
            plan.panes.len(),
            if plan.panes.len() == 1 { "" } else { "s" },
        );
    }
    if !plan.skipped.is_empty() {
        let detail = plan
            .skipped
            .iter()
            .map(|skip| format!("{} ({})", skip.label, resume_skip_reason(skip.reason)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(std::io::stderr(), "not resumed: {detail}");
    }
}

fn resume_skip_reason(reason: rimz::resume::ResumeSkipReason) -> &'static str {
    match reason {
        rimz::resume::ResumeSkipReason::NoResumeSupport => "no resume CLI",
        rimz::resume::ResumeSkipReason::WorktreeMissing => "worktree gone",
        rimz::resume::ResumeSkipReason::OverCap => "over the resume cap",
    }
}

/// The room a sidebar launch or pre-attach gate targets: workspace identity
/// plus the per-machine knobs every [`SidebarPaneOptions`] build shares. One
/// value per command flow, threaded by reference through the launch and gate
/// helpers.
struct RoomTarget<'a> {
    workspace_id: &'a rimz::WorkspaceId,
    /// The workspace root behind the id — paired into the identity pin a
    /// session birth stamps into the mux environment.
    project_root: &'a Path,
    session_name: &'a str,
    cwd: &'a Path,
    mux_config: &'a rimz::config::MultiplexerConfig,
    width: SidebarWidth,
    /// The launching terminal's `(cols, rows)`, probed once per command
    /// ([`rimz::mux::detect_terminal_size`]): the width picks the sidebar's
    /// birth size, the pair sizes a detached tmux birth.
    detected_size: Option<(u16, u16)>,
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

fn build_sidebar_opts(
    target: &RoomTarget<'_>,
    resume_panes: Vec<rimz::mux::ResumePane>,
) -> Result<SidebarPaneOptions> {
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    Ok(SidebarPaneOptions {
        session_name: target.session_name.to_owned(),
        workspace_id: target.workspace_id.clone(),
        project_root: target.project_root.to_path_buf(),
        cwd: target.cwd.to_path_buf(),
        width: target.width,
        birth_size: target.birth_size(),
        rimz_bin,
        replace_existing: false,
        config: target.mux_config.clone(),
        resume_panes,
    })
}

fn launch_sidebar_for_workspace(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
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
    let opts = match build_sidebar_opts(target, resume_panes.to_vec()) {
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
    rimz::sidebar::launch_sidebar_if_needed(backend, &runtime, &opts, daemon)
}

/// Authoritative pre-attach gate: guarantee the imminent `attach` lands on a
/// clean, running room rather than resurrecting a stale serialized one. The
/// best-effort sidebar launch above can skip (a fresh heartbeat short-circuits
/// it) or fail without rebirthing, so this is the un-bypassable check. A probe
/// command error degrades to today's behaviour (attach anyway) rather than
/// blocking; bounded backend health failures return [`SessionHealth::Stuck`] so
/// the reset path can preserve an uninspectable live room.
fn ensure_clean_room(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> SessionHealth {
    let opts = match build_sidebar_opts(target, resume_panes.to_vec()) {
        Ok(opts) => opts,
        Err(err) => {
            tracing::warn!(error = %err, "session health gate skipped; attaching as-is");
            return SessionHealth::Healthy;
        }
    };
    match backend.ensure_clean_session(&opts, daemon) {
        Ok(health) => health,
        Err(err) => {
            tracing::warn!(error = %err, "session health gate failed; attaching as-is");
            SessionHealth::Healthy
        }
    }
}

/// Run the pre-attach health gate and, if the room cannot self-heal, handle the
/// stuck case (offer a reset, or fail fast). The single entry the attach flows
/// call before building the attach command.
fn gate_room_before_attach(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> Result<()> {
    if let SessionHealth::Stuck = ensure_clean_room(backend, target, daemon, resume_panes) {
        recover_stuck_room(backend, target, daemon, resume_panes)?;
    }
    Ok(())
}

/// Handle a room the pre-attach gate could not make clean. Interactively, offer
/// the destructive reset and, on consent, run it and re-gate once. Without a
/// terminal to confirm, fail fast with the fix — never destroy a room unattended.
fn recover_stuck_room(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        return Err(ResetRequired {
            session: target.session_name.to_owned(),
        }
        .into());
    }
    if !confirm_reset(target.session_name)? {
        anyhow::bail!("room left untouched; run `rimz reset` when ready");
    }
    let runtime = RuntimePaths::for_workspace(target.workspace_id.clone())?;
    let report = rimz::mux::recovery::teardown_room(
        backend,
        target.workspace_id,
        target.session_name,
        &runtime,
    );
    print_reset_report(&report)?;
    match ensure_clean_room(backend, target, daemon, resume_panes) {
        SessionHealth::Stuck => {
            anyhow::bail!("the room is still stuck after a reset; inspect with `rimz doctor`")
        }
        SessionHealth::Healthy | SessionHealth::Reborn => Ok(()),
    }
}

/// Single y/N confirmation, modelled on the hook consent prompt: written to
/// stderr (stdout stays clean for scripting), default No. The caller is expected
/// to have already checked `stdin().is_terminal()`.
pub(crate) fn confirm(prompt: &str) -> Result<bool> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{prompt} [y/N] ")?;
    stderr.flush()?;
    drop(stderr);
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

/// The `rimz start` auto-offer confirmation for a stuck room.
fn confirm_reset(session: &str) -> Result<bool> {
    confirm(&format!(
        "Rimz must reset the '{session}' room to clear a stuck or uninspectable \
         Zellij session. Reset now?"
    ))
}

/// Rebuild and attach the room from scratch — the rebirth half of `rimz reset`,
/// run after teardown so the session comes up clean and running.
pub(crate) fn rebirth_room(path: PathBuf, globals: &GlobalFlags) -> Result<()> {
    start(
        StartArgs {
            attach: AttachFlags::default(),
            path,
            // A reset fixes the room; the agents are ledger truth, so the rebirth
            // still resumes them. `rimz reset --no-resume` would clean-slate, but
            // reset itself does not force one.
            no_resume: false,
        },
        globals,
    )
}

/// Report what a teardown removed, to stderr (diagnostic, not stdout output).
pub(crate) fn print_reset_report(report: &rimz::mux::recovery::TeardownReport) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Reset: session {}, {} cache entr{} removed, {} orphan process{} swept.",
        if report.session_killed {
            "deleted"
        } else {
            "absent"
        },
        report.cache_removed.len(),
        if report.cache_removed.len() == 1 {
            "y"
        } else {
            "ies"
        },
        report.processes_swept.len(),
        if report.processes_swept.len() == 1 {
            ""
        } else {
            "es"
        },
    )?;
    Ok(())
}

/// No terminal is available to confirm a destructive reset of a stuck room.
/// `Display` carries the fix, mirroring [`rimz::remote_control::PreflightError`].
#[derive(Debug)]
struct ResetRequired {
    session: String,
}

impl std::fmt::Display for ResetRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The '{}' Zellij room is stuck or cannot be inspected safely enough to self-heal \
             without a destructive reset.\n\
             No terminal is available to confirm one. Run `rimz reset` to rebuild it cleanly.",
            self.session,
        )
    }
}

impl std::error::Error for ResetRequired {}

/// The per-machine config. A malformed config falls back to defaults after a
/// warning, preserving the existing "config read is best-effort" contract for
/// personal preferences.
fn machine_config() -> rimz::config::MachineConfig {
    match rimz::config::MachineConfig::load() {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "reading per-machine config; using built-in defaults",
            );
            rimz::config::MachineConfig::default()
        }
    }
}

/// Build the `rimzd` daemon view for `rimz start`, or `None` when no host applies
/// (so no view is opened and the working view leads alone). Three capabilities
/// feed it:
///
/// - **Codex app-server broker** — a *local* read-only enrichment host (not the
///   account-linking remote-control feature), so it is ungated: a pane in the
///   daemon view whenever `codex` is on PATH.
/// - **Claude remote-control host** — a long-lived foreground host, a pane in the
///   daemon view when the `claude` toggle is on and `claude` is on PATH, run from
///   the project root so `--spawn=worktree` carves sessions off the canonical repo.
///
/// (The third, the per-user **Codex remote-control daemon**, is not a pane — it is
/// ensured separately by [`maybe_launch_remote_control`].)
fn build_daemon_view(
    config: &rimz::config::RemoteControlConfig,
    workspace: &rimz::ResolvedWorkspace,
    mux_config: &rimz::config::MultiplexerConfig,
    room: &RoomTarget<'_>,
) -> Option<BackgroundViewOptions> {
    let rimz_bin = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                session = %workspace.session_name,
                error = %err,
                "daemon view skipped because the current executable is unavailable",
            );
            return None;
        }
    };
    let hosts = background_view_hosts(
        config,
        which::which("claude").is_ok(),
        which::which("codex").is_ok(),
        &rimz_bin,
        &workspace.workspace_id,
        &workspace.session_name,
        &workspace.project_root,
        &workspace.worktree_root,
    );
    if hosts.is_empty() {
        return None;
    }
    // The daemon view is born `sidebar | hosts…`, so it carries the same global
    // sidebar the working view runs (same session, workspace, and `rimz` bin).
    Some(BackgroundViewOptions {
        name: rimz::remote_control::VIEW_NAME.to_owned(),
        hosts,
        sidebar: SidebarPaneOptions {
            session_name: workspace.session_name.clone(),
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            cwd: workspace.worktree_root.clone(),
            width: room.width,
            birth_size: room.birth_size(),
            rimz_bin,
            replace_existing: false,
            config: mux_config.clone(),
            resume_panes: Vec::new(),
        },
    })
}

/// Ensure the per-user Codex remote-control daemon (a detached singleton keyed by
/// its control socket — never a pane; its standalone-install precondition is
/// enforced earlier by [`rimz::remote_control::preflight`]) and open the `rimzd`
/// daemon view, best-effort. On Zellij the view already leads from session birth
/// ([`MuxBackend::open_sidebar`] renders it first), so this is the idempotent
/// `AlreadyRunning` no-op there; on tmux it opens the window and leads it via
/// `swap-window`. Skipped when there is no host pane.
fn maybe_launch_remote_control(
    backend: &dyn MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::RemoteControlConfig,
    daemon_view: Option<&BackgroundViewOptions>,
) {
    rimz::remote_control::ensure_codex_daemon(config);

    let Some(opts) = daemon_view else {
        return;
    };
    match backend.open_background_view(opts) {
        Ok(BackgroundViewLaunch::Launched) => tracing::info!(
            session = %workspace.session_name,
            view = rimz::remote_control::VIEW_NAME,
            "launched the daemon view",
        ),
        Ok(BackgroundViewLaunch::AlreadyRunning) => tracing::debug!(
            session = %workspace.session_name,
            "daemon view already present; skipping",
        ),
        Err(err) => tracing::warn!(
            session = %workspace.session_name,
            error = %err,
            "daemon view launch failed; continuing without it",
        ),
    }
}

/// The host panes for the [`rimz::remote_control::VIEW_NAME`] daemon view, in
/// display order (the first takes focus) — split out pure for testing. The Claude
/// remote-control host leads when its toggle is on *and* `claude` is on PATH (the
/// interactive host); the local Codex app-server broker follows whenever `codex`
/// is on PATH (ungated — it links no account, only reads). Empty when neither
/// applies, so the caller opens no view.
#[allow(clippy::too_many_arguments)]
fn background_view_hosts(
    config: &rimz::config::RemoteControlConfig,
    claude_present: bool,
    codex_present: bool,
    rimz_bin: &Path,
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    project_root: &Path,
    worktree_root: &Path,
) -> Vec<HostPane> {
    let mut hosts = Vec::new();
    if config.claude && claude_present {
        hosts.push(HostPane {
            argv: rimz::remote_control::claude_command(),
            cwd: project_root.to_path_buf(),
        });
    }
    if codex_present {
        hosts.push(HostPane {
            argv: vec![
                rimz_bin.to_string_lossy().into_owned(),
                "codex".to_owned(),
                "app-server".to_owned(),
                "serve".to_owned(),
                "--workspace-id".to_owned(),
                workspace_id.as_str().to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
            ],
            cwd: worktree_root.to_path_buf(),
        });
    }
    hosts
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

/// Report the existing room instead of launching only when the attach mode is
/// opportunistic (`Auto`). Explicit `--print` / `--attach` stay literal escape
/// hatches (scripting / forced exec), so they fall through to the normal path.
fn should_report_already_inside(mode: AttachMode, inside_mux: bool) -> bool {
    matches!(mode, AttachMode::Auto) && inside_mux
}

fn report_already_inside(mux: MuxName, workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "You're already inside a {mux} session, which can't host a nested room.",
    )?;
    writeln!(
        stderr,
        "This directory's room is `{}`. Detach to (re)launch it, or run `rimz` from outside the session.",
        workspace.session_name,
    )?;
    Ok(())
}

/// One stderr line naming a non-repo root class, so the identity choice is
/// visible the moment a marker or directory workspace is chosen. Repo rooms —
/// the common case — stay silent.
fn root_class_notice(workspace: &rimz::ResolvedWorkspace) -> Option<String> {
    use rimz::workspace::RootClass;
    match workspace.root_class {
        RootClass::Repo => None,
        RootClass::Marker => Some(format!(
            "marker-rooted workspace at {} (project marker, no git repository)",
            workspace.project_root.display(),
        )),
        RootClass::Directory => Some(format!(
            "directory workspace rooted at {} (no repository or project marker)",
            workspace.project_root.display(),
        )),
    }
}

/// The known workspaces whose recorded root nests inside or contains this
/// room's root — the candidates the (more expensive) liveness probe filters.
fn overlapping_known<'a>(
    workspace: &rimz::ResolvedWorkspace,
    known: &'a [rimz::workspace::KnownWorkspace],
) -> Vec<&'a rimz::workspace::KnownWorkspace> {
    use rimz::workspace::root_contains;
    known
        .iter()
        .filter(|ws| ws.workspace_id != workspace.workspace_id)
        .filter(|ws| {
            root_contains(&ws.project_root, &workspace.project_root)
                || root_contains(&workspace.project_root, &ws.project_root)
        })
        .collect()
}

/// Every session name live on either backend, best-effort: a missing or
/// wedged mux contributes nothing rather than failing the caller.
pub(crate) fn live_session_names() -> std::collections::BTreeSet<String> {
    let mut live = std::collections::BTreeSet::new();
    for mux in [MuxName::Zellij, MuxName::Tmux] {
        if let Ok(sessions) = rimz::mux::backend_for(mux).list_sessions() {
            live.extend(sessions);
        }
    }
    live
}

/// The `rimz start` notices: the root-class line for a non-repo room, and one
/// line per *live* room whose root nests inside or contains this one. Overlap
/// is legal — an agent belongs to the room its pane lives in — so the notice
/// names the standing situation rather than blocking it. Notices go to stderr;
/// stdout stays the protocol surface.
fn report_start_notices(workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    let mut notices = Vec::new();
    notices.extend(root_class_notice(workspace));
    if let Ok(known) = rimz::workspace::known_workspaces() {
        let candidates = overlapping_known(workspace, &known);
        if !candidates.is_empty() {
            // The two `list-sessions` forks run only when a recorded root
            // actually overlaps — the common start pays a readdir, no mux call.
            let live = live_session_names();
            notices.extend(
                candidates
                    .into_iter()
                    .filter(|ws| live.contains(&ws.session_name))
                    .map(|ws| {
                        format!(
                            "this room overlaps live room `{}` rooted at {}; an agent belongs to the room its pane lives in",
                            ws.session_name,
                            ws.project_root.display(),
                        )
                    }),
            );
        }
    }
    if notices.is_empty() {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    for notice in notices {
        writeln!(stderr, "rimz: {notice}")?;
    }
    Ok(())
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
    fn report_already_inside_only_when_auto_and_nested() {
        // Opportunistic launch inside the selected mux reports the room.
        assert!(should_report_already_inside(AttachMode::Auto, true));
        // Outside the mux there is a room to launch — proceed.
        assert!(!should_report_already_inside(AttachMode::Auto, false));
        // Explicit `--print` / `--attach` stay literal escape hatches.
        assert!(!should_report_already_inside(AttachMode::Print, true));
        assert!(!should_report_already_inside(AttachMode::Attach, true));
    }

    fn resolved(root: &str, class: rimz::workspace::RootClass) -> rimz::ResolvedWorkspace {
        use rimz::ids::WorkspaceId;
        let root = PathBuf::from(root);
        rimz::ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(&root),
            project_root: root.clone(),
            root_class: class,
            worktree_root: root.clone(),
            worktree_branch: None,
            session_name: format!("rimz-{}", root.display()),
            mux_hint: None,
        }
    }

    fn known(root: &str) -> rimz::workspace::KnownWorkspace {
        use rimz::ids::WorkspaceId;
        let root = PathBuf::from(root);
        rimz::workspace::KnownWorkspace {
            workspace_id: WorkspaceId::from_project_root(&root),
            session_name: format!("rimz-{}", root.display()),
            project_root: root,
            root_class: rimz::workspace::RootClass::Repo,
        }
    }

    #[test]
    fn root_class_notice_names_non_repo_rooms_only() {
        use rimz::workspace::RootClass;
        assert_eq!(
            root_class_notice(&resolved("/code/repo", RootClass::Repo)),
            None
        );
        let marker = root_class_notice(&resolved("/code/proj", RootClass::Marker))
            .expect("marker rooms are noticed");
        assert!(marker.contains("/code/proj"), "names the root: {marker}");
        let dir = root_class_notice(&resolved("/tmp/scratch", RootClass::Directory))
            .expect("directory rooms are noticed");
        assert!(
            dir.contains("directory workspace rooted at /tmp/scratch"),
            "names the class and root: {dir}",
        );
    }

    #[test]
    fn overlapping_known_keeps_nesting_rooms_and_drops_the_rest() {
        use rimz::workspace::RootClass;
        // Starting ~/code: the nested repo room overlaps, the disjoint and
        // lookalike-prefix roots do not, and the room itself is excluded.
        let workspace = resolved("/home/m/code", RootClass::Directory);
        let rooms = vec![
            known("/home/m/code"),       // self — excluded by id
            known("/home/m/code/query"), // nested below — overlap
            known("/home/m"),            // contains — overlap
            known("/home/m/codex"),      // component boundary — disjoint
            known("/srv/elsewhere"),     // disjoint
        ];
        let hits: Vec<_> = overlapping_known(&workspace, &rooms)
            .into_iter()
            .map(|ws| ws.project_root.display().to_string())
            .collect();
        assert_eq!(hits, vec!["/home/m/code/query", "/home/m"]);
    }

    #[test]
    fn background_view_hosts_orders_claude_then_the_ungated_broker() {
        use rimz::config::RemoteControlConfig;
        use rimz::ids::WorkspaceId;

        let rimz_bin = Path::new("/usr/bin/rimz");
        let wid = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
        let project = Path::new("/proj");
        let worktree = Path::new("/proj/wt");
        let hosts = |config: &RemoteControlConfig, claude: bool, codex: bool| {
            background_view_hosts(
                config,
                claude,
                codex,
                rimz_bin,
                &wid,
                "rimz-demo",
                project,
                worktree,
            )
        };

        // Nothing enabled or present → no view.
        assert!(hosts(&RemoteControlConfig::default(), true, false).is_empty());

        // Codex on PATH alone → the broker, ungated by the config (it is local
        // enrichment, not remote control). It runs from the worktree.
        let codex = hosts(&RemoteControlConfig::default(), false, true);
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].argv[0], "/usr/bin/rimz");
        assert!(codex[0].argv.iter().any(|arg| arg == "app-server"));
        assert_eq!(codex[0].cwd.as_path(), worktree);

        // Claude needs both the toggle and `claude` on PATH; it runs from the
        // project root so `--spawn=worktree` carves off the canonical repo.
        let claude_only = RemoteControlConfig {
            claude: true,
            codex: false,
        };
        assert!(hosts(&claude_only, false, false).is_empty());
        let claude = hosts(&claude_only, true, false);
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].argv[0], "claude");
        assert_eq!(claude[0].cwd.as_path(), project);

        // Both → Claude leads (it takes the view's focus), the broker follows.
        let both = RemoteControlConfig {
            claude: true,
            codex: true,
        };
        let pair = hosts(&both, true, true);
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].argv[0], "claude");
        assert_eq!(pair[1].argv[0], "/usr/bin/rimz");
        assert!(pair[1].argv.iter().any(|arg| arg == "app-server"));
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
