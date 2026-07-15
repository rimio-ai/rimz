//! Agent lifecycle ingestion policy and automatic event-log rotation gate.

use std::time::Duration;

use crate::agents::AgentLifecycleObservation;
use crate::agents::lifecycle::{LifecycleSignal, Transition, TransitionKind};
use crate::ids::AgentKind;
use crate::store::event::EventEnvelope;

use super::{Store, debounce};
use crate::store::Result;

const MIB: u64 = 1024 * 1024;
pub const DEFAULT_EVENT_LOG_ROTATE_BYTES: u64 = 64 * MIB;
const AUTO_ROTATE_DEBOUNCE: Duration = Duration::from_secs(60);
const AUTO_ROTATE_STAMP: &str = "auto-rotate.stamp";

pub struct AgentLifecycleIntent<'a> {
    pub session_name: &'a str,
    pub agent_kind: AgentKind,
    pub event_name: &'a str,
    pub observation: &'a AgentLifecycleObservation,
    pub transition: Option<Transition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentLifecycleOutcome {
    Suppressed,
    Appended,
    RotationDue,
}

impl Store {
    /// Apply lifecycle append policy and report whether the CLI should launch
    /// the existing detached event-log rotation command.
    #[must_use = "durability barrier; check the result"]
    pub fn append_agent_lifecycle(
        &self,
        intent: AgentLifecycleIntent<'_>,
    ) -> Result<AgentLifecycleOutcome> {
        self.append_agent_lifecycle_with_threshold(intent, DEFAULT_EVENT_LOG_ROTATE_BYTES)
    }

    fn append_agent_lifecycle_with_threshold(
        &self,
        intent: AgentLifecycleIntent<'_>,
        rotation_threshold: u64,
    ) -> Result<AgentLifecycleOutcome> {
        if !append_lifecycle_event(
            &intent.observation.signal,
            intent.transition,
            intent.observation.parent_agent_id.is_some(),
        ) {
            return Ok(AgentLifecycleOutcome::Suppressed);
        }

        let observation = event_lifecycle_observation(intent.observation);
        let envelope = EventEnvelope::agent_lifecycle(
            self.inner.paths.workspace_id.clone(),
            intent.session_name,
            intent.agent_kind.as_str(),
            intent.event_name,
            &observation,
        );
        self.commit(|txn| {
            txn.append(&envelope)?;
            let Ok(metadata) = std::fs::metadata(&txn.paths.events_log) else {
                return Ok(AgentLifecycleOutcome::Appended);
            };
            let stamp = txn.paths.locks_dir.join(AUTO_ROTATE_STAMP);
            if metadata.len() < rotation_threshold
                || !debounce::stamp_due(&stamp, AUTO_ROTATE_DEBOUNCE)
            {
                return Ok(AgentLifecycleOutcome::Appended);
            }
            debounce::touch_stamp(&stamp);
            Ok(AgentLifecycleOutcome::RotationDue)
        })
    }
}

fn proof_of_work_tool(signal: &LifecycleSignal) -> bool {
    matches!(
        signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false
        }
    )
}

fn append_lifecycle_event(
    signal: &LifecycleSignal,
    transition: Option<Transition>,
    child_owned: bool,
) -> bool {
    child_owned
        || !proof_of_work_tool(signal)
        || transition.is_some_and(|transition| {
            transition.compaction_closed
                || transition.waiting_cleared
                || matches!(transition.kind, TransitionKind::Reconciled { .. })
        })
}

fn event_lifecycle_observation(
    observation: &AgentLifecycleObservation,
) -> AgentLifecycleObservation {
    let mut trimmed = observation.clone();
    if observation.signal.establishes_identity() || observation.parent_agent_id.is_some() {
        return trimmed;
    }
    if !matches!(observation.signal, LifecycleSignal::TurnEnded { .. }) {
        trimmed.transcript_path = None;
    }
    trimmed.worktree_path = None;
    trimmed.worktree_branch = None;
    trimmed.launch.role = None;
    trimmed.launch.team = None;
    trimmed.launch.channel = None;
    trimmed.launch.profile = None;
    trimmed
}

#[cfg(test)]
fn auto_rotation_due(log_len: u64, stamp_age: Option<Duration>) -> bool {
    auto_rotation_due_at(log_len, DEFAULT_EVENT_LOG_ROTATE_BYTES, stamp_age)
}

#[cfg(test)]
fn auto_rotation_due_at(log_len: u64, threshold: u64, stamp_age: Option<Duration>) -> bool {
    log_len >= threshold && stamp_age.is_none_or(|age| age >= AUTO_ROTATE_DEBOUNCE)
}

#[cfg(test)]
mod tests {
    use std::fs::{FileTimes, OpenOptions};
    use std::time::SystemTime;

    use super::*;
    use crate::agents::lifecycle::{LifecycleState, TurnPhase};
    use crate::agents::{AgentStatus, LaunchParams};
    use crate::ids::{AgentSessionId, MuxName, PaneId, WorkspaceId};
    use crate::store::event::EventKind;
    use crate::store::{RuntimePaths, StatePaths};

    fn transition(kind: TransitionKind, compaction_closed: bool) -> Transition {
        Transition {
            next: LifecycleState {
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                compacting: false,
            },
            kind,
            compaction_closed,
            waiting_cleared: false,
            opened_turn: false,
        }
    }

    fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
        let store = Store::open(paths, runtime).expect("open store");
        (dir, store)
    }

    fn observation(signal: LifecycleSignal) -> AgentLifecycleObservation {
        AgentLifecycleObservation::new(Some(AgentSessionId::from("sess-1")), signal)
    }

    #[test]
    fn lifecycle_append_gate_keeps_durable_truth_for_progress_signals() {
        let proof = LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        };
        let mutating = LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        };

        assert!(append_lifecycle_event(&mutating, None, false));
        assert!(!append_lifecycle_event(&proof, None, false));
        assert!(append_lifecycle_event(&proof, None, true));
        assert!(!append_lifecycle_event(
            &proof,
            Some(transition(TransitionKind::Normal, false)),
            false,
        ));
        assert!(append_lifecycle_event(
            &proof,
            Some(transition(
                TransitionKind::Reconciled {
                    from: AgentStatus::Idle,
                    reason: "tool used outside a running turn",
                },
                false,
            )),
            false,
        ));
        assert!(append_lifecycle_event(
            &proof,
            Some(transition(TransitionKind::Normal, true)),
            false,
        ));
        let mut clears_waiting = transition(TransitionKind::Normal, false);
        clears_waiting.waiting_cleared = true;
        assert!(append_lifecycle_event(&proof, Some(clears_waiting), false));
    }

    #[test]
    fn lifecycle_event_observation_trims_only_serialized_carry_forward_fields() {
        let mut full = observation(LifecycleSignal::Registered);
        full.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        full.worktree_path = Some("/tmp/project".to_owned());
        full.worktree_branch = Some("feature".to_owned());
        full.launch = LaunchParams {
            role: Some("coder".to_owned()),
            team: Some("forge".to_owned()),
            channel: Some("event-log".to_owned()),
            profile: Some("claude-coder".to_owned()),
            ..LaunchParams::default()
        };
        full.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%1"));
        let original = full.clone();

        assert_eq!(event_lifecycle_observation(&full), full);
        full.signal = LifecycleSignal::TurnStarted;
        let trimmed = event_lifecycle_observation(&full);
        assert!(trimmed.transcript_path.is_none());
        assert!(trimmed.worktree_path.is_none());
        assert!(trimmed.worktree_branch.is_none());
        assert!(trimmed.launch.role.is_none());
        assert!(trimmed.launch.team.is_none());
        assert!(trimmed.launch.channel.is_none());
        assert!(trimmed.launch.profile.is_none());
        assert_eq!(trimmed.pane_id.as_ref().map(PaneId::raw), Some("%1"));
        assert_eq!(
            full.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );

        full.signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        };
        assert_eq!(
            event_lifecycle_observation(&full)
                .transcript_path
                .as_deref(),
            Some("/tmp/transcript.jsonl")
        );
        assert_eq!(original.signal, LifecycleSignal::Registered);
    }

    #[test]
    fn lifecycle_append_outcomes_cover_suppressed_and_appended() {
        let (_dir, store) = test_store();
        let proof = observation(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        });
        let before = std::fs::metadata(&store.paths().events_log)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let suppressed = store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "PreToolUse",
                observation: &proof,
                transition: None,
            })
            .expect("suppress proof-only event");
        assert_eq!(suppressed, AgentLifecycleOutcome::Suppressed);
        assert_eq!(
            std::fs::metadata(&store.paths().events_log)
                .map(|meta| meta.len())
                .unwrap_or(0),
            before
        );
        assert!(!store.paths().locks_dir.join(AUTO_ROTATE_STAMP).exists());

        let started = observation(LifecycleSignal::TurnStarted);
        let appended = store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "UserPromptSubmit",
                observation: &started,
                transition: None,
            })
            .expect("append lifecycle event");
        assert_eq!(appended, AgentLifecycleOutcome::Appended);
        let events = store.read_events().expect("read events");
        assert_eq!(events.len(), 1);
        let EventKind::AgentLifecycle(payload) = events[0].kind() else {
            panic!("agent lifecycle event")
        };
        assert_eq!(payload.event_name.as_deref(), Some("UserPromptSubmit"));
    }

    #[test]
    fn auto_rotation_decision_respects_exact_threshold_and_debounce() {
        assert!(!auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES - 1, None));
        assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));
        assert!(!auto_rotation_due(
            DEFAULT_EVENT_LOG_ROTATE_BYTES,
            Some(AUTO_ROTATE_DEBOUNCE - Duration::from_secs(1))
        ));
        assert!(auto_rotation_due(
            DEFAULT_EVENT_LOG_ROTATE_BYTES,
            Some(AUTO_ROTATE_DEBOUNCE)
        ));
    }

    #[test]
    fn missing_unreadable_and_future_stamps_are_due() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing.stamp");
        assert!(debounce::stamp_due(&missing, AUTO_ROTATE_DEBOUNCE));
        assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));

        #[cfg(unix)]
        {
            let unreadable = dir.path().join("unreadable.stamp");
            std::os::unix::fs::symlink("unreadable.stamp", &unreadable)
                .expect("create symlink loop");
            assert!(debounce::stamp_due(&unreadable, AUTO_ROTATE_DEBOUNCE));
            assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));
        }

        let future = dir.path().join("future.stamp");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&future)
            .expect("create future stamp");
        file.set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)))
            .expect("set future stamp time");
        assert!(debounce::stamp_due(&future, AUTO_ROTATE_DEBOUNCE));
        assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));
    }

    #[test]
    fn rotation_due_touches_stamp_and_fresh_stamp_debounces() {
        let (_dir, store) = test_store();
        let second = Store::open(store.paths().clone(), store.runtime_paths().clone())
            .expect("second store handle");
        let registered = observation(LifecycleSignal::Registered);
        let intent = || AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "SessionStart",
            observation: &registered,
            transition: None,
        };

        assert_eq!(
            second
                .append_agent_lifecycle_with_threshold(intent(), 0)
                .expect("rotation due"),
            AgentLifecycleOutcome::RotationDue
        );
        let stamp = store.paths().locks_dir.join(AUTO_ROTATE_STAMP);
        assert!(stamp.exists());
        assert_eq!(
            store
                .append_agent_lifecycle_with_threshold(intent(), 0)
                .expect("fresh stamp debounces"),
            AgentLifecycleOutcome::Appended
        );
        OpenOptions::new()
            .write(true)
            .open(&stamp)
            .expect("open rotation stamp")
            .set_times(
                FileTimes::new().set_modified(
                    SystemTime::now() - AUTO_ROTATE_DEBOUNCE - Duration::from_secs(1),
                ),
            )
            .expect("age rotation stamp");
        assert!(
            debounce::stamp_due(&stamp, AUTO_ROTATE_DEBOUNCE),
            "aged stamp must pass debounce"
        );
        assert_eq!(
            store
                .append_agent_lifecycle_with_threshold(intent(), 0)
                .expect("aged stamp is due"),
            AgentLifecycleOutcome::RotationDue
        );
        OpenOptions::new()
            .write(true)
            .open(&stamp)
            .expect("open rotation stamp")
            .set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)))
            .expect("future-date rotation stamp");
        assert_eq!(
            store
                .append_agent_lifecycle_with_threshold(intent(), 0)
                .expect("future stamp is due"),
            AgentLifecycleOutcome::RotationDue
        );
    }

    #[test]
    fn failed_lifecycle_append_does_not_touch_rotation_stamp() {
        let (_dir, store) = test_store();
        let stamp = store.paths().locks_dir.join(AUTO_ROTATE_STAMP);
        std::fs::create_dir(&store.paths().events_log).expect("block event log with directory");
        let registered = observation(LifecycleSignal::Registered);

        assert!(
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: AgentKind::new_unchecked("claude"),
                    event_name: "SessionStart",
                    observation: &registered,
                    transition: None,
                })
                .is_err()
        );
        assert!(!stamp.exists());
    }
}
