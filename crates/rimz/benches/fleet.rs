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
    ledger_bytes: u64,
}

impl ProduceFixture {
    fn cold(fleet: usize) -> Self {
        let (tempdir, paths, runtime) = workspace();
        rimz::testkit::fleet::seed_fleet_ledger(&paths, fleet, fleet * EVENTS_PER_AGENT)
            .expect("seed fleet");
        let ledger_bytes = std::fs::metadata(&paths.events_log)
            .expect("log metadata")
            .len();
        publish_fresh_produce_inputs(&runtime, fleet);
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
            cursor: rimz::sidebar::consumer::RollupCursor::new(),
            ledger_bytes,
        }
    }

    fn warm(fleet: usize) -> Self {
        let (tempdir, paths, runtime) = workspace();
        rimz::testkit::fleet::seed_fleet_ledger(&paths, fleet, fleet * EVENTS_PER_AGENT)
            .expect("seed fleet");
        publish_fresh_produce_inputs(&runtime, fleet);
        let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
        rimz::sidebar::produce::produce_snapshot(&mut cursor, &paths, &runtime, &produce_options())
            .expect("cold produce");

        let log_len = std::fs::metadata(&paths.events_log)
            .expect("log metadata")
            .len();
        rimz::ledger::event_log::append(
            &paths.events_log,
            &rimz::testkit::fleet::registered_lifecycle(&paths.workspace_id, 0),
        )
        .expect("append delta");
        let ledger_bytes = std::fs::metadata(&paths.events_log)
            .expect("log metadata")
            .len()
            - log_len;
        publish_fresh_produce_inputs(&runtime, fleet);
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
            cursor,
            ledger_bytes,
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
    }
}

#[divan::bench(args = [20, 50, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
fn produce_cold(bencher: Bencher, fleet: usize) {
    bencher
        .with_inputs(move || ProduceFixture::cold(fleet))
        .input_counter(|fixture| BytesCount::new(fixture.ledger_bytes))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                rimz::sidebar::produce::produce_snapshot(
                    &mut fixture.cursor,
                    &fixture.paths,
                    &fixture.runtime,
                    &produce_options(),
                )
                .expect("cold produce"),
            );
        });
}

#[divan::bench(args = [20, 50, 100], sample_count = 10, sample_size = 1, skip_ext_time)]
fn produce_warm(bencher: Bencher, fleet: usize) {
    bencher
        .with_inputs(move || ProduceFixture::warm(fleet))
        .input_counter(|fixture| BytesCount::new(fixture.ledger_bytes))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                rimz::sidebar::produce::produce_snapshot(
                    &mut fixture.cursor,
                    &fixture.paths,
                    &fixture.runtime,
                    &produce_options(),
                )
                .expect("warm produce"),
            );
        });
}
