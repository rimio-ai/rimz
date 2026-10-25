//! Wrapper-observed hook-drainer gaps and sidebar-readable runtime leases.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::disk::lease::{self, LeaseAttempt, LeaseState};
use crate::disk::paths::RuntimePaths;
use crate::ids::AgentKind;
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
const THRESHOLD: Duration = Duration::from_secs(10);
const RECOVERY_HOLD: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OutageRecord {
    v: u32,
    pub(crate) agent_kind: AgentKind,
    pub(crate) agent_name: Option<String>,
    pub(crate) since_ms: u64,
}

pub struct Tracker {
    dir: PathBuf,
    kind: AgentKind,
    name: Option<String>,
    gaps: GapTracker,
    threshold: Duration,
    lease: Option<(Instant, lease::Lease)>,
}

impl Tracker {
    pub fn new(runtime: &RuntimePaths, kind: AgentKind, name: Option<String>) -> Self {
        Self {
            dir: runtime.hook_drainer_outages_dir(),
            kind,
            name,
            gaps: GapTracker::default(),
            threshold: threshold(),
            lease: None,
        }
    }

    pub fn observe(
        &mut self,
        started: Instant,
        finished: Instant,
        outcome: Result<LeaseRefresh, String>,
    ) -> Option<Transition> {
        let transition = self
            .gaps
            .advance(started, finished, outcome, self.threshold);
        let opened = self.gaps.outage_opened();
        if self
            .lease
            .as_ref()
            .is_some_and(|(held_opened, _)| Some(*held_opened) != opened)
        {
            drop(self.lease.take());
            let _ = lease::remove_if_free(&self.lease_path());
        }
        let Some(opened) = opened else {
            return transition;
        };
        if self.lease.is_some() {
            return transition;
        }
        if std::fs::create_dir_all(&self.dir).is_err() {
            return transition;
        }
        if let Ok(LeaseAttempt::Acquired(lease)) = lease::claim(&self.lease_path(), |file| {
            let record = OutageRecord {
                v: VERSION,
                agent_kind: self.kind.clone(),
                agent_name: self.name.clone(),
                since_ms: since_ms(opened),
            };
            serde_json::to_writer(file, &record).map_err(std::io::Error::other)
        }) {
            self.lease = Some((opened, lease));
        }
        transition
    }

    fn lease_path(&self) -> PathBuf {
        self.dir.join(std::process::id().to_string())
    }
}

pub(crate) fn read(runtime: &RuntimePaths) -> Vec<OutageRecord> {
    let Ok(entries) = std::fs::read_dir(runtime.hook_drainer_outages_dir()) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let Ok(LeaseState::Held(body)) = lease::probe(&entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<OutageRecord>(&body) else {
            continue;
        };
        if record.v == VERSION {
            records.push(record);
        }
    }
    records.sort_by(|a, b| {
        a.since_ms
            .cmp(&b.since_ms)
            .then_with(|| a.agent_kind.as_str().cmp(b.agent_kind.as_str()))
            .then_with(|| a.agent_name.cmp(&b.agent_name))
    });
    records
}

#[cfg(feature = "testkit")]
pub mod testkit {
    pub fn read(runtime: &crate::disk::paths::RuntimePaths) -> Vec<serde_json::Value> {
        super::read(runtime)
            .into_iter()
            .map(|record| serde_json::to_value(record).unwrap())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseRefresh {
    Healthy,
    Reconnected,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Transition {
    Reconnected {
        opened: Instant,
        gap_ms: u64,
        error: Option<String>,
    },
    Unreachable {
        opened: Instant,
        error: Option<String>,
        recovered_after_ms: Option<u64>,
    },
}

impl Transition {
    pub fn since_ms(&self) -> u64 {
        let opened = match self {
            Self::Reconnected { opened, .. } | Self::Unreachable { opened, .. } => *opened,
        };
        since_ms(opened)
    }
}

#[derive(Default)]
struct GapTracker {
    gap: Option<Gap>,
    recovered: Option<RecoveryHold>,
}

struct RecoveryHold {
    opened: Instant,
    until: Instant,
}

struct Gap {
    opened: Instant,
    error: Option<String>,
    phase: Phase,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Waiting,
    Unreachable,
}

impl GapTracker {
    fn outage_opened(&self) -> Option<Instant> {
        if let Some(gap) = &self.gap
            && gap.phase == Phase::Unreachable
        {
            return Some(gap.opened);
        }
        self.recovered.as_ref().map(|hold| hold.opened)
    }

    fn advance(
        &mut self,
        started: Instant,
        finished: Instant,
        outcome: Result<LeaseRefresh, String>,
        threshold: Duration,
    ) -> Option<Transition> {
        if self
            .recovered
            .as_ref()
            .is_some_and(|hold| finished >= hold.until)
        {
            self.recovered = None;
        }
        match outcome {
            Ok(LeaseRefresh::Healthy) => None,
            Ok(LeaseRefresh::Reconnected) => {
                let gap = self.gap.take().unwrap_or(Gap {
                    opened: started,
                    error: None,
                    phase: Phase::Waiting,
                });
                let elapsed = finished.duration_since(gap.opened);
                if gap.phase == Phase::Unreachable || elapsed >= threshold {
                    self.recovered = Some(RecoveryHold {
                        opened: gap.opened,
                        until: finished + RECOVERY_HOLD,
                    });
                    Some(Transition::Unreachable {
                        opened: gap.opened,
                        error: gap.error,
                        recovered_after_ms: Some(millis(elapsed)),
                    })
                } else {
                    Some(Transition::Reconnected {
                        opened: gap.opened,
                        gap_ms: millis(elapsed),
                        error: gap.error,
                    })
                }
            }
            Err(error) => {
                let gap = self.gap.get_or_insert(Gap {
                    opened: started,
                    error: None,
                    phase: Phase::Waiting,
                });
                gap.error = Some(error);
                if gap.phase == Phase::Unreachable
                    || finished.duration_since(gap.opened) < threshold
                {
                    return None;
                }
                gap.phase = Phase::Unreachable;
                self.recovered = None;
                Some(Transition::Unreachable {
                    opened: gap.opened,
                    error: gap.error.clone(),
                    recovered_after_ms: None,
                })
            }
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn since_ms(opened: Instant) -> u64 {
    u64::try_from(jiff::Timestamp::now().as_millisecond())
        .unwrap_or(0)
        .saturating_sub(millis(opened.elapsed()))
}

fn threshold() -> Duration {
    #[cfg(feature = "testkit")]
    if let Some(ms) = std::env::var("RIMZ_TEST_HOOK_DRAINER_OUTAGE_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> (tempfile::TempDir, RuntimePaths, Tracker) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let mut tracker = Tracker::new(&runtime, AgentKind::new_unchecked("claude"), None);
        tracker.threshold = THRESHOLD;
        (dir, runtime, tracker)
    }

    #[test]
    fn crossed_outage_stays_held_until_the_post_attempt_recovery_floor() {
        let (_dir, runtime, mut tracker) = tracker();
        let opened = Instant::now() - Duration::from_secs(30);
        tracker.observe(opened, opened + THRESHOLD, Err("lost".into()));
        let record = read(&runtime).pop().unwrap();
        let recovered = opened + Duration::from_secs(11);
        assert!(matches!(
            tracker.observe(
                recovered - Duration::from_millis(200),
                recovered,
                Ok(LeaseRefresh::Reconnected)
            ),
            Some(Transition::Unreachable {
                recovered_after_ms: Some(11_000),
                ..
            })
        ));
        assert_eq!(read(&runtime), vec![record.clone()]);
        for ms in [1, 2, 3, 2999] {
            let now = recovered + Duration::from_millis(ms);
            assert_eq!(tracker.observe(now, now, Ok(LeaseRefresh::Healthy)), None);
            assert_eq!(read(&runtime), vec![record.clone()]);
        }
        let due = recovered + Duration::from_secs(3);
        assert_eq!(
            tracker.observe(
                due - Duration::from_millis(100),
                due,
                Ok(LeaseRefresh::Healthy)
            ),
            None
        );
        assert!(read(&runtime).is_empty());
        assert!(!tracker.lease_path().exists());
    }

    #[test]
    fn reconnecting_at_the_threshold_claims_a_recovery_hold() {
        let (_dir, runtime, mut tracker) = tracker();
        let opened = Instant::now() - Duration::from_secs(30);
        assert_eq!(
            tracker.observe(
                opened,
                opened + Duration::from_millis(200),
                Err("lost".into())
            ),
            None
        );
        let recovered = opened + THRESHOLD;
        assert!(matches!(
            tracker.observe(
                recovered - Duration::from_millis(100),
                recovered,
                Ok(LeaseRefresh::Reconnected)
            ),
            Some(Transition::Unreachable {
                recovered_after_ms: Some(10_000),
                ..
            })
        ));
        assert_eq!(read(&runtime).len(), 1);
        let due = recovered + Duration::from_secs(3);
        let before = due - Duration::from_millis(1);
        assert_eq!(
            tracker.observe(before, before, Ok(LeaseRefresh::Healthy)),
            None
        );
        assert_eq!(read(&runtime).len(), 1);
        assert_eq!(
            tracker.observe(
                due - Duration::from_millis(200),
                due,
                Err("next gap".into())
            ),
            None
        );
        assert!(read(&runtime).is_empty());
    }

    #[test]
    fn dropping_a_recovery_hold_frees_its_lease() {
        let (_dir, runtime, mut tracker) = tracker();
        let opened = Instant::now() - Duration::from_secs(30);
        tracker.observe(opened, opened + THRESHOLD, Err("lost".into()));
        let recovered = opened + Duration::from_secs(11);
        tracker.observe(recovered, recovered, Ok(LeaseRefresh::Reconnected));
        assert_eq!(
            read(&runtime).len(),
            1,
            "recovery must retain the lease before drop"
        );
        drop(tracker);
        assert!(read(&runtime).is_empty());
    }

    #[test]
    fn recrossing_during_a_recovery_hold_replaces_the_outage_start() {
        let (_dir, runtime, mut tracker) = tracker();
        tracker.threshold = Duration::from_millis(100);
        let opened = Instant::now() - Duration::from_secs(30);
        tracker.observe(opened, opened + tracker.threshold, Err("lost".into()));
        let previous = read(&runtime).pop().unwrap();
        let recovered = opened + Duration::from_millis(200);
        tracker.observe(recovered, recovered, Ok(LeaseRefresh::Reconnected));
        assert_eq!(read(&runtime), vec![previous.clone()]);
        let next = opened + Duration::from_secs(1);
        assert!(matches!(
            tracker.observe(next, next + tracker.threshold, Err("lost again".into())),
            Some(Transition::Unreachable {
                recovered_after_ms: None,
                ..
            })
        ));
        let records = read(&runtime);
        assert_eq!(records.len(), 1);
        assert!(
            records[0].since_ms >= previous.since_ms + 900,
            "the held body must name the new outage: {records:?}"
        );
        let old_due = recovered + Duration::from_secs(3);
        assert_eq!(
            tracker.observe(old_due, old_due, Err("still lost".into())),
            None
        );
        assert_eq!(read(&runtime), records);
    }

    #[test]
    fn gaps_follow_loss_time_post_attempt_thresholds_and_transition_only_records() {
        let opened = Instant::now();
        let threshold = Duration::from_millis(1000);
        let short = |gap_ms, error: Option<&str>| {
            Some(Transition::Reconnected {
                opened,
                gap_ms,
                error: error.map(str::to_owned),
            })
        };
        let long = |recovered_after_ms, error: Option<&str>| {
            Some(Transition::Unreachable {
                opened,
                error: error.map(str::to_owned),
                recovered_after_ms,
            })
        };
        let failed = |error: &str| Err(error.to_owned());
        let cases = [
            (vec![(0, 0, Ok(LeaseRefresh::Healthy))], vec![None]),
            (
                vec![(0, 25, Ok(LeaseRefresh::Reconnected))],
                vec![short(25, None)],
            ),
            (
                vec![
                    (0, 200, failed("first")),
                    (400, 600, failed("last")),
                    (650, 700, Ok(LeaseRefresh::Reconnected)),
                ],
                vec![None, None, short(700, Some("last"))],
            ),
            (
                vec![(0, 1000, failed("slow"))],
                vec![long(None, Some("slow"))],
            ),
            (
                vec![
                    (0, 200, failed("first")),
                    (900, 1100, failed("crossing")),
                    (1200, 1400, failed("last")),
                    (1500, 1550, Ok(LeaseRefresh::Reconnected)),
                    (1600, 1600, Ok(LeaseRefresh::Healthy)),
                ],
                vec![
                    None,
                    long(None, Some("crossing")),
                    None,
                    long(Some(1550), Some("last")),
                    None,
                ],
            ),
            (
                vec![
                    (0, 200, failed("first")),
                    (900, 1000, Ok(LeaseRefresh::Reconnected)),
                ],
                vec![None, long(Some(1000), Some("first"))],
            ),
            (
                vec![(0, 1100, Ok(LeaseRefresh::Reconnected))],
                vec![long(Some(1100), None)],
            ),
        ];
        let (actual, expected): (Vec<_>, Vec<_>) = cases
            .into_iter()
            .map(|(checks, expected)| {
                let mut tracker = GapTracker::default();
                let actual = checks
                    .into_iter()
                    .map(|(start, finish, outcome)| {
                        tracker.advance(
                            opened + Duration::from_millis(start),
                            opened + Duration::from_millis(finish),
                            outcome,
                            threshold,
                        )
                    })
                    .collect::<Vec<_>>();
                (actual, expected)
            })
            .unzip();
        assert_eq!(actual, expected);
    }
}
