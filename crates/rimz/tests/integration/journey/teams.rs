use rimz::ids::MuxName;

use super::{RoomHarness, SETTLE, session_start, session_start_at};
use crate::common::Env;

#[test]
fn team_groups_roles_under_worktrees() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex", "claude"]);

    room.agent_hook_as(
        "coder",
        "codex",
        &session_start("team-coder", "GPT-5.5", "high", "main"),
    );
    room.agent_hook_as(
        "reviewer",
        "claude",
        &session_start("team-reviewer", "Opus", "xhigh", "main"),
    );
    room.agent_hook_as(
        "planner",
        "codex",
        &session_start("team-planner", "GPT-5.5", "low", "feature-x"),
    );

    let screen = room.wait_for(
        |s| {
            s.contains("¤ 3")
                && s.contains("coder")
                && s.contains("reviewer")
                && s.contains("planner")
                && s.contains("feature-x")
        },
        SETTLE,
    );

    assert!(
        screen.contains("¤ 3"),
        "live agent tally renders:\n{screen}"
    );
    assert_group_contains(&screen, "main", &["coder", "reviewer"]);
    assert_group_contains(&screen, "feature-x", &["planner"]);
}

#[test]
fn standalone_team_role_uses_team_channel_and_role_handle() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);

    room.agent_hook_as_team(
        "planner",
        "pcr",
        "codex",
        &session_start_at(
            "team-planner",
            "GPT-5.5",
            "low",
            env.project_root.display().to_string(),
            None,
        ),
    );

    let screen = room.wait_for(|s| s.contains("¤ 1") && s.contains("planner"), SETTLE);
    assert!(
        screen.contains("planner"),
        "team role handle renders:\n{screen}"
    );

    let snapshot = env.store().snapshot().expect("snapshot");
    let planner = snapshot
        .agents
        .iter()
        .find(|agent| agent.role.as_deref() == Some("planner"))
        .expect("planner agent");
    assert_eq!(planner.team.as_deref(), Some("pcr"));
    assert_eq!(
        rimz::harness::target::agent_channel(planner).as_deref(),
        Some("project/pcr")
    );
}

fn assert_group_contains(screen: &str, header: &str, roles: &[&str]) {
    let lines = screen.lines().collect::<Vec<_>>();
    let header_index = lines
        .iter()
        .position(|line| line.contains(header))
        .unwrap_or_else(|| panic!("missing {header} group:\n{screen}"));
    for role in roles {
        let role_index = lines
            .iter()
            .position(|line| line.contains(role))
            .unwrap_or_else(|| panic!("missing {role} row:\n{screen}"));
        assert!(
            header_index < role_index,
            "{role} should render under {header}:\n{screen}"
        );
    }
}
