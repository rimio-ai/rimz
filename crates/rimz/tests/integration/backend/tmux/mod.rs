//! Live tmux backend tests on isolated private servers.

#![allow(clippy::print_stdout, clippy::print_stderr)]

/// Skip a live test when tmux is unavailable.
macro_rules! require_tmux {
    () => {
        if which::which("tmux").is_err() {
            eprintln!("tmux not on PATH; skipping test");
            return;
        }
    };
}

mod agent_lifecycle;
mod layout;
mod pane_io;
mod presence;
mod reconcile;
mod server_cwd;
mod session;
mod sidebar;
mod support;

pub(in crate::backend) use support::TmuxServer;
