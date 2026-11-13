//! `rimz tab` — show the room's fixed views: the dashboard and the numbered
//! terminals. stdout stays empty, so a key binding never opens tmux's
//! `run-shell` overlay.

use std::io::Write;

use anyhow::Result;
use clap::{Args, Subcommand};
use rimz::room::{FixedView, RoomContext, RoomSizing};

use super::{GlobalFlags, render};

#[derive(Debug, Args)]
pub struct TabArgs {
    #[command(subcommand)]
    command: TabSubcmd,
}

#[derive(Debug, Subcommand)]
enum TabSubcmd {
    /// Show the dashboard tab, reopening it when it was closed.
    Dashboard {
        #[arg(long)]
        session_name: Option<String>,
    },
    /// Show terminal N, a shell tab at the main checkout, opening it on first use.
    Terminal {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=9))]
        n: u8,
        #[arg(long)]
        session_name: Option<String>,
    },
}

pub fn run(args: TabArgs, globals: &GlobalFlags) -> Result<()> {
    let (verb, session_name) = match &args.command {
        TabSubcmd::Dashboard { session_name } => ("dashboard", session_name),
        TabSubcmd::Terminal { session_name, .. } => ("terminal", session_name),
    };
    let session = match session_name {
        Some(name) => name.clone(),
        None => {
            rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())?.session_name
        }
    };
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let record = rimz::room::session::workspace_record_for_session(&session)?
        .ok_or_else(|| anyhow::anyhow!("session {session} is not a managed RimZ room"))?;
    let room = RoomContext::from_record(
        &record,
        super::machine_config(),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let shown = match args.command {
        TabSubcmd::Dashboard { .. } => room.show_dashboard()?,
        TabSubcmd::Terminal { n, .. } => room.show_terminal(n)?,
    };
    if shown == FixedView::NoClient {
        writeln!(
            render::err(),
            "tab {verb}: no client is attached to session {session}"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn terminal_numbers_outside_one_to_nine_are_usage_errors() {
        for n in ["0", "10"] {
            let error = crate::cli::Cli::try_parse_from(["rimz", "tab", "terminal", n])
                .expect_err("out of range");
            assert_eq!(error.exit_code(), 2, "{n}");
        }
        assert!(crate::cli::Cli::try_parse_from(["rimz", "tab", "terminal"]).is_err());
        for argv in [
            &["rimz", "tab", "terminal", "9", "--session-name", "room"][..],
            &["rimz", "tab", "dashboard"][..],
        ] {
            assert!(crate::cli::Cli::try_parse_from(argv).is_ok(), "{argv:?}");
        }
    }
}
