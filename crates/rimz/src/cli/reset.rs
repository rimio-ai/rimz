//! `rimz reset` — the explicit escape hatch for a wedged room. Tears the live
//! room's mux session down to a clean slate (delete + cache purge + orphan
//! sweep) and, by default, rebuilds and re-enters it. Attached `rimz start`
//! auto-reset runs the same [`rimz::room::teardown::teardown_room`] routine.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::{AttachFlags, GlobalFlags, StartArgs};
use rimz::room::session::MissingSessionReport;
use rimz::room::{RoomContext, RoomSizing};
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
    /// Rebuild the room under this provider account (repeatable); the room comes back with no agents.
    #[arg(long, value_name = "KIND=NAME", value_parser = super::accounts::parse_account_flag, conflicts_with = "no_start")]
    pub account: Vec<super::accounts::AccountFlag>,
    /// Path to use as the workspace cwd.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: ResetArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(&args.path, globals.root.clone())
        .with_context(|| format!("resolving workspace at {}", args.path.display()))?;

    // Reset the backend that owns the live room, so teardown and shared-store
    // reset target the same session. An explicit rival `--mux` refuses before
    // prompting or destroying anything.
    let mux = super::render::room::present_mux_pick(rimz::room::session::pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    ))?;
    super::render::room::print_notices(rimz::room::session::ensure_single_backend_room(
        mux,
        &workspace.session_name,
    )?)?;
    // The rebirth takes the state dir name; refuse before the teardown when
    // that name cannot be born.
    if !args.no_start {
        super::room::birth_socket_preflight(mux, false, &workspace.project_root)?;
    }

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "`rimz reset` deletes the session and sweeps its processes; \
                 pass --yes to confirm without a terminal"
            );
        }
        if !super::confirm(&reset_confirmation(&workspace.session_name))? {
            writeln!(std::io::stderr().lock(), "Reset aborted; nothing changed.")?;
            return Ok(());
        }
    }

    let context = RoomContext::from_resolved(
        &workspace,
        super::machine_config(),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let report = context.reset(args.hard)?;
    super::render::room::print_reset_report(&report)?;

    if args.no_start {
        writeln!(
            std::io::stderr().lock(),
            "Room torn down. Run `rimz start` to rebuild it.",
        )?;
        return Ok(());
    }
    super::room::start(
        StartArgs {
            attach: AttachFlags::default(),
            path: args.path,
            // A manual reset is a deliberate fresh start. Store carryover stays
            // available for audit, but it does not re-seed the reborn room.
            no_resume: true,
            refresh_ms: None,
            account: args.account,
        },
        globals,
    )
}

fn reset_confirmation(session: &str) -> String {
    format!(
        "Reset the '{session}' room? This deletes the mux session, purges its \
         resurrection cache, archives its records, clears live coordination \
         state, signals its orphaned processes, and removes its tmp files. \
         The rebuilt room comes back with no agents; its panes are not seeded again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_confirmation_names_the_room_and_empty_rebuild() {
        let prompt = reset_confirmation("rimz-project");
        assert!(prompt.starts_with("Reset the 'rimz-project' room?"));
        assert!(prompt.contains("comes back with no agents; its panes are not seeded again."));
    }
}
