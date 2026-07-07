//! Cross-process single-flight election.
//!
//! Several short-lived processes (one sidebar per tab) periodically refresh the
//! *same* external data — the store rollup + `list-panes`, and the
//! per-worktree git diff-stats. Left alone, each process refetches every tick,
//! multiplying subprocess forks and IPC against shared servers (the Zellij
//! server, `git`). This helper elects ONE producer per refresh window so the
//! rest read its result back instead of stampeding.
//!
//! It owns only the lock-and-poll dance — open an advisory lock, `try_lock` it,
//! and either win (produce + write the shared cache) or lose (poll briefly for
//! the winner's write, then fall back to producing locally without caching).
//! The *freshness predicate*, the *production*, and the *cache write* belong to
//! each call site, which differ too much to share (a single `(session)`-keyed
//! value with carry-forward vs a multi-key, per-entry-TTL map). Only the
//! election is common, so only the election lives here.
//!
//! Pure `std`: it imports no store-writer module, so it is safe to call
//! from any subtree (including the sidebar's read-only import graph) and is
//! unit-testable off the hot files.

use std::path::Path;
use std::time::Duration;

/// Outcome of contending for the single-flight lock.
pub enum Coalesced<T> {
    /// A peer already produced a value this caller can use — read back from the
    /// shared cache, either on the post-win re-check or while polling. Use it
    /// directly; do not produce.
    Shared(T),
    /// This caller won the election: it is the sole producer. Produce the value
    /// and write the shared cache, then drop the held guard to release the
    /// lock. The guard keeps the lock exclusive until then.
    Produce(ProducerGuard),
    /// No producer's result is available — the lock could not be opened, or a
    /// peer held it but never published in time. Produce the value locally but
    /// do NOT write the cache: a wedged producer must not strand this caller,
    /// and a late local write must not clobber the producer's fresher result.
    ProduceLocal,
}

/// Holds the exclusive single-flight lock for the elected producer. Releases on
/// drop (the flock also auto-releases when the fd closes or the process exits),
/// so the producer keeps it alive across its produce-and-write.
pub struct ProducerGuard {
    file: std::fs::File,
}

impl Drop for ProducerGuard {
    fn drop(&mut self) {
        // Best-effort: a failed unlock only defers release to fd close.
        let _ = self.file.unlock();
    }
}

impl std::fmt::Debug for ProducerGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProducerGuard").finish_non_exhaustive()
    }
}

/// Elect one producer for a refresh window via the advisory lock at
/// `lock_path`. `fresh` reads back a peer-produced value when one is available
/// (returning `None` when nothing usable is cached yet); it is consulted once
/// on a win (a peer may have published between the caller's miss and the lock)
/// and on each poll step while a peer holds the lock.
///
/// The caller checks its own fast path *before* calling this — by the time we
/// contend, the caller has already missed a fresh read.
pub fn coalesce<T>(
    lock_path: &Path,
    wait_step: Duration,
    wait_steps: u32,
    fresh: impl Fn() -> Option<T>,
) -> Coalesced<T> {
    let Some(file) = open_lock(lock_path) else {
        // Nowhere to coordinate (e.g. the runtime dir is missing on a bare CLI
        // call): just produce, uncached.
        return Coalesced::ProduceLocal;
    };
    match file.try_lock() {
        // We are the single producer. A peer may have published between our
        // miss and acquiring the lock, so re-check before doing the work.
        Ok(()) => match fresh() {
            Some(value) => Coalesced::Shared(value),
            None => Coalesced::Produce(ProducerGuard { file }),
        },
        // A peer is producing: poll briefly for its write, then fall back to an
        // uncached local produce rather than block on a wedged producer.
        Err(_) => {
            for _ in 0..wait_steps {
                std::thread::sleep(wait_step);
                if let Some(value) = fresh() {
                    return Coalesced::Shared(value);
                }
            }
            Coalesced::ProduceLocal
        }
    }
}

/// Open (creating if needed) the advisory lock file. `None` means the caller
/// cannot coordinate — e.g. the parent runtime dir does not exist — and should
/// produce directly.
fn open_lock(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const STEP: Duration = Duration::from_millis(5);
    const STEPS: u32 = 3;

    #[test]
    fn elects_one_producer_then_frees_on_drop() {
        // The election picks exactly one producer; a second contender while the
        // lock is held cannot also produce, and the lock frees once the
        // producer drops its guard.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("x.lock");

        let guard = match coalesce::<()>(&lock, STEP, STEPS, || None) {
            Coalesced::Produce(guard) => guard,
            _ => panic!("the first caller must win the election"),
        };

        // A second caller, with nothing published, polls then falls back to an
        // uncached local produce — never a second producer.
        let second = coalesce::<()>(&lock, STEP, STEPS, || None);
        assert!(
            matches!(second, Coalesced::ProduceLocal),
            "a contender must not double-produce while the lock is held",
        );

        drop(guard);
        let third = coalesce::<()>(&lock, STEP, STEPS, || None);
        assert!(
            matches!(third, Coalesced::Produce(_)),
            "the lock frees once the producer drops its guard",
        );
    }

    #[test]
    fn loser_uses_a_peers_published_value() {
        // While a producer holds the lock, a contender whose fresh-read returns
        // a value mid-poll takes that shared value instead of producing.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("x.lock");
        let _held = match coalesce::<u32>(&lock, STEP, STEPS, || None) {
            Coalesced::Produce(guard) => guard,
            _ => panic!("the first caller must win the election"),
        };

        let polls = AtomicU32::new(0);
        let outcome = coalesce(&lock, STEP, STEPS, || {
            // Publish on the second poll to exercise the wait loop, not just
            // the first read.
            (polls.fetch_add(1, Ordering::SeqCst) >= 1).then_some(42u32)
        });
        assert!(
            matches!(outcome, Coalesced::Shared(42)),
            "a contender uses the peer's published value rather than producing",
        );
    }

    #[test]
    fn lock_open_failure_produces_locally() {
        // A lock path under a non-existent directory cannot be opened, so the
        // caller produces directly rather than coordinating.
        let missing = Path::new("/nonexistent-rimz-single-flight-dir/x.lock");
        let outcome = coalesce::<()>(missing, STEP, STEPS, || None);
        assert!(
            matches!(outcome, Coalesced::ProduceLocal),
            "an unopenable lock falls back to a local, uncached produce",
        );
    }
}
