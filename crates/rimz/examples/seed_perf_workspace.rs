#![expect(
    clippy::print_stdout,
    reason = "manual profiling helper prints exported paths and commands"
)]

use std::collections::{BTreeMap, HashMap};
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
    let codex_home = scratch_root.join("codex-home");

    let workspace =
        WorkspaceResolver::resolve(&project_root, None).context("resolving workspace")?;
    Store::open(paths.clone(), runtime.clone())
        .context("opening store")?
        .record_workspace(&workspace)
        .context("recording workspace")?;
    let spending_scopes = seed_spending_scopes(&state_root, &runtime_root, &scratch_root, &args)?;

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
    seed_spending_history(&runtime, &codex_home, &project_root, &args)?;
    let panes_json = serde_json::to_vec(&panes).context("serializing pane fixture")?;
    std::fs::write(&pane_fixture, panes_json)
        .with_context(|| format!("writing pane fixture {}", pane_fixture.display()))?;

    println!("workspace_id={workspace_id}");
    println!("project_root={}", project_root.display());
    println!("XDG_STATE_HOME={}", state_root.display());
    println!("XDG_RUNTIME_DIR={}", runtime_root.display());
    println!("RIMZ_TEST_PANE_LIST={}", pane_fixture.display());
    println!("CODEX_HOME={}", codex_home.display());
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
    for (index, (workspace_id, session_name)) in spending_scopes.iter().enumerate() {
        println!(
            "spending_scope_{}_command=./target/release/rimz sidebar snapshot --json --workspace-id {workspace_id} --mux zellij --session-name {session_name}",
            index + 1
        );
    }
    Ok(())
}

struct Args {
    fleet: usize,
    scratch_root: PathBuf,
    history_events: usize,
    git_worktrees: bool,
    spending_files: usize,
    spending_entries: usize,
}

impl Args {
    fn parse() -> Result<Self> {
        let raw: Vec<String> = std::env::args().skip(1).collect();
        let mut args = Vec::new();
        let mut git_worktrees = false;
        let mut spending_files = 0;
        let mut spending_entries = 0;
        let mut index = 0;
        while index < raw.len() {
            match raw[index].as_str() {
                "--git-worktrees" => git_worktrees = true,
                "--spending-files" | "--spending-entries" => {
                    let flag = raw[index].as_str();
                    index += 1;
                    let value = raw
                        .get(index)
                        .with_context(|| format!("missing value for {flag}"))?;
                    let value = parse_usize(value, flag.trim_start_matches("--"))?;
                    if flag == "--spending-files" {
                        spending_files = value;
                    } else {
                        spending_entries = value;
                    }
                }
                value => args.push(value.to_owned()),
            }
            index += 1;
        }
        match args.as_slice() {
            [fleet, scratch_root] => {
                let fleet = parse_usize(fleet, "fleet")?;
                Ok(Self {
                    fleet,
                    scratch_root: Path::new(scratch_root).to_path_buf(),
                    history_events: fleet * EVENTS_PER_AGENT,
                    git_worktrees,
                    spending_files,
                    spending_entries,
                })
            }
            [fleet, scratch_root, history_events] => {
                let fleet = parse_usize(fleet, "fleet")?;
                Ok(Self {
                    fleet,
                    scratch_root: Path::new(scratch_root).to_path_buf(),
                    history_events: parse_usize(history_events, "history-events")?,
                    git_worktrees,
                    spending_files,
                    spending_entries,
                })
            }
            _ => bail!(
                "usage: seed_perf_workspace <fleet> <scratch-root> [history-events] [--git-worktrees] [--spending-files N --spending-entries N]"
            ),
        }
    }
}

fn seed_spending_history(
    runtime: &RuntimePaths,
    codex_home: &Path,
    project_root: &Path,
    args: &Args,
) -> Result<()> {
    if args.spending_files == 0 && args.spending_entries == 0 {
        return Ok(());
    }
    if args.spending_files == 0 {
        bail!("--spending-files must be positive when --spending-entries is set");
    }
    match std::fs::remove_file(runtime.shared_provider_spending_path()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("removing warm provider-spending fixture"),
    }
    let sessions = codex_home.join("sessions");
    std::fs::create_dir_all(&sessions)
        .with_context(|| format!("creating spending fixture {}", sessions.display()))?;
    let cache_path = runtime.shared_spending_cursor_path();
    let mut cache = rimz::agents::spending::read_spending_cache(&cache_path);
    cache.files = HashMap::with_capacity(args.spending_files);
    let now = rimz::agents::spending::unix_secs_now();
    let base = args.spending_entries / args.spending_files;
    let remainder = args.spending_entries % args.spending_files;
    let mut entry_index = 0;
    for file_index in 0..args.spending_files {
        let path = sessions.join(format!("rollout-{file_index:06}.jsonl"));
        std::fs::File::create(&path)
            .with_context(|| format!("creating transcript fixture {}", path.display()))?;
        let count = base + usize::from(file_index < remainder);
        let entries = (0..count)
            .map(|_| {
                let index = entry_index;
                entry_index += 1;
                rimz::agents::spending::CachedEntry {
                    ts_secs: now.saturating_sub((index % (7 * 86_400)) as u64),
                    cost_usd: 0.001,
                    input: 1_200,
                    output: 80,
                    cache_write: 0,
                    cache_read: 800,
                    tool_calls: Default::default(),
                    message_id: Some(format!("msg-{index}")),
                    request_id: Some(format!("req-{index}")),
                    dedup_key: None,
                    thread_id: Some(format!("thread-{index}")),
                    is_sidechain: false,
                    has_speed: false,
                    model: Some("gpt-5.4".to_owned()),
                    rolled: false,
                }
            })
            .collect();
        cache.files.insert(
            path.to_string_lossy().into_owned(),
            rimz::agents::spending::FileCacheEntry {
                stat: rimz::agents::TranscriptStat::from_path(&path)
                    .context("reading transcript fixture stat")?,
                cursor: rimz::agents::spending::SpendCursor::default(),
                origin_path: Some(project_root.to_path_buf()),
                entries,
                unknown_models: BTreeMap::new(),
            },
        );
    }
    if !rimz::agents::spending::write_spending_cache(&cache_path, &cache) {
        bail!("writing spending fixture {}", cache_path.display());
    }
    Ok(())
}

fn seed_spending_scopes(
    state_root: &Path,
    runtime_root: &Path,
    scratch_root: &Path,
    args: &Args,
) -> Result<Vec<(WorkspaceId, String)>> {
    if args.spending_files == 0 {
        return Ok(Vec::new());
    }
    let scopes_root = scratch_root.join("spending-scopes");
    (1..=3)
        .map(|index| {
            let root = scopes_root.join(format!("scope-{index}"));
            std::fs::create_dir_all(&root)
                .with_context(|| format!("creating spending scope {}", root.display()))?;
            let workspace = WorkspaceResolver::resolve(&root, None)
                .with_context(|| format!("resolving spending scope {}", root.display()))?;
            let paths = StatePaths::under(workspace.workspace_id.clone(), state_root)
                .context("building spending scope state paths")?;
            paths
                .ensure_dirs()
                .context("creating spending scope state dirs")?;
            let mut runtime = RuntimePaths::under(workspace.workspace_id.clone(), runtime_root)
                .context("building spending scope runtime paths")?;
            runtime.persistent_shared_root = state_root.join("rimz").join("shared");
            runtime
                .ensure_dirs()
                .context("creating spending scope runtime dirs")?;
            Store::open(paths, runtime)
                .context("opening spending scope store")?
                .record_workspace(&workspace)
                .context("recording spending scope workspace")?;
            Ok((workspace.workspace_id, workspace.session_name))
        })
        .collect()
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
    run_git(project_root, &["config", "user.name", "RimZ Perf"])?;
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
