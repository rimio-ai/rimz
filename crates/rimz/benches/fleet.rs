use divan::Bencher;
use divan::counter::BytesCount;
use tempfile::TempDir;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

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
