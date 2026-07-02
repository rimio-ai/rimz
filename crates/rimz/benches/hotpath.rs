use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;

use divan::Bencher;
use tempfile::TempDir;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
const SPENDING_FILES: usize = 4;
const SPENDING_ENTRIES_PER_FILE: usize = 5_000;
const SPENDING_NOW_SECS: u64 = 1_780_394_400;

fn main() {
    divan::main();
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
        rimz::testkit::fleet::seed_fleet_ledger(&self.paths, fleet, history_events)
            .expect("seed fleet");
    }

    fn publish_inputs(&self, fleet: usize) {
        rimz::testkit::fleet::publish_fresh_produce_inputs(&self.runtime, fleet)
            .expect("publish produce inputs");
    }
}

struct SnapshotFixture {
    _workspace: BenchWorkspace,
    snapshot: rimz::SidebarSnapshot,
}

struct FuseFixture {
    _workspace: BenchWorkspace,
    snapshot: rimz::SidebarSnapshot,
    events: rimz::sidebar::events::EventStore,
    now_ms: u64,
}

struct EnrichFixture {
    _workspace: BenchWorkspace,
    runtime: rimz::RuntimePaths,
    snapshot: rimz::SidebarSnapshot,
    frame: rimz::sidebar::frame::PaneFrame,
}

struct FoldFixture {
    _workspace: BenchWorkspace,
    paths: rimz::StatePaths,
    cursor: rimz::sidebar::consumer::RollupCursor,
}

struct SpendingFixture {
    _tempdir: TempDir,
    cache_path: PathBuf,
    files: Vec<(&'static dyn rimz::agents::AgentAdapter, PathBuf)>,
    prices: rimz::agents::PriceBook,
    walker: rimz::agents::spending::SpendingWalker,
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
    let mut events = rimz::sidebar::events::EventStore::default();
    let pane_id = rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_0");
    let now_ms = snapshot
        .panes_produced_at_ms
        .unwrap_or_else(rimz::sidebar::timing::unix_now_ms)
        .saturating_add(1);
    events.append(
        rimz::schema::sidebar_event::SidebarEvent::CommandChanged {
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

fn enrich_fixture() -> EnrichFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    workspace.publish_inputs(FLEET);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    let snapshot =
        rimz::sidebar::consumer::rollup_snapshot(&workspace.paths, &mut cursor).expect("rollup");
    let frame = rimz::sidebar::frame::assemble_frame(
        rimz::testkit::fleet::synthetic_panes(FLEET),
        rimz::sidebar::timing::unix_now_ms(),
        rimz::testkit::fleet::SESSION_NAME,
    );
    EnrichFixture {
        runtime: workspace.runtime.clone(),
        _workspace: workspace,
        snapshot,
        frame,
    }
}

fn fold_fixture() -> FoldFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    cursor.fold(&workspace.paths).expect("cold fold");
    rimz::ledger::event_log::append(
        &workspace.paths.events_log,
        &rimz::testkit::fleet::registered_lifecycle(&workspace.paths.workspace_id, 0),
    )
    .expect("append delta");
    FoldFixture {
        paths: workspace.paths.clone(),
        cursor,
        _workspace: workspace,
    }
}

fn spending_fixture(warm: bool) -> SpendingFixture {
    let tempdir = TempDir::new().expect("tempdir");
    let cache_path = tempdir.path().join("spending.json");
    let mut cache = rimz::agents::spending::read_spending_cache(&cache_path);
    cache.files = HashMap::new();
    let mut files = Vec::new();
    for file_index in 0..SPENDING_FILES {
        let transcript = tempdir.path().join(format!("cached-{file_index}.jsonl"));
        std::fs::write(&transcript, b"").expect("transcript");
        let metadata = std::fs::metadata(&transcript).expect("metadata");
        let mtime_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let entries = (0..SPENDING_ENTRIES_PER_FILE)
            .map(|offset| {
                let index = file_index * SPENDING_ENTRIES_PER_FILE + offset;
                rimz::agents::spending::CachedEntry {
                    ts_secs: SPENDING_NOW_SECS - 86_400
                        + u64::try_from(offset % 3_600).expect("offset fits u64"),
                    cost_usd: 0.001,
                    input: 1200,
                    output: 80,
                    cache_write: 0,
                    cache_read: 800,
                    message_id: Some(format!("msg-{index}")),
                    request_id: Some(format!("req-{index}")),
                    thread_id: Some(format!("thread-{index}")),
                    is_sidechain: false,
                    model: Some("claude-opus-4-8".to_owned()),
                    rolled: false,
                }
            })
            .collect();
        cache.files.insert(
            transcript.to_string_lossy().into_owned(),
            rimz::agents::spending::FileCacheEntry {
                mtime_secs,
                len: metadata.len(),
                cursor: rimz::agents::spending::SpendCursor::default(),
                origin_path: None,
                entries,
                unknown_models: BTreeMap::new(),
            },
        );
        files.push((
            &rimz::agents::ClaudeAdapter as &'static dyn rimz::agents::AgentAdapter,
            transcript,
        ));
    }
    rimz::agents::spending::write_spending_cache(&cache_path, &cache);
    let prices = rimz::agents::PriceBook::default();
    let mut walker = rimz::agents::spending::SpendingWalker::new();
    if warm {
        let _ = walker.walk(
            &cache_path,
            &files,
            &prices,
            SPENDING_NOW_SECS,
            &Default::default(),
            None,
            &rimz::agents::spending::HeadlineSpec::default(),
            &mut rimz::agents::spending::SilentWalk,
        );
    }
    SpendingFixture {
        _tempdir: tempdir,
        cache_path,
        files,
        prices,
        walker,
    }
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn fuse(bencher: Bencher) {
    bencher
        .with_inputs(fuse_fixture)
        .bench_local_values(|fixture| {
            divan::black_box(rimz::sidebar::fuse::fuse(
                &fixture.snapshot,
                &fixture.events,
                fixture.now_ms,
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn rollup_fold_warm(bencher: Bencher) {
    bencher
        .with_inputs(fold_fixture)
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("warm fold"));
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_walk_cold(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture(false))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &fixture.files,
                &fixture.prices,
                SPENDING_NOW_SECS,
                &Default::default(),
                None,
                &rimz::agents::spending::HeadlineSpec::default(),
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_walk_warm_no_change(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture(true))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &fixture.files,
                &fixture.prices,
                SPENDING_NOW_SECS,
                &Default::default(),
                None,
                &rimz::agents::spending::HeadlineSpec::default(),
                &mut rimz::agents::spending::SilentWalk,
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
                Some(fixture.frame),
                &fixture.runtime,
                None,
                None,
                rimz::sidebar::enrich::FoldOpts {
                    producing: false,
                    fresh_roots: None,
                    config: None,
                    lanes: None,
                },
                &rimz::diag::DiagSink::disabled(),
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn render_fixed(bencher: Bencher) {
    bencher
        .with_inputs(snapshot_fixture)
        .bench_local_values(|fixture| {
            rimz::sidebar_pane::render::render_fixed(io::sink(), &fixture.snapshot, None, 54, 200)
                .expect("render");
        });
}
