use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;

use divan::Bencher;
use tempfile::TempDir;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
/// Rotated history rows in the fold benches: the main room's carryover shape
/// (~1,500 rows of ~8 KB JSON each).
const HISTORY_CARRYOVER: usize = 1_500;
const CARRYOVER_PROMPT_BYTES: usize = 2_500;
const SPENDING_FILES: usize = 4;
const SPENDING_ENTRIES_PER_FILE: usize = 5_000;
const SPENDING_NOW_SECS: u64 = 1_780_394_400;

fn main() {
    divan::main();
}

#[divan::bench(sample_count = 1, sample_size = 1)]
#[expect(
    clippy::print_stdout,
    reason = "benchmark reports paired sequential and burst wall budgets"
)]
fn hook_stop_budget(bencher: Bencher) {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    fn sample(mut command: Command, payload: &[u8], hook: bool) -> Duration {
        let start = Instant::now();
        let mut child = command.spawn().unwrap();
        let written = child.stdin.take().unwrap().write_all(payload);
        assert!(
            written.is_ok() || (!hook && written.unwrap_err().kind() == io::ErrorKind::BrokenPipe)
        );
        let status = child.wait().unwrap();
        let wall = start.elapsed();
        // These commands have bounded output; collection is outside timing,
        // without extra reader threads.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        assert!(status.success(), "{status:?}; stderr {stderr:?}");
        if hook {
            assert!(stdout.is_empty(), "{stdout:?}");
        }
        wall
    }

    fn p99(mut samples: Vec<Duration>) -> Duration {
        samples.sort_unstable();
        samples[(samples.len() * 99).div_ceil(100) - 1]
    }

    let home_root = tempfile::Builder::new()
        .prefix("rimz-test-home-")
        .tempdir()
        .unwrap();
    let runtime_root = tempfile::Builder::new().prefix("rr").tempdir().unwrap();
    let root = home_root.path();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("rimz");
    assert!(
        binary.is_file(),
        "build rimz in the bench profile before measuring"
    );
    let copied_home = std::env::var_os("RIMZ_HOOK_BUDGET_HOME");
    let home = root.join(if copied_home.is_some() {
        ".rimz"
    } else {
        "state"
    });
    let runtime = runtime_root.path();
    let (project_root, paths) = if let Some(source) = copied_home.as_ref() {
        assert!(
            Command::new("cp")
                .arg("-a")
                .arg("--")
                .arg(PathBuf::from(source).join(".rimz"))
                .arg(&home)
                .status()
                .unwrap()
                .success()
        );
        let mut rooms = rimz::workspace::known_workspaces_under(
            &rimz::disk::paths::workspaces_dir_under(&home),
        )
        .unwrap();
        assert_eq!(rooms.len(), 1, "copied home must contain exactly one room");
        let room = rooms.remove(0);
        let paths = rimz::StatePaths::under_named(room.workspace_id, room.dir_name, &home);
        for queue in [&paths.hook_ingress_log, &paths.hook_drain_cursor] {
            if queue.exists() {
                std::fs::remove_file(queue).unwrap();
            }
        }
        (room.project_root, paths)
    } else {
        (
            root.to_path_buf(),
            rimz::StatePaths::for_project_root_under(root, &home).unwrap(),
        )
    };
    let runtime_paths = rimz::RuntimePaths::for_state_under(&paths, runtime);
    let _sandbox = rimz::testkit::sandbox::TestSandbox::arm(
        rimz::testkit::sandbox::SandboxSpec {
            home_root: root.into(),
            runtime_root: runtime.into(),
        },
        &binary.with_file_name("rimz-test-reaper"),
    )
    .unwrap();
    let store = rimz::Store::open(paths.clone(), runtime_paths).unwrap();
    if copied_home.is_none() {
        let resolved =
            rimz::WorkspaceResolver::resolve_under(root, Some(root.into()), &home).unwrap();
        store.record_workspace(&resolved).unwrap();
        rimz::testkit::fleet::seed_fleet_store(&paths, FLEET, HISTORY_EVENTS).unwrap();
    }
    store.snapshot().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop", "session_id": "bench-stop", "turn_id": "bench-turn",
        "cwd": project_root, "last_assistant_message": "done"
    })
    .to_string();
    let command = || {
        let mut command = Command::new(&binary);
        command
            .env_clear()
            .env("HOME", root)
            .env("RIMZ_HOME", &home)
            .env("XDG_RUNTIME_DIR", runtime)
            .env("PATH", "/usr/bin:/bin")
            .env("RIMZ_WORKSPACE_ID", paths.workspace_id.as_str())
            .env("RIMZ_PROJECT_ROOT", &project_root)
            .env("RIMZ_AGENT_PID", std::process::id().to_string())
            .current_dir(&project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };
    let burst = |hook| {
        let barrier = Barrier::new(24);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..24)
                .map(|_| {
                    let barrier = &barrier;
                    let payload = payload.as_bytes();
                    let mut command = command();
                    if hook {
                        command.args(["hooks", "feed", "--source", "codex"]);
                    } else {
                        command.arg("--version");
                    }
                    scope.spawn(move || {
                        barrier.wait();
                        sample(command, payload, hook)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        })
    };
    let mut worker = command();
    let mut drainer = worker
        .args(["hooks", "drain", "--project-root"])
        .arg(&project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let deadline = Instant::now() + Duration::from_secs(5);
    let _lease = loop {
        match UnixStream::connect(&socket) {
            Ok(lease) => break lease,
            Err(error) => {
                assert!(Instant::now() < deadline, "drainer did not listen: {error}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let mut warm = command();
    warm.args(["hooks", "feed", "--source", "codex"]);
    sample(warm, payload.as_bytes(), true);
    assert!(
        command()
            .args(["hooks", "drain", "--project-root"])
            .arg(&project_root)
            .arg("--once")
            .output()
            .unwrap()
            .status
            .success()
    );
    for hook in [true, false] {
        drop(burst(hook));
    }
    let room = if copied_home.is_some() {
        "live-copy"
    } else {
        "seeded"
    };
    bencher.bench_local(|| {
        let mut hooks = Vec::new();
        let mut versions = Vec::new();
        for pair in 0usize..48 {
            for hook_first in [true, false] {
                let hook = pair.is_multiple_of(2) == hook_first;
                let mut command = command();
                if hook {
                    command.args(["hooks", "feed", "--source", "codex"]);
                } else {
                    command.arg("--version");
                }
                let measured = sample(command, payload.as_bytes(), hook);
                if hook {
                    hooks.push(measured);
                } else {
                    versions.push(measured);
                }
            }
        }
        let hook = p99(hooks);
        let version = p99(versions);
        let ceiling = Duration::from_millis(4).max(version * 2);
        println!("{room} sequential: hook p99 {hook:?}; --version p99 {version:?}; ceiling {ceiling:?}");
        let mut hook_bursts = Vec::new();
        let mut version_bursts = Vec::new();
        for pair in 0..10 {
            for hook in [true, false] {
                let samples = burst(hook);
                println!(
                    "{room} burst {pair} {}: wall p99 {:?}",
                    if hook { "hook" } else { "version" },
                    p99(samples.clone()),
                );
                if hook {
                    hook_bursts.extend(samples);
                } else {
                    version_bursts.extend(samples);
                }
            }
        }
        let hook_burst = p99(hook_bursts);
        let version_burst = p99(version_bursts);
        let burst_ceiling = Duration::from_millis(4).max(version_burst * 2);
        println!("{room} 24-way: hook p99 {hook_burst:?}; --version p99 {version_burst:?}; ceiling {burst_ceiling:?}");
        assert!(
            hook <= ceiling,
            "hook p99 {hook:?} > {ceiling:?}; --version p99 {version:?}"
        );
        assert!(
            hook_burst <= burst_ceiling,
            "24-way hook p99 {hook_burst:?} > {burst_ceiling:?}; --version p99 {version_burst:?}"
        );
    });
    // Cleanup also includes the unmeasured warm-up; serial apply on the
    // live-sized copy can outlast the CLI's 30-second wait.
    let drain_start = Instant::now();
    rimz::harness::hook_drain::drain_through(&store, 0, None, Duration::from_secs(120)).unwrap();
    println!("{room} after-return drain: {:?}", drain_start.elapsed());
    drainer.kill().unwrap();
    drainer.wait().unwrap();
}

struct BenchWorkspace {
    _tempdir: TempDir,
    paths: rimz::StatePaths,
    runtime: rimz::RuntimePaths,
}

impl BenchWorkspace {
    fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let state_root = tempdir.path().join("state");
        let runtime_root = tempdir.path().join("runtime");
        let workspace_id = rimz::WorkspaceId::from_project_root(tempdir.path());
        let paths = rimz::StatePaths::under(workspace_id.clone(), &state_root).expect("paths");
        paths.ensure_dirs().expect("state dirs");
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("runtime");
        std::fs::create_dir_all(&runtime.root).expect("runtime root");
        std::fs::create_dir_all(&runtime.shared_root).expect("shared runtime root");
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
        }
    }

    fn seed_fleet(&self, fleet: usize, history_events: usize) {
        rimz::testkit::fleet::seed_fleet_store(&self.paths, fleet, history_events)
            .expect("seed fleet");
    }

    fn publish_inputs(&self, fleet: usize) {
        rimz::testkit::fleet::publish_fresh_produce_inputs(&self.runtime, fleet)
            .expect("publish produce inputs");
    }
}

struct SnapshotFixture {
    _workspace: BenchWorkspace,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
}

struct FuseFixture {
    _workspace: BenchWorkspace,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
    events: rimz::sidebar::event_store::EventStore,
    now_ms: u64,
}

struct EnrichFixture {
    _workspace: BenchWorkspace,
    runtime: rimz::RuntimePaths,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
    frame: rimz::sidebar::frame::PaneFrame,
}

struct ConsumerAdoptFixture {
    _workspace: BenchWorkspace,
    paths: rimz::StatePaths,
    reader: rimz::sidebar::consumer::PublishedSnapshotReader,
}

struct FoldFixture {
    _workspace: BenchWorkspace,
    paths: rimz::StatePaths,
    cursor: rimz::sidebar::consumer::RollupCursor,
}

struct SpendingFixture {
    _tempdir: TempDir,
    cache_path: PathBuf,
    files: Vec<rimz::agents::spending::SpendingFile>,
    prices: rimz::agents::PriceBook,
    walker: rimz::agents::spending::SpendingWalker,
    sources: Vec<rimz::agents::spending::SpendingSource>,
}

struct ChangedSessionFixture {
    _tempdir: TempDir,
    refresh: rimz::testkit::ChangedSessionRefreshFixture,
}

fn changed_session_fixture(
    build: impl FnOnce(&std::path::Path, usize) -> rimz::testkit::ChangedSessionRefreshFixture,
) -> ChangedSessionFixture {
    let tempdir = TempDir::new().expect("tempdir");
    let refresh = build(tempdir.path(), 500);
    ChangedSessionFixture {
        _tempdir: tempdir,
        refresh,
    }
}

fn snapshot_fixture() -> SnapshotFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    workspace.publish_inputs(FLEET);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    let snapshot = rimz::sidebar::produce::produce_snapshot(
        &mut cursor,
        &workspace.paths,
        &workspace.runtime,
        &rimz::testkit::fleet::produce_options(),
    )
    .expect("produce snapshot");
    SnapshotFixture {
        _workspace: workspace,
        snapshot,
    }
}

fn fuse_fixture() -> FuseFixture {
    let SnapshotFixture {
        _workspace,
        snapshot,
    } = snapshot_fixture();
    let mut events = rimz::sidebar::event_store::EventStore::default();
    let pane_id = rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_0");
    let now_ms = snapshot
        .panes_produced_at_ms
        .unwrap_or_else(rimz::utils::time::unix_now_ms)
        .saturating_add(1);
    events.append(
        rimz::wakeup::events::SidebarEvent::CommandChanged {
            pane_id,
            command: "claude".to_owned(),
        },
        now_ms,
        now_ms,
    );
    FuseFixture {
        _workspace,
        snapshot,
        events,
        now_ms,
    }
}

fn owned_fuse_fixture() -> FuseFixture {
    let SnapshotFixture {
        _workspace,
        snapshot,
    } = snapshot_fixture();
    let now_ms = snapshot
        .panes_produced_at_ms
        .unwrap_or_else(rimz::utils::time::unix_now_ms)
        .saturating_add(1);
    FuseFixture {
        _workspace,
        snapshot,
        events: rimz::sidebar::event_store::EventStore::default(),
        now_ms,
    }
}

fn enrich_fixture() -> EnrichFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    workspace.publish_inputs(FLEET);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    let snapshot =
        rimz::sidebar::consumer::rollup_snapshot(&workspace.paths, &mut cursor).expect("rollup");
    let frame = rimz::sidebar::frame::assemble_frame(
        rimz::testkit::fleet::synthetic_panes(FLEET),
        rimz::utils::time::unix_now_ms(),
        rimz::testkit::fleet::SESSION_NAME,
    );
    EnrichFixture {
        runtime: workspace.runtime.clone(),
        _workspace: workspace,
        snapshot,
        frame,
    }
}

fn consumer_adopt_fixture(warm_parse: bool) -> ConsumerAdoptFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    workspace.publish_inputs(FLEET);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    let snapshot =
        rimz::sidebar::consumer::rollup_snapshot(&workspace.paths, &mut cursor).expect("rollup");
    std::fs::write(
        &workspace.paths.latest_snapshot,
        serde_json::to_vec(&snapshot).expect("serialize latest"),
    )
    .expect("publish latest");
    let mut frame = rimz::sidebar::frame::assemble_frame(
        rimz::testkit::fleet::synthetic_panes(FLEET),
        rimz::utils::time::unix_now_ms(),
        rimz::testkit::fleet::SESSION_NAME,
    );
    frame.topology_stamp_ms = Some(1);
    frame.metrics_stamp_ms = Some(1);
    std::fs::write(
        workspace.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).expect("serialize frame"),
    )
    .expect("publish frame");
    let projection = rimz::sidebar::enrich::enrich_workspace(
        snapshot,
        Some(&frame),
        &workspace.paths,
        &workspace.runtime,
        None,
        rimz::sidebar::enrich::FoldOpts {
            producing: false,
            fresh_roots: None,
            config: None,
            lanes: None,
            agent_projection: Default::default(),
        },
        &rimz::diag::DiagSink::disabled(),
    );
    rimz::sidebar::workspace_projection::WorkspaceProjectionPublisher::default()
        .publish(
            &workspace.runtime,
            rimz::testkit::fleet::SESSION_NAME,
            &projection,
            &frame,
        )
        .expect("publish workspace projection");
    let mut reader = rimz::sidebar::consumer::PublishedSnapshotReader::new(
        workspace.runtime.clone(),
        rimz::testkit::fleet::SESSION_NAME,
        None,
    );
    if warm_parse {
        reader
            .read_adopting(&workspace.paths)
            .expect("warm adoption");
    }
    ConsumerAdoptFixture {
        paths: workspace.paths.clone(),
        reader,
        _workspace: workspace,
    }
}

/// A warm cursor over `FLEET` live agents and `history_carryover` rotated
/// history rows (production-weight: each carries a `CARRYOVER_PROMPT_BYTES`
/// prompt), with one appended lifecycle frame when `append` is set.
fn fold_fixture(history_carryover: usize, append: bool) -> FoldFixture {
    let workspace = BenchWorkspace::new();
    if history_carryover > 0 {
        let store = rimz::Store::open(workspace.paths.clone(), workspace.runtime.clone())
            .expect("open store");
        rimz::testkit::fleet::seed_history_carryover(
            &store,
            history_carryover,
            CARRYOVER_PROMPT_BYTES,
        )
        .expect("stage carryover");
    }
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    cursor.fold(&workspace.paths).expect("cold fold");
    if append {
        rimz::store::event_log::append(
            &workspace.paths.events_log,
            &rimz::testkit::fleet::registered_lifecycle(&workspace.paths.workspace_id, 0),
        )
        .expect("append delta");
    }
    FoldFixture {
        paths: workspace.paths.clone(),
        cursor,
        _workspace: workspace,
    }
}

fn ended_carryover_fixture(rows: usize, rebirth: bool) -> FoldFixture {
    let workspace = BenchWorkspace::new();
    let store =
        rimz::Store::open(workspace.paths.clone(), workspace.runtime.clone()).expect("open store");
    rimz::testkit::fleet::seed_ended_carryover(&store, rows).expect("seed carryover");
    let export_dir = std::env::var_os("RIMZ_CARRYOVER_FIXTURE_DIR").map(PathBuf::from);
    if let Some(root) = &export_dir {
        std::fs::create_dir_all(root).expect("fixture export directory");
        let target = root.join(format!("carryover-{rows}.json"));
        if !target.exists() {
            std::fs::copy(&workspace.paths.agents_carryover, target).expect("export fixture");
        }
    }
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    if rebirth {
        rimz::store::event_log::append(
            &workspace.paths.events_log,
            &rimz::store::event::EventEnvelope::session_rebirth(
                workspace.paths.workspace_id.clone(),
                "bench-session",
            ),
        )
        .expect("append rebirth");
        if let Some(root) = &export_dir {
            std::fs::copy(
                &workspace.paths.events_log,
                root.join(format!("rebirth-{rows}.log.jsonl")),
            )
            .expect("export rebirth log");
        }
        cursor.fold(&workspace.paths).expect("prime reborn cursor");
        rimz::store::event_log::append(
            &workspace.paths.events_log,
            &rimz::testkit::fleet::registered_lifecycle(&workspace.paths.workspace_id, 0),
        )
        .expect("append registration");
    }
    FoldFixture {
        paths: workspace.paths.clone(),
        cursor,
        _workspace: workspace,
    }
}

fn spending_fixture(warm: bool) -> SpendingFixture {
    spending_fixture_scaled(SPENDING_FILES, SPENDING_ENTRIES_PER_FILE, warm, false)
}

fn spending_fixture_scaled(
    files_count: usize,
    entries_per_file: usize,
    warm: bool,
    scoped: bool,
) -> SpendingFixture {
    let tempdir = TempDir::new().expect("tempdir");
    let cache_path = tempdir.path().join("spending.json");
    let history_root = tempdir.path().join("history");
    let mut cache = rimz::agents::spending::read_spending_cache(&cache_path);
    cache.files = HashMap::new();
    let mut files = Vec::new();
    for file_index in 0..files_count {
        let transcript = history_root
            .join(format!("{:04}", file_index / 100))
            .join(format!("cached-{file_index}.jsonl"));
        std::fs::create_dir_all(transcript.parent().expect("history parent"))
            .expect("history directory");
        std::fs::write(&transcript, b"").expect("transcript");
        let entries = (0..entries_per_file)
            .map(|offset| {
                let index = file_index * entries_per_file + offset;
                rimz::agents::spending::CachedEntry {
                    ts_secs: SPENDING_NOW_SECS - 86_400
                        + u64::try_from(offset % 3_600).expect("offset fits u64"),
                    cost_usd: 0.001,
                    input: 1200,
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
                    model: Some("claude-opus-4-8".to_owned()),
                    rolled: false,
                }
            })
            .collect();
        cache.files.insert(
            transcript.to_string_lossy().into_owned(),
            rimz::agents::spending::FileCacheEntry {
                stat: rimz::agents::TranscriptStat::from_path(&transcript)
                    .expect("transcript stat"),
                cursor: rimz::agents::spending::SpendCursor::default(),
                origin_path: scoped.then(|| tempdir.path().to_path_buf()),
                entries,
                unknown_models: BTreeMap::new(),
            },
        );
        files.push(rimz::agents::spending::SpendingFile {
            adapter: rimz::agents::definition_by_kind("claude").expect("Claude definition"),
            login: rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
            path: transcript,
        });
    }
    rimz::agents::spending::write_spending_cache(&cache_path, &cache);
    let prices = rimz::agents::PriceBook::default();
    let sources = vec![rimz::agents::spending::SpendingSource::group(vec![
        rimz::agents::spending::SpendingSourceTree::new(&history_root, "**/*.jsonl")
            .expect("benchmark pattern"),
    ])];
    let mut walker = rimz::agents::spending::SpendingWalker::new();
    if warm {
        let origin_overrides = HashMap::new();
        let user_inputs = Vec::new();
        let spec = rimz::agents::spending::HeadlineSpec::default();
        let req = rimz::agents::spending::WalkRequest {
            files: &files,
            prices: &prices,
            now_secs: SPENDING_NOW_SECS,
            origin_overrides: &origin_overrides,
            user_inputs: &user_inputs,
            scope: None,
            spec: &spec,
        };
        let _ = walker.walk(&cache_path, &req, &mut rimz::agents::spending::SilentWalk);
    }
    SpendingFixture {
        _tempdir: tempdir,
        cache_path,
        files,
        prices,
        walker,
        sources,
    }
}

fn spending_discovery_fixture(warm: bool) -> SpendingFixture {
    let mut fixture = spending_fixture_scaled(6_000, 17, true, true);
    if warm {
        let _ = fixture.walker.discover_declared_spending_files(
            rimz::agents::definition_by_kind("claude").expect("Claude definition"),
            fixture.sources.clone(),
            SPENDING_NOW_SECS,
        );
    }
    fixture
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn fuse(bencher: Bencher) {
    bencher
        .with_inputs(fuse_fixture)
        .bench_local_values(|fixture| {
            divan::black_box(rimz::sidebar::fuse::fuse(
                &fixture.snapshot,
                &fixture.events,
                None,
                fixture.now_ms,
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn fuse_owned_no_overlay(bencher: Bencher) {
    bencher
        .with_inputs(owned_fuse_fixture)
        .bench_local_values(|fixture| {
            divan::black_box(rimz::sidebar::fuse::fuse_owned(
                fixture.snapshot,
                &fixture.events,
                None,
                fixture.now_ms,
            ));
        });
}

/// One appended frame folded onto a warm cursor, at an empty and at a
/// production-sized (`HISTORY_CARRYOVER`) rotation carryover. The fold benches
/// return the fixture so divan drops its tempdir and cursor outside the timing.
#[divan::bench(args = [0, HISTORY_CARRYOVER], sample_count = 20, sample_size = 1, skip_ext_time)]
fn rollup_fold_warm(bencher: Bencher, history_carryover: usize) {
    bencher
        .with_inputs(|| fold_fixture(history_carryover, true))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("warm fold"));
            fixture
        });
}

/// A warm cursor re-served with nothing appended — the idle wakeup.
#[divan::bench(args = [0, HISTORY_CARRYOVER], sample_count = 20, sample_size = 1, skip_ext_time)]
fn rollup_fold_unchanged(bencher: Bencher, history_carryover: usize) {
    bencher
        .with_inputs(|| fold_fixture(history_carryover, false))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("unchanged fold"));
            fixture
        });
}

#[divan::bench(args = [3_300, 13_200], sample_count = 20, sample_size = 1, skip_ext_time)]
fn carryover_fold_cold(bencher: Bencher, rows: usize) {
    bencher
        .with_inputs(|| ended_carryover_fixture(rows, false))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("cold fold"));
            fixture
        });
}

#[divan::bench(args = [3_300, 13_200], sample_count = 20, sample_size = 1, skip_ext_time)]
fn carryover_fold_rebirth_warm(bencher: Bencher, rows: usize) {
    bencher
        .with_inputs(|| ended_carryover_fixture(rows, true))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                fixture
                    .cursor
                    .fold(&fixture.paths)
                    .expect("reborn delta fold"),
            );
            fixture
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_walk_cold(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture(false))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_walk_warm_no_change(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture(true))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_cold_hydrate(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture_scaled(6_000, 17, false, true))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_warm_global_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture_scaled(6_000, 17, true, true))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_cold_discovery_inclusive(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_discovery_fixture(false))
        .bench_local_values(|mut fixture| {
            let files = fixture.walker.discover_declared_spending_files(
                rimz::agents::definition_by_kind("claude").expect("Claude definition"),
                fixture.sources.clone(),
                SPENDING_NOW_SECS,
            );
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_warm_discovery_inclusive(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_discovery_fixture(true))
        .bench_local_values(|mut fixture| {
            let files = fixture.walker.discover_declared_spending_files(
                rimz::agents::definition_by_kind("claude").expect("Claude definition"),
                fixture.sources.clone(),
                SPENDING_NOW_SECS,
            );
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_live_scale_warm_discovery_only(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_discovery_fixture(true))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.walker.discover_declared_spending_files(
                rimz::agents::definition_by_kind("claude").expect("Claude definition"),
                fixture.sources,
                SPENDING_NOW_SECS,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_additional_workspace_scope(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture_scaled(6_000, 17, true, true))
        .bench_local_values(|mut fixture| {
            let root = fixture._tempdir.path().to_path_buf();
            let scope = rimz::agents::spending::SpendScope::from_roots(Some(&root), &[]);
            let spec = rimz::agents::spending::HeadlineSpec::default();
            divan::black_box(rimz::testkit::spending_scope_from_warm_walker(
                &mut fixture.walker,
                &fixture.cache_path,
                &fixture.files,
                &scope,
                SPENDING_NOW_SECS,
                &spec,
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn enrich_cached(bencher: Bencher) {
    bencher
        .with_inputs(enrich_fixture)
        .bench_local_values(|fixture| {
            divan::black_box(rimz::sidebar::enrich::enrich(
                fixture.snapshot,
                Some(&fixture.frame),
                &fixture._workspace.paths,
                &fixture.runtime,
                None,
                None,
                rimz::sidebar::enrich::FoldOpts {
                    producing: false,
                    fresh_roots: None,
                    config: None,
                    lanes: None,
                    agent_projection: Default::default(),
                },
                &rimz::diag::DiagSink::disabled(),
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn consumer_adopt_parse_cached(bencher: Bencher) {
    bencher
        .with_inputs(|| consumer_adopt_fixture(true))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                fixture
                    .reader
                    .read_adopting(&fixture.paths)
                    .expect("cached adoption"),
            );
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn consumer_adopt_changed_file(bencher: Bencher) {
    bencher
        .with_inputs(|| consumer_adopt_fixture(false))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                fixture
                    .reader
                    .read_adopting(&fixture.paths)
                    .expect("changed-file adoption"),
            );
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn kimi_changed_session_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| changed_session_fixture(rimz::testkit::changed_kimi_session_fixture))
        .bench_local_values(|fixture| divan::black_box(fixture.refresh.refresh()));
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn grok_changed_session_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| changed_session_fixture(rimz::testkit::changed_grok_session_fixture))
        .bench_local_values(|fixture| divan::black_box(fixture.refresh.refresh()));
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn droid_changed_session_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| changed_session_fixture(rimz::testkit::changed_droid_session_fixture))
        .bench_local_values(|fixture| divan::black_box(fixture.refresh.refresh()));
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn render_fixed(bencher: Bencher) {
    bencher
        .with_inputs(snapshot_fixture)
        .bench_local_values(|fixture| {
            rimz::sidebar_pane::render::render_fixed(io::sink(), &fixture.snapshot, 54, 200)
                .expect("render");
        });
}
