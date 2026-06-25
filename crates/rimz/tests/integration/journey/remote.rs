use rimz::ids::MuxName;
use rimz::remote::link::{LinkStats, LinkStatsFile};
use rimz::sidebar::cache::unix_now_ms;

use super::{RoomHarness, SETTLE, session_start_at};
use crate::common::Env;

#[test]
fn remote_link_badge_renders_with_reconstructed_agents() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch_wide(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook(
        "codex",
        &session_start_at(
            "sess-remote",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        ),
    );
    room.publish_link_stats(&LinkStatsFile::new(
        unix_now_ms(),
        "client".to_owned(),
        LinkStats {
            rtt_ms: Some(210),
            miss_pct: 0,
            window: 12,
            bandwidth_bps: None,
        },
    ));

    let screen = room.wait_for(
        |s| s.contains("coder") && s.contains("⇄ remote 210ms"),
        SETTLE,
    );
    assert!(
        screen.contains("coder"),
        "rendered remote room should reconstruct agent rows from the ledger:\n{screen}"
    );
    assert!(
        screen.contains("⇄ remote 210ms"),
        "footer should render the remote link-health badge:\n{screen}"
    );
}
