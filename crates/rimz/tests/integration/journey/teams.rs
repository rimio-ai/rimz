use rimz::ids::MuxName;

use super::{RoomHarness, SETTLE, session_start};
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
