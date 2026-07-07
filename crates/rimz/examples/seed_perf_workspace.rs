#![expect(
    clippy::print_stdout,
    reason = "manual profiling helper prints exported paths and commands"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rimz::{RuntimePaths, StatePaths, Store, WorkspaceId, WorkspaceResolver};

const EVENTS_PER_AGENT: usize = 50;

fn main() -> Result<()> {
    let args = Args::parse()?;
    let scratch_root = args.scratch_root.clone();
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

    let workspace =
        WorkspaceResolver::resolve(&project_root, None).context("resolving workspace")?;
    Store::open(paths.clone(), runtime.clone())
        .context("opening store")?
        .record_workspace(&workspace)
        .context("recording workspace")?;

    let panes = synthetic_panes(&scratch_root, &project_root, &args)?;
    if args.git_worktrees {
        rimz::testkit::fleet::seed_fleet_store_with_panes(&paths, &panes, args.history_events)
            .context("seeding git-backed fleet store")?;
    } else {
        rimz::testkit::fleet::seed_fleet_store(&paths, args.fleet, args.history_events)
            .context("seeding fleet store")?;
    }
    rimz::testkit::fleet::publish_fresh_produce_inputs_for_panes(&runtime, panes.clone())
        .context("publishing fresh produce inputs")?;
    let panes_json = serde_json::to_vec(&panes).context("serializing pane fixture")?;
    std::fs::write(&pane_fixture, panes_json)
        .with_context(|| format!("writing pane fixture {}", pane_fixture.display()))?;

    println!("workspace_id={workspace_id}");
    println!("project_root={}", project_root.display());
    println!("XDG_STATE_HOME={}", state_root.display());
    println!("XDG_RUNTIME_DIR={}", runtime_root.display());
    println!("RIMZ_TEST_PANE_LIST={}", pane_fixture.display());
    println!("git_worktrees={}", args.git_worktrees);
    println!(
        "snapshot_command=./target/release/rimz sidebar snapshot --json --workspace-id {workspace_id} --mux zellij --session-name {}",
        rimz::testkit::fleet::SESSION_NAME
    );
    println!(
        "profiling_snapshot_command=./target/profiling/rimz sidebar snapshot --json --workspace-id {workspace_id} --mux zellij --session-name {}",
        rimz::testkit::fleet::SESSION_NAME
    );
    println!(
        "consumer_snapshot_command=env -u RIMZ_TEST_PANE_LIST ./target/release/rimz sidebar snapshot --json --no-produce --workspace-id {workspace_id} --mux zellij --session-name {}",
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
    git_worktrees: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        let git_worktrees = args.last().is_some_and(|arg| arg == "--git-worktrees");
        if git_worktrees {
            args.pop();
        }
        match args.as_slice() {
            [fleet, scratch_root] => {
                let fleet = parse_usize(fleet, "fleet")?;
                Ok(Self {
                    fleet,
                    scratch_root: Path::new(scratch_root).to_path_buf(),
                    history_events: fleet * EVENTS_PER_AGENT,
                    git_worktrees,
                })
            }
            [fleet, scratch_root, history_events] => {
                let fleet = parse_usize(fleet, "fleet")?;
                Ok(Self {
                    fleet,
                    scratch_root: Path::new(scratch_root).to_path_buf(),
                    history_events: parse_usize(history_events, "history-events")?,
                    git_worktrees,
                })
            }
            _ => bail!(
                "usage: seed_perf_workspace <fleet> <scratch-root> [history-events] [--git-worktrees]"
            ),
        }
    }
}

fn parse_usize(raw: &str, name: &str) -> Result<usize> {
    raw.parse()
        .with_context(|| format!("parsing {name} as an unsigned integer"))
}

fn synthetic_panes(
    scratch_root: &Path,
    project_root: &Path,
    args: &Args,
) -> Result<Vec<rimz::pane::PaneRef>> {
    let mut panes = rimz::testkit::fleet::synthetic_panes(args.fleet);
    if !args.git_worktrees {
        return Ok(panes);
    }
    let worktrees = seed_git_worktrees(project_root, scratch_root, args.fleet)?;
    for (pane, worktree) in panes.iter_mut().zip(worktrees) {
        pane.command = Some("claude".to_owned());
        pane.cwd = Some(worktree.to_string_lossy().into_owned());
    }
    Ok(panes)
}

fn seed_git_worktrees(
    project_root: &Path,
    scratch_root: &Path,
    fleet: usize,
) -> Result<Vec<PathBuf>> {
    if fleet == 0 {
        return Ok(Vec::new());
    }

    run_git(project_root, &["init", "-q", "-b", "main"])?;
    run_git(
        project_root,
        &["config", "user.email", "perf@example.invalid"],
    )?;
    run_git(project_root, &["config", "user.name", "Rimz Perf"])?;
    std::fs::write(project_root.join("README.md"), "perf fixture\n")
        .context("writing git fixture README")?;
    run_git(project_root, &["add", "README.md"])?;
    run_git(project_root, &["commit", "-q", "-m", "base"])?;

    let worktree_root = scratch_root.join("worktrees");
    std::fs::create_dir_all(&worktree_root)
        .with_context(|| format!("creating worktree root {}", worktree_root.display()))?;
    let mut worktrees = Vec::with_capacity(fleet);
    for slot in 0..fleet {
        let branch = format!("wt-{slot}");
        let worktree = worktree_root.join(&branch);
        git_worktree_add(project_root, &branch, &worktree)?;
        std::fs::write(worktree.join(format!("slot-{slot}.txt")), "one\ntwo\n")
            .with_context(|| format!("writing fixture file in {}", worktree.display()))?;
        worktrees.push(worktree);
    }
    Ok(worktrees)
}

fn git_worktree_add(project_root: &Path, branch: &str, worktree: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "add", "-q", "-b", branch])
        .arg(worktree)
        .arg("HEAD")
        .output()
        .with_context(|| format!("spawning git worktree add {}", worktree.display()))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git worktree add {} failed: {}",
        worktree.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .with_context(|| format!("spawning git -C {} {}", cwd.display(), args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git -C {} {} failed: {}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}
