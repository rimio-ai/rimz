//! `rimz doctor` — workspace health report: trust state, protocol versions,
//! socket-path budget, hook wiring, agent problems, and message-delivery
//! failures.
//!
//! The report is collected once into a [`model::DoctorReport`], then either
//! rendered as the human report ([`render`]) or serialized as JSON — to stdout
//! or atomically to a file. Collection lives in the sibling modules; presentation
//! lives in [`render`]; this file only assembles and emits.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

use super::GlobalFlags;
use crate::cli::render as ui;
use rimz::workspace::WorkspaceResolver;

mod agents;
mod messages;
mod model;
mod protocol;
mod render;
mod runtime;

use model::DoctorReport;

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Widen the agent section to every observed session, not just live problem rows.
    #[arg(long)]
    audit: bool,
    /// Emit machine-readable JSON instead of the human report.
    #[arg(long)]
    json: bool,
    /// Write the report to a file (atomically) instead of stdout.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
}

pub fn run(args: DoctorArgs, globals: &GlobalFlags) -> Result<()> {
    let report = collect_report(globals, args.audit);
    emit(&report, args.json, args.output.as_deref())
}

fn collect_report(globals: &GlobalFlags, audit: bool) -> DoctorReport {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone());
    let ws = workspace.as_ref().ok();
    DoctorReport {
        version: super::version::VERSION,
        host: runtime::collect_host(),
        workspace: match &workspace {
            Ok(ws) => model::Probe::Ready(workspace_view(ws)),
            Err(err) => model::Probe::Unavailable {
                error: err.to_string(),
            },
        },
        mux: runtime::collect_mux(globals.mux, ws),
        terminal: runtime::collect_terminal(),
        hooks: agents::collect_hooks(),
        plugins: agents::collect_plugins(),
        loop_tasks: collect_loop(),
        remote_control: runtime::collect_remote_control(),
        disk_usage: runtime::collect_storage(),
        protocols: ws.map(protocol::collect_protocols),
        trust: ws.map(agents::collect_trust),
        agents: ws.map(|ws| agents::collect_agent_rollup(ws, audit)),
        messages: ws.map(messages::collect_messages),
        diagnostics: ws.map(runtime::collect_diagnostics),
        last_incident: ws.and_then(runtime::collect_last_incident),
    }
}

/// The loop tasks from config plus transient instance state. Read-only and
/// workspace-independent: it surfaces the scheduled-execution surface this box
/// carries; `rimz loop list` reports whether each task's room is open.
fn collect_loop() -> model::LoopTasks {
    let mut tasks = rimz::harness::schedule::instances::load().0;
    tasks.extend(
        rimz::config::MachineConfig::load()
            .map(|config| config.r#loop.tasks.0)
            .unwrap_or_default(),
    );
    let rows = tasks
        .into_iter()
        .map(|(name, entry)| {
            let (when, valid) = match rimz::harness::schedule::parse_schedule(&name, &entry) {
                Ok(schedule) => (schedule.describe(), true),
                Err(err) => (format!("invalid: {err}"), false),
            };
            model::LoopTaskRow {
                name,
                spec: entry
                    .agent
                    .or_else(|| entry.wake.map(|target| target.handle))
                    .unwrap_or_else(|| "<invalid>".to_owned()),
                when,
                root: entry.root.display().to_string(),
                valid,
            }
        })
        .collect();
    model::LoopTasks { tasks: rows }
}

fn workspace_view(ws: &rimz::ResolvedWorkspace) -> model::Workspace {
    model::Workspace {
        workspace_id: ws.workspace_id.as_str().to_owned(),
        project_root: ws.project_root.display().to_string(),
        root_class: ws.root_class,
        worktree_root: ws.worktree_root.display().to_string(),
        worktree_branch: ws.worktree_branch.clone(),
        session_name: ws.session_name.clone(),
        sock_headroom: runtime::collect_socket_headroom(ws),
    }
}

/// Render or serialize the report to its destination. A file destination writes
/// plain text (ANSI stripped) atomically; stdout flows through the shared styled
/// stream, which keeps color only on a terminal.
fn emit(report: &DoctorReport, json: bool, output: Option<&Path>) -> Result<()> {
    if let Some(path) = output {
        let bytes = if json {
            let mut json = serde_json::to_string_pretty(report).expect("DoctorReport serializes");
            json.push('\n');
            json.into_bytes()
        } else {
            let mut stream = anstream::StripStream::new(Vec::new());
            render::render_human(report, &mut stream)?;
            stream.into_inner()
        };
        return rimz::store::atomic::write_bytes_atomically(path, &bytes)
            .with_context(|| format!("writing doctor report to {}", path.display()));
    }

    let mut out = ui::out();
    if json {
        let rendered = serde_json::to_string_pretty(report).expect("DoctorReport serializes");
        writeln!(out, "{rendered}")?;
    } else {
        render::render_human(report, &mut out)?;
    }
    Ok(())
}

pub fn ping() -> Result<()> {
    #[expect(clippy::print_stdout, reason = "liveness check output")]
    {
        println!("ok");
    }
    Ok(())
}
