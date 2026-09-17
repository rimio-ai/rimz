//! `rimz paths` — the on-disk ladder for the invoking context.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use super::GlobalFlags;
use crate::cli::render::{self, Table, cell};
use rimz::config::Isolation;
use rimz::config::MachineConfig;
use rimz::disk::paths;
use rimz::remote::aliases::RemoteAliases;
use rimz::sandbox::TmpView;
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths};

const SCHEMA: &str = "rimz.paths.v1";

#[derive(Debug, Args)]
pub struct PathsArgs {
    /// Print one JSON object instead of the table.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct PathsReport {
    schema: &'static str,
    home: PathBuf,
    config: PathBuf,
    theme: PathBuf,
    loop_config: PathBuf,
    remote: PathBuf,
    agents_home: PathBuf,
    workspace_id: String,
    workspace_dir: String,
    project_root: PathBuf,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    room_tmp: PathBuf,
    scratch: PathBuf,
    /// The invoking agent's scratch dir as it sees it: `/tmp/scratchpad` under
    /// sandbox isolation, the host path otherwise.
    scratch_agent_view: PathBuf,
    handoffs: PathBuf,
    runtime_root: PathBuf,
    logs: PathBuf,
    loops: PathBuf,
    web: PathBuf,
    shared: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    builds: PathBuf,
}

pub fn run(args: PathsArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let state =
        StatePaths::for_project_root(&workspace.project_root).context("resolving state paths")?;
    let runtime = RuntimePaths::for_state(&state).context("resolving runtime paths")?;
    let handle = std::env::var(rimz::harness::launch::ENV_AGENT_NAME)
        .ok()
        .filter(|name| rimz::agents::petname::valid_agent_name(name));
    let view = TmpView::current(invoking_isolation(), handle.as_deref(), &state);
    let scratch = state.scratch_dir(handle.as_deref());
    let report = PathsReport {
        schema: SCHEMA,
        home: paths::rimz_home(),
        config: MachineConfig::config_path(),
        theme: MachineConfig::theme_path(),
        loop_config: MachineConfig::loop_path(),
        remote: RemoteAliases::config_path(),
        agents_home: paths::agents_home(),
        workspace_id: state.workspace_id.to_string(),
        workspace_dir: state.dir_name.to_string(),
        project_root: workspace.project_root,
        scratch_agent_view: view.agent_path(&scratch),
        scratch,
        room_tmp: state.tmp_dir,
        state_dir: state.root,
        runtime_dir: runtime.root,
        handoffs: paths::handoffs_dir(),
        runtime_root: paths::runtime_rimz_root(),
        logs: paths::logs_dir(),
        loops: paths::loops_dir(),
        web: paths::web_dir(),
        shared: paths::shared_dir(),
        data: paths::data_dir(),
        cache: paths::cache_dir(),
        builds: paths::builds_dir(),
    };
    if args.json {
        render::json_pretty(&report)
    } else {
        render::finish(render_table(&report, &mut render::out()))
    }
}

/// The isolation the invoking agent runs under, stamped by its launch plan;
/// outside a launched agent, machine policy decides.
fn invoking_isolation() -> Option<Isolation> {
    match std::env::var("RIMZ_ISOLATION").ok()?.as_str() {
        "sandbox" => Some(Isolation::Sandbox),
        "host" => Some(Isolation::Host),
        _ => None,
    }
}

fn render_table(report: &PathsReport, w: &mut impl std::io::Write) -> std::io::Result<()> {
    let path = |path: &PathBuf| path.display().to_string();
    let mut table = Table::new(["PATH", "LOCATION"]);
    for (label, value) in [
        ("home", path(&report.home)),
        ("config", path(&report.config)),
        ("theme", path(&report.theme)),
        ("loop config", path(&report.loop_config)),
        ("remote", path(&report.remote)),
        ("agents home", path(&report.agents_home)),
        ("workspace id", report.workspace_id.clone()),
        ("workspace dir", report.workspace_dir.clone()),
        ("project root", path(&report.project_root)),
        ("state dir", path(&report.state_dir)),
        ("runtime dir", path(&report.runtime_dir)),
        ("room tmp", path(&report.room_tmp)),
        ("scratch", path(&report.scratch)),
        ("scratch (agent view)", path(&report.scratch_agent_view)),
        ("handoffs", path(&report.handoffs)),
        ("runtime root", path(&report.runtime_root)),
        ("logs", path(&report.logs)),
        ("loops", path(&report.loops)),
        ("web", path(&report.web)),
        ("shared", path(&report.shared)),
        ("data", path(&report.data)),
        ("cache", path(&report.cache)),
        ("builds", path(&report.builds)),
    ] {
        table.row([cell(label), cell(value)]);
    }
    table.render(w)
}
