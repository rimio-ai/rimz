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
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
            cursor: rimz::sidebar::consumer::RollupCursor::new(),
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
        rimz::sidebar::produce::produce_snapshot(
            &mut cursor,
            &paths,
            &runtime,
            &rimz::testkit::fleet::produce_options(),
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
                )
                .expect("cold produce"),
            );
        });
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
                )
                .expect("warm produce"),
            );
        });
}
