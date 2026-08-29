//! Observer writer thread: elder-only real-world cross-checks and emission of
//! [`DiagEvent::FrameAnomaly`] records through the shared diagnostics sink,
//! which owns the rate limit — the render thread never does IO for the observer.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Instant;

use crate::diag::DiagSink;
use crate::diag::record::DiagEvent;
use crate::sidebar::ProducerElectionTracker;
use crate::sidebar::cache::read_snapshot_cache;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{
    OBSERVE_CROSSCHECK_TTL, OBSERVE_DEADPID_CONFIRMATIONS, OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS,
};
use crate::store::paths::RuntimePaths;
use jiff::Timestamp;

use super::{AnomalyDraft, AnomalyKind, ObserveMsg, ObserveRole, RosterSig, cap_vec};

pub fn spawn(
    runtime: RuntimePaths,
    sink: DiagSink,
    election: ProducerElectionTracker,
    rx: Receiver<ObserveMsg>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        Writer {
            runtime,
            sink,
            election,
            latest_roster: None,
            last_crosscheck: Instant::now(),
            dead_pids: DeadPidTracker::default(),
            hostless_agents: HostedAgentTracker::default(),
        }
        .run(rx);
    })
}

fn crosscheck_enabled(role: ObserveRole) -> bool {
    matches!(role, ObserveRole::Elder)
}

struct Writer {
    runtime: RuntimePaths,
    sink: DiagSink,
    election: ProducerElectionTracker,
    latest_roster: Option<RosterSig>,
    last_crosscheck: Instant,
    dead_pids: DeadPidTracker,
    hostless_agents: HostedAgentTracker,
}

impl Writer {
    fn run(&mut self, rx: Receiver<ObserveMsg>) {
        loop {
            match rx.recv_timeout(OBSERVE_CROSSCHECK_TTL) {
                Ok(ObserveMsg::Anomaly(draft)) => self.emit_anomaly(*draft),
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

    /// The sink is the one rate limit: it keys on `identity_key()`, so repeats
    /// on one subject collapse while a fault on a different row reports now.
    fn emit_anomaly(&mut self, draft: AnomalyDraft) {
        let role = current_role(&self.election);
        self.sink.emit_at_ms(
            DiagEvent::FrameAnomaly {
                role,
                anomaly: draft.kind,
                window_ms: draft.window_ms,
                frame: draft.frame,
                events_recent: draft.events_recent,
                gate_reject_streak: draft.gate_reject_streak,
                health_failure_streak: draft.health_failure_streak,
                dropped_msgs: draft.dropped_msgs,
            },
            draft.at_ms,
        );
    }

    fn run_crosschecks(&mut self) {
        let role = current_role(&self.election);
        if !crosscheck_enabled(role) {
            return;
        }
        let Some(roster) = self.latest_roster.clone() else {
            return;
        };
        if let Some(frame) =
            read_snapshot_cache(&self.runtime.pane_frame_path(), self.sink.session_name())
        {
            for kind in compare_roster_to_frame(&roster, &frame) {
                self.emit_anomaly(AnomalyDraft::from_roster(unix_now_ms(), &roster, kind));
            }
        }
        if crate::proc::process_start(std::process::id()).is_some() {
            for kind in self.dead_pids.check(&roster, crate::proc::process_start) {
                self.emit_anomaly(AnomalyDraft::from_roster(unix_now_ms(), &roster, kind));
            }
            for kind in self.hostless_agents.check(
                &roster,
                crate::proc::process_start,
                &crate::proc::hosted_agent_absent_under_root,
            ) {
                self.emit_anomaly(AnomalyDraft::from_roster(unix_now_ms(), &roster, kind));
            }
        }
    }
}

fn current_role(election: &ProducerElectionTracker) -> ObserveRole {
    if election.elder_instance().is_some() {
        ObserveRole::Consumer
    } else {
        ObserveRole::Elder
    }
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
                    if row.pane_process_start.is_some_and(|expected| {
                        timestamp_diff_gt(
                            expected,
                            actual,
                            crate::sidebar::timing::PROCESS_START_MATCH_TOLERANCE,
                        )
                    }) =>
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
                observation.emitted = false;
                observation.reason = Some(reason.clone());
            }
            observation.confirmations = observation.confirmations.saturating_add(1);
            // A dead or reused pid is a standing fact, not a recurring event: a
            // pane present in the topology cache (which carries no pid) keeps a
            // pid frozen from an earlier CLI read, so a reused pid would
            // otherwise re-report every cooldown for the pane's whole life.
            // Report each episode once; a changed reason or a re-armed key (the
            // mismatch cleared and recurred) starts a new one.
            if observation.confirmations >= OBSERVE_DEADPID_CONFIRMATIONS && !observation.emitted {
                anomalies.push(AnomalyKind::DeadPid {
                    row_id: row.row_id.clone(),
                    pid,
                    reason,
                });
                observation.emitted = true;
            }
        }
        self.by_pid.retain(|key, _| active.contains(key));
        cap_vec(anomalies)
    }
}

#[derive(Default)]
struct DeadPidObservation {
    confirmations: u32,
    emitted: bool,
    reason: Option<String>,
}

#[derive(Default)]
struct HostedAgentTracker {
    by_pid: BTreeMap<(String, u32), HostedAgentObservation>,
}

impl HostedAgentTracker {
    fn check(
        &mut self,
        roster: &RosterSig,
        process_start: fn(u32) -> Option<Timestamp>,
        hosted_agent_absent: &dyn Fn(&str, u32) -> bool,
    ) -> Vec<AnomalyKind> {
        let mut active = BTreeSet::new();
        let mut anomalies = Vec::new();
        for row in &roster.rows {
            let (Some(kind), Some(pid)) = (row.agent_kind.as_deref(), row.pane_pid) else {
                continue;
            };
            if process_start(pid).is_none() {
                continue;
            }
            let key = (row.row_id.clone(), pid);
            active.insert(key.clone());
            if !hosted_agent_absent(kind, pid) {
                self.by_pid.remove(&key);
                continue;
            }
            let observation = self.by_pid.entry(key).or_default();
            observation.confirmations = observation.confirmations.saturating_add(1);
            if observation.confirmations >= OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS
                && !observation.emitted
            {
                anomalies.push(AnomalyKind::AgentCardWithoutProcess {
                    row_id: row.row_id.clone(),
                    pane_id: row.pane_id.clone(),
                    pid,
                    kind: kind.to_owned(),
                });
                observation.emitted = true;
            }
        }
        self.by_pid.retain(|key, _| active.contains(key));
        cap_vec(anomalies)
    }
}

#[derive(Default)]
struct HostedAgentObservation {
    confirmations: u32,
    emitted: bool,
}

fn timestamp_diff_gt(left: Timestamp, right: Timestamp, tolerance: std::time::Duration) -> bool {
    left.as_second().abs_diff(right.as_second()) > tolerance.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::record::DiagEnvelope;
    use crate::ids::{MuxName, PaneId, SidebarInstanceId, ViewKind, WorkspaceId};
    use crate::pane::PaneRef;
    use crate::sidebar::frame::assemble_frame;
    use crate::sidebar::observe::RosterRowSig;

    fn roster(produced_at: u64, rows: Vec<(&str, &str)>) -> RosterSig {
        RosterSig {
            panes_produced_at_ms: Some(produced_at),
            rows: rows
                .into_iter()
                .map(|(row_id, pane)| RosterRowSig {
                    row_id: row_id.to_owned(),
                    agent_kind: Some("claude".to_owned()),
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
                agent_kind: Some("claude".to_owned()),
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

    fn mismatched_process_start(_: u32) -> Option<Timestamp> {
        Some(Timestamp::from_second(100).unwrap())
    }

    fn check_hosted_agent(
        tracker: &mut HostedAgentTracker,
        roster: &RosterSig,
        process_start: fn(u32) -> Option<Timestamp>,
        absent: bool,
    ) -> Vec<AnomalyKind> {
        tracker.check(roster, process_start, &|_, _| absent)
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
            title: None,
            is_floating: false,
            command: Some("zsh".to_owned()),
            foreground_cmdline: None,
            spawn_command: None,
            cwd: Some("/repo".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            first_seen_at_ms: None,
            resumed_session_id: None,
            elevated_agent: None,
        }
    }

    #[test]
    fn roster_frame_compare_reports_missing_panes() {
        // Timestamp skew alone is no anomaly: a roster matched by a frame with a
        // later produced-at stamp compares clean.
        let skewed = roster(10, vec![("a", "terminal_1")]);
        let frame = assemble_frame(vec![pane("terminal_1")], 11, "rimz-test");
        assert!(
            compare_roster_to_frame(&skewed, &frame).is_empty(),
            "produced_at_ms skew alone reports no anomaly"
        );

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

    #[test]
    fn dead_pid_reports_each_episode_once_and_re_arms_on_reason_change() {
        let mut tracker = DeadPidTracker::default();
        let roster = roster_with_pid("a", 123, Timestamp::from_second(10).unwrap());

        // Two confirmations arm the first report; the standing condition then
        // stays quiet however many frames it persists.
        assert!(tracker.check(&roster, missing_process_start).is_empty());
        assert_eq!(tracker.check(&roster, missing_process_start).len(), 1);
        assert!(tracker.check(&roster, missing_process_start).is_empty());
        assert!(tracker.check(&roster, missing_process_start).is_empty());

        // A different reason is a new episode: it re-arms and reports once more.
        assert!(tracker.check(&roster, mismatched_process_start).is_empty());
        assert!(matches!(
            tracker.check(&roster, mismatched_process_start).as_slice(),
            [AnomalyKind::DeadPid { reason, .. }] if reason == "starttime-mismatch"
        ));
        assert!(tracker.check(&roster, mismatched_process_start).is_empty());
    }

    #[test]
    fn hostless_agent_requires_confirmations_emits_once_and_re_arms() {
        let mut tracker = HostedAgentTracker::default();
        let roster = roster_with_pid("a", 123, Timestamp::from_second(10).unwrap());

        assert!(check_hosted_agent(&mut tracker, &roster, live_process_start, true).is_empty());
        assert!(check_hosted_agent(&mut tracker, &roster, live_process_start, false).is_empty());
        assert!(check_hosted_agent(&mut tracker, &roster, live_process_start, true).is_empty());
        assert!(matches!(
            check_hosted_agent(&mut tracker, &roster, live_process_start, true).as_slice(),
            [AnomalyKind::AgentCardWithoutProcess {
                row_id,
                pane_id: Some(pane_id),
                pid: 123,
                kind,
            }] if row_id == "a" && pane_id == "zellij:terminal_1" && kind == "claude"
        ));
        assert!(check_hosted_agent(&mut tracker, &roster, live_process_start, true).is_empty());

        assert!(check_hosted_agent(&mut tracker, &roster, live_process_start, false).is_empty());
        assert!(check_hosted_agent(&mut tracker, &roster, live_process_start, true).is_empty());
        assert_eq!(
            check_hosted_agent(&mut tracker, &roster, live_process_start, true).len(),
            1
        );
    }

    #[test]
    fn hostless_agent_ignores_process_rows_pidless_rows_and_dead_pids() {
        let mut tracker = HostedAgentTracker::default();
        let mut rows = roster_with_pid("process", 123, Timestamp::from_second(10).unwrap());
        rows.rows[0].agent_kind = None;
        rows.rows.push(RosterRowSig {
            row_id: "pidless".to_owned(),
            agent_kind: Some("claude".to_owned()),
            pane_id: Some(pane_id("terminal_2").to_string()),
            pane_pid: None,
            pane_process_start: None,
        });
        assert!(check_hosted_agent(&mut tracker, &rows, live_process_start, true).is_empty());
        assert!(check_hosted_agent(&mut tracker, &rows, live_process_start, true).is_empty());

        let agent = roster_with_pid("agent", 456, Timestamp::from_second(10).unwrap());
        assert!(check_hosted_agent(&mut tracker, &agent, missing_process_start, true).is_empty());
        assert!(check_hosted_agent(&mut tracker, &agent, missing_process_start, true).is_empty());
    }

    #[test]
    fn writer_emits_frame_anomaly_records_into_the_diag_log() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime");
        let sink =
            crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace, "rimz-test", None);
        let log_path = sink.log_path().unwrap();
        let instance = SidebarInstanceId::new();
        let (tx, rx) = std::sync::mpsc::sync_channel::<ObserveMsg>(4);

        let election = ProducerElectionTracker::new(runtime.clone(), instance);
        let handle = spawn(runtime, sink, election, rx);
        let sig_rows = roster(10, vec![("a", "terminal_1")]);
        let mut draft = AnomalyDraft::from_roster(
            42,
            &sig_rows,
            AnomalyKind::DuplicateRowId {
                row_id: "a".to_owned(),
                count: 2,
            },
        );
        draft.dropped_msgs = 7;
        tx.send(ObserveMsg::Anomaly(Box::new(draft))).unwrap();
        drop(tx);
        handle.join().unwrap();

        let text = std::fs::read_to_string(log_path).unwrap();
        let record: DiagEnvelope = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(record.at_ms, 42);
        assert!(matches!(
            record.event,
            crate::diag::record::DiagEvent::FrameAnomaly {
                anomaly: AnomalyKind::DuplicateRowId { .. },
                dropped_msgs: 7,
                ..
            }
        ));
    }
}
