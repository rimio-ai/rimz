use std::fs::{FileTimes, OpenOptions};
use std::time::SystemTime;

use super::*;
use crate::agents::AgentStatus;
use crate::agents::lifecycle::{LifecycleState, TurnPhase};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::ids::{AgentSessionId, MuxName, PaneId, WorkspaceId};
use crate::store::event::EventKind;

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

pub(super) fn test_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    (dir, store)
}

pub(super) fn observation(signal: LifecycleSignal) -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(Some(AgentSessionId::from("sess-1")), signal)
}

#[test]
fn lifecycle_append_gate_keeps_durable_truth_for_progress_signals() {
    let proof = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
    };
    let mutating = LifecycleSignal::ToolUsed {
        mutates: true,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
    };
    let named = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: Some("Read".to_owned()),
        native_key: None,
        turn_id: None,
    };

    assert!(append_lifecycle_event(&mutating, None, false));
    assert!(append_lifecycle_event(&named, None, false));
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
fn lifecycle_receipt_carries_appended_event_and_suppressed_diagnostics() {
    let (_dir, store) = test_store();
    let started = observation(LifecycleSignal::TurnStarted { turn_id: None });
    let appended = store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "UserPromptSubmit",
            observation: &started,
            spawned_subagents: &[],
        })
        .expect("append lifecycle event");
    assert_eq!(
        appended.primary_event_id.as_ref(),
        Some(&appended.events[0].event_id)
    );
    assert_eq!(appended.events.len(), 1);
    assert_eq!(
        appended.events[0].signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );

    let proof = observation(LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
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
            spawned_subagents: &[],
        })
        .expect("suppress proof-only event");
    assert!(suppressed.primary_event_id.is_none());
    assert!(suppressed.events.is_empty());
    assert_eq!(suppressed.prior_status, Some(AgentStatus::Running));
    assert!(suppressed.transition.is_some());
    assert_eq!(
        std::fs::metadata(&store.paths().events_log)
            .map(|meta| meta.len())
            .unwrap_or(0),
        before
    );
    assert!(!store.paths().locks_dir.join(AUTO_ROTATE_STAMP).exists());
    let events = store.read_events().expect("read events");
    assert_eq!(events.len(), 1);
    let EventKind::AgentLifecycle(payload) = events[0].kind() else {
        panic!("agent lifecycle event")
    };
    assert_eq!(payload.event_name.as_deref(), Some("UserPromptSubmit"));
    assert_eq!(appended.events[0].event_id, events[0].event_id);
    assert_eq!(appended.events[0].at, events[0].timestamp);
}

#[test]
fn side_conversation_receipts_append_only_the_registration() {
    let (_dir, store) = test_store();
    let append = |event_name, observation: &AgentLifecycleObservation, threshold| {
        store
            .append_agent_lifecycle_with_threshold(
                AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: AgentKind::new_unchecked("codex"),
                    event_name,
                    observation,
                    spawned_subagents: &[],
                },
                threshold,
            )
            .expect("append lifecycle")
    };
    let in_root_process = |mut observation: AgentLifecycleObservation| {
        observation.pane_id = Some(PaneId::parse("tmux:%1").unwrap());
        observation.agent_pid = Some(std::process::id());
        observation
    };
    let mut root = in_root_process(observation(LifecycleSignal::Registered));
    assert!(
        append("SessionStart", &root, u64::MAX)
            .side_conversation
            .is_none()
    );
    root.signal = LifecycleSignal::TurnStarted { turn_id: None };
    append("UserPromptSubmit", &root, u64::MAX);
    let agents = || snapshot::catch_up_rollup(store.paths()).expect("rollup").1;
    let before = agents();
    assert_eq!(before.len(), 1);
    let mut side = in_root_process(AgentLifecycleObservation::new(
        Some(AgentSessionId::from("side")),
        LifecycleSignal::Registered,
    ));
    side.origin = Some(SessionOrigin::SideConversation);
    let receipt = append("SessionStart", &side, 0);
    assert_eq!(receipt.side_conversation, Some(SideConversation::default()));
    assert!(receipt.primary_event_id.is_some());
    assert!(receipt.events.is_empty());
    assert!(receipt.prior_status.is_none());
    assert!(receipt.transition.is_none());
    assert!(!receipt.waiting_cleared);
    assert!(receipt.rotation_due);
    assert_eq!(store.read_events().unwrap().len(), 3);

    side.origin = None;
    side.signal = LifecycleSignal::TurnStarted { turn_id: None };
    let receipt = append("UserPromptSubmit", &side, 0);
    assert_eq!(
        receipt,
        AgentLifecycleReceipt {
            prior_status: None,
            transition: None,
            waiting_cleared: false,
            primary_event_id: None,
            events: Vec::new(),
            rotation_due: false,
            side_conversation: Some(SideConversation {
                host: Some(AgentSessionId::from("sess-1")),
                host_running: true,
            }),
        }
    );
    assert_eq!(store.read_events().unwrap().len(), 3);
    assert_eq!(agents(), before);
}

#[test]
fn late_turn_reports_leave_the_started_turn_on_ingest_and_replay() {
    let (_dir, store) = test_store();
    let append = |event_name, signal| {
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("grok"),
                event_name,
                observation: &observation(signal),
                spawned_subagents: &[],
            })
            .expect("append lifecycle event")
    };
    let turn_id = |id: &str| Some(id.to_owned());
    let dropped = |receipt: &AgentLifecycleReceipt| {
        receipt
            .transition
            .is_some_and(|transition| matches!(transition.kind, TransitionKind::Ignored { .. }))
    };
    append(
        "UserPromptSubmit",
        LifecycleSignal::TurnStarted {
            turn_id: turn_id("prompt-1"),
        },
    );
    append(
        "UserPromptSubmit",
        LifecycleSignal::TurnStarted {
            turn_id: turn_id("prompt-2"),
        },
    );

    let late = append(
        "StopCancelled",
        LifecycleSignal::TurnInterrupted {
            turn_id: turn_id("prompt-1"),
        },
    );
    assert!(dropped(&late), "{late:?}");
    let agent = &store.snapshot().unwrap().agents[0];
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.started_turn_id.as_deref(), Some("prompt-2"));

    let completed = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: turn_id("prompt-2"),
    };
    assert!(!dropped(&append("Stop", completed)));
    let killed_stop_hook = append(
        "StopCancelled",
        LifecycleSignal::TurnInterrupted {
            turn_id: turn_id("prompt-2"),
        },
    );
    assert!(dropped(&killed_stop_hook), "{killed_stop_hook:?}");
    assert_eq!(
        store.snapshot().unwrap().agents[0].status,
        AgentStatus::Success
    );

    // prompt-3's start never reached the store; its own report still settles it.
    append(
        "PostToolUse",
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: None,
            turn_id: None,
        },
    );
    let unseen_turn = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: turn_id("prompt-3"),
    };
    assert!(!dropped(&append("Stop", unseen_turn)));
    assert_eq!(
        store.snapshot().unwrap().agents[0].status,
        AgentStatus::Success
    );
}

#[test]
fn read_only_tool_uses_latest_durable_waiting_state() {
    let (_dir, store) = test_store();
    let kind = AgentKind::new_unchecked("claude");
    for (event_name, signal) in [
        (
            "UserPromptSubmit",
            LifecycleSignal::TurnStarted { turn_id: None },
        ),
        (
            "PermissionRequest",
            LifecycleSignal::AwaitingInput {
                ask_id: None,
                kind: crate::agents::AskKind::Permission,
                detail: None,
                native_key: None,
            },
        ),
    ] {
        let observation = observation(signal);
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind.clone(),
                event_name,
                observation: &observation,
                spawned_subagents: &[],
            })
            .expect("seed lifecycle state");
    }

    let proof = observation(LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
    });
    let receipt = store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: kind,
            event_name: "PostToolUse",
            observation: &proof,
            spawned_subagents: &[],
        })
        .expect("record waiting-clearing proof");

    assert_eq!(receipt.prior_status, Some(AgentStatus::Waiting));
    assert!(receipt.waiting_cleared);
    assert!(receipt.primary_event_id.is_some());
    assert_eq!(
        store.snapshot_cached().unwrap().agents[0].status,
        AgentStatus::Running
    );
}

#[test]
fn observed_child_adoption_is_guarded_flattened_and_ordered() {
    let (_dir, store) = test_store();
    let kind = AgentKind::new_unchecked("pi");
    let pane = PaneId::from_parts(MuxName::Tmux, "%1");
    let foreign_pane = PaneId::from_parts(MuxName::Tmux, "%2");
    let append = |event_name: &str, observation: &AgentLifecycleObservation| {
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind.clone(),
                event_name,
                observation,
                spawned_subagents: &[],
            })
            .expect("append lifecycle")
    };

    let mut root = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("root")),
        LifecycleSignal::Registered,
    );
    root.pane_id = Some(pane.clone());
    append("SessionStart", &root);
    let mut nested = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("nested")),
        LifecycleSignal::SubagentStarted,
    );
    nested.parent_agent_id = Some(AgentSessionId::from("root"));
    nested.task = Some("nested parent".to_owned());
    nested.pane_id = Some(pane.clone());
    append("SubagentStart", &nested);

    let mut child = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child")),
        LifecycleSignal::Registered,
    );
    child.pane_id = Some(pane.clone());
    append("SessionStart", &child);
    let mut observed = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child")),
        LifecycleSignal::SubagentStarted,
    );
    observed.parent_agent_id = Some(AgentSessionId::from("nested"));
    observed.task = Some("adopt me".to_owned());
    observed.pane_id = Some(pane.clone());
    let receipt = append("SubagentStart", &observed);
    assert_eq!(receipt.events.len(), 2);
    assert_eq!(receipt.events[0].prior_status, Some(AgentStatus::Idle));
    assert_eq!(receipt.events[1].prior_status, Some(AgentStatus::Running));
    assert_eq!(receipt.events[1].parent_agent_id.as_deref(), Some("root"));
    let events = store.read_events().unwrap();
    let appended = &events[events.len() - 2..];
    let names = appended
        .iter()
        .map(|event| match event.kind() {
            EventKind::AgentLifecycle(payload) => payload.event_name.unwrap_or_default(),
            _ => String::new(),
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["SubagentStart", "SubagentAdopted"]);
    for (event, envelope) in receipt.events.iter().zip(appended) {
        assert_eq!(event.event_id, envelope.event_id);
        assert_eq!(event.at, envelope.timestamp);
    }
    assert_eq!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "child")
            .and_then(|state| state.parent_agent_id.as_deref()),
        Some("root")
    );
    assert_eq!(append("SubagentStart", &observed).events.len(), 1);

    let mut foreign = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("foreign")),
        LifecycleSignal::Registered,
    );
    foreign.pane_id = Some(foreign_pane);
    append("SessionStart", &foreign);
    foreign.signal = LifecycleSignal::SubagentStarted;
    foreign.parent_agent_id = Some(AgentSessionId::from("nested"));
    foreign.task = Some("wrong pane".to_owned());
    foreign.pane_id = Some(pane);
    assert_eq!(append("SubagentStart", &foreign).events.len(), 1);
    assert_eq!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "foreign")
            .and_then(|state| state.parent_agent_id.as_ref()),
        None
    );
}

#[test]
fn ingress_stamps_the_rooms_account_only_on_rows_it_creates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace =
        crate::workspace::WorkspaceResolver::resolve(dir.path(), None).expect("workspace");
    let paths = StatePaths::under(workspace.workspace_id.clone(), dir.path()).expect("state");
    let runtime = RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).expect("runtime");
    let store = Store::open(paths.clone(), runtime).expect("open store");
    let append = |kind: &str, id: &str, signal: LifecycleSignal| {
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked(kind),
                event_name: "SessionStart",
                observation: &AgentLifecycleObservation::new(
                    Some(AgentSessionId::from(id)),
                    signal,
                ),
                spawned_subagents: &[],
            })
            .expect("append lifecycle")
    };
    let login = |id: &str| {
        snapshot::catch_up_rollup(&paths)
            .expect("rollup")
            .1
            .into_iter()
            .find(|state| state.agent_id == id)
            .map(|state| state.login)
    };

    append("claude", "before", LifecycleSignal::Registered);
    let work = "work".parse::<LoginName>().expect("login name");
    store
        .record_room_logins(
            &workspace,
            &crate::ids::RoomLogins::from([(AgentKind::new_unchecked("claude"), work.clone())]),
        )
        .expect("freeze accounts");
    append("claude", "before", LifecycleSignal::Registered);
    append("claude", "resumed", LifecycleSignal::Registered);
    append(
        "claude",
        "unseen",
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    append("codex", "other", LifecycleSignal::Registered);

    assert_eq!(login("before"), Some(None));
    assert_eq!(login("resumed"), Some(Some(work.clone())));
    assert_eq!(login("unseen"), Some(Some(work)));
    assert_eq!(login("other"), Some(None));
}

#[test]
fn spawned_child_reconciliation_closes_running_then_dedupes_by_metadata() {
    let (_dir, store) = test_store();
    let kind = AgentKind::new_unchecked("copilot");
    let pane = PaneId::from_parts(MuxName::Tmux, "%1");
    let mut parent = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("parent")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    parent.pane_id = Some(pane.clone());
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: kind.clone(),
            event_name: "TurnStart",
            observation: &parent,
            spawned_subagents: &[],
        })
        .unwrap();
    let mut child = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child")),
        LifecycleSignal::SubagentStarted,
    );
    child.parent_agent_id = Some(AgentSessionId::from("parent"));
    child.task = Some("child task".to_owned());
    child.launch.model = Some("old-model".to_owned());
    child.usage.total_tokens = Some(10);
    child.pane_id = Some(pane);
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: kind.clone(),
            event_name: "SubagentStart",
            observation: &child,
            spawned_subagents: &[],
        })
        .unwrap();

    parent.signal = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
    };
    let spawned = |model: &str, total_tokens| SpawnedSubagent {
        child_agent_id: AgentSessionId::from("child"),
        agent_name: Some("child".to_owned()),
        role: Some("coder".to_owned()),
        prompt: Some("child task".to_owned()),
        model: Some(model.to_owned()),
        total_tokens: Some(total_tokens),
    };
    for (facts, expected) in [
        (spawned("old-model", 10), 1),
        (spawned("old-model", 10), 0),
        (spawned("new-model", 20), 1),
        (spawned("new-model", 20), 0),
    ] {
        let receipt = store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind.clone(),
                event_name: "PostToolUse",
                observation: &parent,
                spawned_subagents: std::slice::from_ref(&facts),
            })
            .unwrap();
        assert_eq!(receipt.events.len(), expected);
        if let Some(event) = receipt.events.first() {
            assert!(matches!(
                event.transition,
                crate::agents::LifecycleTransition::Normal
            ));
            assert!(event.prior_status.is_some());
            assert_eq!(event.status, AgentStatus::Success);
        }
    }
    let state = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|state| state.agent_id == "child")
        .unwrap();
    assert_eq!(state.model.as_deref(), Some("new-model"));
    assert_eq!(state.usage.total_tokens, Some(20));
    assert_eq!(state.status, AgentStatus::Success);
}

#[test]
fn missing_unreadable_and_future_stamps_are_due() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.stamp");
    assert!(debounce::stamp_due(&missing, AUTO_ROTATE_DEBOUNCE));

    #[cfg(unix)]
    {
        let unreadable = dir.path().join("unreadable.stamp");
        std::os::unix::fs::symlink("unreadable.stamp", &unreadable).expect("create symlink loop");
        assert!(debounce::stamp_due(&unreadable, AUTO_ROTATE_DEBOUNCE));
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
                spawned_subagents: &[],
            })
            .is_err()
    );
    assert!(!stamp.exists());
}
