//! Live tmux backend tests on isolated private servers.

#![allow(clippy::print_stdout)]

/// Skip a live test when tmux is unavailable.
macro_rules! require_tmux {
    () => {
        if which::which("tmux").is_err() {
            crate::common::skip("tmux not on PATH");
            return;
        }
    };
}

mod agent_lifecycle;
mod card_commands;
mod containment;
mod hidden_geometry;
mod keymap;
mod large_paste;
mod layout;
mod pane_io;
mod presence;
mod reconcile;
mod remote_reconnect;
mod run_command;
mod server_cwd;
mod session;
mod sidebar;
mod status;
mod support;
mod team_stage;
mod title;

pub(in crate::backend) use support::TmuxServer;
