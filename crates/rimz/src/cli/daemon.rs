//! Hidden daemon dashboard helpers: content slots and foreground host wrappers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonSubcmd,
}

#[derive(Debug, Subcommand)]
enum DaemonSubcmd {
    /// Supervise one rimzd middle-column content slot. Humans do not run this.
    #[command(hide = true)]
    Content(ContentArgs),
    /// Supervise one rimzd host. Humans do not run this.
    #[command(hide = true)]
    Host(HostArgs),
}

#[derive(Debug, Args)]
struct ContentArgs {
    /// Zero-based content slot this pane owns.
    #[arg(long)]
    slot: usize,
    /// Room worktree root used for relative daemon pane cwd values.
    #[arg(long)]
    worktree_root: PathBuf,
    #[arg(long)]
    workspace_id: Option<rimz::ids::WorkspaceId>,
}

#[derive(Debug, Args)]
struct HostArgs {
    #[arg(long)]
    workspace_id: rimz::ids::WorkspaceId,
    #[arg(last = true, required = true)]
    command: Vec<std::ffi::OsString>,
}

pub fn run(args: DaemonArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        DaemonSubcmd::Content(args) => run_content(args),
        DaemonSubcmd::Host(args) => {
            let status = rimz::daemon_content::run_host(&args.workspace_id, &args.command)
                .context("running daemon host wrapper")?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

fn run_content(args: ContentArgs) -> Result<()> {
    let status = rimz::daemon_content::run_supervisor(
        args.slot,
        &args.worktree_root,
        args.workspace_id.as_ref(),
    )
    .context("running daemon content supervisor")?;
    std::process::exit(status.code().unwrap_or(1));
}
