//! Live Zellij backend tests.
//!
//! Each test spawns a real `zellij` server under its own throwaway env root and
//! drives the `ZellijBackend` against it via [`ZellijBackend::with_runtime_dir`].
//! The per-test root is the isolation seam — it gives every test a private
//! server, cache, config, and log, so the suite runs in parallel and concurrently
//! across git worktrees with no shared lock. Mirrors the tmux backend's
//! `with_socket` isolation. The whole module becomes a no-op (early-return per
//! test, message printed once) when the `zellij` binary is not on PATH.

#![allow(clippy::print_stdout, clippy::print_stderr)]

/// Skip the test (return) if the host has no `zellij` binary on PATH.
macro_rules! require_zellij {
    () => {
        if which::which("zellij").is_err() {
            eprintln!("zellij not on PATH; skipping test");
            return;
        }
    };
}

mod containment;
mod daemon;
mod launch;
mod pane_io;
mod presence;
mod reap;
mod reconcile;
mod resume;
mod self_close;
mod support;
mod tabs;
mod width;
