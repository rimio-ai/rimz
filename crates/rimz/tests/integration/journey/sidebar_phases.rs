//! The user journey, phase by phase (`docs/guide/product.md` and
//! `docs/guide/experience.md`).
//!
//! Backend-neutral: each test drives the renderer once over a real ledger and
//! reads the parsed pane. Renderer mechanics live in `docs/internals/sidebar/sidebar.md`;
//! layout/tabs/focus live in `backend/zellij.rs`; the actual mux-pane content
//! smokes live in `journey/deep.rs`.

use rimz::ids::MuxName;

use super::{
    RoomHarness, SETTLE, permission_request, post_tool_use, session_start, user_prompt_submit,
};
use crate::common::Env;

/// Phase 0 → 1 onboarding. Running an agent before wiring its hooks is not
/// invisible: the pane is live, so it shows as a dim `○ codex` process row (the
/// same hollow idle glyph an agent shows, set apart by the dim process tone). But
/// the row carries no agent enrichment — without an installed hook nothing reaches
/// the ledger, so no model, status, or task folds in. Only after `rimz hooks
/// install` does a fresh `SessionStart` light up the agent row with its model. The
/// room is deliberately correct to require `rimz hooks install` (Rimz never
/// silently rewrites the user's agent config).
///
/// The harness fires agents through their *installed* hook, so an un-onboarded
/// `agent_hook` reaches the ledger as a no-op — exactly what a real agent does
/// with no Rimz hook configured — while the pane it runs in stays live.
#[test]
fn phase0_onboarding_hint_then_wire_then_agent_appears() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);

    // Empty room: cockpit shows ◎ 0, no agents yet.
    let _screen = room.wait_for(|s| s.contains("◎ 0"), SETTLE);

    // The user runs codex before wiring it. The pane is live, so it shows as a
    // process row and the first-run hint steps aside — but with no installed hook
    // nothing reaches the ledger, so the row carries no agent enrichment. The
    // idle process row and an idle agent row share the hollow `○`, so the model
    // (`GPT 5.5`), which only an enriched agent row shows, is what tells them
    // apart here.
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    let screen = room.wait_for(|s| s.contains("○ codex"), SETTLE);
    assert!(
        screen.contains("○ codex"),
        "an un-onboarded codex is still a live pane, so it shows as a process row:\n{screen}"
    );
    assert!(
        !screen.contains("GPT 5.5"),
        "with no installed hook nothing reaches the ledger, so the process row carries no agent model:\n{screen}"
    );

    // The user follows the hint, installs hooks, and runs codex again. Now the
    // installed `SessionStart` reaches the ledger and the process row resolves
    // into an idle agent row addressed by its team role.
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    let screen = room.wait_for(|s| s.contains("○ coder"), SETTLE);
    assert!(
        screen.contains("○ coder"),
        "a wired agent registers idle the moment its SessionStart lands:\n{screen}"
    );
}

/// Phases 1 → 3 — an onboarded agent registers idle, moves to running with the
/// prompt, then waits on a permission request without rendering the raw
/// question.
#[test]
fn phase1_to_3_agent_moves_from_idle_to_running_to_waiting() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));

    // Wait for the lifecycle-backed agent row, not the bare substring "codex"
    // (the first-run hint contains it) or the synthesized idle Codex row that
    // can appear from live-pane presence before the event-fresh model folds in.
    let screen = room.wait_for(|s| s.contains("○ coder") && s.contains("GPT 5.5"), SETTLE);
    assert!(
        screen.contains("main ┄"),
        "the agent groups under its worktree:\n{screen}"
    );
    assert!(
        screen.contains("○ coder"),
        "a launched-but-unprompted agent is idle:\n{screen}"
    );
    assert!(
        screen.contains("GPT 5.5"),
        "the capability line shows the model:\n{screen}"
    );
    assert!(
        !screen.contains("all clear"),
        "idle never demands an all-clear attention line:\n{screen}"
    );

    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));

    let screen = room.wait_for(|s| s.contains("fix auth flow"), SETTLE);
    assert!(
        thinking_row(&screen, "coder"),
        "a prompted agent opens its turn in the thinking phase:\n{screen}"
    );
    assert!(
        screen.contains("fix auth flow"),
        "the task descriptor is the prompt:\n{screen}"
    );

    room.agent_hook("codex", &permission_request("sess-1", "DO_NOT_RENDER_ME"));

    let screen = room.wait_for(
        |s| s.contains("? coder") && s.contains("fix auth flow"),
        SETTLE,
    );
    assert!(
        screen.contains("? coder"),
        "a permission prompt makes the agent wait:\n{screen}"
    );
    assert!(
        screen.contains("? 1"),
        "the attention line counts the waiting agent:\n{screen}"
    );
    assert!(
        screen.contains("fix auth flow"),
        "the waiting row keeps the task that led to the prompt:\n{screen}"
    );
    assert!(
        !screen.contains("echo"),
        "the sidebar must not reproduce the raw command:\n{screen}"
    );
    assert!(
        !screen.contains("DO_NOT_RENDER_ME"),
        "the sidebar notifies and navigates; it never reproduces the question:\n{screen}"
    );
}

/// The turn phase on a rendered frame: a prompted agent opens with the thinking
/// head, a shell command keeps it (work, but no file written), and the turn's
/// first file edit flips the leading cell to the working fill.
#[test]
fn turn_phase_flips_thinking_to_working_on_first_edit() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));

    let screen = room.wait_for(|s| thinking_row(s, "coder"), SETTLE);
    assert!(
        thinking_row(&screen, "coder"),
        "a prompted agent opens its turn thinking:\n{screen}"
    );

    // A shell command mutates but edits nothing — the thinking head stays.
    room.agent_hook("codex", &post_tool_use("sess-1", "shell"));
    let screen = room.wait_for(|s| thinking_row(s, "coder"), SETTLE);
    assert!(
        thinking_row(&screen, "coder"),
        "a command-only turn is still thinking:\n{screen}"
    );

    room.agent_hook("codex", &post_tool_use("sess-1", "apply_patch"));
    let screen = room.wait_for(|s| running_row(s, "coder"), SETTLE);
    assert!(
        running_row(&screen, "coder"),
        "the first file edit flips the turn to working:\n{screen}"
    );
}

/// Phase 4 — a fleet across worktrees. Agents spread across `main` and
/// `feature-migration`, and one agent waits on input. Grouping and the worktree
/// headers stay stable while the waiting row drives the room tally.
#[test]
fn phase4_fleet_groups_and_tallies() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex", "claude"]);
    room.agent_hook("codex", &session_start("m1", "GPT-5.5", "high", "main"));
    room.agent_hook("claude", &session_start("m2", "Opus", "xhigh", "main"));
    room.agent_hook(
        "codex",
        &session_start("f1", "GPT-5.5", "low", "feature-migration"),
    );
    room.agent_hook("codex", &permission_request("m1", "DO_NOT_RENDER_ME"));

    let screen = room.wait_for(|s| s.contains("feature-migration"), SETTLE);
    assert!(
        screen.contains("main"),
        "the main worktree group renders:\n{screen}"
    );
    assert!(
        screen.contains("feature-migration"),
        "the feature-migration worktree group renders:\n{screen}"
    );
    assert!(
        screen.contains("? coder"),
        "the waiting agent is marked:\n{screen}"
    );
    assert!(
        screen.contains("? 1"),
        "exactly one item waits across the room:\n{screen}"
    );
}

/// Phase 6 — detach and reattach. Walk away (drop the renderer); the ledger
/// keeps the state. A fresh renderer reconstructs every agent where you left
/// it. (Implemented — the renderer is a stateless projection of the ledger.)
#[test]
fn phase6_reattach_reconstructs_from_ledger() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    {
        let room = RoomHarness::launch(&env, MuxName::Tmux);
        room.onboard(&["codex"]);
        room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
        room.wait_for(|s| s.contains("○ coder"), SETTLE);
    } // walk away — the renderer drops, the ledger stays.

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    let screen = room.wait_for(|s| s.contains("○ coder"), SETTLE);
    assert!(
        screen.contains("main ┄"),
        "reattach reconstructs the worktree group:\n{screen}"
    );
    assert!(
        screen.contains("○ coder"),
        "every agent returns where you left it:\n{screen}"
    );
}

/// Phase 9 — degraded refresh. The renderer keeps running when its in-process
/// produce fails every cycle (here: an unreadable pane fixture, the same
/// degraded outcome a dead mux feeds), labels the frame as degraded, and
/// suppresses the healthy empty-room hint.
#[test]
fn phase9_degraded_loop_shows_banner_not_first_run_hint() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch_degraded(&env, MuxName::Tmux);

    let screen = room.wait_for(|s| s.contains("Sidebar degraded"), SETTLE);
    assert!(
        screen.contains("Sidebar degraded"),
        "a failed produce should surface a degraded banner:\n{screen}"
    );
    assert!(
        !screen.contains("rimz hooks install") && !screen.contains("run claude or codex"),
        "a degraded empty frame must not look like a healthy empty room:\n{screen}"
    );
}

/// A running agent's head animates the working spinner, so a live capture may
/// show any frame — match on `<frame> <name>` across the whole cycle (the
/// static fallback `⢿` is one of these frames).
fn running_row(screen: &str, name: &str) -> bool {
    ['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷']
        .iter()
        .any(|frame| screen.contains(&format!("{frame} {name}")))
}

/// A running row still in its pre-edit thinking phase leads with the animated
/// thinking orbit, so a live capture may show any frame — confirm the leading
/// cell is one of them.
fn thinking_row(screen: &str, name: &str) -> bool {
    [
        '⠁', '⠂', '⠄', '⡀', '⡈', '⡐', '⡠', '⣀', '⣁', '⣂', '⣄', '⣌', '⣔', '⣤', '⣥', '⣦', '⣮', '⣶',
        '⣷', '⣿', '⡿', '⠿', '⢟', '⠟', '⡛', '⠛', '⠫', '⢋', '⠋', '⠍', '⡉', '⠉', '⠑', '⠡', '⢁',
    ]
    .iter()
    .any(|frame| screen.contains(&format!("{frame} {name}")))
}
