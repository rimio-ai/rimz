//! `rimz list` — show running and recently-active workspaces.
//!
//! "Known" means a `workspace.json` record exists under
//! `$XDG_STATE_HOME/rimz/workspaces/<id>/` from a previous `rimz start` or
//! ledger write. "Running" means the session name shows up in
//! `zellij list-sessions` or `tmux list-sessions`. The two are joined by
//! session name so reattach decisions stay local — no daemon, no index file.
//!
//! By default the table shows running sessions plus workspaces touched within
//! the last 24h; `--all` adds the dormant ones. A workspace directory missing
//! its `workspace.json` is skipped silently — it is not a usable workspace, and
//! `rimz gc` reaps it. A *corrupt* record is still surfaced.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use clap::Args;
use jiff::Timestamp;
use serde::Serialize;
use tracing::warn;

use super::GlobalFlags;
use crate::cli::render;
use rimz::ids::MuxName;
use rimz::ledger::paths::workspaces_dir;

/// Workspaces idle longer than this are hidden from the default view; `--all`
/// reveals them.
const RECENT_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Show every known workspace, including dormant ones. By default only
    /// running sessions and workspaces active within the last 24h appear.
    #[arg(long, short = 'a')]
    all: bool,
    /// Emit machine-readable JSON instead of the human table.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, Serialize)]
struct WorkspaceRow {
    workspace_id: String,
    project_root: String,
    session_name: String,
    running_on: Option<String>,
    last_activity: Option<String>,
}

pub fn run(args: ListArgs, _globals: &GlobalFlags) -> Result<()> {
    let rows = collect_rows(args.all).context("listing workspaces")?;
    if args.json {
        let rendered = serde_json::to_string_pretty(&rows).expect("WorkspaceRow vec serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }
    print_human(&rows)?;
    Ok(())
}

fn collect_rows(all: bool) -> Result<Vec<WorkspaceRow>> {
    let known = rimz::workspace::known_workspaces().context("reading known workspaces")?;
    let zellij_sessions = backend_sessions(MuxName::Zellij);
    let tmux_sessions = backend_sessions(MuxName::Tmux);
    let now = SystemTime::now();
    let root = workspaces_dir();

    let mut rows: Vec<WorkspaceRow> = known
        .into_iter()
        .filter_map(|known| {
            let last_activity = activity_for(&root.join(known.workspace_id.as_str()));
            let running_on = if zellij_sessions.contains(&known.session_name) {
                Some(MuxName::Zellij.as_str().to_owned())
            } else if tmux_sessions.contains(&known.session_name) {
                Some(MuxName::Tmux.as_str().to_owned())
            } else {
                None
            };
            // Default view: running sessions plus anything touched recently.
            // `--all` keeps dormant workspaces in the listing.
            if !all && running_on.is_none() && !is_recent(last_activity, now) {
                return None;
            }
            Some(WorkspaceRow {
                workspace_id: known.workspace_id.as_str().to_owned(),
                project_root: known.project_root.display().to_string(),
                session_name: known.session_name,
                running_on,
                last_activity: last_activity
                    .and_then(|at| Timestamp::try_from(at).ok())
                    .map(|ts| ts.to_string()),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        // Running sessions first, then by most recent activity, then by id.
        match (a.running_on.is_some(), b.running_on.is_some()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b
                .last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.workspace_id.cmp(&b.workspace_id)),
        }
    });
    Ok(rows)
}

/// Best-effort "last activity" instant — newest mtime across the files that
/// move when the workspace is in use. Used purely for the operator's reattach
/// decision and the default recency filter; never gates correctness.
fn activity_for(workspace_dir: &std::path::Path) -> Option<SystemTime> {
    let candidates = [
        workspace_dir.join("events.log.jsonl"),
        workspace_dir.join("snapshots").join("latest.json"),
        workspace_dir.join("workspace.json"),
    ];
    candidates
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .filter_map(|meta| meta.modified().ok())
        .max()
}

/// Whether a workspace counts as recently active for the default view. A
/// future mtime (clock skew) is treated as recent rather than hidden.
fn is_recent(last_activity: Option<SystemTime>, now: SystemTime) -> bool {
    let Some(at) = last_activity else {
        return false;
    };
    match now.duration_since(at) {
        Ok(age) => age <= RECENT_WINDOW,
        // Future mtime (clock skew) counts as recent rather than hidden.
        Err(_) => true,
    }
}

/// Query a backend's `list_sessions`, treating "not installed" and any other
/// transient failure as an empty session list. The list view is best-effort
/// reporting; an offline mux must not fail the command.
fn backend_sessions(mux: MuxName) -> Vec<String> {
    match rimz::mux::backend_for(mux).list_sessions() {
        Ok(sessions) => sessions,
        Err(err) => {
            warn!(mux = %mux, error = %err, "list_sessions failed; treating as empty");
            Vec::new()
        }
    }
}

fn print_human(rows: &[WorkspaceRow]) -> std::io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut table = render::Table::new([
        "WORKSPACE",
        "SESSION",
        "PROJECT_ROOT",
        "RUNNING",
        "LAST_ACTIVITY",
    ]);
    for row in rows {
        let running = row.running_on.as_deref().unwrap_or("-");
        let last = row.last_activity.as_deref().unwrap_or("-");
        let running_style = if row.running_on.is_some() {
            render::palette::GOOD
        } else {
            render::palette::FAINT
        };
        table.row([
            render::cell(row.workspace_id.as_str()).fg(render::palette::ACCENT),
            render::cell(row.session_name.as_str()),
            render::cell(row.project_root.as_str()).fg(render::palette::BODY),
            render::cell(running).fg(running_style),
            render::cell(last).dash(),
        ]);
    }
    table.render(&mut render::out())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_window_bounds() {
        let now = SystemTime::now();
        assert!(is_recent(Some(now), now), "now is recent");
        assert!(
            is_recent(Some(now - RECENT_WINDOW + Duration::from_secs(1)), now),
            "just inside the window is recent"
        );
        assert!(
            !is_recent(Some(now - RECENT_WINDOW - Duration::from_secs(1)), now),
            "just outside the window is dormant"
        );
        assert!(!is_recent(None, now), "no activity is dormant");
        assert!(
            is_recent(Some(now + Duration::from_secs(60)), now),
            "future mtime (clock skew) counts as recent"
        );
    }
}
