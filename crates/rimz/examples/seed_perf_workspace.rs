#![expect(
    clippy::print_stdout,
    reason = "manual profiling helper prints exported paths and commands"
)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rimz::{RuntimePaths, StatePaths, WorkspaceId};

const EVENTS_PER_AGENT: usize = 50;

fn main() -> Result<()> {
    let args = Args::parse()?;
    let scratch_root = args.scratch_root;
    let project_root = scratch_root.join("project-root");
    let state_root = scratch_root.join("state");
    let runtime_root = scratch_root.join("runtime");
    let pane_fixture = scratch_root.join("panes.json");
    std::fs::create_dir_all(&project_root)
        .with_context(|| format!("creating project root {}", project_root.display()))?;

    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let paths = StatePaths::under(workspace_id.clone(), &state_root).context("building paths")?;
    paths.ensure_dirs().context("creating state dirs")?;

    let mut runtime =
        RuntimePaths::under(workspace_id.clone(), &runtime_root).context("building runtime")?;
    runtime.persistent_shared_root = state_root.join("rimz").join("shared");
    runtime.ensure_dirs().context("creating runtime dirs")?;

    rimz::testkit::fleet::seed_fleet_ledger(&paths, args.fleet, args.history_events)
        .context("seeding fleet ledger")?;
    rimz::testkit::fleet::publish_fresh_produce_inputs(&runtime, args.fleet)
        .context("publishing fresh produce inputs")?;
    let panes = serde_json::to_vec(&rimz::testkit::fleet::synthetic_panes(args.fleet))
        .context("serializing pane fixture")?;
    std::fs::write(&pane_fixture, panes)
        .with_context(|| format!("writing pane fixture {}", pane_fixture.display()))?;

    println!("workspace_id={workspace_id}");
    println!("project_root={}", project_root.display());
    println!("XDG_STATE_HOME={}", state_root.display());
    println!("XDG_RUNTIME_DIR={}", runtime_root.display());
    println!("RIMZ_TEST_PANE_LIST={}", pane_fixture.display());
    println!(
        "snapshot_command=./target/release/rimz sidebar snapshot --json --no-produce --workspace-id {workspace_id} --mux zellij --session-name {}",
        rimz::testkit::fleet::SESSION_NAME
    );
    println!(
        "serve_command=./target/release/rimz sidebar serve --workspace-id {workspace_id} --mux zellij --session-name {}",
        rimz::testkit::fleet::SESSION_NAME
    );
    Ok(())
}

struct Args {
    fleet: usize,
    scratch_root: PathBuf,
    history_events: usize,
}

impl Args {
    fn parse() -> Result<Self> {
        let args: Vec<String> = std::env::args().collect();
        match args.as_slice() {
            [_, fleet, scratch_root] => {
                let fleet = parse_usize(fleet, "fleet")?;
                Ok(Self {
                    fleet,
                    scratch_root: Path::new(scratch_root).to_path_buf(),
                    history_events: fleet * EVENTS_PER_AGENT,
                })
            }
            [_, fleet, scratch_root, history_events] => {
                let fleet = parse_usize(fleet, "fleet")?;
                Ok(Self {
                    fleet,
                    scratch_root: Path::new(scratch_root).to_path_buf(),
                    history_events: parse_usize(history_events, "history-events")?,
                })
            }
            _ => bail!("usage: seed_perf_workspace <fleet> <scratch-root> [history-events]"),
        }
    }
}

fn parse_usize(raw: &str, name: &str) -> Result<usize> {
    raw.parse()
        .with_context(|| format!("parsing {name} as an unsigned integer"))
}
