//! `rimz list` — show running and recently-active workspaces.
//!
//! "Known" means a `workspace.json` record exists under
//! `$XDG_STATE_HOME/rimz/workspaces/<id>/` from a previous `rimz start` or
//! store write. "Running" means the session name shows up in
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
use rimz::store::event::{LastDeathMarker, SessionDeathCause};
use rimz::store::paths::workspaces_dir;

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
    last_activity: Option<Timestamp>,
    last_death: Option<String>,
}

pub fn run(args: ListArgs, _globals: &GlobalFlags) -> Result<()> {
    let rows = collect_rows(args.all).context("listing workspaces")?;
    if args.json {
        return crate::cli::render::json_pretty(&rows);
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
            let workspace_dir = root.join(known.workspace_id.as_str());
            let last_activity = activity_for(&workspace_dir);
            let last_death = death_for(&workspace_dir);
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
                last_activity: last_activity.and_then(|at| Timestamp::try_from(at).ok()),
                last_death,
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

fn death_for(workspace_dir: &std::path::Path) -> Option<String> {
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(workspace_dir.join("last-death.json")).ok()?).ok()?;
    Some(death_summary(&marker))
}

fn death_summary(marker: &LastDeathMarker) -> String {
    let verb = match marker.cause {
        SessionDeathCause::Crash => "crashed",
        SessionDeathCause::Reboot => "rebooted",
    };
    let at = marker.at.strftime("%Y-%m-%d %H:%M");
    match marker.lost_agents.len() {
        0 => format!("{verb} · {at}"),
        1 => format!("{verb} · 1 agent · {at}"),
        n => format!("{verb} · {n} agents · {at}"),
    }
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

/// Query a backend's `list_sessions` for the best-effort list view. A mux that
/// isn't installed contributes an empty list silently; any other failure on an
/// installed mux is warned and treated as empty. An offline mux never fails the
/// command.
fn backend_sessions(mux: MuxName) -> Vec<String> {
    match rimz::mux::backend_for(mux).list_sessions() {
        Ok(sessions) => sessions,
        Err(rimz::mux::MuxErr::NotInstalled { .. }) => Vec::new(),
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
        "LAST_SEEN",
    ]);
    for row in rows {
        let running = row.running_on.as_deref().unwrap_or("-");
        let seen = last_seen(row);
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
            render::cell(seen).dash(),
        ]);
    }
    table.render(&mut render::out())
}

fn last_seen(row: &WorkspaceRow) -> String {
    let activity = row
        .last_activity
        .as_ref()
        .map(|ts| ts.strftime("%Y-%m-%d %H:%M").to_string());
    match (&row.running_on, &row.last_death) {
        (None, Some(death)) => death.clone(),
        _ => activity.unwrap_or_else(|| "-".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::AgentKind;
    use rimz::store::event::SessionDeathAgent;

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

    #[test]
    fn death_summary_reports_crash_with_plural_agents() {
        let summary = death_summary(&marker(SessionDeathCause::Crash, 8));

        assert_eq!(summary, "crashed · 8 agents · 1970-01-01 00:00");
        assert!(!summary.contains("died"), "{summary}");
    }

    #[test]
    fn death_summary_reports_singular_agent() {
        let summary = death_summary(&marker(SessionDeathCause::Crash, 1));

        assert!(summary.contains("1 agent · "), "{summary}");
        assert!(!summary.contains("1 agents"), "{summary}");
        assert!(!summary.contains("died"), "{summary}");
    }

    #[test]
    fn death_summary_omits_empty_agent_count_for_reboot() {
        let summary = death_summary(&marker(SessionDeathCause::Reboot, 0));

        assert_eq!(summary, "rebooted · 1970-01-01 00:00");
        assert!(!summary.contains("agent"), "{summary}");
        assert!(!summary.contains("died"), "{summary}");
    }

    #[test]
    fn last_seen_prefers_running_activity_over_stale_death_marker() {
        let death = "crashed · 2 agents · 1970-01-01 00:00";
        let mut row = row(Some("tmux"), Some(death), Some(Timestamp::UNIX_EPOCH));

        assert_eq!(last_seen(&row), "1970-01-01 00:00");

        row.running_on = None;
        assert_eq!(last_seen(&row), death);

        row.last_death = None;
        row.last_activity = None;
        assert_eq!(last_seen(&row), "-");
    }

    fn marker(cause: SessionDeathCause, agents: usize) -> LastDeathMarker {
        LastDeathMarker {
            cause,
            lost_agents: (0..agents)
                .map(|index| SessionDeathAgent {
                    kind: AgentKind::new_unchecked("claude"),
                    agent_id: format!("sess-{index}").into(),
                    name: None,
                })
                .collect(),
            at: Timestamp::UNIX_EPOCH,
            recovered: None,
        }
    }

    fn row(
        running_on: Option<&str>,
        last_death: Option<&str>,
        last_activity: Option<Timestamp>,
    ) -> WorkspaceRow {
        WorkspaceRow {
            workspace_id: "ws_000000000000000000000000".to_owned(),
            project_root: "/repo".to_owned(),
            session_name: "rimz-repo-000000".to_owned(),
            running_on: running_on.map(str::to_owned),
            last_activity,
            last_death: last_death.map(str::to_owned),
        }
    }
}
