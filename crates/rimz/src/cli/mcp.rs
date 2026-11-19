//! `rimz mcp` — the stdio MCP server a Claude or Codex launch starts.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use rimz::mcp::catalog::{Capability, LoadMode, Tool, tools_for, tools_list};

use crate::cli::render::{self, Table, cell};

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
        /// Mark this capability's tools for Claude to load from the start (repeatable).
        #[arg(long, value_name = "CAPABILITY", value_parser = parse_capability)]
        always: Vec<Capability>,
        /// Mark this capability's tools for Claude to defer until its tool search
        /// finds them (repeatable).
        #[arg(long, value_name = "CAPABILITY", value_parser = parse_capability)]
        deferred: Vec<Capability>,
        /// Give each one-shot the parent process's environment beneath this
        /// server's own, for a provider that starts the server with a reduced one.
        #[arg(long)]
        parent_env: bool,
    },
    /// List the tools the server offers, each with its estimated size in tokens.
    Tools {
        /// List only this capability's tools (repeatable); every capability when absent.
        #[arg(long, value_name = "CAPABILITY", value_parser = parse_capability)]
        only: Vec<Capability>,
        /// Print the server's `tools/list` result instead.
        #[arg(long)]
        json: bool,
    },
}

/// What `serve` prints instead of serving when a person runs it at a terminal.
const TERMINAL_NOTE: &str = "\
rimz mcp serve is the MCP server RimZ hands to every Claude and Codex launch.
The provider starts it over stdio, so there is nothing to serve here.
To see its tools and their sizes: rimz mcp tools
To see the arguments one launch passes it: rimz agents explain <profile> --mcps";

pub fn run(args: McpArgs) -> Result<()> {
    match args.command {
        McpSubcmd::Serve { .. } if std::io::stdin().is_terminal() => {
            render::finish(writeln!(render::err(), "{TERMINAL_NOTE}"))
        }
        McpSubcmd::Serve {
            only,
            always,
            deferred,
            parent_env,
        } => {
            if let Some(both) = always
                .iter()
                .find(|capability| deferred.contains(capability))
            {
                anyhow::bail!(
                    "rimz mcp serve: {} is marked both --always and --deferred",
                    both.name()
                );
            }
            let modes: Vec<(Capability, LoadMode)> = always
                .into_iter()
                .map(|capability| (capability, LoadMode::Always))
                .chain(
                    deferred
                        .into_iter()
                        .map(|capability| (capability, LoadMode::Deferred)),
                )
                .collect();
            rimz::mcp::serve(&only, &modes, parent_env).context("rimz mcp serve")
        }
        McpSubcmd::Tools { only, json } => {
            let tools: Vec<&Tool> = tools_for(&only).collect();
            let rendered = if json {
                render_json(&tools)
            } else {
                render_tools(&tools, &mut render::out())
            };
            render::finish(rendered)
        }
    }
}

fn render_json(tools: &[&Tool]) -> std::io::Result<()> {
    let listing =
        serde_json::to_string_pretty(&tools_list(tools, &[])).map_err(std::io::Error::other)?;
    writeln!(render::out(), "{listing}")
}

fn render_tools(tools: &[&Tool], w: &mut impl Write) -> std::io::Result<()> {
    let mut table = Table::new(["CAPABILITY", "TOOL", "TOKENS"]).right(&[2]);
    let mut total = 0;
    for tool in tools {
        let size = tool.tokens();
        total += size;
        table.row([
            cell(tool.capability.name()),
            cell(tool.name),
            cell(format!("~{size}")),
        ]);
    }
    table.render(w)?;
    writeln!(w, "total: ~{total} tokens in {} tools", tools.len())
}

fn parse_capability(name: &str) -> std::result::Result<Capability, String> {
    Capability::from_name(name).ok_or_else(|| {
        let names: Vec<_> = Capability::ALL.map(Capability::name).into();
        format!("expected one of {}", names.join(", "))
    })
}
