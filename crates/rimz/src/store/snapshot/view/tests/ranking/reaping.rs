use super::*;
use crate::agents::SessionOrigin::{self, Forked, Fresh};

fn session(kind: &str, id: &str, age_secs: i64) -> AgentState {
    agent(kind, id, AgentStatus::Running, 0)
        .worktree(&format!("/repo/{id}"))
        .active_ago(age_secs)
}

fn pane_session(kind: &str, id: &str, pane: &str, age_secs: i64) -> AgentState {
    session(kind, id, age_secs).in_pane(pane)
}

fn with_owner(
    mut agent: AgentState,
    kind: RuntimeOwnerKind,
    pid: u32,
    process_start: Option<&str>,
) -> AgentState {
    agent.runtime_owner = Some(RuntimeOwner::new(
        kind,
        agent.agent_id.to_string(),
        pid,
        process_start.map(str::to_owned),
    ));
    agent
}

fn with_origin(mut agent: AgentState, origin: SessionOrigin) -> AgentState {
    agent.origin = Some(origin);
    agent
}

fn with_pane_start(mut agent: AgentState, age_secs: i64) -> AgentState {
    agent
        .pane
        .as_mut()
        .expect("pane session")
        .pane_process_start = Some(ago(age_secs));
    agent
}

fn assert_survivors(label: &str, agents: Vec<AgentState>, expected: &[&str]) {
    let mut snapshot = room(agents);
    snapshot.reap_stale_sessions();
    let mut ids = snapshot
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, expected, "{label}");
}

#[test]
fn root_session_reaper_drops_only_unprovable_ghosts() {
    assert_survivors(
        "strict ghost TTL and runtime-owner classification",
        vec![
            session("claude", "stale-ownerless", GHOST_SESSION_TTL_SECS + 1),
            with_owner(
                session("codex", "stale-daemon", GHOST_SESSION_TTL_SECS + 1),
                RuntimeOwnerKind::Daemon,
                77,
                None,
            ),
            session("claude", "boundary", GHOST_SESSION_TTL_SECS),
            with_owner(
                session("codex", "agent-owned", GHOST_SESSION_TTL_SECS + 1),
                RuntimeOwnerKind::Agent,
                88,
                None,
            ),
        ],
        &["agent-owned", "boundary"],
    );

    assert_survivors(
        "paneless roots collapse only within one worktree and branch",
        vec![
            session("codex", "older", 120)
                .worktree("/repo/a")
                .branch("main"),
            session("codex", "newer", 60)
                .worktree("/repo/a")
                .branch("main"),
        ],
        &["newer"],
    );
    assert_survivors(
        "a stamped root cannot prove a paneless root dead",
        vec![
            session("codex", "older", 120)
                .worktree("/repo/a")
                .branch("main"),
            pane_session("codex", "newer", "%2", 60)
                .worktree("/repo/a")
                .branch("main"),
        ],
        &["newer", "older"],
    );

    assert_survivors(
        "a different owner relaunch in one pane supersedes across checkout",
        vec![
            with_owner(
                pane_session("claude", "older", "%1", 120)
                    .worktree("/repo/a")
                    .branch("main"),
                RuntimeOwnerKind::Agent,
                111,
                None,
            ),
            with_owner(
                pane_session("claude", "newer", "%1", 60)
                    .worktree("/repo/a")
                    .branch("feature"),
                RuntimeOwnerKind::Agent,
                222,
                None,
            ),
        ],
        &["newer"],
    );
    assert_survivors(
        "distinct stamped panes remain concurrent",
        vec![
            with_owner(
                pane_session("claude", "older", "%1", 120),
                RuntimeOwnerKind::Agent,
                111,
                None,
            ),
            with_owner(
                pane_session("claude", "newer", "%2", 60),
                RuntimeOwnerKind::Agent,
                222,
                None,
            ),
        ],
        &["newer", "older"],
    );

    assert_survivors(
        "fresh same-pane roots replace older roots but preserve forks",
        vec![
            with_origin(pane_session("codex", "older", "%1", 120), Fresh),
            with_origin(pane_session("codex", "fork", "%1", 90), Forked),
            with_origin(pane_session("codex", "newer", "%1", 60), Fresh),
        ],
        &["fork", "newer"],
    );
    assert_survivors(
        "missing older lineage fails safe",
        vec![
            pane_session("codex", "older", "%1", 120),
            with_origin(pane_session("codex", "newer", "%1", 60), Fresh),
        ],
        &["newer", "older"],
    );
    assert_survivors(
        "missing newer lineage fails safe",
        vec![
            with_origin(pane_session("codex", "older", "%1", 120), Fresh),
            pane_session("codex", "newer", "%1", 60),
        ],
        &["newer", "older"],
    );
    assert_survivors(
        "fresh roots in distinct panes remain concurrent",
        vec![
            with_origin(pane_session("codex", "older", "%1", 120), Fresh),
            with_origin(pane_session("codex", "newer", "%2", 60), Fresh),
        ],
        &["newer", "older"],
    );
    assert_survivors(
        "a paneless fresh root does not yield to a stamped fresh root",
        vec![
            with_origin(session("codex", "older", 120), Fresh),
            with_origin(pane_session("codex", "newer", "%1", 60), Fresh),
        ],
        &["newer", "older"],
    );

    let follow_latest = |id, age, token| {
        with_owner(
            pane_session("antigravity", id, "%1", age),
            RuntimeOwnerKind::Agent,
            9_999,
            Some(token),
        )
    };
    assert_survivors(
        "follow-latest providers replace same-process conversations",
        vec![
            follow_latest("older", 120, "process-a"),
            follow_latest("newer", 60, "process-a"),
        ],
        &["newer"],
    );
    assert_survivors(
        "follow-latest process identity mismatch fails safe",
        vec![
            follow_latest("older", 120, "process-a"),
            follow_latest("newer", 60, "process-b"),
        ],
        &["newer", "older"],
    );
    assert_survivors(
        "follow-latest pane incarnation mismatch fails safe",
        vec![
            with_pane_start(follow_latest("older", 120, "process-a"), 600),
            with_pane_start(follow_latest("newer", 60, "process-a"), 300),
        ],
        &["newer", "older"],
    );
}

#[test]
fn reaper_never_drops_a_subagent() {
    assert_survivors(
        "subagents survive both ghost TTL and root supersession rules",
        vec![
            session("claude", "sess-root", 0),
            child_state(
                "sess-root",
                "child-old",
                AgentStatus::Idle,
                GHOST_SESSION_TTL_SECS + 1,
            ),
            child_state("sess-root", "child-new", AgentStatus::Running, 5),
        ],
        &["child-new", "child-old", "sess-root"],
    );
}
