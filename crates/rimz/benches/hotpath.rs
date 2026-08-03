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
    events: rimz::sidebar::events::EventStore,
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
    files: Vec<(&'static rimz::agents::AgentDefinition, PathBuf)>,
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
    let mut events = rimz::sidebar::events::EventStore::default();
    let pane_id = rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_0");
    let now_ms = snapshot
        .panes_produced_at_ms
        .unwrap_or_else(rimz::sidebar::timing::unix_now_ms)
        .saturating_add(1);
    events.append(
        rimz::sidebar::events::SidebarEvent::CommandChanged {
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
        .unwrap_or_else(rimz::sidebar::timing::unix_now_ms)
        .saturating_add(1);
    FuseFixture {
        _workspace,
        snapshot,
        events: rimz::sidebar::events::EventStore::default(),
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
        rimz::sidebar::timing::unix_now_ms(),
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
        let read = reader
            .read_adopting(&workspace.paths)
            .expect("warm adoption");
        assert_eq!(
            read.source,
            rimz::sidebar::consumer::ConsumerSnapshotSource::Adoption
        );
    }
    ConsumerAdoptFixture {
        paths: workspace.paths.clone(),
        reader,
        _workspace: workspace,
    }
}

fn fold_fixture() -> FoldFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    cursor.fold(&workspace.paths).expect("cold fold");
    rimz::store::event_log::append(
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
        files.push((
            rimz::agents::definition_by_kind("claude").expect("Claude definition"),
            transcript,
        ));
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
            rimz::sidebar_pane::render::render_fixed(io::sink(), &fixture.snapshot, None, 54, 200)
                .expect("render");
        });
}
