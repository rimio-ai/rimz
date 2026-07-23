//! `rimz sessions` — pick, create, and enter managed rooms.

mod picker;

use std::io::IsTerminal as _;

use anyhow::{Result, bail};
use clap::Args;

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct SessionsArgs {}

pub fn run(_args: SessionsArgs, globals: &GlobalFlags) -> Result<()> {
    if inside_mux() {
        bail!(
            "already inside a multiplexer; detach (or open a new terminal) and rerun `rimz sessions`, or use `rimz attach`"
        );
    }
    if !picker_available() {
        let rooms = rimz::room::session::live_rooms()?;
        bail!("{}", session_listing(&rooms));
    }
    if !picker::run(picker::Mode::Terminal, None, None, globals)? {
        bail!("could not open the session manager in this terminal");
    }
    Ok(())
}

pub(crate) fn picker_available() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

pub(crate) fn run_web_picker(
    rejected_session: Option<&str>,
    initial_attach: Option<(&str, &rimz::mux::CommandSpec)>,
    globals: &GlobalFlags,
) -> Result<bool> {
    picker::run(picker::Mode::Web, rejected_session, initial_attach, globals)
}

pub(crate) fn session_display_name(session: &str) -> String {
    picker::session_display_name(session)
}

fn inside_mux() -> bool {
    ["ZELLIJ", "ZELLIJ_PANE_ID", "TMUX", "TMUX_PANE"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some())
}

fn session_listing(rooms: &[rimz::room::session::LiveRoom]) -> String {
    let listing = if rooms.is_empty() {
        "  (none)".to_owned()
    } else {
        rooms
            .iter()
            .map(|room| format!("  {} ({})", room.session_name, room.mux))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!("`rimz sessions` needs an interactive terminal\n\nLive RimZ sessions:\n{listing}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noninteractive_listing_matches_the_web_session_shape() {
        assert_eq!(
            session_listing(&[]),
            "`rimz sessions` needs an interactive terminal\n\nLive RimZ sessions:\n  (none)"
        );
    }
}
