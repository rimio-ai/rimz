use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, open_ledger};
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::atomic;
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
        SidebarSubcmd::Snapshot { workspace_id, json } => {
            let ledger = match workspace_id {
                Some(raw) => open_ledger_by_workspace_id(raw.parse()?),
                None => {
                    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
                    open_ledger(&workspace)
                }
            }?;
            let snapshot = ledger.snapshot()?;
            if json {
                let rendered = serde_json::to_string_pretty(&snapshot)?;
                #[expect(clippy::print_stdout, reason = "json emitter for sidebar")]
                {
                    println!("{rendered}");
                }
            } else {
                #[expect(clippy::print_stdout, reason = "human summary")]
                {
                    println!("Needs your attention: {}", snapshot.needs_attention.len());
                    println!("Resolver is working: {}", snapshot.resolver_working.len());
                    println!("Recently answered:   {}", snapshot.recently_answered.len());
                    println!("Recent activity:     {}", snapshot.recent_activity.len());
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
            let path = runtime
                .heartbeat_dir
                .join(format!("sidebar.{}.json", instance_id.as_str()));
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
            // Honour `RIMZ_SIDEBAR_BIN` so tests and tooling can point this at
            // a built binary in target/ without installing it on PATH.
            let program =
                std::env::var("RIMZ_SIDEBAR_BIN").unwrap_or_else(|_| "rimz-sidebar".to_owned());
            let status = Command::new(program)
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
                .status()
                .context("running `rimz-sidebar serve`")?;
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
