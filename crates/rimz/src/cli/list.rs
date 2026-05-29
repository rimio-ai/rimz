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
//! `rimz workspace prune` reaps it. A *corrupt* record is still surfaced.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use clap::Args;
use jiff::Timestamp;
use serde::Serialize;
use tracing::warn;

use super::GlobalFlags;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::paths::workspaces_dir;
use rimz::ledger::workspace_record::{self, WorkspaceRecordErr};

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
    print_human(&rows);
    Ok(())
}

fn collect_rows(all: bool) -> Result<Vec<WorkspaceRow>> {
    let known = read_known_workspaces()?;
    let zellij_sessions = backend_sessions(MuxName::Zellij);
    let tmux_sessions = backend_sessions(MuxName::Tmux);
    let now = SystemTime::now();

    let mut rows: Vec<WorkspaceRow> = known
        .into_iter()
        .filter_map(|known| {
            let running_on = if zellij_sessions.contains(&known.session_name) {
                Some(MuxName::Zellij.as_str().to_owned())
            } else if tmux_sessions.contains(&known.session_name) {
                Some(MuxName::Tmux.as_str().to_owned())
            } else {
                None
            };
            // Default view: running sessions plus anything touched recently.
            // `--all` keeps dormant workspaces in the listing.
            if !all && running_on.is_none() && !is_recent(known.last_activity, now) {
                return None;
            }
            Some(WorkspaceRow {
                workspace_id: known.workspace_id.as_str().to_owned(),
                project_root: known.project_root.display().to_string(),
                session_name: known.session_name,
                running_on,
                last_activity: known
                    .last_activity
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

struct KnownWorkspace {
    workspace_id: WorkspaceId,
    project_root: PathBuf,
    session_name: String,
    last_activity: Option<SystemTime>,
}

fn read_known_workspaces() -> Result<Vec<KnownWorkspace>> {
    let root = workspaces_dir();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", root.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(workspace_id) = WorkspaceId::parse(name) else {
            continue;
        };
        let record_path = path.join("workspace.json");
        let record = match workspace_record::read(&record_path) {
            Ok(record) => record,
            // A dir without a record isn't a usable workspace — half-removed or
            // never finished. Skip it quietly; `rimz workspace prune` reaps it.
            Err(WorkspaceRecordErr::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            // A record that exists but won't parse is a real anomaly worth
            // surfacing.
            Err(err) => {
                warn!(workspace = %workspace_id, error = %err, "skipping workspace with unreadable record");
                continue;
            }
        };
        let last_activity = activity_for(&path);
        out.push(KnownWorkspace {
            workspace_id,
            project_root: record.project_root,
            session_name: record.session_name,
            last_activity,
        });
    }
    Ok(out)
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

fn print_human(rows: &[WorkspaceRow]) {
    if rows.is_empty() {
        return;
    }
    let id_w = rows
        .iter()
        .map(|r| r.workspace_id.len())
        .max()
        .unwrap_or(12)
        .max(12);
    let session_w = rows
        .iter()
        .map(|r| r.session_name.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let root_w = rows
        .iter()
        .map(|r| r.project_root.len())
        .max()
        .unwrap_or(12)
        .max(12);
    #[expect(clippy::print_stdout, reason = "user-facing table emitter")]
    {
        println!(
            "{:<id_w$}  {:<session_w$}  {:<root_w$}  {:<7}  LAST_ACTIVITY",
            "WORKSPACE", "SESSION", "PROJECT_ROOT", "RUNNING",
        );
        for row in rows {
            let running = row.running_on.as_deref().unwrap_or("-");
            let last = row.last_activity.as_deref().unwrap_or("-");
            println!(
                "{:<id_w$}  {:<session_w$}  {:<root_w$}  {:<7}  {last}",
                row.workspace_id, row.session_name, row.project_root, running,
            );
        }
    }
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
