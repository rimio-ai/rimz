use std::collections::{BTreeMap, BTreeSet};

use super::*;

use super::super::view::{attach_sub_agents, row_from_agent, sub_agent_from_state};
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentStatus, LaunchParams, SessionOrigin};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use crate::store::event::{
    AgentAttachPayload, AgentLaunchPayload, AgentLaunchState, EventEnvelope,
};
use crate::store::snapshot::SidebarSnapshot;
use crate::store::snapshot::testkit::*;
use jiff::Timestamp;
use serde_json::json;

fn raw_lifecycle(source: &str, params: serde_json::Value) -> EventEnvelope {
    raw_lifecycle_in(&workspace(), source, params)
}

fn raw_lifecycle_in(
    workspace: &WorkspaceId,
    source: &str,
    params: serde_json::Value,
) -> EventEnvelope {
    EventEnvelope::new(
        workspace.clone(),
        "session",
        source,
        "agent-hook",
        "agent.lifecycle",
        params,
    )
}

fn raw_lifecycle_at(
    source: &str,
    secs_after_epoch: i64,
    params: serde_json::Value,
) -> EventEnvelope {
    let mut event = raw_lifecycle(source, params);
    event.timestamp = Timestamp::from_second(epoch().as_second() + secs_after_epoch).unwrap();
    event
}

fn launch_payload(agent_id: &str, agent_name: &str) -> AgentLaunchPayload {
    AgentLaunchPayload {
        agent_id: agent_id.into(),
        launch_id: None,
        agent_name: agent_name.to_owned(),
        agent_name_explicit: false,
        launch: LaunchParams::default(),
        state: AgentLaunchState::Bound,
        run_id: None,
        pane_id: None,
        runtime_owner: None,
        worktree_path: Some("/tmp/x".to_owned()),
        worktree_branch: Some("main".to_owned()),
        prompt: Some("boot".to_owned()),
        description: None,
    }
}

fn launch_event(kind: &str, payload: AgentLaunchPayload) -> EventEnvelope {
    EventEnvelope::agent_launched(
        workspace(),
        "session",
        &AgentKind::new_unchecked(kind),
        payload,
    )
}

fn attach_event(
    kind: &str,
    agent_id: &str,
    launch_id: Option<&str>,
    pane_id: &str,
    pane_pid: Option<u32>,
    owner_pid: u32,
) -> EventEnvelope {
    EventEnvelope::agent_attached(
        workspace(),
        "session",
        &AgentKind::new_unchecked(kind),
        AgentAttachPayload {
            agent_id: AgentSessionId::from(agent_id),
            launch_id: launch_id.map(AgentSessionId::from),
            pane_id: PaneId::parse(pane_id).expect("pane id"),
            pane_pid,
            runtime_owner: RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                agent_id,
                owner_pid,
                Some(format!("start-{owner_pid}")),
            ),
        },
    )
}

fn raw_launch(
    state: AgentLaunchState,
    agent_id: &str,
    agent_name: &str,
    pane_id: Option<&str>,
) -> EventEnvelope {
    raw_launch_with_description(state, agent_id, agent_name, pane_id, None)
}

fn raw_launch_with_description(
    state: AgentLaunchState,
    agent_id: &str,
    agent_name: &str,
    pane_id: Option<&str>,
    description: Option<&str>,
) -> EventEnvelope {
    launch_event(
        "claude",
        AgentLaunchPayload {
            state,
            pane_id: pane_id.map(|raw| PaneId::parse(raw).expect("pane id")),
            description: description.map(ToOwned::to_owned),
            ..launch_payload(agent_id, agent_name)
        },
    )
}

mod capability;
mod compaction;
mod pane_binding;
mod phase_status;
mod prompt_task;
mod subagents;
mod timestamps;
mod tool_stats;

#[test]
fn attach_moves_only_pane_and_runtime_owner() {
    let prior = reduce_agent_states(&[raw_lifecycle_at(
        "codex",
        1,
        json!({
            "agent_id": "sess-resumed",
            "agent_name": "steady-coder",
            "kind_ordinal": 4,
            "signal": { "signal": "turn_started" },
            "pane_id": "tmux:%1",
            "runtime_owner": {
                "kind": "agent",
                "subject_id": "sess-resumed",
                "pid": 40,
                "process_start": "start-40",
            },
            "prompt": "keep the lifecycle intact",
        }),
    )])
    .pop()
    .expect("prior card");
    let attached = reduce_agent_states_seeded(
        BTreeMap::from([((prior.kind.clone(), prior.agent_id.clone()), prior.clone())]),
        &[attach_event(
            "codex",
            "sess-resumed",
            Some("launch-resumed"),
            "tmux:%4",
            Some(84),
            84,
        )],
    )
    .into_values()
    .next()
    .expect("attached card");

    assert_eq!(attached.registered_at, prior.registered_at);
    assert_eq!(attached.status, prior.status);
    assert_eq!(attached.phase, prior.phase);
    assert_eq!(attached.last_activity, prior.last_activity);
    assert_eq!(attached.turn_started_at, prior.turn_started_at);
    assert_eq!(attached.kind_ordinal, prior.kind_ordinal);
    assert_eq!(attached.name, prior.name);
    let mut expected = prior;
    expected.launch_id = Some(AgentSessionId::from("launch-resumed"));
    expected.pane = Some(PaneRef {
        pane_pid: Some(84),
        ..PaneRef::from_id(PaneId::parse("tmux:%4").expect("pane id"))
    });
    expected.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "sess-resumed",
        84,
        Some("start-84".to_owned()),
    ));
    assert_eq!(attached, expected);
}

#[test]
fn legacy_attach_for_unknown_session_mints_no_card() {
    assert!(
        reduce_agent_states(&[attach_event(
            "codex",
            "unknown",
            None,
            "tmux:%4",
            Some(84),
            84,
        )])
        .is_empty()
    );
}

#[test]
fn identified_attach_seeds_a_discovered_resume() {
    let states = reduce_agent_states(&[attach_event(
        "codex",
        "sess-discovered",
        Some("sess-discovered"),
        "tmux:%4",
        Some(84),
        84,
    )]);
    let state = states
        .iter()
        .find(|state| state.kind == "codex" && state.agent_id == "sess-discovered")
        .expect("identified attach seeds session");
    assert_eq!(
        state.launch_id.as_deref(),
        Some("sess-discovered"),
        "the exported resume identity is durable before the provider starts"
    );
    assert_eq!(
        state.pane.as_ref().map(|pane| &pane.pane_id),
        Some(&PaneId::parse("tmux:%4").expect("pane id"))
    );
}

#[test]
fn later_registration_in_attached_pane_supersedes_resumed_card() {
    let events = vec![
        raw_lifecycle_at(
            "codex",
            1,
            json!({
                "agent_id": "sess-resumed",
                "agent_name": "steady-coder",
                "signal": { "signal": "registered" },
                "runtime_owner": {
                    "kind": "agent",
                    "subject_id": "sess-resumed",
                    "pid": 40,
                    "process_start": "start-40",
                },
            }),
        ),
        attach_event(
            "codex",
            "sess-resumed",
            Some("launch-resumed"),
            "tmux:%4",
            Some(84),
            84,
        ),
        raw_lifecycle_at(
            "codex",
            2,
            json!({
                "agent_id": "sess-new",
                "agent_name": "new-coder",
                "signal": { "signal": "registered" },
                "pane_id": "tmux:%4",
                "runtime_owner": {
                    "kind": "agent",
                    "subject_id": "sess-new",
                    "pid": 85,
                    "process_start": "start-85",
                },
            }),
        ),
    ];
    let mut snapshot = room(reduce_agent_states(&events));
    snapshot.reap_stale_sessions();

    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id.as_str(), "sess-new");
}

#[test]
fn serialized_lifecycle_event_folds_like_constructed_event() {
    let event = raw_lifecycle_at(
        "claude",
        1,
        json!({
            "event_name": "SessionStart",
            "agent_id": "session-a",
            "agent_name": "lucid-atlas",
            "signal": { "signal": "registered" },
            "pane_id": "tmux:%1",
            "context_pct": 42,
            "context_window": 200_000,
            "total_tokens": 84_000,
        }),
    );
    let encoded = serde_json::to_vec(&event).expect("encode event");
    let decoded: EventEnvelope = serde_json::from_slice(&encoded).expect("decode event");

    assert_eq!(
        reduce_agent_states(&[decoded]),
        reduce_agent_states(&[event]),
        "RawValue envelope params must fold identically after event-log round trip"
    );
}

#[test]
fn assigns_and_carries_card_identity() {
    let events = vec![
        raw_lifecycle_at(
            "claude",
            1,
            json!({
                "agent_id": "session-a",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
        raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "session-a",
                "signal": { "signal": "turn_started" },
            }),
        ),
        raw_lifecycle_at(
            "claude",
            3,
            json!({
                "agent_id": "session-b",
                "signal": { "signal": "registered" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);
    let first = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "session-a")
        .expect("session-a");
    assert_eq!(first.name.as_deref(), Some("lucid-atlas"));
    assert_eq!(first.kind_ordinal, Some(1));
    let second = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "session-b")
        .expect("session-b");
    assert!(
        second
            .name
            .as_deref()
            .is_some_and(|name| name.contains('-'))
    );
    assert_ne!(second.name, first.name);
    assert_eq!(second.kind_ordinal, Some(2));
}

#[test]
fn rebirth_resets_ordinals_and_keeps_names() {
    let events = vec![
        raw_lifecycle_at(
            "claude",
            1,
            json!({
                "agent_id": "session-a",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
        EventEnvelope::session_rebirth(workspace(), "session"),
        raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "session-a",
                "signal": { "signal": "registered" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "session-a")
        .expect("session-a");
    assert_eq!(agent.name.as_deref(), Some("lucid-atlas"));
    assert_eq!(agent.kind_ordinal, Some(1));
}

#[test]
fn carries_codex_origin_forward() {
    let events = vec![
        raw_lifecycle_at(
            "codex",
            1,
            json!({
                "agent_id": "root",
                "signal": { "signal": "registered" },
                "origin": "fresh",
            }),
        ),
        raw_lifecycle_at(
            "codex",
            2,
            json!({
                "agent_id": "root",
                "signal": { "signal": "turn_started" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);

    assert_eq!(agents[0].origin, Some(SessionOrigin::Fresh));
}

#[test]
fn bound_launch_creates_a_running_card_and_keeps_the_starting_ordinal() {
    // A Bound launch with a prompt mints one Running/Reasoning card pinned to
    // the bound pane at ordinal 1 — and a prior Starting launch that already
    // claimed that ordinal must not bump it.
    for (label, launches) in [
        (
            "bound only",
            vec![raw_launch(
                AgentLaunchState::Bound,
                "launch_a",
                "lucid-atlas",
                Some("zellij:terminal_1"),
            )],
        ),
        (
            "starting then bound",
            vec![
                raw_launch(AgentLaunchState::Starting, "launch_a", "lucid-atlas", None),
                raw_launch(
                    AgentLaunchState::Bound,
                    "launch_a",
                    "lucid-atlas",
                    Some("zellij:terminal_1"),
                ),
            ],
        ),
    ] {
        let agents = reduce_agent_states(&launches);
        assert_eq!(agents.len(), 1, "{label}");
        let agent = &agents[0];
        assert_eq!(agent.agent_id.as_str(), "launch_a", "{label}");
        assert_eq!(agent.name.as_deref(), Some("lucid-atlas"), "{label}");
        assert_eq!(agent.kind_ordinal, Some(1), "{label}");
        assert_eq!(agent.status, AgentStatus::Running, "{label}");
        assert_eq!(agent.phase, TurnPhase::Reasoning, "{label}");
        assert_eq!(
            agent.pane.as_ref().map(|pane| pane.pane_id.to_string()),
            Some("zellij:terminal_1".to_owned()),
            "{label}"
        );
    }
}

#[test]
fn launch_description_reduces_and_survives_provisional_adoption() {
    let launch = raw_launch_with_description(
        AgentLaunchState::Bound,
        "launch_a",
        "lucid-atlas",
        Some("zellij:terminal_1"),
        Some("port auth"),
    );
    let launched = reduce_agent_states(std::slice::from_ref(&launch));
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].description.as_deref(), Some("port auth"));

    let lifecycle = raw_lifecycle_at(
        "claude",
        2,
        json!({
            "agent_id": "real-session",
            "agent_name": "lucid-atlas",
            "signal": { "signal": "registered" },
        }),
    );
    let adopted = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].agent_id.as_str(), "real-session");
    assert_eq!(adopted[0].description.as_deref(), Some("port auth"));
}

#[test]
fn launched_child_identity_and_ancestry_survive_provider_adoption() {
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some(AgentSessionId::from("launch_child")),
            launch: LaunchParams {
                parent_agent_id: Some(AgentSessionId::from("root-session")),
                parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                launch_depth: Some(2),
                role: Some("reviewer".to_owned()),
                ..Default::default()
            },
            pane_id: Some(PaneId::parse("tmux:%2").expect("pane id")),
            ..launch_payload("launch_child", "reviewer")
        },
    );
    let lifecycle = raw_lifecycle_at(
        "codex",
        2,
        json!({
            "agent_id": "provider-session",
            "agent_name": "reviewer",
            "signal": { "signal": "registered" },
            "pane_id": "tmux:%2",
        }),
    );

    let agents = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(agents.len(), 1);
    let child = &agents[0];
    assert_eq!(child.agent_id.as_str(), "provider-session");
    assert_eq!(child.launch_id.as_deref(), Some("launch_child"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("root-session"));
    assert_eq!(
        child.parent_agent_kind.as_ref(),
        Some(&AgentKind::new_unchecked("claude"))
    );
    assert_eq!(child.launch_depth, Some(2));
    assert_eq!(child.role.as_deref(), Some("reviewer"));
    assert!(child.is_launched_child());
}

#[test]
fn launched_peer_generation_survives_provider_adoption_without_parenting() {
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some(AgentSessionId::from("launch_peer")),
            launch: LaunchParams {
                launch_depth: Some(2),
                ..Default::default()
            },
            pane_id: Some(PaneId::parse("tmux:%2").expect("pane id")),
            ..launch_payload("launch_peer", "reviewer")
        },
    );
    let lifecycle = raw_lifecycle_at(
        "codex",
        2,
        json!({
            "agent_id": "provider-session",
            "agent_name": "reviewer",
            "signal": { "signal": "registered" },
            "pane_id": "tmux:%2",
        }),
    );

    let agents = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(agents.len(), 1);
    let peer = &agents[0];
    assert_eq!(peer.agent_id.as_str(), "provider-session");
    assert_eq!(peer.launch_id.as_deref(), Some("launch_peer"));
    assert_eq!(peer.parent_agent_id, None);
    assert_eq!(peer.parent_agent_kind, None);
    assert_eq!(peer.launch_depth, Some(2));
    assert!(!peer.is_launched_child());
    assert!(!peer.is_provider_subagent());
}

#[test]
fn explicit_launch_name_survives_session_adoption() {
    let launch = launch_event(
        "claude",
        AgentLaunchPayload {
            agent_name_explicit: true,
            ..launch_payload("launch_a", "writer")
        },
    );
    let lifecycle = raw_lifecycle_at(
        "claude",
        2,
        json!({
            "agent_id": "real-session",
            "agent_name": "writer",
            "signal": { "signal": "registered" },
        }),
    );

    let agents = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id.as_str(), "real-session");
    assert_eq!(agents[0].name.as_deref(), Some("writer"));
    assert!(agents[0].name_explicit);
}

#[test]
fn explicit_name_collision_remint_clears_explicit_bit() {
    let first = launch_event(
        "claude",
        AgentLaunchPayload {
            agent_name_explicit: true,
            ..launch_payload("launch_a", "writer")
        },
    );
    let second = launch_event(
        "claude",
        AgentLaunchPayload {
            agent_name_explicit: true,
            ..launch_payload("session-b", "writer")
        },
    );

    let agents = reduce_agent_states(&[first, second]);

    let original = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "launch_a")
        .expect("original");
    let reminted = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "session-b")
        .expect("reminted");
    assert_eq!(original.name.as_deref(), Some("writer"));
    assert!(original.name_explicit);
    assert_ne!(reminted.name.as_deref(), Some("writer"));
    assert!(!reminted.name_explicit);
}

#[test]
fn launch_description_without_prompt_creates_idle_card() {
    // Interactive launch (no prompt) must be Idle so a card-only description
    // does not make the agent look busy.
    let agents = reduce_agent_states(&[launch_event(
        "codex",
        AgentLaunchPayload {
            prompt: None,
            description: Some("port auth".to_owned()),
            ..launch_payload("launch_a", "lucid-atlas")
        },
    )]);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].status, AgentStatus::Idle);
    assert_eq!(agents[0].phase, TurnPhase::Idle);
    assert_eq!(agents[0].description.as_deref(), Some("port auth"));
}

#[test]
fn launch_cohort_identity_reduces_and_survives_bound_event_without_fields() {
    let launch = launch_event(
        "claude",
        AgentLaunchPayload {
            launch: LaunchParams {
                launch_group: Some("launch_group_1".to_owned()),
                launch_ordinal: Some(1),
                ..LaunchParams::default()
            },
            state: AgentLaunchState::Starting,
            prompt: None,
            ..launch_payload("launch_a", "lucid-atlas")
        },
    );
    let bound = raw_launch(
        AgentLaunchState::Bound,
        "launch_a",
        "lucid-atlas",
        Some("tmux:%1"),
    );

    let agents = reduce_agent_states(&[launch, bound]);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].launch_group.as_deref(), Some("launch_group_1"));
    assert_eq!(agents[0].launch_ordinal, Some(1));
}

#[test]
fn launch_seeds_model_and_effort_until_lifecycle_observes_them() {
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch: LaunchParams {
                model: Some("gpt-5.5-codex".to_owned()),
                effort: Some("xhigh".to_owned()),
                ..LaunchParams::default()
            },
            prompt: None,
            ..launch_payload("launch_a", "lucid-atlas")
        },
    );

    let launched = reduce_agent_states(std::slice::from_ref(&launch));
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].model.as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(launched[0].effort.as_deref(), Some("xhigh"));

    let lifecycle = raw_lifecycle(
        "codex",
        json!({
            "agent_id": "sess-1",
            "agent_name": "lucid-atlas",
            "signal": { "signal": "registered" },
            "model": "gpt-6-codex",
            "effort": "medium",
        }),
    );
    let observed = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].agent_id.as_str(), "sess-1");
    assert_eq!(observed[0].model.as_deref(), Some("gpt-6-codex"));
    assert_eq!(observed[0].effort.as_deref(), Some("medium"));
}

#[test]
fn launch_role_and_profile_survive_roleless_lifecycle() {
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch: LaunchParams {
                profile: Some("codex-coder".to_owned()),
                role: Some("coder".to_owned()),
                team: Some("forge".to_owned()),
                launch_group: Some("launch_group_1".to_owned()),
                launch_ordinal: Some(2),
                ..LaunchParams::default()
            },
            state: AgentLaunchState::Starting,
            prompt: None,
            ..launch_payload("launch_a", "lucid-atlas")
        },
    );
    let lifecycle = raw_lifecycle(
        "codex",
        json!({
            "agent_id": "sess-1",
            "agent_name": "lucid-atlas",
            "signal": { "signal": "registered" },
        }),
    );

    let agents = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id.as_str(), "sess-1");
    assert_eq!(agents[0].profile.as_deref(), Some("codex-coder"));
    assert_eq!(agents[0].role.as_deref(), Some("coder"));
    assert_eq!(agents[0].team.as_deref(), Some("forge"));
    assert_eq!(agents[0].launch_group.as_deref(), Some("launch_group_1"));
    assert_eq!(agents[0].launch_ordinal, Some(2));
}

#[test]
fn launch_role_and_profile_survive_nameless_pane_lifecycle() {
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch: LaunchParams {
                profile: Some("codex-coder".to_owned()),
                role: Some("coder".to_owned()),
                team: Some("forge".to_owned()),
                channel: Some("auth".to_owned()),
                ..LaunchParams::default()
            },
            pane_id: Some(PaneId::parse("zellij:terminal_1").expect("pane id")),
            prompt: None,
            ..launch_payload("launch_a", "lucid-atlas")
        },
    );
    let lifecycle = raw_lifecycle(
        "codex",
        json!({
            "agent_id": "sess-1",
            "pane_id": "zellij:terminal_1",
            "signal": { "signal": "registered" },
        }),
    );

    let agents = reduce_agent_states(&[launch, lifecycle]);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id.as_str(), "sess-1");
    assert_eq!(agents[0].name.as_deref(), Some("lucid-atlas"));
    assert_eq!(agents[0].profile.as_deref(), Some("codex-coder"));
    assert_eq!(agents[0].role.as_deref(), Some("coder"));
    assert_eq!(agents[0].team.as_deref(), Some("forge"));
    assert_eq!(agents[0].channel.as_deref(), Some("auth"));
    assert_eq!(
        agents[0].pane.as_ref().map(|pane| pane.pane_id.to_string()),
        Some("zellij:terminal_1".to_owned())
    );
}

#[test]
fn stamped_turn_releases_provisional_from_pre_binding_split() {
    let pane_id = "zellij:terminal_1";
    let events = [
        launch_event(
            "codex",
            AgentLaunchPayload {
                pane_id: Some(PaneId::parse(pane_id).expect("pane id")),
                prompt: None,
                ..launch_payload("launch_a", "lucid-atlas")
            },
        ),
        raw_lifecycle_at(
            "codex",
            2,
            json!({
                "agent_id": "sess-1",
                "signal": { "signal": "registered" },
            }),
        ),
        raw_lifecycle_at(
            "codex",
            3,
            json!({
                "agent_id": "sess-1",
                "pane_id": pane_id,
                "signal": { "signal": "turn_started" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id.as_str(), "sess-1");
    assert_eq!(
        agents[0].pane.as_ref().map(|pane| pane.pane_id.as_str()),
        Some(pane_id)
    );
}

#[test]
fn stamped_turn_keeps_provisional_when_existing_session_already_has_pane() {
    let launch_pane = "zellij:terminal_1";
    let events = [
        launch_event(
            "codex",
            AgentLaunchPayload {
                pane_id: Some(PaneId::parse(launch_pane).expect("pane id")),
                prompt: None,
                ..launch_payload("launch_a", "lucid-atlas")
            },
        ),
        raw_lifecycle_at(
            "codex",
            2,
            json!({
                "agent_id": "sess-1",
                "pane_id": "zellij:terminal_2",
                "signal": { "signal": "registered" },
            }),
        ),
        raw_lifecycle_at(
            "codex",
            3,
            json!({
                "agent_id": "sess-1",
                "pane_id": launch_pane,
                "signal": { "signal": "turn_started" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);

    assert_eq!(agents.len(), 2);
    assert!(
        agents
            .iter()
            .any(|agent| agent.agent_id.as_str() == "launch_a")
    );
}

#[test]
fn lifecycle_role_and_profile_project_without_launch_placeholder() {
    let lifecycle = raw_lifecycle(
        "claude",
        json!({
            "agent_id": "sess-1",
            "agent_name": "lucid-atlas",
            "role": "reviewer",
            "channel": "design",
            "profile": "claude-reviewer",
            "signal": { "signal": "registered" },
        }),
    );

    let agents = reduce_agent_states(&[lifecycle]);

    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id.as_str(), "sess-1");
    assert_eq!(agents[0].profile.as_deref(), Some("claude-reviewer"));
    assert_eq!(agents[0].role.as_deref(), Some("reviewer"));
    assert_eq!(agents[0].channel.as_deref(), Some("design"));
}

#[test]
fn lifecycle_registration_merges_provisional_card_by_name() {
    let events = vec![
        raw_launch(
            AgentLaunchState::Bound,
            "launch_a",
            "lucid-atlas",
            Some("zellij:terminal_1"),
        ),
        raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "real-session",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.agent_id.as_str(), "real-session");
    assert_eq!(agent.name.as_deref(), Some("lucid-atlas"));
    assert_eq!(agent.kind_ordinal, Some(1));
    assert_eq!(agent.worktree_path.as_deref(), Some("/tmp/x"));
    assert_eq!(
        agent.pane.as_ref().map(|pane| pane.pane_id.to_string()),
        Some("zellij:terminal_1".to_owned())
    );
}

#[test]
fn failed_launch_event_does_not_resurrect_a_consumed_provisional() {
    let events = vec![
        raw_launch(AgentLaunchState::Starting, "launch_a", "lucid-atlas", None),
        raw_launch(
            AgentLaunchState::Bound,
            "launch_a",
            "lucid-atlas",
            Some("zellij:terminal_1"),
        ),
        raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "real-session",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
        raw_lifecycle_at(
            "claude",
            3,
            json!({
                "event_name": "SessionEnd",
                "agent_id": "real-session",
                "signal": { "signal": "ended" },
            }),
        ),
        raw_launch(AgentLaunchState::Failed, "launch_a", "lucid-atlas", None),
    ];

    let agents = reduce_agent_states(&events);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id.as_str(), "real-session");
    assert!(agents[0].ended_at.is_some());
    assert!(
        agents.iter().all(|agent| agent.agent_id != "launch_a"),
        "late wrapper failure must not recreate a failed provisional card: {agents:#?}"
    );
}

#[test]
fn late_launch_event_does_not_recreate_provisional_when_name_is_owned() {
    let real = reduce_agent_states(&[raw_lifecycle_at(
        "claude",
        1,
        json!({
            "agent_id": "real-session",
            "agent_name": "lucid-atlas",
            "signal": { "signal": "registered" },
        }),
    )])
    .pop()
    .expect("registered card");
    let kind = AgentKind::new_unchecked("claude");
    let real_id = AgentSessionId::from("real-session");
    let seed = BTreeMap::from([((kind.clone(), real_id.clone()), real)]);
    let identity = AgentIdentityState {
        names: BTreeMap::from([("lucid-atlas".to_owned(), (kind.clone(), real_id))]),
        next_ordinal: BTreeMap::from([(kind, 2)]),
        consumed_launches: BTreeSet::new(),
    };

    let events = [raw_launch(
        AgentLaunchState::Failed,
        "launch_a",
        "lucid-atlas",
        None,
    )];
    let events = decode_events(&events);
    let (agents, _) = reduce_agent_states_seeded_with_identity(seed, identity, &events);

    assert_eq!(agents.len(), 1);
    assert!(
        agents
            .values()
            .any(|agent| agent.agent_id == "real-session")
    );
    assert!(
        agents.values().all(|agent| agent.agent_id != "launch_a"),
        "a late wrapper failure must not create a second provisional card"
    );
}

#[test]
fn failed_launch_without_prior_or_owner_is_ignored() {
    let agents = reduce_agent_states(&[raw_launch(
        AgentLaunchState::Failed,
        "launch_a",
        "lucid-atlas",
        None,
    )]);

    assert!(
        agents.is_empty(),
        "a lone failed launch has no live provisional card to update"
    );
}

#[test]
fn rebirth_registration_with_new_session_id_adopts_prior_named_card() {
    let events = vec![
        raw_lifecycle_at(
            "claude",
            1,
            json!({
                "agent_id": "old-session",
                "agent_name": "lucid-atlas",
                "worktree_path": "/tmp/x",
                "signal": { "signal": "registered" },
            }),
        ),
        EventEnvelope::session_rebirth(workspace(), "session"),
        raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "new-session",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);

    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.agent_id.as_str(), "new-session");
    assert_eq!(agent.name.as_deref(), Some("lucid-atlas"));
    assert_eq!(agent.kind_ordinal, Some(1));
    assert_eq!(agent.worktree_path.as_deref(), Some("/tmp/x"));
}

#[test]
fn ended_session_keeps_card_name_bound_and_blocks_colliding_provisional() {
    let events = vec![
        raw_launch(
            AgentLaunchState::Bound,
            "launch_a",
            "lucid-atlas",
            Some("zellij:terminal_1"),
        ),
        raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "real-session",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
        raw_lifecycle_at(
            "claude",
            3,
            json!({
                "event_name": "SessionEnd",
                "agent_id": "real-session",
                "signal": { "signal": "ended" },
            }),
        ),
        raw_launch(
            AgentLaunchState::Bound,
            "launch_b",
            "lucid-atlas",
            Some("zellij:terminal_2"),
        ),
    ];

    let agents = reduce_agent_states(&events);
    assert_eq!(agents.len(), 1);
    let ended = agents
        .iter()
        .find(|agent| agent.agent_id == "real-session")
        .expect("retained ended session");
    assert_eq!(ended.name.as_deref(), Some("lucid-atlas"));
    assert!(ended.ended_at.is_some());
    assert!(
        agents.iter().all(|agent| agent.agent_id != "launch_b"),
        "the retained name cannot ambiguously bind a second provisional card"
    );
}

#[test]
fn stale_identity_state_does_not_block_reused_launch_name() {
    let stale = AgentIdentityState {
        names: BTreeMap::from([(
            "lucid-atlas".to_owned(),
            (
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from("gone-session"),
            ),
        )]),
        next_ordinal: BTreeMap::new(),
        consumed_launches: BTreeSet::new(),
    };
    let events = [raw_launch(
        AgentLaunchState::Bound,
        "launch_a",
        "lucid-atlas",
        Some("zellij:terminal_1"),
    )];
    let events = decode_events(&events);
    let (agents, _) = reduce_agent_states_seeded_with_identity(BTreeMap::new(), stale, &events);

    let agent = agents
        .values()
        .next()
        .expect("launch with reused name should survive");
    assert_eq!(agent.agent_id.as_str(), "launch_a");
    assert_eq!(agent.name.as_deref(), Some("lucid-atlas"));
}
