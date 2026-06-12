use super::*;

fn stat(
    state: char,
    cpu_ticks: u64,
    child_cpu_ticks: u64,
    rss_kb: u64,
    start_ticks: u64,
) -> crate::proc::StatMetrics {
    crate::proc::StatMetrics {
        state,
        cpu_ticks,
        child_cpu_ticks,
        rss_kb,
        start_ticks,
    }
}

#[test]
fn pane_tree_sample_aggregates_root_children_and_grandchildren() {
    let children = HashMap::from([(10, vec![20, 30]), (20, vec![40])]);
    let stats = HashMap::from([
        (10, stat('S', 5, 100, 1_000, 100)),
        (20, stat('R', 7, 3, 2_000, 200)),
        (30, stat('S', 11, 0, 3_000, 300)),
        (40, stat('S', 13, 0, 4_000, 400)),
    ]);
    let io = HashMap::from([(10, 100), (20, 200), (30, 300), (40, 400)]);

    let sample = sample_pane_tree(
        10,
        &children,
        true,
        &|pid| stats.get(&pid).copied(),
        &|pid| io.get(&pid).copied(),
        &|pid| panic!("walk-backed sample must not read /proc children for {pid}"),
    )
    .expect("root stat exists");

    assert_eq!(sample.direct_children, vec![20, 30]);
    assert_eq!(sample.process_count, 4);
    assert_eq!(sample.cpu_ticks, 139);
    assert_eq!(sample.rss_kb, 10_000);
    assert_eq!(sample.io_bytes, Some(1_000));
    assert_eq!(
        sample
            .state_samples
            .iter()
            .map(|sample| sample.pid)
            .collect::<Vec<_>>(),
        vec![10, 30, 20, 40]
    );
}

#[test]
fn pane_tree_sample_walk_free_recurses_through_proc_children() {
    let proc_children = HashMap::from([(10, vec![20, 30]), (20, vec![40])]);
    let stats = HashMap::from([
        (10, stat('S', 5, 100, 1_000, 100)),
        (20, stat('R', 7, 3, 2_000, 200)),
        (30, stat('S', 11, 0, 3_000, 300)),
        (40, stat('S', 13, 0, 4_000, 400)),
    ]);
    let io = HashMap::from([(10, 100), (20, 200), (30, 300), (40, 400)]);

    let sample = sample_pane_tree(
        10,
        &HashMap::new(),
        false,
        &|pid| stats.get(&pid).copied(),
        &|pid| io.get(&pid).copied(),
        &|pid| proc_children.get(&pid).cloned().unwrap_or_default(),
    )
    .expect("root stat exists");

    assert_eq!(sample.direct_children, vec![20, 30]);
    assert_eq!(sample.process_count, 4);
    assert_eq!(sample.cpu_ticks, 139);
    assert_eq!(sample.rss_kb, 10_000);
    assert_eq!(sample.io_bytes, Some(1_000));
    assert_eq!(
        sample
            .state_samples
            .iter()
            .map(|sample| sample.pid)
            .collect::<Vec<_>>(),
        vec![10, 30, 20, 40]
    );
}

#[test]
fn pane_tree_rates_on_stable_root_when_children_churn() {
    let mut frame = frame_from_panes(vec![pane(
        "terminal_1",
        Some("cargo xtask install"),
        Some("/repo"),
    )]);
    let pane = frame.pane_states_mut().next().unwrap();
    pane.current.pid = Some(10);
    let mut prior = fresh_entry(10, 100, "cargo xtask install", 1_000);
    prior.cpu_ticks = 1_000;
    prior.io_bytes = 500;
    prior.tree_process_count = 2;
    let sample = PaneTreeSample {
        direct_children: vec![21],
        process_count: 2,
        cpu_ticks: 1_300,
        io_bytes: Some(800),
        rss_kb: 1_024,
        root_start_ticks: 100,
        state_samples: vec![ProcessStateSample {
            pid: 10,
            start_ticks: 100,
            state: 'S',
        }],
    };

    let (cpu_pct, io_bps) = rate_metrics(Some(&prior), pane, &sample, 100.0, 2_000);

    assert_eq!(cpu_pct, Some(300));
    assert_eq!(io_bps, Some(300));
}

#[test]
fn pane_tree_io_rate_waits_for_a_complete_prior_baseline() {
    let mut frame = frame_from_panes(vec![pane("terminal_1", Some("cargo build"), Some("/repo"))]);
    let pane = frame.pane_states_mut().next().unwrap();
    pane.current.pid = Some(10);
    let mut prior = fresh_entry(10, 100, "cargo build", 1_000);
    prior.cpu_ticks = 1_000;
    prior.io_bytes = 0;
    prior.io_bytes_valid = false;
    let sample = PaneTreeSample {
        direct_children: vec![20],
        process_count: 2,
        cpu_ticks: 1_100,
        io_bytes: Some(10_000),
        rss_kb: 1_024,
        root_start_ticks: 100,
        state_samples: vec![ProcessStateSample {
            pid: 10,
            start_ticks: 100,
            state: 'S',
        }],
    };

    let (cpu_pct, io_bps) = rate_metrics(Some(&prior), pane, &sample, 100.0, 2_000);

    assert_eq!(cpu_pct, Some(100));
    assert_eq!(io_bps, None);
}

#[test]
fn tree_stuck_detection_tracks_pid_start_identity() {
    let prior = [ProcessStateSample {
        pid: 20,
        start_ticks: 200,
        state: 'D',
    }];
    let still_d = [ProcessStateSample {
        pid: 20,
        start_ticks: 200,
        state: 'D',
    }];
    let reused_pid = [ProcessStateSample {
        pid: 20,
        start_ticks: 201,
        state: 'D',
    }];
    let zombie = [ProcessStateSample {
        pid: 30,
        start_ticks: 300,
        state: 'Z',
    }];

    assert_eq!(
        process_state_from_tree(&still_d, &prior),
        Some(ProcessState::Stuck)
    );
    assert_eq!(process_state_from_tree(&reused_pid, &prior), None);
    assert_eq!(
        process_state_from_tree(&zombie, &[]),
        Some(ProcessState::Stuck)
    );
}
