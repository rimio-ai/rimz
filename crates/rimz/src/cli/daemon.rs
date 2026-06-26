//! Hidden daemon dashboard helpers. The mux runs `rimz daemon content` inside
//! each middle-column pane so the visible child command can reload from config.

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
}

#[derive(Debug, Args)]
struct ContentArgs {
    /// Zero-based content slot this pane owns.
    #[arg(long)]
    slot: usize,
    /// Room worktree root used for relative daemon pane cwd values.
    #[arg(long)]
    worktree_root: PathBuf,
}

pub fn run(args: DaemonArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        DaemonSubcmd::Content(args) => run_content(args),
    }
}

fn run_content(args: ContentArgs) -> Result<()> {
    let status = rimz::daemon_content::run_supervisor(args.slot, &args.worktree_root)
        .context("running daemon content supervisor")?;
    std::process::exit(status.code().unwrap_or(1));
}
