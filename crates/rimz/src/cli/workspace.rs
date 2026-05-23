use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    command: WorkspaceSubcmd,
}

#[derive(Debug, Subcommand)]
enum WorkspaceSubcmd {
    /// Resolve a path to a workspace and print the result as JSON.
    Resolve {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

pub fn run(args: WorkspaceArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        WorkspaceSubcmd::Resolve { path } => {
            let workspace = WorkspaceResolver::resolve(&path, globals.root.clone())?;
            let rendered = serde_json::to_string_pretty(&workspace)?;
            #[expect(clippy::print_stdout, reason = "json emitter")]
            {
                println!("{rendered}");
            }
            Ok(())
        }
    }
}
