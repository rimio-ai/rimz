use super::*;
use crate::sidebar_pane::app::fetch::{FetchMode, FetchPhase, FetchUpdate, SnapshotSource};
use crate::sidebar_pane::app::views::Views;
use crate::testkit::host_pass::{Counter, thread_counters};
use std::cell::Cell;
use std::sync::mpsc::channel;

fn fold(sampled_at: Instant) -> Arc<WorkspaceFold> {
    Arc::new(WorkspaceFold {
        sampled_at,
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(
            super::super::fixtures::agent_snapshot(&super::super::fixtures::workspace()),
        ),
        frame: None,
    })
}

fn publish(views: &mut Views, fold: Arc<WorkspaceFold>) {
    views.publish(FetchUpdate::Fold {
        fold,
        phase: FetchPhase::Interim,
        source: SnapshotSource::Cached,
    });
}

#[test]
fn fold_freshness_excludes_tick_equality() {
    let tick = Duration::from_secs(1);
    let now = Instant::now();
    let mut views = Views::default();
    let fresh: Vec<_> = [tick - Duration::from_nanos(1), tick, tick + tick]
        .into_iter()
        .map(|age| {
            publish(&mut views, fold(now - age));
            views.fresh_fold(now, tick).is_some()
        })
        .collect();
    assert_eq!(fresh, [true, false, false]);
}

#[test]
fn fresh_fold_runs_heavy_lanes_without_another_fold_or_request() {
    let data = Mutex::new(HostData::default());
    let retained = fold(Instant::now());
    publish(
        &mut data.lock().unwrap().views.lock().unwrap(),
        retained.clone(),
    );
    let (requests, pending) = channel();
    let refreshed = Cell::new(false);
    let maintained = Cell::new(false);
    let counts = thread_counters();
    let before = counts.get(Counter::HostFold);
    let result = refresh_pass(
        &data,
        &requests,
        Duration::from_secs(60),
        |acquired| {
            refreshed.set(std::ptr::eq(acquired, retained.as_ref()));
            Ok(())
        },
        || maintained.set(true),
    );
    assert!(result.is_ok(), "refresh failed: {result:?}");
    assert!(
        refreshed.get(),
        "heavy lanes must consume the retained fold"
    );
    assert!(maintained.get(), "timers and daemon repair must run");
    assert_eq!(counts.get(Counter::HostFold) - before, 0);
    assert_eq!(pending.try_iter().count(), 0);
}

#[test]
fn stale_fold_waits_for_the_first_post_request_sample() {
    let data = Arc::new(Mutex::new(HostData::default()));
    let tick = Duration::from_secs(1);
    let old = fold(Instant::now() - tick);
    publish(&mut data.lock().unwrap().views.lock().unwrap(), old.clone());
    let (requests, pending) = channel::<FetchRequest>();
    let worker_data = data.clone();
    let worker = std::thread::spawn(move || {
        let Ok(request) = pending.recv_timeout(Duration::from_secs(2)) else {
            return None;
        };
        assert_eq!(request.mode(), FetchMode::Normal);
        let first = fold(Instant::now());
        let views = worker_data.lock().unwrap().views.clone();
        let mut views = views.lock().unwrap();
        publish(&mut views, old);
        publish(&mut views, first.clone());
        publish(&mut views, fold(Instant::now()));
        assert_eq!(pending.try_iter().count(), 0, "request only once per pass");
        Some(first)
    });
    let acquired = acquire_fold(&data, &requests, tick);
    let first = worker.join().unwrap();
    assert!(
        acquired.is_ok(),
        "stale fold was not refreshed: {:?}",
        acquired.as_ref().err()
    );
    assert!(Arc::ptr_eq(&acquired.unwrap(), &first.unwrap()));
}

#[test]
fn cold_host_uses_the_workers_first_frameless_fold() {
    let data = Arc::new(Mutex::new(HostData::default()));
    let (requests, pending) = channel();
    let worker_data = data.clone();
    let worker = std::thread::spawn(move || {
        if pending.recv_timeout(Duration::from_secs(2)).is_err() {
            return;
        }
        publish(
            &mut worker_data.lock().unwrap().views.lock().unwrap(),
            fold(Instant::now()),
        );
    });
    let acquired = acquire_fold(&data, &requests, Duration::from_secs(1));
    worker.join().unwrap();
    assert!(
        acquired.is_ok(),
        "cold host did not acquire a fold: {:?}",
        acquired.as_ref().err()
    );
    assert!(acquired.unwrap().frame.is_none());
}

#[test]
fn failure_skips_heavy_lanes_but_not_maintenance_even_if_success_follows() {
    let data = Arc::new(Mutex::new(HostData::default()));
    let (requests, pending) = channel();
    let worker_data = data.clone();
    let worker = std::thread::spawn(move || {
        if pending.recv_timeout(Duration::from_secs(2)).is_err() {
            return;
        }
        let views = worker_data.lock().unwrap().views.clone();
        let mut views = views.lock().unwrap();
        views.publish(FetchUpdate::Failed {
            error: "worker failed".into(),
        });
        publish(&mut views, fold(Instant::now()));
    });
    let refreshed = Cell::new(false);
    let maintained = Cell::new(false);
    let result = refresh_pass(
        &data,
        &requests,
        Duration::from_secs(1),
        |_| {
            refreshed.set(true);
            Ok(())
        },
        || maintained.set(true),
    );
    worker.join().unwrap();
    assert_eq!(result, Err("worker failed".into()));
    assert!(!refreshed.get(), "a failed publication skips heavy lanes");
    assert!(maintained.get(), "timers and daemon repair must still run");
}

#[test]
fn missing_publication_waits_one_tick_and_still_runs_maintenance() {
    let data = Mutex::new(HostData::default());
    let (requests, pending) = channel();
    let tick = Duration::from_millis(30);
    let refreshed = Cell::new(false);
    let maintained = Cell::new(false);
    let started = Instant::now();
    let result = refresh_pass(
        &data,
        &requests,
        tick,
        |_| {
            refreshed.set(true);
            Ok(())
        },
        || maintained.set(true),
    );
    assert!(result.is_err(), "a timeout must skip the pass");
    assert!(
        started.elapsed() >= tick,
        "giving up must wait the full tick"
    );
    assert!(!refreshed.get());
    assert!(maintained.get(), "timers and daemon repair must still run");
    assert_eq!(pending.try_iter().count(), 1);
}

#[test]
fn heavy_lane_panic_does_not_stop_maintenance_or_the_next_pass() {
    let data = Mutex::new(HostData::default());
    publish(
        &mut data.lock().unwrap().views.lock().unwrap(),
        fold(Instant::now()),
    );
    let (requests, _pending) = channel();
    let maintained = Cell::new(0);
    let failed = refresh_pass(
        &data,
        &requests,
        Duration::from_secs(60),
        |_| panic!("heavy lane panic"),
        || maintained.set(maintained.get() + 1),
    );
    assert!(
        failed
            .as_ref()
            .is_err_and(|error| error.contains("heavy lane panic")),
        "panic must be isolated: {failed:?}"
    );
    let next = refresh_pass(
        &data,
        &requests,
        Duration::from_secs(60),
        |_| Ok(()),
        || maintained.set(maintained.get() + 1),
    );
    assert!(next.is_ok(), "next pass must recover: {next:?}");
    assert_eq!(maintained.get(), 2);
}
