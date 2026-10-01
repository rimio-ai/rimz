//! `rimz paths` — the on-disk ladder for the invoking context.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use super::GlobalFlags;
use crate::cli::render::{self, Table, cell};
use rimz::config::MachineConfig;
use rimz::disk::paths;
use rimz::remote::aliases::RemoteAliases;
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths};

const SCHEMA: &str = "rimz.paths.v2";

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
    shared: PathBuf,
    out: PathBuf,
    handoffs: PathBuf,
    runtime_root: PathBuf,
    logs: PathBuf,
    loops: PathBuf,
    web: PathBuf,
    accounts: PathBuf,
    cache: PathBuf,
    providers_cache: PathBuf,
    builds: PathBuf,
}

pub fn run(args: PathsArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let state =
        StatePaths::for_project_root(&workspace.project_root).context("resolving state paths")?;
    let runtime = RuntimePaths::for_state(&state).context("resolving runtime paths")?;
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
        room_tmp: state.tmp_dir,
        shared: state.room_shared_dir,
        out: state.out_dir,
        state_dir: state.root,
        runtime_dir: runtime.root,
        handoffs: paths::handoffs_dir(),
        runtime_root: paths::runtime_rimz_root(),
        logs: paths::logs_dir(),
        loops: paths::loops_dir(),
        web: paths::web_dir(),
        accounts: paths::accounts_dir(),
        cache: paths::cache_dir(),
        providers_cache: paths::providers_cache_dir(),
        builds: paths::builds_dir(),
    };
    if args.json {
        render::json_pretty(&report)
    } else {
        render::finish(render_table(&report, &mut render::out()))
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
        ("shared", path(&report.shared)),
        ("out", path(&report.out)),
        ("handoffs", path(&report.handoffs)),
        ("runtime root", path(&report.runtime_root)),
        ("logs", path(&report.logs)),
        ("loops", path(&report.loops)),
        ("web", path(&report.web)),
        ("accounts", path(&report.accounts)),
        ("cache", path(&report.cache)),
        ("providers cache", path(&report.providers_cache)),
        ("builds", path(&report.builds)),
    ] {
        table.row([cell(label), cell(value)]);
    }
    table.render(w)
}
