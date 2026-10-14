use divan::Bencher;
use divan::counter::BytesCount;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write as _;
use tempfile::TempDir;

#[global_allocator]
static ALLOC: divan::AllocProfiler<CountingAllocator> =
    divan::AllocProfiler::new(CountingAllocator);

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

fn record_allocation(bytes: usize) {
    let _ = ALLOCATIONS.try_with(|counts| {
        let (allocations, allocated_bytes) = counts.get();
        counts.set((allocations + 1, allocated_bytes + bytes));
    });
}

// Benchmark-only instrumentation: forward each allocation unchanged to System.
#[expect(
    unsafe_code,
    reason = "GlobalAlloc requires unsafe delegation; only the fleet bench links this wrapper"
)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let ptr = unsafe { System.realloc(ptr, layout, size) };
        if !ptr.is_null() {
            record_allocation(size);
        }
        ptr
    }
}

fn counted(run: impl FnOnce()) -> (usize, usize) {
    let before = ALLOCATIONS.get();
    run();
    let after = ALLOCATIONS.get();
    (after.0 - before.0, after.1 - before.1)
}

const EVENTS_PER_AGENT: usize = 50;

fn main() {
    divan::main();
}

struct ProduceFixture {
    _tempdir: TempDir,
    paths: rimz::StatePaths,
    runtime: rimz::RuntimePaths,
    cursor: rimz::sidebar::consumer::RollupCursor,
    producer: rimz::sidebar::produce::SnapshotProducer,
    store_bytes: u64,
}

impl ProduceFixture {
    fn cold(fleet: usize) -> Self {
        let (tempdir, paths, runtime) = workspace();
        rimz::testkit::fleet::seed_fleet_store(&paths, fleet, fleet * EVENTS_PER_AGENT)
            .expect("seed fleet");
        let store_bytes = std::fs::metadata(&paths.events_log)
            .expect("log metadata")
            .len();
        rimz::testkit::fleet::publish_fresh_produce_inputs(&runtime, fleet)
            .expect("publish produce inputs");
        let producer = rimz::sidebar::produce::SnapshotProducer::seeded(
            &runtime,
            rimz::testkit::fleet::SESSION_NAME,
        );
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
            cursor: rimz::sidebar::consumer::RollupCursor::new(),
            producer,
            store_bytes,
        }
    }

    fn warm(fleet: usize) -> Self {
        let (tempdir, paths, runtime) = workspace();
        rimz::testkit::fleet::seed_fleet_store(&paths, fleet, fleet * EVENTS_PER_AGENT)
            .expect("seed fleet");
        rimz::testkit::fleet::publish_fresh_produce_inputs(&runtime, fleet)
            .expect("publish produce inputs");
        let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
        let mut producer = rimz::sidebar::produce::SnapshotProducer::seeded(
            &runtime,
            rimz::testkit::fleet::SESSION_NAME,
        );
        rimz::sidebar::produce::produce_snapshot(
            &mut cursor,
            &paths,
            &runtime,
            &rimz::testkit::fleet::produce_options(),
            &mut producer,
        )
        .expect("cold produce");

        let log_len = std::fs::metadata(&paths.events_log)
            .expect("log metadata")
            .len();
        rimz::store::event_log::append(
            &paths.events_log,
            &rimz::testkit::fleet::registered_lifecycle(&paths.workspace_id, 0),
        )
        .expect("append delta");
        let store_bytes = std::fs::metadata(&paths.events_log)
            .expect("log metadata")
            .len()
            - log_len;
        rimz::testkit::fleet::publish_fresh_produce_inputs(&runtime, fleet)
            .expect("publish produce inputs");
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
            cursor,
            producer,
            store_bytes,
        }
    }
}

fn workspace() -> (TempDir, rimz::StatePaths, rimz::RuntimePaths) {
    let tempdir = TempDir::new().expect("tempdir");
    let state_root = tempdir.path().join("state");
    let runtime_root = tempdir.path().join("runtime");
    let workspace_id = rimz::WorkspaceId::from_project_root(tempdir.path());
    let paths = rimz::StatePaths::under(workspace_id.clone(), &state_root).expect("paths");
    paths.ensure_dirs().expect("state dirs");
    let runtime = rimz::RuntimePaths::under(workspace_id, &runtime_root).expect("runtime");
    std::fs::create_dir_all(&runtime.root).expect("runtime root");
    std::fs::create_dir_all(&runtime.shared_root).expect("shared runtime root");
    (tempdir, paths, runtime)
}

#[divan::bench(args = [20, 50, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
fn produce_cold(bencher: Bencher, fleet: usize) {
    bencher
        .with_inputs(move || ProduceFixture::cold(fleet))
        .input_counter(|fixture| BytesCount::new(fixture.store_bytes))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                rimz::sidebar::produce::produce_snapshot(
                    &mut fixture.cursor,
                    &fixture.paths,
                    &fixture.runtime,
                    &rimz::testkit::fleet::produce_options(),
                    &mut fixture.producer,
                )
                .expect("cold produce"),
            );
        });
}

mod publish {
    use super::*;
    use rimz::sidebar::enrich::{FoldOpts, WorkspaceSnapshot, enrich_workspace, project_local};
    use rimz::testkit::fleet::{ScaleRoom, seed_scale_room};

    struct Fixture {
        _tempdir: TempDir,
        paths: rimz::StatePaths,
        runtime: rimz::RuntimePaths,
        base: rimz::store::snapshot::SidebarSnapshot,
        frame: rimz::sidebar::frame::PaneFrame,
        own_panes: Vec<rimz::PaneId>,
    }

    impl Fixture {
        fn new(panes: usize) -> Self {
            Self::with_orphans(panes, 0)
        }

        fn with_orphans(panes: usize, orphan_contexts: usize) -> Self {
            let (tempdir, paths, runtime) = workspace();
            let (agents, ended_sessions) = match panes {
                24 => (51, 4_600),
                100 => (200, 5_000),
                _ => unreachable!("benchmark dimensions are fixed"),
            };
            let frame = seed_scale_room(
                &paths,
                &runtime,
                ScaleRoom {
                    agents,
                    panes,
                    ended_sessions,
                    orphan_contexts,
                },
            )
            .expect("scale room");
            let store = rimz::Store::open(paths.clone(), runtime.clone()).expect("store");
            let base = store.snapshot().expect("snapshot");
            Self {
                _tempdir: tempdir,
                paths,
                runtime,
                base,
                frame,
                own_panes: (0..panes)
                    .map(|index| {
                        rimz::PaneId::from_parts(rimz::MuxName::Zellij, format!("sidebar_{index}"))
                    })
                    .collect(),
            }
        }

        fn fold(&self) -> WorkspaceSnapshot {
            enrich_workspace(
                self.base.clone(),
                Some(&self.frame),
                &self.paths,
                &self.runtime,
                None,
                FoldOpts {
                    producing: false,
                    fresh_roots: None,
                    config: None,
                    lanes: None,
                    agent_projection: Default::default(),
                },
                &rimz::diag::DiagSink::disabled(),
            )
        }

        fn project(&self, workspace: &WorkspaceSnapshot, exclude_agent: bool) {
            let snapshots: Vec<_> = self
                .own_panes
                .iter()
                .map(|own| {
                    let excluded = if exclude_agent {
                        &self.frame.tabs[0].panes[0].pane_id
                    } else {
                        own
                    };
                    project_local(workspace.clone(), Some(&self.frame), Some(excluded))
                })
                .collect();
            divan::black_box(snapshots);
        }
    }

    #[divan::bench(args = [0, 3_000], sample_count = 10, sample_size = 1, skip_ext_time)]
    fn scale_with_orphans(bencher: Bencher, orphans: usize) {
        const MAX_PUBLISH_BYTES: usize = 11_500_000;
        let mut counts = (0, 0);
        bencher
            .with_inputs(|| {
                let fixture = Fixture::with_orphans(100, orphans);
                fixture.fold();
                fixture
            })
            .bench_local_refs(|fixture| {
                counts = counted(|| {
                    let workspace = fixture.fold();
                    fixture.project(&workspace, false);
                });
                assert!(
                    counts.1 <= MAX_PUBLISH_BYTES,
                    "200-agent/100-subscriber publication requested {} bytes; ceiling {MAX_PUBLISH_BYTES} bytes",
                    counts.1,
                );
            });
        writeln!(
            std::io::stderr(),
            "publish orphans={orphans}: allocations={} bytes={}",
            counts.0,
            counts.1
        )
        .expect("benchmark counts");
    }

    #[divan::bench(args = [24, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
    fn fold_and_project(bencher: Bencher, panes: usize) {
        bencher
            .with_inputs(|| Fixture::new(panes))
            .bench_local_refs(|fixture| {
                let workspace = fixture.fold();
                fixture.project(&workspace, false);
            });
    }

    #[divan::bench(args = [24, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
    fn exclude_agent(bencher: Bencher, panes: usize) {
        bencher
            .with_inputs(|| Fixture::new(panes))
            .bench_local_refs(|fixture| {
                let workspace = fixture.fold();
                fixture.project(&workspace, true);
            });
    }

    #[divan::bench(args = [24, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
    fn local_only(bencher: Bencher, panes: usize) {
        bencher
            .with_inputs(|| {
                let fixture = Fixture::new(panes);
                let workspace = fixture.fold();
                (fixture, workspace)
            })
            .bench_local_refs(|(fixture, workspace)| fixture.project(workspace, false));
    }

    #[divan::bench(args = [24, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
    fn local_fixed_room(bencher: Bencher, subscribers: usize) {
        bencher
            .with_inputs(|| {
                let mut fixture = Fixture::new(100);
                fixture.own_panes.truncate(subscribers);
                let workspace = fixture.fold();
                (fixture, workspace)
            })
            .bench_local_refs(|(fixture, workspace)| fixture.project(workspace, false));
    }
}

#[divan::bench(args = [0, 3_000], sample_count = 10, sample_size = 1, skip_ext_time)]
fn idle_cycle(bencher: Bencher, orphans: usize) {
    bench_idle_cycle(bencher, orphans, true);
}

#[divan::bench(args = [0, 3_000], sample_count = 10, sample_size = 1, skip_ext_time)]
fn idle_cycle_worker(bencher: Bencher, orphans: usize) {
    bench_idle_cycle(bencher, orphans, false);
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn idle_cycle_hidden_fanout(bencher: Bencher) {
    // About 3x the coalesced 23,776 requests, below the uncoalesced 116,624.
    const MAX_ALLOCATIONS: usize = 72_000;
    let mut counts = (0, 0);
    bencher
        .with_inputs(|| {
            let (tempdir, paths, runtime) = workspace();
            let frame = rimz::testkit::fleet::seed_scale_room(
                &paths,
                &runtime,
                rimz::testkit::fleet::ScaleRoom {
                    agents: 50,
                    panes: 57,
                    ended_sessions: 0,
                    orphan_contexts: 0,
                },
            )
            .expect("scale room");
            let mut panes = frame.to_pane_refs();
            let mut sidebars = Vec::new();
            for (index, pane) in panes.iter_mut().enumerate() {
                pane.view_id = Some(format!("tab_{index}"));
                let mut sidebar = pane.clone();
                sidebar.pane_id =
                    rimz::PaneId::from_parts(rimz::MuxName::Zellij, format!("sidebar_{index}"));
                sidebar.command = Some("rimz-sidebar".to_owned());
                sidebars.push(sidebar);
            }
            let viewed = panes[0].pane_id.clone();
            panes.extend(sidebars);
            let mut frame = rimz::sidebar::frame::assemble_frame(
                panes,
                frame.produced_at_ms,
                &frame.session_name,
            );
            frame.viewed_panes = vec![viewed];
            let mut cycle =
                rimz::sidebar_pane::app::IdleCycleFixture::new(paths, runtime, frame, 57);
            cycle.enable_renderer_apply();
            cycle.run_cycle();
            cycle.paint_fanout();
            (tempdir, cycle)
        })
        .bench_local_refs(|(_, cycle)| {
            counts = counted(|| cycle.run_content_cycle());
            assert!(
                counts.0 <= MAX_ALLOCATIONS,
                "50-agent/57-subscriber hidden fanout requested {} allocations; ceiling {MAX_ALLOCATIONS}",
                counts.0,
            );
        });
    writeln!(
        std::io::stderr(),
        "idle_cycle_hidden_fanout: allocations={} bytes={}",
        counts.0,
        counts.1
    )
    .expect("benchmark counts");
}

fn bench_idle_cycle(bencher: Bencher, orphans: usize, apply: bool) {
    let mut counts = (0, 0);
    let mut durations = Vec::new();
    bencher
        .with_inputs(|| {
            let (tempdir, paths, runtime) = workspace();
            let frame = rimz::testkit::fleet::seed_scale_room(
                &paths,
                &runtime,
                rimz::testkit::fleet::ScaleRoom {
                    agents: 200,
                    panes: 100,
                    ended_sessions: 5_000,
                    orphan_contexts: orphans,
                },
            )
            .expect("scale room");
            let mut cycle =
                rimz::sidebar_pane::app::IdleCycleFixture::new(paths, runtime, frame, 100);
            if apply {
                cycle.enable_renderer_apply();
            }
            cycle.run_cycle();
            (tempdir, cycle)
        })
        .bench_local_refs(|(_, cycle)| {
            let started = std::time::Instant::now();
            counts = counted(|| {
                cycle.run_cycle();
            });
            durations.push(started.elapsed());
        });
    if apply && orphans == 3_000 {
        const MAX_IDLE_CYCLE: std::time::Duration = std::time::Duration::from_millis(33);
        durations.sort_unstable();
        let median = durations[durations.len() / 2];
        assert!(
            median <= MAX_IDLE_CYCLE,
            "200-agent/100-subscriber idle cycle with 3000 orphans took {median:?} (median); ceiling {MAX_IDLE_CYCLE:?}",
        );
        writeln!(
            std::io::stderr(),
            "idle_cycle median={median:?} ceiling={MAX_IDLE_CYCLE:?}",
        )
        .expect("benchmark timing");
    }
    writeln!(
        std::io::stderr(),
        "idle_cycle apply={apply} orphans={orphans}: allocations={} bytes={}",
        counts.0,
        counts.1
    )
    .expect("benchmark counts");
}

#[divan::bench(args = [20, 50, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
fn produce_warm(bencher: Bencher, fleet: usize) {
    bencher
        .with_inputs(move || ProduceFixture::warm(fleet))
        .input_counter(|fixture| BytesCount::new(fixture.store_bytes))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                rimz::sidebar::produce::produce_snapshot(
                    &mut fixture.cursor,
                    &fixture.paths,
                    &fixture.runtime,
                    &rimz::testkit::fleet::produce_options(),
                    &mut fixture.producer,
                )
                .expect("warm produce"),
            );
        });
}
