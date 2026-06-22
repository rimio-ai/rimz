use std::io;

use divan::Bencher;
use tempfile::TempDir;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;

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
        publish_fresh_produce_inputs(&self.runtime, fleet);
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

fn publish_fresh_produce_inputs(runtime: &rimz::RuntimePaths, fleet: usize) {
    let now_ms = rimz::sidebar::cache::unix_now_ms();
    let frame = rimz::sidebar::frame::assemble_frame(
        rimz::testkit::fleet::synthetic_panes(fleet),
        now_ms,
        rimz::testkit::fleet::SESSION_NAME,
    );
    std::fs::write(
        runtime.root.join("snapshot.json"),
        serde_json::to_vec(&frame).expect("serialize pane frame"),
    )
    .expect("publish pane frame");
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        now_ms,
        &rimz::agents::spending::Spending::default(),
    );
    let accounts = rimz::sidebar::cache::AccountsCache {
        refreshed_at_ms: now_ms,
        accounts: Default::default(),
        ok: false,
    };
    std::fs::write(
        runtime.shared_accounts_path(),
        serde_json::to_vec(&accounts).expect("serialize accounts"),
    )
    .expect("publish accounts");
}

fn produce_options() -> rimz::sidebar::produce::ProduceOptions {
    rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: rimz::testkit::fleet::SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: None,
        heavy_lanes: rimz::sidebar::produce::HeavyLaneMode::Refresh,
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
        &produce_options(),
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
        .unwrap_or_else(rimz::sidebar::cache::unix_now_ms)
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
        rimz::sidebar::cache::unix_now_ms(),
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
                rimz::sidebar::enrich::EnrichMode::Cached,
                None,
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
