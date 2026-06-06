//! Rimz sidebar process. Renders the snapshot model.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

use std::io::{self, Read};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rimz::SidebarInstanceId;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::workspace::WorkspaceResolver;
use rimz_sidebar::app::{self, ServeConfig};
use tracing_subscriber::EnvFilter;

const DEFAULT_LOG_FILTER: &str = "off";

fn main() -> Result<()> {
    install_tracing();

    match Cli::parse().command {
        Subcmd::Render { width, height } => render_from_stdin(width, height),
        Subcmd::Serve {
            workspace_id,
            mux,
            session_name,
            tick_seconds,
        } => serve(workspace_id, mux, session_name, tick_seconds),
    }
}

#[derive(Parser, Debug)]
#[command(name = "rimz-sidebar", version, about = "Rimz sidebar renderer")]
struct Cli {
    #[command(subcommand)]
    command: Subcmd,
}

#[derive(Subcommand, Debug)]
enum Subcmd {
    /// Read a snapshot JSON from stdin and render once.
    Render {
        #[arg(long, default_value_t = 80)]
        width: u16,
        #[arg(long, default_value_t = 24)]
        height: u16,
    },
    /// Serve the in-process fetch loop and redraw after wakeups or ticks.
    Serve {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long, default_value_t = 1)]
        tick_seconds: u64,
    },
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_LOG_FILTER))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .init();
}

fn render_from_stdin(width: u16, height: u16) -> Result<()> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    let snapshot = serde_json::from_str(&buf).context("parsing snapshot from stdin")?;
    rimz_sidebar::render::render_fixed(io::stdout(), &snapshot, None, width, height)
        .context("rendering snapshot")?;
    Ok(())
}

fn serve(
    workspace_id: Option<String>,
    mux: Option<MuxName>,
    session_name: Option<String>,
    tick_seconds: u64,
) -> Result<()> {
    let (workspace_id, session_name) = match (workspace_id, session_name) {
        (Some(ws), Some(sess)) => (WorkspaceId::parse(&ws)?, sess),
        (ws_opt, sess_opt) => {
            let resolved = WorkspaceResolver::resolve_participant(".", None)?;
            let ws = match ws_opt {
                Some(raw) => WorkspaceId::parse(&raw)?,
                None => resolved.workspace_id,
            };
            let sess = sess_opt.unwrap_or(resolved.session_name);
            (ws, sess)
        }
    };
    let mux = match mux {
        Some(mux) => mux,
        None => rimz::mux::auto_detect_backend(None)?,
    };

    app::serve(ServeConfig {
        workspace_id,
        mux,
        session_name,
        instance_id: SidebarInstanceId::new(),
        tick_seconds,
    })
    .context("serving sidebar")
}
