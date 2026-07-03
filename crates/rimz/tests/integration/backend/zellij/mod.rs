//! Live Zellij backend tests.
//!
//! Each test spawns a real `zellij` server under its own throwaway
//! `XDG_RUNTIME_DIR` (Zellij locates its server socket there) and drives the
//! `ZellijBackend` against it via [`ZellijBackend::with_runtime_dir`]. The
//! per-test runtime dir is the isolation seam — it gives every test a private
//! server, so the suite runs in parallel and concurrently across git worktrees
//! with no shared lock. Mirrors the tmux backend's `with_socket` isolation.
//! The whole module becomes a no-op (early-return per test, message printed once)
//! when the `zellij` binary is not on PATH.

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

mod daemon;
mod launch;
mod pane_io;
mod presence;
mod reconcile;
mod resume;
mod self_close;
mod support;
mod tabs;
mod width;
