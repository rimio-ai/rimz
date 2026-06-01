//! `rimz reload` — pick up a freshly-installed build and restore the room's
//! sidebars to a healthy state.
//!
//! Two passes, both best-effort and run-once:
//!
//! 1. **Reload live sidebars in place.** Every fresh sidebar of this workspace
//!    is told to re-exec its own binary, so a new build takes effect without a
//!    session rebirth or pane churn. (The per-tick `rimz sidebar snapshot`
//!    subprocess already reloads on its own; this covers the long-lived
//!    renderer process.)
//! 2. **Recover lost sidebars.** Any Rimz tab/window that still has working
//!    panes but lost its sidebar gains one back in place — never by rebirthing
//!    the session, so the user's panes survive. A single pass: a tab that fails
//!    to gain a sidebar is reported and left alone, never retried in a loop.

use anyhow::{Context, Result};
use clap::Args;

use super::{DEFAULT_SIDEBAR_WIDTH_PERCENT, GlobalFlags};
use rimz::RuntimePaths;
use rimz::ledger::wakeup;
use rimz::mux::{SidebarPaneOptions, SidebarRecovery};
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct ReloadArgs {}

pub fn run(_args: ReloadArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving workspace for reload")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let signaled = wakeup::reload_sidebars(&runtime).context("signaling sidebars to reload")?;
    let recovery = recover_lost_sidebars(globals, &workspace);

    report(&workspace.session_name, signaled, recovery);
    Ok(())
}

/// Re-add a sidebar to every Rimz tab/window of this session that lost one.
/// Best-effort: a missing mux or a backend error degrades to "recovered
/// nothing" rather than failing the command — the reload signal already landed.
fn recover_lost_sidebars(globals: &GlobalFlags, workspace: &ResolvedWorkspace) -> SidebarRecovery {
    let mux = match rimz::mux::auto_detect_backend(globals.mux) {
        Ok(mux) => mux,
        Err(err) => {
            tracing::warn!(error = %err, "sidebar recovery skipped: no multiplexer detected");
            return SidebarRecovery::default();
        }
    };
    let rimz_bin = std::env::current_exe().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "current executable unavailable; recovery uses bare `rimz`");
        std::path::PathBuf::from("rimz")
    });
    let machine_config = rimz::config::MachineConfig::load().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "reading per-machine config; using built-in defaults");
        rimz::config::MachineConfig::default()
    });
    let opts = SidebarPaneOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        cwd: workspace.worktree_root.clone(),
        width_percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
        rimz_bin,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::from(&machine_config),
    };
    rimz::mux::backend_for(mux)
        .recover_sidebars(&opts)
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "sidebar recovery pass failed");
            SidebarRecovery::default()
        })
}

#[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
fn report(session_name: &str, signaled: usize, recovery: SidebarRecovery) {
    if signaled > 0 {
        println!(
            "Reloaded {} for {session_name}.",
            count(signaled, "sidebar")
        );
    }
    if recovery.recovered > 0 {
        println!(
            "Recovered {} in place.",
            count(recovery.recovered, "lost sidebar")
        );
    }
    if recovery.failed > 0 {
        println!(
            "{} could not be re-added; run `rimz attach` to rebirth the session.",
            count(recovery.failed, "sidebar")
        );
    }
    if signaled == 0 && recovery.recovered == 0 && recovery.failed == 0 {
        println!("No sidebar to reload or recover for {session_name}.");
        println!("Launch one with `rimz start` or `rimz attach`.");
    }
}

/// `"1 sidebar"` / `"3 sidebars"`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
