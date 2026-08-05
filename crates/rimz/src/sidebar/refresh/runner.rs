//! Shared mechanics for independent refresh-lane work.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::de::DeserializeOwned;

pub(super) fn read_json_cache<T: Default + DeserializeOwned>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn bounded_map<T: Sync, R: Send>(
    work_lane: crate::lane::WorkLane,
    max_workers: usize,
    items: &[T],
    map: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    if items.is_empty() {
        return Vec::new();
    }
    let next = AtomicUsize::new(0);
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        let workers = max_workers.max(1).min(items.len());
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    crate::lane::set(work_lane);
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        let result = map(item);
                        results
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)[index] =
                            Some(result);
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = handle.join();
        }
    });
    results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect()
}
