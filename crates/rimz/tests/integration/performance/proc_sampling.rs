use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use rimz::sidebar::consumer::RollupCursor;
use rimz::sidebar::produce::{SnapshotProducer, produce_snapshot};
use rimz::testkit::fleet::{produce_options, synthetic_panes};
use rimz::testkit::host_pass::{Counter::*, thread_counters};

use crate::common::Harness;

struct SleepingProcess(Child);

impl SleepingProcess {
    fn new() -> Self {
        Self(
            Command::new("sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for SleepingProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn sample(pid: u32, agent_bound: bool) -> [u64; 4] {
    let h = Harness::new();
    let mut panes = synthetic_panes(1);
    panes[0].pane_pid = Some(pid);
    let mut opts = produce_options();
    if agent_bound {
        opts.agent_panes.insert(panes[0].pane_id.clone());
    }
    h.publish_fresh_produce_inputs(&opts.session_name, panes);
    let mut producer = SnapshotProducer::seeded(&h.runtime_paths, &opts.session_name);
    let counts = thread_counters();
    counts.reset();
    produce_snapshot(
        &mut RollupCursor::new(),
        h.store.paths(),
        &h.runtime_paths,
        &opts,
        &mut producer,
    )
    .unwrap();
    [ProcStat, ProcChildren, ProcTaskDir, ProcIo].map(|kind| counts.get(kind))
}

#[test]
fn agent_metrics_cost_one_root_stat_with_one_or_sixty_four_threads() {
    let process = SleepingProcess::new();
    assert_eq!(sample(process.0.id(), true), [1, 0, 0, 0]);

    std::thread::scope(|scope| {
        let existing = std::fs::read_dir("/proc/self/task").unwrap().count();
        let extra = 64 - existing;
        let (ready, ready_rx) = mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..extra {
            let (release, released) = mpsc::channel::<()>();
            releases.push(release);
            let ready = ready.clone();
            scope.spawn(move || {
                ready.send(()).unwrap();
                let _ = released.recv();
            });
        }
        for _ in 0..extra {
            ready_rx.recv().unwrap();
        }
        assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 64);
        assert_eq!(sample(std::process::id(), true), [1, 0, 0, 0]);
        drop(releases);
    });
}

#[test]
fn process_metrics_keep_io_and_single_thread_children_without_task_scan() {
    let process = SleepingProcess::new();
    assert_eq!(sample(process.0.id(), false), [1, 1, 0, 1]);
}

#[test]
fn single_thread_children_skips_the_task_directory() {
    let process = SleepingProcess::new();
    let counts = thread_counters();
    counts.reset();
    assert!(rimz::proc::children(process.0.id()).is_empty());
    assert_eq!(counts.get(ProcChildren), 1);
    assert_eq!(counts.get(ProcTaskDir), 0);
    assert_eq!(counts.get(ProcStat), 1);
}
