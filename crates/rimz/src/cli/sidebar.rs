use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, open_ledger};
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::atomic;
use rimz::ledger::workspace_record;
use rimz::mux::PaneListOptions;
use rimz::schema::heartbeat::SidebarHeartbeat;
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths};

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
        #[arg(long)]
        json: bool,
    },
    /// Record sidebar liveness for wakeup fanout.
    Heartbeat {
        #[arg(long)]
        workspace_id: String,
        #[arg(long)]
        instance_id: String,
        #[arg(long)]
        mux: MuxName,
        #[arg(long)]
        session_name: String,
        #[arg(long)]
        wakeup_socket: PathBuf,
    },
    /// Run the terminal sidebar renderer.
    Serve {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long, default_value_t = 2)]
        tick_seconds: u64,
    },
}

pub fn run(args: SidebarArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        SidebarSubcmd::Snapshot {
            workspace_id,
            mux,
            session_name,
            exclude_pane_id,
            json,
        } => {
            let mut resolved_session = None;
            let ledger = match workspace_id {
                Some(raw) => open_ledger_by_workspace_id(raw.parse()?),
                None => {
                    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
                    resolved_session = Some(workspace.session_name.clone());
                    open_ledger(&workspace)
                }
            }?;
            let mut snapshot = ledger.snapshot()?;
            // The serve loop names its session explicitly; a bare CLI/inspection
            // call resolves it from the record. Only the former treats a
            // pane-discovery failure as fatal (see the match below).
            let explicit_session = session_name.is_some();
            let session_name = session_name
                .or(resolved_session)
                .or_else(|| session_name_from_record(&ledger));
            let exclude = exclude_pane_id
                .as_deref()
                .map(rimz::ids::PaneId::parse)
                .transpose()?;
            if let Some(session_name) = session_name {
                if let Some(panes) = pane_list_fixture()? {
                    snapshot = snapshot.with_live_panes(panes, exclude.as_ref());
                } else {
                    let mux = mux.or(globals.mux);
                    if let Some(mux) = mux.or_else(|| rimz::mux::auto_detect_backend(None).ok()) {
                        let backend = rimz::mux::backend_for(mux);
                        match backend.list_panes(PaneListOptions {
                            session_name: Some(session_name),
                        }) {
                            Ok(panes) => {
                                snapshot = snapshot.with_live_panes(panes, exclude.as_ref());
                            }
                            // The serve loop owns a live session, so a discovery
                            // failure there is real: fail hard and let the loop
                            // hold its last good frame via the degraded path,
                            // rather than flashing the raw ledger rollup (every
                            // agent the log ever saw) for a single tick.
                            Err(err) if explicit_session => {
                                return Err(err).context("sidebar snapshot pane discovery");
                            }
                            // A bare inspection call has no live session to
                            // trust; fall back to the ledger rollup.
                            Err(err) => {
                                tracing::warn!(error = %err, "sidebar snapshot pane discovery failed; showing ledger rollup");
                            }
                        }
                    }
                }
            }
            // Hook-install state is environment, not ledger, so the reducer
            // can't know it — fill it here so the renderer's first-run hint
            // can point at `rimz hooks install` until a supported agent is
            // wired. Unsupported adapters must not make the room look ready.
            snapshot.agent_hooks_ready = rimz::agents::KNOWN_AGENTS.iter().any(|name| {
                rimz::agents::integration_by_name(name)
                    .map(|agent| agent.supports_hook_install() && agent.hooks_installed())
                    .unwrap_or(false)
            });
            if json {
                let rendered = serde_json::to_string_pretty(&snapshot)?;
                #[expect(clippy::print_stdout, reason = "json emitter for sidebar")]
                {
                    println!("{rendered}");
                }
            } else {
                let waiting = snapshot
                    .worktree_groups
                    .iter()
                    .flat_map(|group| &group.status_counts)
                    .filter(|count| count.status == rimz::feed::AgentStatus::Waiting)
                    .map(|count| count.count)
                    .sum::<usize>();
                let failed = snapshot
                    .worktree_groups
                    .iter()
                    .flat_map(|group| &group.status_counts)
                    .filter(|count| count.status == rimz::feed::AgentStatus::Failed)
                    .map(|count| count.count)
                    .sum::<usize>();
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
        SidebarSubcmd::Heartbeat {
            workspace_id,
            instance_id,
            mux,
            session_name,
            wakeup_socket,
        } => {
            let workspace_id: WorkspaceId = workspace_id.parse()?;
            let instance_id: SidebarInstanceId = instance_id.parse()?;
            let runtime =
                RuntimePaths::for_workspace(workspace_id.clone()).context("runtime paths")?;
            runtime.ensure_dirs().context("preparing runtime dirs")?;
            let heartbeat = SidebarHeartbeat::new(
                workspace_id,
                instance_id.clone(),
                mux,
                session_name,
                wakeup_socket,
            );
            let path = runtime.sidebar_heartbeat_path(&instance_id);
            atomic::write_temp_then_rename(&path, &heartbeat)
                .with_context(|| format!("writing sidebar heartbeat {}", path.display()))?;
            #[expect(clippy::print_stdout, reason = "stable interface for sidebar process")]
            {
                println!("heartbeat-ok");
            }
            Ok(())
        }
        SidebarSubcmd::Serve {
            workspace_id,
            mux,
            session_name,
            tick_seconds,
        } => {
            let needs_workspace_resolve = workspace_id.is_none() || session_name.is_none();
            let resolved = if needs_workspace_resolve {
                Some(WorkspaceResolver::resolve(".", globals.root.clone())?)
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
    }
}

fn open_ledger_by_workspace_id(workspace_id: WorkspaceId) -> Result<Ledger> {
    let paths = StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    Ledger::open(paths, runtime).context("opening ledger")
}

fn session_name_from_record(ledger: &Ledger) -> Option<String> {
    workspace_record::read(&ledger.paths().workspace_record)
        .ok()
        .map(|record| record.session_name)
}

fn pane_list_fixture() -> Result<Option<Vec<rimz::feed::PaneRef>>> {
    let Some(path) = std::env::var_os("RIMZ_TEST_PANE_LIST").filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading RIMZ_TEST_PANE_LIST {}", path.display()))?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub(crate) fn sidebar_renderer_program() -> PathBuf {
    if let Some(path) = env_path("RIMZ_SIDEBAR_BIN") {
        return path;
    }
    if let Some(path) = sibling_renderer_bin().filter(|path| path.is_file()) {
        return path;
    }
    if let Ok(path) = which::which(renderer_bin_name()) {
        return path;
    }
    PathBuf::from(renderer_bin_name())
}

pub(crate) fn sidebar_renderer_present() -> bool {
    if let Some(path) = env_path("RIMZ_SIDEBAR_BIN") {
        return path.is_file();
    }
    sibling_renderer_bin().is_some_and(|path| path.is_file())
        || which::which(renderer_bin_name()).is_ok()
}

fn sibling_renderer_bin() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let parent = current.parent()?;
    Some(parent.join(renderer_bin_name()))
}

fn renderer_bin_name() -> String {
    format!("rimz-sidebar{}", std::env::consts::EXE_SUFFIX)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn rimz_cli_program() -> PathBuf {
    env_path("RIMZ_BIN")
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(rimz_bin_name()))
}

fn rimz_bin_name() -> String {
    format!("rimz{}", std::env::consts::EXE_SUFFIX)
}
