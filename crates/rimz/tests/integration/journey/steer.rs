use std::time::Duration;

use rimz::ids::MuxName;

use super::{KEY_DOWN, KEY_UP, RoomHarness, SETTLE, session_start_at};
use crate::common::Env;

#[test]
fn sidebar_keys_move_selection_between_rendered_agent_rows() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);

    let alpha = env.home_root.join("alpha");
    let beta = env.home_root.join("beta");
    std::fs::create_dir_all(&alpha).expect("mkdir alpha");
    std::fs::create_dir_all(&beta).expect("mkdir beta");
    room.agent_hook_as(
        "planner",
        "codex",
        &session_start_at(
            "sess-alpha",
            "GPT-5.5",
            "high",
            alpha.display().to_string(),
            Some("alpha"),
        ),
    );
    room.agent_hook_as(
        "reviewer",
        "codex",
        &session_start_at(
            "sess-beta",
            "GPT-5.5",
            "high",
            beta.display().to_string(),
            Some("beta"),
        ),
    );

    let screen = room.wait_for(
        |s| s.contains("planner") && s.contains("reviewer") && selected_group(s, "alpha"),
        SETTLE,
    );
    assert!(
        selected_group(&screen, "alpha"),
        "initial selection should sit on the first worktree group:\n{screen}"
    );

    let screen = send_until(&room, KEY_DOWN, |s| selected_group(s, "beta"));
    assert!(
        selected_group(&screen, "beta"),
        "down key should move the rendered selection to the next worktree:\n{screen}"
    );

    let screen = send_until(&room, KEY_UP, |s| selected_group(s, "alpha"));
    assert!(
        selected_group(&screen, "alpha"),
        "up key should move the rendered selection back to the previous worktree:\n{screen}"
    );
}

fn send_until(room: &RoomHarness<'_>, key: &[u8], pred: impl Fn(&str) -> bool) -> String {
    let mut screen = room.screen();
    for _ in 0..4 {
        room.send_keys(key);
        screen = room.wait_for(&pred, Duration::from_millis(750));
        if pred(&screen) {
            return screen;
        }
    }
    screen
}

fn selected_group(screen: &str, branch: &str) -> bool {
    screen
        .lines()
        .any(|line| line.contains(&format!("⑂ {branch}")) && line.trim_start().starts_with('▎'))
}
