//! `rimz reload` — tell the current workspace's live sidebars to re-exec their
//! own binary in place, so a freshly-installed build takes effect without a
//! session rebirth or pane churn.
//!
//! The per-tick `rimz sidebar snapshot` subprocess already reloads on its own
//! (it is a fresh child each tick); this command covers the long-lived renderer
//! process. A wedged or already-dead sidebar receives nothing — relaunch it
//! with `rimz start`/`rimz attach`.

use anyhow::{Context, Result};
use clap::Args;

use super::GlobalFlags;
use rimz::RuntimePaths;
use rimz::ledger::wakeup;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct ReloadArgs {}

pub fn run(_args: ReloadArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving workspace for reload")?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let signaled = wakeup::reload_sidebars(&runtime).context("signaling sidebars to reload")?;

    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        match signaled {
            0 => {
                println!("No live sidebar to reload for {}.", workspace.session_name);
                println!("Launch one with `rimz start` or `rimz attach`.");
            }
            1 => println!("Reloaded 1 sidebar for {}.", workspace.session_name),
            n => println!("Reloaded {n} sidebars for {}.", workspace.session_name),
        }
    }
    Ok(())
}
