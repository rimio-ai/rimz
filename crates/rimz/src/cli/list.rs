//! `rimz list` — show running and known workspaces.
//!
//! "Known" means a `workspace.json` record exists under
//! `$XDG_STATE_HOME/rimz/workspaces/<id>/` from a previous `rimz start` or
//! ledger write. "Running" means the session name shows up in
//! `zellij list-sessions` or `tmux list-sessions`. The two are joined by
//! session name so reattach decisions stay local — no daemon, no index file.

use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use clap::Args;
use jiff::Timestamp;
use serde::Serialize;
use tracing::warn;

use super::GlobalFlags;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::paths::workspaces_dir;
use rimz::ledger::workspace_record;

#[derive(Debug, Args)]
pub struct ListArgs {
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
    let rows = collect_rows().context("listing workspaces")?;
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

fn collect_rows() -> Result<Vec<WorkspaceRow>> {
    let known = read_known_workspaces()?;
    let zellij_sessions = backend_sessions(MuxName::Zellij);
    let tmux_sessions = backend_sessions(MuxName::Tmux);

    let mut rows: Vec<WorkspaceRow> = known
        .into_iter()
        .map(|known| {
            let running_on = if zellij_sessions.contains(&known.session_name) {
                Some(MuxName::Zellij.as_str().to_owned())
            } else if tmux_sessions.contains(&known.session_name) {
                Some(MuxName::Tmux.as_str().to_owned())
            } else {
                None
            };
            WorkspaceRow {
                workspace_id: known.workspace_id.as_str().to_owned(),
                project_root: known.project_root.display().to_string(),
                session_name: known.session_name,
                running_on,
                last_activity: known.last_activity.map(|ts| ts.to_string()),
            }
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
    last_activity: Option<Timestamp>,
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

/// Best-effort "last activity" timestamp — newest mtime across the files
/// that move when the workspace is in use. Used purely for the operator's
/// reattach decision; never gates correctness.
fn activity_for(workspace_dir: &std::path::Path) -> Option<Timestamp> {
    let candidates = [
        workspace_dir.join("events.log.jsonl"),
        workspace_dir.join("snapshots").join("latest.json"),
        workspace_dir.join("workspace.json"),
    ];
    let newest: SystemTime = candidates
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .filter_map(|meta| meta.modified().ok())
        .max()?;
    Timestamp::try_from(newest).ok()
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
