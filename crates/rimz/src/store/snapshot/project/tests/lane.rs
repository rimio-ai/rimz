use super::*;
use crate::ids::{Lane, LaneHome};

fn named(home: LaneHome, path: &str, name: &str) -> Lane {
    Lane::own(home, path).named(name.parse().expect("lane name"))
}

fn lane_launch(state: AgentLaunchState, launch: LaunchParams) -> EventEnvelope {
    launch_event(
        "codex",
        AgentLaunchPayload {
            state,
            launch_id: Some(AgentSessionId::from("launch_a")),
            launch,
            pane_id: Some(PaneId::parse("tmux:%1").expect("pane id")),
            prompt: None,
            ..launch_payload("launch_a", "coder")
        },
    )
}

fn registration(id: &str, extra: serde_json::Value) -> EventEnvelope {
    let mut params = json!({
        "agent_id": id,
        "pane_id": "tmux:%1",
        "signal": { "signal": "registered" },
    });
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    raw_lifecycle("codex", params)
}

fn lane_json(lane: &Lane) -> serde_json::Value {
    serde_json::to_value(lane).unwrap()
}

#[test]
fn the_first_lane_stands_against_registration_bound_launch_and_old_channel() {
    let design = named(LaneHome::Trunk, "/repo", "design");
    let starting = lane_launch(
        AgentLaunchState::Starting,
        LaunchParams {
            lane: Some(design.clone()),
            ..LaunchParams::default()
        },
    );
    let registered = registration(
        "sess-1",
        json!({
            "channel": "stale",
            "lane": lane_json(&Lane::own(LaneHome::Trunk, "/repo")),
        }),
    );
    let bound = lane_launch(
        AgentLaunchState::Bound,
        LaunchParams {
            channel: Some("old".to_owned()),
            lane: Some(Lane::own(LaneHome::Worktree, "/repo-wt/auth")),
            ..LaunchParams::default()
        },
    );

    let events = [starting, registered, bound];
    for end in 1..=events.len() {
        let agents = reduce_agent_states(&events[..end]);
        let agent = agents.last().unwrap();
        assert_eq!(agent.lane.as_ref(), Some(&design), "prefix {end}");
        assert_eq!(agent.channel.as_deref(), Some("design"), "prefix {end}");
        assert!(!agent.root_lane, "prefix {end}");
        assert_eq!(agent.lane_path(), Some("/repo"), "prefix {end}");
    }
}

#[test]
fn a_lane_less_row_adopts_its_old_stamp_at_the_first_lane() {
    for (stamp, expect) in [
        ("main", Lane::own(LaneHome::Trunk, "/repo")),
        ("repo", Lane::own(LaneHome::Trunk, "/repo")),
        ("design", named(LaneHome::Trunk, "/repo", "design")),
    ] {
        let old = lane_launch(
            AgentLaunchState::Starting,
            LaunchParams {
                channel: Some(stamp.to_owned()),
                ..LaunchParams::default()
            },
        );
        let agents = reduce_agent_states(std::slice::from_ref(&old));
        assert_eq!(agents[0].lane, None, "{stamp}");
        assert_eq!(agents[0].channel.as_deref(), Some(stamp), "{stamp}");

        let registered = registration(
            "sess-1",
            json!({ "lane": lane_json(&Lane::own(LaneHome::Trunk, "/repo")) }),
        );
        let agents = reduce_agent_states(&[old, registered]);
        let agent = agents.last().unwrap();
        assert_eq!(agent.lane.as_ref(), Some(&expect), "{stamp}");
        assert_eq!(agent.root_lane, expect.is_trunk(), "{stamp}");
        assert_eq!(
            agent.channel.as_deref(),
            expect.stored_name(),
            "{stamp}: the stamp gives way to the lane's stored name"
        );
    }
}

#[test]
fn a_displaced_lane_less_row_keeps_its_reading_through_registration() {
    let old = lane_launch(
        AgentLaunchState::Starting,
        LaunchParams {
            checkout: Some("/repo".to_owned()),
            ..LaunchParams::default()
        },
    );
    let registered = registration(
        "sess-1",
        json!({ "lane": lane_json(&Lane::own(LaneHome::Foreign, "/home/u/.rimz/empty/a")) }),
    );

    let agents = reduce_agent_states(&[old, registered]);

    assert_eq!(agents.last().unwrap().lane, None);
    assert_eq!(agents.last().unwrap().lane_path(), Some("/repo"));
}

#[test]
fn a_conversation_successor_keeps_the_launch_lane() {
    let design = named(LaneHome::Trunk, "/repo", "design");
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some(AgentSessionId::from("launch_coder")),
            launch: LaunchParams {
                lane: Some(design.clone()),
                ..LaunchParams::default()
            },
            pane_id: Some(PaneId::parse("tmux:%1").expect("pane id")),
            runtime_owner: Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "launch_coder",
                42,
                Some("agent-start".to_owned()),
            )),
            prompt: None,
            ..launch_payload("launch_coder", "coder-card")
        },
    );
    let first = same_process_registration("conversation-a", "coder-card", 2, "tmux:%1", 42);
    let mut successor =
        same_process_registration("conversation-b", "successor-card", 3, "tmux:%1", 42);
    let mut params: serde_json::Value = serde_json::from_str(successor.params.get()).unwrap();
    params["lane"] = lane_json(&Lane::own(LaneHome::Worktree, "/repo-wt/auth"));
    successor.params = serde_json::value::to_raw_value(&params).unwrap();

    let agents = reduce_agent_states(&[launch, first, successor]);
    let successor = agents
        .iter()
        .find(|agent| agent.agent_id == "conversation-b")
        .expect("successor conversation");

    assert_eq!(successor.lane.as_ref(), Some(&design));
    assert_eq!(successor.channel.as_deref(), Some("design"));
}

#[test]
fn a_conversation_successor_of_a_displaced_lane_less_row_takes_no_lane() {
    let launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some(AgentSessionId::from("launch_coder")),
            launch: LaunchParams {
                checkout: Some("/repo".to_owned()),
                ..LaunchParams::default()
            },
            pane_id: Some(PaneId::parse("tmux:%1").expect("pane id")),
            runtime_owner: Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "launch_coder",
                42,
                Some("agent-start".to_owned()),
            )),
            prompt: None,
            ..launch_payload("launch_coder", "coder-card")
        },
    );
    let first = same_process_registration("conversation-a", "coder-card", 2, "tmux:%1", 42);
    let mut successor =
        same_process_registration("conversation-b", "successor-card", 3, "tmux:%1", 42);
    let mut params: serde_json::Value = serde_json::from_str(successor.params.get()).unwrap();
    params["lane"] = lane_json(&Lane::own(LaneHome::Foreign, "/home/u/.rimz/empty/a"));
    successor.params = serde_json::value::to_raw_value(&params).unwrap();

    let agents = reduce_agent_states(&[launch, first, successor]);
    let successor = agents
        .iter()
        .find(|agent| agent.agent_id == "conversation-b")
        .expect("successor conversation");

    assert_eq!(successor.lane, None);
    assert_eq!(successor.lane_path(), Some("/repo"));
}

#[test]
fn a_launch_event_serializes_its_lane_and_reads_a_broken_one_as_absent() {
    let lane = named(LaneHome::Worktree, "/repo-wt/auth", "design");
    let event = lane_launch(
        AgentLaunchState::Starting,
        LaunchParams {
            lane: Some(lane.clone()),
            ..LaunchParams::default()
        },
    );
    let params: serde_json::Value = serde_json::from_str(event.params.get()).unwrap();
    assert_eq!(
        params["lane"],
        json!({ "home": "worktree", "path": "/repo-wt/auth", "name": "design" })
    );
    assert_eq!(
        lane_json(&Lane::own(LaneHome::Trunk, "/repo")),
        json!({ "home": "trunk", "path": "/repo" })
    );

    for broken in [
        json!({ "home": "moon", "path": "/repo" }),
        json!({ "home": "trunk", "path": "repo" }),
        json!({ "home": "trunk", "path": "/repo", "name": "two words" }),
        json!("trunk"),
    ] {
        let mut event = event.clone();
        let mut params = params.clone();
        params["lane"] = broken.clone();
        params["role"] = json!("coder");
        event.params = serde_json::value::to_raw_value(&params).unwrap();
        let agents = reduce_agent_states(&[event]);
        assert_eq!(agents.len(), 1, "{broken}");
        assert_eq!(agents[0].lane, None, "{broken}");
        assert_eq!(agents[0].role.as_deref(), Some("coder"), "{broken}");
    }
}
