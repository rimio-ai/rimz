//! `rimz mcp` — the stdio MCP server a Claude or Codex launch starts.

use std::io::IsTerminal;

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

/// What `serve` prints instead of serving when a person runs it at a terminal.
const TERMINAL_NOTE: &str = "\
rimz mcp serve is the MCP server RimZ hands to every Claude and Codex launch.
The provider starts it over stdio, so there is nothing to serve here.
To see the arguments one launch passes it: rimz agents explain <profile>";

pub fn run(args: McpArgs) -> Result<()> {
    match args.command {
        McpSubcmd::Serve { .. } if std::io::stdin().is_terminal() => {
            let note = format!("{TERMINAL_NOTE}\n");
            let written =
                std::io::Write::write_all(&mut crate::cli::render::err(), note.as_bytes());
            crate::cli::render::finish(written)
        }
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
