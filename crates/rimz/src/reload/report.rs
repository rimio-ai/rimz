//! Per-room maintenance facts shared by reload and structural repair.

use std::path::PathBuf;

use crate::ids::MuxName;
use crate::mux::room_settings::PendingSettings;

use super::ReloadOutcome;

/// Per-room reload results and machine-wide maintenance effects.
#[derive(Clone, Debug, Default)]
pub struct ReloadReport {
    pub rooms: Vec<RoomReload>,
    pub machine: MachineReload,
}

/// Maintenance effects and failures not attributable to one room.
#[derive(Clone, Debug, Default)]
pub struct MachineReload {
    pub stats_reloaded: usize,
    pub brokers_retired: usize,
    pub brokers_marked: usize,
    pub brokers_kept: usize,
    pub brokers_unchecked: usize,
    pub dead_swept: usize,
    pub web_restarted: bool,
    pub web_error: Option<String>,
}

/// Upgrade facts and optional structural-repair results for one live room.
#[derive(Clone, Debug)]
pub struct RoomReload {
    pub project_root: PathBuf,
    pub mux: MuxName,
    pub session_name: String,
    /// Upgrade maintenance counters, with settling refreshed after repair.
    pub outcome: ReloadOutcome,
    /// Fresh session heartbeats after the final upgrade or repair pass.
    pub sidebars: usize,
    /// Located fresh heartbeats that identify the staged build.
    pub current_sidebars: usize,
    /// Failure to durably record the room's binary and build.
    pub record_error: Option<String>,
    /// Failure returned while applying tmux room options.
    pub options_error: Option<String>,
    /// Failure returned while converging the Zellij presence plugin.
    pub plugin_error: Option<String>,
    /// An enabled Claude host was live before a different build was recorded.
    pub claude_restarts: bool,
    /// Settings awaiting another room start or managed attach, or pre-tracking.
    pub pending: PendingSettings,
    /// Structural repair counters when the caller requested repair.
    pub repair: Option<ReloadOutcome>,
}

/// Report priority for a room, ordered from user action to healthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RoomState {
    NeedsYou,
    Settling,
    Healthy,
}

/// A room-level problem requiring a targeted fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomProblem {
    MissingSidebar,
    Drainer,
    Record(String),
    Options(String),
    Plugin(String),
    RepairFailed,
    RepairDeferred,
    Misdocked,
    RepairBusy,
    DaemonReopen,
    PresenceSilent,
}

/// A machine-wide reload problem requiring user action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineProblem {
    KeptBrokers(usize),
    UncheckedBrokers,
    Web(String),
}

impl ReloadReport {
    pub fn needs_attention(&self) -> usize {
        self.rooms
            .iter()
            .map(|room| room.problems().len())
            .sum::<usize>()
            + self.machine.problems().len()
    }

    pub fn repair_totals(&self) -> ReloadOutcome {
        let mut total = ReloadOutcome::default();
        for room in &self.rooms {
            if let Some(repair) = room.repair {
                total.merge(repair);
            }
        }
        total
    }
}

impl RoomReload {
    pub(super) fn record_upgrade(
        &mut self,
        awaiting: &std::collections::HashSet<crate::ids::PaneId>,
        heartbeats: &[crate::wakeup::heartbeat::SidebarHeartbeat],
        build: &str,
    ) {
        self.record_sidebars(heartbeats, build);
        self.outcome.reexeced = awaiting
            .intersection(&super::current_build_claims(heartbeats, build))
            .count();
        // A managed room always runs a sidebar, so a live session with no fresh,
        // protocol-current heartbeat has either lost its pane or is running a
        // renderer this build cannot signal. Reload converges neither, and repair
        // mounts or replaces both, so the report owes the reader that verb.
        self.outcome.sidebar_missing = usize::from(heartbeats.is_empty());
    }

    pub(super) fn record_repair(
        &mut self,
        repair: ReloadOutcome,
        presence: Option<Result<(), String>>,
        heartbeats: &[crate::wakeup::heartbeat::SidebarHeartbeat],
        build: &str,
    ) {
        self.repair = Some(repair);
        if let Some(presence) = presence {
            self.plugin_error = presence.err();
        }
        self.record_sidebars(heartbeats, build);
    }

    fn record_sidebars(
        &mut self,
        heartbeats: &[crate::wakeup::heartbeat::SidebarHeartbeat],
        build: &str,
    ) {
        self.sidebars = heartbeats.len();
        self.current_sidebars = super::current_located_count(heartbeats, build);
        self.outcome.unconverged = super::awaiting_panes(heartbeats, build).len();
        self.outcome.unverified = super::unlocated_unverified_count(heartbeats, build);
    }

    pub fn state(&self) -> RoomState {
        if !self.problems().is_empty() {
            return RoomState::NeedsYou;
        }
        if self.settling() > 0 {
            return RoomState::Settling;
        }
        RoomState::Healthy
    }

    pub fn problems(&self) -> Vec<RoomProblem> {
        let mut problems = Vec::new();
        if self.outcome.sidebar_missing > 0 && self.repair.is_none() {
            problems.push(RoomProblem::MissingSidebar);
        }
        if self.outcome.drainers_stop_failed > 0 {
            problems.push(RoomProblem::Drainer);
        }
        if let Some(error) = &self.record_error {
            problems.push(RoomProblem::Record(error.clone()));
        }
        if let Some(error) = &self.options_error {
            problems.push(RoomProblem::Options(error.clone()));
        }
        if let Some(error) = &self.plugin_error {
            problems.push(RoomProblem::Plugin(error.clone()));
        }
        if let Some(repair) = self.repair {
            for (present, problem) in [
                (repair.failed > 0, RoomProblem::RepairFailed),
                (
                    repair.deferred > 0 || repair.daemon_reopen_deferred > 0,
                    RoomProblem::RepairDeferred,
                ),
                (repair.misdocked > 0, RoomProblem::Misdocked),
                (repair.busy > 0, RoomProblem::RepairBusy),
                (repair.daemon_reopen_failed > 0, RoomProblem::DaemonReopen),
                (repair.presence_dead > 0, RoomProblem::PresenceSilent),
            ] {
                if present {
                    problems.push(problem);
                }
            }
        }
        problems
    }

    pub fn sidebar_counts(&self) -> (usize, usize) {
        (self.current_sidebars, self.sidebars)
    }

    pub fn settling(&self) -> usize {
        self.outcome.unconverged + self.outcome.unverified
    }
}

impl MachineReload {
    pub fn problems(&self) -> Vec<MachineProblem> {
        let mut problems = Vec::new();
        if self.brokers_kept > 0 {
            problems.push(MachineProblem::KeptBrokers(self.brokers_kept));
        }
        if self.brokers_unchecked > 0 {
            problems.push(MachineProblem::UncheckedBrokers);
        }
        if let Some(error) = &self.web_error {
            problems.push(MachineProblem::Web(error.clone()));
        }
        problems
    }
}

pub(super) fn claude_restart(
    enabled: bool,
    prior: Option<&str>,
    staged: &str,
    liveness: crate::agents::runtime_control::RuntimeControlLiveness,
) -> bool {
    enabled
        && prior != Some(staged)
        && liveness == crate::agents::runtime_control::RuntimeControlLiveness::Up
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat(build: &str, pane: usize) -> crate::wakeup::heartbeat::SidebarHeartbeat {
        let mut heartbeat = crate::wakeup::heartbeat::SidebarHeartbeat::new(
            crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
            crate::ids::SidebarInstanceId::new(),
            MuxName::Tmux,
            "room",
            PathBuf::new(),
            Some(format!("tmux:%{pane}").parse().unwrap()),
        );
        heartbeat.build = Some(build.into());
        heartbeat
    }

    #[test]
    fn repair_counts_the_verified_replacement_not_the_closed_pre_repair_sidebars() {
        let mut room = room();
        room.outcome.unconverged = 1;
        room.record_repair(
            ReloadOutcome {
                recovered: 1,
                closed: 2,
                ..Default::default()
            },
            None,
            &[heartbeat("new", 3)],
            "new",
        );
        assert_eq!(room.sidebar_counts(), (1, 1));
        assert_eq!(room.state(), RoomState::Healthy);
        assert_eq!(room.settling(), 0);
        room.record_repair(
            ReloadOutcome::default(),
            None,
            &[heartbeat("old", 4)],
            "new",
        );
        assert_eq!(room.state(), RoomState::Settling);
        assert_eq!(room.sidebar_counts(), (0, 1));
    }

    #[test]
    fn remote_restart_requires_an_enabled_live_host_and_a_different_build() {
        use crate::agents::runtime_control::RuntimeControlLiveness::{Down, Unknown, Up};
        assert!(claude_restart(true, Some("old"), "new", Up));
        assert!(claude_restart(true, None, "new", Up));
        assert!(!claude_restart(false, Some("old"), "new", Up));
        assert!(!claude_restart(true, Some("new"), "new", Up));
        for liveness in [Unknown, Down] {
            assert!(!claude_restart(true, Some("old"), "new", liveness));
        }
    }

    #[test]
    fn upgrade_does_not_count_a_disappeared_pane_as_settling() {
        let before = [heartbeat("old", 1), heartbeat("old", 2)];
        let after = [heartbeat("new", 1)];
        let mut room = room();
        room.record_upgrade(&super::super::awaiting_panes(&before, "new"), &after, "new");
        assert_eq!(room.sidebar_counts(), (1, 1));
        assert_eq!(room.outcome.reexeced, 1);
        assert_eq!(room.settling(), 0);
        assert_eq!(room.state(), RoomState::Healthy);
    }

    #[test]
    fn upgrade_counts_an_old_build_pane_that_arrives_during_the_wait() {
        let before = [heartbeat("old", 1), heartbeat("old", 2)];
        let after = [heartbeat("new", 1), heartbeat("old", 3)];
        let mut room = room();
        room.record_upgrade(&super::super::awaiting_panes(&before, "new"), &after, "new");
        assert_eq!(room.sidebar_counts(), (1, 2));
        assert_eq!(room.outcome.reexeced, 1);
        assert_eq!(room.settling(), 1);
        assert_eq!(room.state(), RoomState::Settling);
    }

    #[test]
    fn repair_replaces_the_plugin_error_only_when_a_load_was_attempted() {
        for (presence, expected) in [
            (Some(Ok(())), None),
            (
                Some(Err("artifact unavailable".into())),
                Some("artifact unavailable"),
            ),
            (None, Some("upgrade failed")),
        ] {
            let mut room = room();
            room.plugin_error = Some("upgrade failed".into());
            room.record_repair(
                ReloadOutcome::default(),
                presence,
                &[heartbeat("new", 1)],
                "new",
            );
            assert_eq!(room.plugin_error.as_deref(), expected);
        }
    }

    fn room() -> RoomReload {
        RoomReload {
            project_root: "/project".into(),
            mux: MuxName::Tmux,
            session_name: "room".into(),
            outcome: ReloadOutcome::default(),
            sidebars: 2,
            current_sidebars: 1,
            record_error: None,
            options_error: None,
            plugin_error: None,
            claude_restarts: false,
            pending: PendingSettings::Tracked(vec![]),
            repair: None,
        }
    }

    #[test]
    fn settling_is_unconfirmed_and_a_problem_takes_precedence() {
        let mut room = room();
        room.outcome.unverified = 1;
        assert_eq!(room.state(), RoomState::Settling);
        assert_eq!(room.sidebar_counts(), (1, 2));
        room.record_error = Some("denied".into());
        assert_eq!(room.state(), RoomState::NeedsYou);
        let report = ReloadReport {
            rooms: vec![room],
            machine: MachineReload {
                brokers_kept: 2,
                brokers_unchecked: 1,
                web_error: Some("refused".into()),
                ..Default::default()
            },
        };
        assert_eq!(report.needs_attention(), 4);
    }

    #[test]
    fn repair_replaces_missing_with_its_result_and_preserves_totals() {
        let mut room = room();
        room.outcome.sidebar_missing = 1;
        assert_eq!(room.problems(), vec![RoomProblem::MissingSidebar]);
        room.record_repair(
            ReloadOutcome {
                sessions: 1,
                recovered: 2,
                ..Default::default()
            },
            None,
            &[
                heartbeat("new", 1),
                heartbeat("new", 2),
                heartbeat("new", 3),
                heartbeat("old", 4),
            ],
            "new",
        );
        assert!(room.problems().is_empty());
        assert_eq!(room.sidebar_counts(), (3, 4));
        let report = ReloadReport {
            rooms: vec![room.clone()],
            ..Default::default()
        };
        assert_eq!(report.repair_totals().recovered, 2);
        room.repair.as_mut().unwrap().deferred = 1;
        assert_eq!(room.problems(), vec![RoomProblem::RepairDeferred]);
    }
}
