use rimz::EventEnvelope;
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::{AgentSessionId, MuxName, PaneId};

use super::{RoomHarness, SETTLE};
use crate::common::Env;

#[test]
fn rendered_room_recovers_post_rebirth_agents_from_the_store() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    append_rebirth(&env);
    append_registered_agent(&env, "sess-resume", "coder", "%0", "main");

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    let screen = room.wait_for(|s| s.contains("○ coder") && s.contains("main"), SETTLE);
    assert!(
        screen.contains("○ coder") && screen.contains("main"),
        "reborn rendered room should recover durable agent rows from the store:\n{screen}"
    );
}

fn append_rebirth(env: &Env) {
    env.store()
        .append_event(&EventEnvelope::session_rebirth(
            env.workspace_id.clone(),
            "rimz-journey",
        ))
        .expect("append rebirth");
}

fn append_registered_agent(env: &Env, session: &str, role: &str, pane: &str, branch: &str) {
    let mut obs = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session)),
        LifecycleSignal::Registered,
    );
    obs.launch.role = Some(role.to_owned());
    obs.launch.profile = Some(format!("codex-{role}"));
    obs.worktree_path = Some(env.project_root.display().to_string());
    obs.worktree_branch = Some(branch.to_owned());
    obs.launch.model = Some("GPT-5.5".to_owned());
    obs.launch.effort = Some("high".to_owned());
    obs.pane_id = Some(PaneId::from_parts(MuxName::Tmux, pane));
    let event = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "rimz-journey",
        "codex",
        "SessionStart",
        &obs,
    );
    env.store().append_event(&event).expect("append lifecycle");
}
