use std::time::SystemTime;

use super::*;
use crate::agents::{AgentAdapter, AmpAdapter, LaunchParams, SessionOrigin};
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use crate::store::event::EventKind;
use crate::store::event_log;
use crate::store::paths::RuntimePaths;
use serde_json::json;

fn store() -> (tempfile::TempDir, Store, WorkspaceId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    (dir, store, workspace_id)
}

fn lifecycle(
    workspace_id: &WorkspaceId,
    agent_id: &str,
    pid: Option<u32>,
    parent: Option<&str>,
) -> EventEnvelope {
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::Registered,
    );
    observation.launch = LaunchParams::default();
    observation.agent_pid = pid;
    observation.parent_agent_id = parent.map(AgentSessionId::from);
    observation.worktree_path = Some(format!("/repo/{agent_id}"));
    EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        "rimz-test",
        "codex",
        if parent.is_some() {
            "SubagentStart"
        } else {
            "SessionStart"
        },
        &observation,
    )
}

fn worktree_lifecycle(
    workspace_id: &WorkspaceId,
    agent_id: &str,
    pid: Option<u32>,
    parent: Option<&str>,
    path: &str,
    branch: &str,
    signal: LifecycleSignal,
) -> EventEnvelope {
    let event_name = if matches!(&signal, LifecycleSignal::Ended) {
        "SessionEnd"
    } else if parent.is_some() {
        "SubagentStart"
    } else {
        "SessionStart"
    };
    let mut observation =
        AgentLifecycleObservation::new(Some(AgentSessionId::from(agent_id)), signal);
    observation.agent_pid = pid;
    observation.parent_agent_id = parent.map(AgentSessionId::from);
    observation.worktree_path = Some(path.to_owned());
    observation.worktree_branch = Some(branch.to_owned());
    EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        "rimz-test",
        "codex",
        event_name,
        &observation,
    )
}

fn fresh_pane_lifecycle(
    workspace_id: &WorkspaceId,
    agent_id: &str,
    pane_id: &str,
) -> EventEnvelope {
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::Registered,
    );
    observation.agent_pid = Some(std::process::id());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, pane_id));
    observation.origin = Some(SessionOrigin::Fresh);
    EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        "rimz-test",
        "codex",
        "SessionStart",
        &observation,
    )
}

fn turn_started_lifecycle(workspace_id: &WorkspaceId, agent_id: &str) -> EventEnvelope {
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::TurnStarted,
    );
    EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        "rimz-test",
        "codex",
        "UserPromptSubmit",
        &observation,
    )
}

fn amp_focus_lifecycle(workspace_id: &WorkspaceId, agent_id: &str, pane_id: &str) -> EventEnvelope {
    let mut observation = AmpAdapter
        .decode_hook(
            "session_start",
            &json!({ "session_id": agent_id, "cwd": "/repo" }),
        )
        .expect("Amp session start decodes")
        .lifecycle()
        .cloned()
        .expect("Amp session start observation");
    observation.agent_pid = Some(std::process::id());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, pane_id));
    EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        "rimz-test",
        "amp",
        "session_start",
        &observation,
    )
}

fn daemon_lifecycle(workspace_id: &WorkspaceId, agent_id: &str) -> EventEnvelope {
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::Registered,
    );
    observation.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Daemon,
        agent_id,
        std::process::id(),
        None,
    ));
    EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        "rimz-test",
        "codex",
        "SessionStart",
        &observation,
    )
}

#[cfg(unix)]
#[test]
fn reap_dead_sessions_stamps_store_provable_roots_once() {
    let (_dir, store, workspace_id) = store();
    let now = Timestamp::now();
    let mut stale_ownerless = lifecycle(&workspace_id, "stale-ownerless", None, None);
    stale_ownerless.timestamp =
        now - Duration::from_secs((session_death::GHOST_SESSION_TTL_SECS + 60) as u64);
    let mut stale_daemon = daemon_lifecycle(&workspace_id, "stale-daemon");
    stale_daemon.timestamp =
        now - Duration::from_secs((session_death::GHOST_SESSION_TTL_SECS + 60) as u64);
    let mut cleared = fresh_pane_lifecycle(&workspace_id, "cleared", "%1");
    cleared.timestamp = now - Duration::from_secs(2);
    let mut replacement = fresh_pane_lifecycle(&workspace_id, "replacement", "%1");
    replacement.timestamp = now - Duration::from_secs(1);
    for event in [
        lifecycle(&workspace_id, "dead-root", Some(u32::MAX), None),
        stale_ownerless,
        stale_daemon,
        lifecycle(&workspace_id, "fresh-ownerless", None, None),
        lifecycle(&workspace_id, "live-root", Some(std::process::id()), None),
        cleared,
        replacement,
        lifecycle(
            &workspace_id,
            "dead-child",
            Some(u32::MAX),
            Some("dead-root"),
        ),
    ] {
        event_log::append(&store.paths().events_log, &event).expect("append event");
    }

    let reaped = store.reap_dead_sessions().expect("reap dead sessions");

    assert_eq!(reaped, 4);
    let projection = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("audit projection");
    let ids = projection
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&"fresh-ownerless")
            && ids.contains(&"live-root")
            && ids.contains(&"replacement")
            && ids.contains(&"dead-root")
            && ids.contains(&"stale-ownerless")
            && ids.contains(&"stale-daemon")
            && ids.contains(&"cleared"),
        "audit retains active and ended roots: {ids:?}"
    );
    for agent_id in ["dead-root", "stale-ownerless", "stale-daemon", "cleared"] {
        assert!(
            projection
                .agents
                .iter()
                .any(|agent| agent.agent_id == agent_id && agent.ended_at.is_some()),
            "missing end stamp for {agent_id}"
        );
    }
    let events = store.read_events().expect("read reap events");
    for (agent_id, event_name) in [
        ("dead-root", "ReapedDead"),
        ("stale-ownerless", "ReapedStale"),
        ("stale-daemon", "ReapedStale"),
        ("cleared", "ReapedSuperseded"),
    ] {
        assert!(
            events.iter().any(|event| {
                matches!(
                    event.kind(),
                    EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some(event_name)
                            && payload.observation.agent_id.as_deref() == Some(agent_id)
                )
            }),
            "missing {event_name} end stamp event for {agent_id}"
        );
    }
    assert_eq!(
        store.reap_dead_sessions().expect("reap again"),
        0,
        "second pass is idempotent and subagents are not reaped independently"
    );
}

#[cfg(unix)]
#[test]
fn amp_focus_switch_retires_then_revives_threads_in_one_pane() {
    let (_dir, store, workspace_id) = store();
    let now = Timestamp::now();
    let mut thread_a = amp_focus_lifecycle(&workspace_id, "T-a", "%1");
    thread_a.timestamp = now - Duration::from_secs(2);
    let mut thread_b = amp_focus_lifecycle(&workspace_id, "T-b", "%1");
    thread_b.timestamp = now - Duration::from_secs(1);
    for event in [thread_a, thread_b] {
        event_log::append(&store.paths().events_log, &event).expect("append focus event");
    }

    assert_eq!(store.reap_dead_sessions().expect("reap thread A"), 1);
    let after_a_to_b = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("projection after A to B");
    assert!(
        after_a_to_b
            .agents
            .iter()
            .any(|agent| { agent.agent_id == "T-a" && agent.ended_at.is_some() }),
        "expected T-a ended in {:#?}",
        after_a_to_b.agents
    );
    assert!(
        after_a_to_b
            .agents
            .iter()
            .any(|agent| { agent.agent_id == "T-b" && agent.ended_at.is_none() }),
        "expected T-b active in {:#?}",
        after_a_to_b.agents
    );

    let mut thread_a = amp_focus_lifecycle(&workspace_id, "T-a", "%1");
    thread_a.timestamp = Timestamp::now() + Duration::from_secs(1);
    event_log::append(&store.paths().events_log, &thread_a).expect("append switch-back event");

    assert_eq!(store.reap_dead_sessions().expect("reap thread B"), 1);
    let after_b_to_a = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("projection after B to A");
    assert!(
        after_b_to_a
            .agents
            .iter()
            .any(|agent| { agent.agent_id == "T-a" && agent.ended_at.is_none() })
    );
    assert!(
        after_b_to_a
            .agents
            .iter()
            .any(|agent| { agent.agent_id == "T-b" && agent.ended_at.is_some() })
    );
}

#[cfg(unix)]
#[test]
fn interrupted_replacement_bypasses_roster_and_replays_durably() {
    let (_dir, store, workspace_id) = store();
    let rollouts = tempfile::tempdir().expect("rollout tempdir");
    let now = Timestamp::now();
    let mut older = fresh_pane_lifecycle(&workspace_id, "interrupted", "%1");
    older.timestamp = now - Duration::from_secs(4);
    let mut turn_started = turn_started_lifecycle(&workspace_id, "interrupted");
    turn_started.timestamp = now - Duration::from_secs(3);
    let interrupted_at = now - Duration::from_secs(2);
    let mut replacement = fresh_pane_lifecycle(&workspace_id, "replacement", "%1");
    replacement.timestamp = now - Duration::from_secs(1);
    for event in [older, turn_started, replacement] {
        event_log::append(&store.paths().events_log, &event).expect("append lifecycle");
    }
    live_roster::publish(
        &store.paths().live_roster,
        [(
            AgentKind::new_unchecked("codex"),
            AgentSessionId::from("interrupted"),
        )]
        .into_iter()
        .collect(),
    )
    .expect("publish roster");

    assert_eq!(
        crate::agents::codex::with_codex_sessions_root(rollouts.path(), || {
            store.reap_dead_sessions().expect("reap without evidence")
        }),
        0,
        "structural conflict alone keeps the running owner"
    );

    let record = json!({
        "timestamp": interrupted_at.to_string(),
        "type": "event_msg",
        "payload": { "type": "turn_aborted", "reason": "interrupted" }
    });
    std::fs::write(
        rollouts.path().join("rollout-interrupted.jsonl"),
        format!("{record}\n"),
    )
    .expect("write interrupted rollout");

    assert_eq!(
        crate::agents::codex::with_codex_sessions_root(rollouts.path(), || {
            store.reap_dead_sessions().expect("reap interrupted owner")
        }),
        1
    );
    assert_eq!(store.reap_dead_sessions().expect("idempotent reap"), 0);

    let audit = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("replay audit projection");
    assert!(
        audit
            .agents
            .iter()
            .any(|agent| { agent.agent_id == "interrupted" && agent.ended_at.is_some() })
    );
    assert!(
        audit
            .agents
            .iter()
            .any(|agent| { agent.agent_id == "replacement" && agent.ended_at.is_none() })
    );
    assert!(
        store
            .read_events()
            .expect("read events")
            .iter()
            .any(|event| {
                matches!(
                    event.kind(),
                    EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("ReapedInterrupted")
                            && payload.observation.agent_id.as_deref() == Some("interrupted")
                )
            })
    );
}

#[cfg(unix)]
#[test]
fn live_roster_does_not_protect_superseded_owner() {
    let (_dir, store, workspace_id) = store();
    let now = Timestamp::now();
    let mut older = fresh_pane_lifecycle(&workspace_id, "older", "%1");
    older.timestamp = now - Duration::from_secs(2);
    let mut replacement = fresh_pane_lifecycle(&workspace_id, "replacement", "%1");
    replacement.timestamp = now - Duration::from_secs(1);
    for event in [older, replacement] {
        event_log::append(&store.paths().events_log, &event).expect("append lifecycle");
    }
    let older_key = (
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("older"),
    );
    live_roster::publish(
        &store.paths().live_roster,
        [older_key.clone()].into_iter().collect(),
    )
    .expect("publish roster");

    assert_eq!(store.reap_dead_sessions().expect("reap superseded"), 1);
    let audit = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("audit projection");
    assert!(audit.ended.contains(&older_key));
}

#[cfg(unix)]
#[test]
fn live_roster_protects_crash_recovery_candidate_until_removed() {
    let (_dir, store, workspace_id) = store();
    event_log::append(
        &store.paths().events_log,
        &lifecycle(&workspace_id, "recoverable", Some(u32::MAX), None),
    )
    .expect("append dead-owner event");
    let key = (
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("recoverable"),
    );
    live_roster::publish(
        &store.paths().live_roster,
        [key.clone()].into_iter().collect(),
    )
    .expect("publish roster");

    assert_eq!(store.reap_dead_sessions().expect("guarded reap"), 0);
    let guarded = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("guarded projection");
    assert!(guarded.agents.iter().any(|agent| agent.agent_id == key.1));
    assert!(!guarded.ended.contains(&key));

    std::fs::remove_file(&store.paths().live_roster).expect("remove roster");
    assert_eq!(store.reap_dead_sessions().expect("unguarded reap"), 1);
    let reaped = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("reaped projection");
    assert!(
        reaped
            .agents
            .iter()
            .any(|agent| agent.agent_id == key.1 && agent.ended_at.is_some())
    );
    assert!(reaped.ended.contains(&key));
}

#[cfg(unix)]
#[test]
fn retire_worktree_sessions_ends_matching_dead_and_unknown_roots() {
    let (_dir, store, workspace_id) = store();
    let removed_path = Path::new("/r/a");
    for (agent_id, pid, parent, path, branch) in [
        ("dead-root", Some(u32::MAX), None, "/r/a/./", "b"),
        ("pidless-root", None, None, "/r/b", "a"),
        ("live-root", Some(std::process::id()), None, "/r/a", "a"),
        ("other-root", None, None, "/r/b", "b"),
        ("already-ended", None, None, "/r/a", "a"),
        ("child", None, Some("dead-root"), "/r/a", "a"),
    ] {
        let event = worktree_lifecycle(
            &workspace_id,
            agent_id,
            pid,
            parent,
            path,
            branch,
            LifecycleSignal::Registered,
        );
        event_log::append(&store.paths().events_log, &event).expect("append lifecycle");
    }
    let ended = worktree_lifecycle(
        &workspace_id,
        "already-ended",
        None,
        None,
        "/r/a",
        "a",
        LifecycleSignal::Ended,
    );
    event_log::append(&store.paths().events_log, &ended).expect("append end lifecycle");

    assert_eq!(
        store
            .retire_worktree_sessions(removed_path, Some("a"))
            .expect("retire worktree sessions"),
        2
    );

    let audit = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("audit projection");
    for agent_id in ["dead-root", "pidless-root"] {
        assert!(
            audit
                .agents
                .iter()
                .any(|agent| agent.agent_id == agent_id && agent.ended_at.is_some()),
            "expected {agent_id} to be retired"
        );
    }
    for agent_id in ["live-root", "other-root", "child"] {
        assert!(
            audit
                .agents
                .iter()
                .any(|agent| agent.agent_id == agent_id && agent.ended_at.is_none()),
            "expected {agent_id} to stay active"
        );
    }
    let runtime = store
        .runtime_projection(RuntimeScope::Runtime)
        .expect("runtime projection");
    assert!(
        runtime
            .agents
            .iter()
            .all(|agent| agent.agent_id != "dead-root" && agent.agent_id != "pidless-root")
    );
    let mut retired_ids = store
        .read_events()
        .expect("read lifecycle events")
        .iter()
        .filter_map(|event| match event.kind() {
            EventKind::AgentLifecycle(payload)
                if payload.event_name.as_deref() == Some("WorktreeRemoved") =>
            {
                payload
                    .observation
                    .agent_id
                    .as_ref()
                    .map(|agent_id| agent_id.as_str().to_owned())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    retired_ids.sort();
    assert_eq!(retired_ids, ["dead-root", "pidless-root"]);
}

#[cfg(unix)]
#[test]
fn retire_worktree_sessions_bypasses_live_roster_protection() {
    let (_dir, store, workspace_id) = store();
    let event = lifecycle(&workspace_id, "recoverable", Some(u32::MAX), None);
    event_log::append(&store.paths().events_log, &event).expect("append lifecycle");
    let key = (
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("recoverable"),
    );
    live_roster::publish(
        &store.paths().live_roster,
        [key.clone()].into_iter().collect(),
    )
    .expect("publish roster");

    assert_eq!(
        store
            .retire_worktree_sessions(Path::new("/repo/recoverable"), None)
            .expect("retire protected session"),
        1
    );
    let audit = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("audit projection");
    assert!(audit.ended.contains(&key));
}

#[test]
fn retired_session_revives_on_next_lifecycle_event() {
    let (_dir, store, workspace_id) = store();
    let registered = lifecycle(&workspace_id, "resumed", None, None);
    event_log::append(&store.paths().events_log, &registered).expect("append lifecycle");
    assert_eq!(
        store
            .retire_worktree_sessions(Path::new("/repo/resumed"), None)
            .expect("retire session"),
        1
    );

    let resumed = lifecycle(&workspace_id, "resumed", None, None);
    store
        .append_event(&resumed)
        .expect("append resume lifecycle");
    let audit = store
        .runtime_projection(RuntimeScope::Audit)
        .expect("audit projection");
    assert!(
        audit
            .agents
            .iter()
            .any(|agent| agent.agent_id == "resumed" && agent.ended_at.is_none())
    );
}

#[test]
fn reap_due_tracks_missing_fresh_and_aged_stamp() {
    let (_dir, store, _workspace_id) = store();
    let paths = store.paths();

    assert!(reap_due(paths), "missing stamp is due");
    debounce::touch_stamp(&dead_reap_stamp(paths));
    assert!(!reap_due(paths), "fresh stamp is not due");

    std::fs::File::options()
        .write(true)
        .open(dead_reap_stamp(paths))
        .expect("stamp exists")
        .set_modified(SystemTime::now() - REAP_INTERVAL - Duration::from_secs(1))
        .expect("age stamp");
    assert!(reap_due(paths), "aged stamp is due");
}
