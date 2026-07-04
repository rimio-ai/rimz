//! `rimz reset` — the explicit escape hatch for a wedged room. Tears the live
//! room's mux session down to a clean slate (delete + cache purge + orphan
//! sweep) and, by default, rebuilds and re-enters it. Attached `rimz start`
//! auto-reset runs the same [`rimz::mux::recovery::teardown_room`] routine.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::GlobalFlags;
use crate::cli::room::{
    MissingSessionReport, ensure_single_backend_room, pick_mux_for_session, print_reset_report,
    rebirth_room,
};
use rimz::RuntimePaths;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Skip the confirmation prompt (for scripts).
    #[arg(long)]
    pub yes: bool,
    /// Tear the room down but do not rebuild or attach — just print the rerun hint.
    #[arg(long)]
    pub no_start: bool,
    /// Archive the current room records but do not seed prior agents on rebirth.
    #[arg(long)]
    pub hard: bool,
    /// Path to use as the workspace cwd.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: ResetArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(&args.path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", args.path.display()))?;

    // Reset the backend that owns the live room, so teardown and shared-ledger
    // reset target the same session. An explicit rival `--mux` refuses before
    // prompting or destroying anything.
    let mux = pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    )?;
    ensure_single_backend_room(mux, &workspace.session_name)?;

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "`rimz reset` deletes the session and sweeps its processes; \
                 pass --yes to confirm without a terminal"
            );
        }
        if !super::confirm(&format!(
            "Reset the '{}' room? This deletes the mux session, purges its \
             resurrection cache, archives its records, clears live coordination \
             state, and signals its orphaned processes.",
            workspace.session_name
        ))? {
            writeln!(std::io::stderr().lock(), "Reset aborted; nothing changed.")?;
            return Ok(());
        }
    }

    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let backend = rimz::mux::backend_for(mux);
    let report = rimz::mux::recovery::teardown_room(
        backend.as_ref(),
        &workspace.workspace_id,
        &workspace.session_name,
        &runtime,
    );
    let ledger = super::open_ledger(&workspace)?;
    let records = ledger
        .reset_records(&workspace.session_name, args.hard)
        .context("resetting workspace records")?;
    print_reset_report(&report, Some(&records))?;

    if args.no_start {
        writeln!(
            std::io::stderr().lock(),
            "Room torn down. Run `rimz start` to rebuild it.",
        )?;
        return Ok(());
    }
    rebirth_room(args.path, globals)
}
