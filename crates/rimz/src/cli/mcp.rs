//! `rimz mcp` — the stdio MCP server a Claude, Codex, Antigravity, or OpenCode launch starts.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use rimz::mcp::catalog::{Capability, LoadMode, Tool, tools_for, tools_list};
use rimz::mcp::launch_env;
use rimz::mcp::wording::Wording;

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
        /// Restart once with the parent process's environment beneath this
        /// server's own, for a provider that starts the server with a reduced one.
        #[arg(long)]
        parent_env: bool,
        /// Serve what the launch names in RIMZ_MCP_SERVE, running each call in
        /// RIMZ_MCP_CWD, for a provider that starts one fixed server for every session.
        #[arg(long, conflicts_with_all = ["only", "always", "deferred"])]
        launch_env: bool,
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
rimz mcp serve is the MCP server RimZ hands to Claude, Codex, Antigravity, and OpenCode launches.
The provider starts it over stdio, so there is nothing to serve here.
To see its tools and their sizes: rimz mcp tools
To see what one launch passes it and the text of each tool: rimz agents explain <profile> --mcps";

/// What `serve --launch-env` writes before it exits when the launch serves nothing. agy shows
/// it in `/mcp` as the server's error.
const NOTHING_SERVED: &str = "rimz mcp serve: RimZ serves no tools in this session, as expected \
in an Antigravity session RimZ did not launch or a launch whose profile serves none";

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
            launch_env,
        } => {
            if parent_env {
                rimz::mcp_tools::restore_parent_env();
            }
            if launch_env {
                let Some(selection) = launch_env::read(|name| std::env::var(name).ok())
                    .context("rimz mcp serve --launch-env")?
                else {
                    return render::finish(writeln!(render::err(), "{NOTHING_SERVED}"));
                };
                let (only, modes): (Vec<Capability>, Vec<(Capability, LoadMode)>) = (
                    selection
                        .served
                        .iter()
                        .map(|(capability, _)| *capability)
                        .collect(),
                    selection
                        .served
                        .iter()
                        .filter_map(|(capability, mode)| Some((*capability, (*mode)?)))
                        .collect(),
                );
                return rimz::mcp::serve(&only, &modes, rimz::mcp_tools::Tools::new(selection.cwd))
                    .context("rimz mcp serve");
            }
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
            rimz::mcp::serve(&only, &modes, rimz::mcp_tools::Tools::new(None))
                .context("rimz mcp serve")
        }
        McpSubcmd::Tools { only, json } => {
            let wording = Wording::read_home();
            for warning in wording.warnings() {
                render::finish(writeln!(render::err(), "rimz: warning: {warning}"))?;
            }
            let tools: Vec<&Tool> = tools_for(&only).collect();
            let rendered = if json {
                render_json(&tools, &wording)
            } else {
                render_tools(&tools, &wording, &mut render::out())
            };
            render::finish(rendered)
        }
    }
}

fn render_json(tools: &[&Tool], wording: &Wording) -> std::io::Result<()> {
    let listing = serde_json::to_string_pretty(&tools_list(tools, &[], wording))
        .map_err(std::io::Error::other)?;
    writeln!(render::out(), "{listing}")
}

fn render_tools(tools: &[&Tool], wording: &Wording, w: &mut impl Write) -> std::io::Result<()> {
    let mut table = Table::new(["CAPABILITY", "TOOL", "TOKENS"]).right(&[2]);
    let mut total = 0;
    for tool in tools {
        let size = tool.tokens(wording);
        total += size;
        table.row([
            cell(tool.group_name()),
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
