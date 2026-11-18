//! `rimz mcp` — the stdio MCP server a Claude or Codex launch starts.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rimz::mcp::catalog::Capability;

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    command: McpSubcmd,
}

#[derive(Debug, Subcommand)]
enum McpSubcmd {
    /// Serve RimZ tools over stdio to the provider that started this process.
    Serve {
        /// Serve only this capability's tools (repeatable); every capability when absent.
        #[arg(long, value_name = "CAPABILITY", value_parser = parse_capability)]
        only: Vec<Capability>,
        /// Give each one-shot the parent process's environment beneath this
        /// server's own, for a provider that starts the server with a reduced one.
        #[arg(long)]
        parent_env: bool,
    },
}

pub fn run(args: McpArgs) -> Result<()> {
    match args.command {
        McpSubcmd::Serve { only, parent_env } => {
            rimz::mcp::serve(&only, parent_env).context("rimz mcp serve")
        }
    }
}

fn parse_capability(name: &str) -> std::result::Result<Capability, String> {
    Capability::from_name(name).ok_or_else(|| {
        let names: Vec<_> = Capability::ALL.map(Capability::name).into();
        format!("expected one of {}", names.join(", "))
    })
}
