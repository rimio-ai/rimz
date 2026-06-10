use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Instant;

use jiff::Timestamp;

use crate::ids::{SidebarInstanceId, WorkspaceId};
use crate::ledger::paths::RuntimePaths;
use crate::sidebar::cache::{read_snapshot_cache, unix_now_ms};
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::{
    OBSERVE_COOLDOWN, OBSERVE_CROSSCHECK_TTL, OBSERVE_DEADPID_CONFIRMATIONS, OBSERVE_LOG_MAX_BYTES,
};

use super::{
    AnomalyDraft, AnomalyKind, ObserveMsg, ObserveRecord, ObserveRole, RosterSig, cap_vec,
};

const OBSERVE_LOG_NAME: &str = "observe.log.jsonl";

pub fn path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(OBSERVE_LOG_NAME)
}

pub fn spawn_writer(
    runtime: RuntimePaths,
    workspace_id: WorkspaceId,
    session: String,
    instance: SidebarInstanceId,
    rx: Receiver<ObserveMsg>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        Writer {
            runtime,
            workspace_id,
            session,
            instance,
            cooldowns: Cooldowns::default(),
            latest_roster: None,
            last_crosscheck: Instant::now(),
            role: RoleCache::default(),
            dead_pids: DeadPidTracker::default(),
        }
        .run(rx);
    })
}

pub fn crosscheck_enabled(role: ObserveRole) -> bool {
    matches!(role, ObserveRole::Elder)
}

struct Writer {
    runtime: RuntimePaths,
    workspace_id: WorkspaceId,
    session: String,
    instance: SidebarInstanceId,
    cooldowns: Cooldowns,
    latest_roster: Option<RosterSig>,
    last_crosscheck: Instant,
    role: RoleCache,
    dead_pids: DeadPidTracker,
}

impl Writer {
    fn run(&mut self, rx: Receiver<ObserveMsg>) {
        loop {
            match rx.recv_timeout(OBSERVE_CROSSCHECK_TTL) {
                Ok(ObserveMsg::Anomaly(draft)) => self.append_anomaly(*draft),
                Ok(ObserveMsg::Roster(roster)) => self.latest_roster = Some(roster),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.last_crosscheck.elapsed() >= OBSERVE_CROSSCHECK_TTL {
                self.last_crosscheck = Instant::now();
                self.run_crosschecks();
            }
        }
    }

    fn append_anomaly(&mut self, draft: AnomalyDraft) {
        let Some(suppressed_since_last) = self.cooldowns.allow(&draft.kind, draft.at_ms) else {
            return;
        };
        let role = self.role.current(&self.runtime, &self.instance);
        let record = ObserveRecord {
            at_ms: draft.at_ms,
            workspace_id: self.workspace_id.as_str().to_owned(),
            session: self.session.clone(),
            instance: self.instance.as_str().to_owned(),
            role,
            anomaly: draft.kind,
            window_ms: draft.window_ms,
            frame: draft.frame,
            events_recent: draft.events_recent,
            gate_reject_streak: draft.gate_reject_streak,
            health_failure_streak: draft.health_failure_streak,
            suppressed_since_last,
            dropped_msgs: draft.dropped_msgs,
        };
        crate::diag_log::append(&path(&self.runtime), OBSERVE_LOG_MAX_BYTES, &record);
    }

    fn run_crosschecks(&mut self) {
        let role = self.role.current(&self.runtime, &self.instance);
        if !crosscheck_enabled(role) {
            return;
        }
        let Some(roster) = self.latest_roster.clone() else {
            return;
        };
        if let Some(frame) =
            read_snapshot_cache(&self.runtime.root.join("snapshot.json"), &self.session)
        {
            for kind in compare_roster_to_frame(&roster, &frame) {
                self.append_anomaly(AnomalyDraft::from_roster(unix_now_ms(), &roster, kind));
            }
        }
        if crate::proc::process_start(std::process::id()).is_some() {
            for kind in self.dead_pids.check(&roster, crate::proc::process_start) {
                self.append_anomaly(AnomalyDraft::from_roster(unix_now_ms(), &roster, kind));
            }
        }
    }
}

#[derive(Default)]
struct RoleCache {
    role: Option<ObserveRole>,
    polled_at: Option<Instant>,
}

impl RoleCache {
    fn current(&mut self, runtime: &RuntimePaths, instance: &SidebarInstanceId) -> ObserveRole {
        if self
            .polled_at
            .is_none_or(|last| last.elapsed() >= OBSERVE_CROSSCHECK_TTL)
        {
            self.polled_at = Some(Instant::now());
            self.role = Some(
                if crate::sidebar::elder_sidebar_present(runtime, instance) {
                    ObserveRole::Consumer
                } else {
                    ObserveRole::Elder
                },
            );
        }
        self.role.unwrap_or(ObserveRole::Consumer)
    }
}

#[derive(Default)]
struct Cooldowns {
    by_kind: BTreeMap<&'static str, Cooldown>,
}

impl Cooldowns {
    fn allow(&mut self, kind: &AnomalyKind, at_ms: u64) -> Option<u32> {
        let cooldown = self.by_kind.entry(kind.key()).or_default();
        let window = OBSERVE_COOLDOWN.as_millis() as u64;
        if cooldown
            .last_emit_ms
            .is_some_and(|last| at_ms.saturating_sub(last) < window)
        {
            cooldown.suppressed = cooldown.suppressed.saturating_add(1);
            return None;
        }
        let suppressed = std::mem::take(&mut cooldown.suppressed);
        cooldown.last_emit_ms = Some(at_ms);
        Some(suppressed)
    }
}

#[derive(Default)]
struct Cooldown {
    last_emit_ms: Option<u64>,
    suppressed: u32,
}

pub fn compare_roster_to_frame(roster: &RosterSig, frame: &PaneFrame) -> Vec<AnomalyKind> {
    if roster.panes_produced_at_ms != Some(frame.produced_at_ms) {
        return Vec::new();
    }
    let frame_panes = frame
        .pane_states()
        .map(|pane| pane.pane_id.to_string())
        .collect::<BTreeSet<_>>();
    let mut anomalies = Vec::new();
    if roster.rows.len() > frame_panes.len() {
        anomalies.push(AnomalyKind::CardsExceedPanes {
            rows: roster.rows.len(),
            frame_panes: frame_panes.len(),
            frame_produced_at_ms: frame.produced_at_ms,
        });
    }
    for row in &roster.rows {
        let Some(pane_id) = row.pane_id.as_ref() else {
            continue;
        };
        if !frame_panes.contains(pane_id) {
            anomalies.push(AnomalyKind::RowPaneMissingFromFrame {
                row_id: row.row_id.clone(),
                pane_id: pane_id.clone(),
                frame_produced_at_ms: frame.produced_at_ms,
            });
        }
    }
    cap_vec(anomalies)
}

#[derive(Default)]
struct DeadPidTracker {
    by_pid: BTreeMap<(String, u32), DeadPidObservation>,
}

impl DeadPidTracker {
    fn check(
        &mut self,
        roster: &RosterSig,
        process_start: fn(u32) -> Option<Timestamp>,
    ) -> Vec<AnomalyKind> {
        let mut active = BTreeSet::new();
        let mut anomalies = Vec::new();
        for row in &roster.rows {
            let Some(pid) = row.pane_pid else {
                continue;
            };
            let key = (row.row_id.clone(), pid);
            active.insert(key.clone());
            let reason = match process_start(pid) {
                None => Some("gone".to_owned()),
                Some(actual)
                    if row
                        .pane_process_start
                        .is_some_and(|expected| timestamp_diff_gt(expected, actual, 2)) =>
                {
                    Some("starttime-mismatch".to_owned())
                }
                Some(_) => None,
            };
            let Some(reason) = reason else {
                self.by_pid.remove(&key);
                continue;
            };
            let observation = self.by_pid.entry(key.clone()).or_default();
            if observation.reason.as_deref() != Some(reason.as_str()) {
                observation.confirmations = 0;
                observation.reason = Some(reason.clone());
            }
            observation.confirmations = observation.confirmations.saturating_add(1);
            if observation.confirmations >= OBSERVE_DEADPID_CONFIRMATIONS {
                anomalies.push(AnomalyKind::DeadPid {
                    row_id: row.row_id.clone(),
                    pid,
                    reason,
                });
                observation.confirmations = 0;
            }
        }
        self.by_pid.retain(|key, _| active.contains(key));
        cap_vec(anomalies)
    }
}

#[derive(Default)]
struct DeadPidObservation {
    confirmations: u32,
    reason: Option<String>,
}

fn timestamp_diff_gt(left: Timestamp, right: Timestamp, secs: i64) -> bool {
    left.as_second().abs_diff(right.as_second()) > secs as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::PaneRef;
    use crate::ids::{MuxName, PaneId, ViewKind};
    use crate::sidebar::frame::assemble_frame;
    use crate::sidebar::observe::RosterRowSig;

    fn roster(produced_at: u64, rows: Vec<(&str, &str)>) -> RosterSig {
        RosterSig {
            panes_produced_at_ms: Some(produced_at),
            rows: rows
                .into_iter()
                .map(|(row_id, pane)| RosterRowSig {
                    row_id: row_id.to_owned(),
                    is_agent: true,
                    pane_id: Some(pane_id(pane).to_string()),
                    pane_pid: None,
                    pane_process_start: None,
                })
                .collect(),
        }
    }

    fn roster_with_pid(row_id: &str, pid: u32, started_at: Timestamp) -> RosterSig {
        RosterSig {
            panes_produced_at_ms: Some(10),
            rows: vec![RosterRowSig {
                row_id: row_id.to_owned(),
                is_agent: true,
                pane_id: Some(pane_id("terminal_1").to_string()),
                pane_pid: Some(pid),
                pane_process_start: Some(started_at),
            }],
        }
    }

    fn missing_process_start(_: u32) -> Option<Timestamp> {
        None
    }

    fn live_process_start(_: u32) -> Option<Timestamp> {
        Some(Timestamp::from_second(10).unwrap())
    }

    fn pane_id(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    fn pane(raw: &str) -> PaneRef {
        PaneRef {
            pane_id: pane_id(raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some("tab_0".to_owned()),
            view_kind: Some(ViewKind::Tab),
            view_name: None,
            is_focused: false,
            command: Some("zsh".to_owned()),
            spawn_command: None,
            cwd: Some("/repo".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
        }
    }

    #[test]
    fn roster_frame_compare_ignores_timestamp_skew() {
        let roster = roster(10, vec![("a", "terminal_1")]);
        let frame = assemble_frame(vec![pane("terminal_1")], 11, "rimz-test");

        assert!(compare_roster_to_frame(&roster, &frame).is_empty());
    }

    #[test]
    fn roster_frame_compare_reports_missing_panes() {
        let roster = roster(10, vec![("a", "terminal_1"), ("b", "terminal_2")]);
        let frame = assemble_frame(vec![pane("terminal_1")], 10, "rimz-test");

        let anomalies = compare_roster_to_frame(&roster, &frame);

        assert!(matches!(
            anomalies.as_slice(),
            [
                AnomalyKind::CardsExceedPanes { .. },
                AnomalyKind::RowPaneMissingFromFrame { row_id, .. }
            ] if row_id == "b"
        ));
    }

    #[test]
    fn cooldown_suppresses_per_kind_and_flushes_count() {
        let mut cooldowns = Cooldowns::default();
        let kind = AnomalyKind::DuplicateRowId {
            row_id: "a".to_owned(),
            count: 2,
        };

        assert_eq!(cooldowns.allow(&kind, 1_000), Some(0));
        assert_eq!(cooldowns.allow(&kind, 2_000), None);
        assert_eq!(cooldowns.allow(&kind, 31_001), Some(1));
    }

    #[test]
    fn dead_pid_requires_consecutive_confirmations_and_clears_on_recovery() {
        let mut tracker = DeadPidTracker::default();
        let roster = roster_with_pid("a", 123, Timestamp::from_second(10).unwrap());

        assert!(tracker.check(&roster, missing_process_start).is_empty());
        assert!(tracker.check(&roster, live_process_start).is_empty());
        assert!(tracker.check(&roster, missing_process_start).is_empty());

        let anomalies = tracker.check(&roster, missing_process_start);

        assert!(matches!(
            anomalies.as_slice(),
            [AnomalyKind::DeadPid { row_id, pid: 123, reason }]
                if row_id == "a" && reason == "gone"
        ));
    }
}
