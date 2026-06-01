//! The user journey, phase by phase (`docs/guide/product.md` and
//! `docs/guide/experience.md`).
//!
//! Backend-neutral: each test drives the renderer once over a real ledger and
//! reads the parsed pane. Renderer mechanics live in `docs/internals/sidebar.md`;
//! layout/tabs/focus live in `backend/zellij.rs`; the actual mux-pane content
//! smokes live in `journey/deep.rs`.

use rimz::feed::{FeedItem, FeedKind, RuntimeOwnerKind, Surface};
use rimz::ids::MuxName;
use rimz::ledger::runtime::current_process_owner;

use super::{
    RoomHarness, SETTLE, permission_request, session_start, session_start_at, user_prompt_submit,
};
use crate::common::Env;

/// Phase 0 → 1 onboarding. Running an agent before wiring its hooks is not
/// invisible: the pane is live, so it shows as a dim `· codex` process row, and
/// because a known agent is visible the first-run hint steps aside. But no
/// `○ codex` *agent* row registers — without an installed hook nothing reaches
/// the ledger, so the agent carries no status, model, or task. Only after
/// `rimz hooks install` does a fresh `SessionStart` light up the agent row. The
/// room is deliberately correct to require `rimz hooks install` (Rimz never
/// silently rewrites the user's agent config); the empty-room hint says exactly
/// that.
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

    // Empty room: the hint names the real next step — installing hooks — not
    // "run claude or codex", which does nothing until the hooks are wired.
    let screen = room.wait_for(|s| s.contains("no agents yet"), SETTLE);
    assert!(
        screen.contains("rimz hooks install"),
        "an empty room must point the user at `rimz hooks install`; agents are \
         invisible until their hooks are wired:\n{screen}"
    );
    assert!(
        !screen.contains("all clear"),
        "the empty-room nudge should not spend the top line on all-clear copy:\n{screen}"
    );

    // The user runs codex before wiring it. The pane is live, so it shows as a
    // process row and the first-run hint steps aside — but with no installed
    // hook nothing reaches the ledger, so no `○ codex` agent row registers.
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    let screen = room.wait_for(|s| s.contains("· codex"), SETTLE);
    assert!(
        screen.contains("· codex"),
        "an un-onboarded codex is still a live pane, so it shows as a process row:\n{screen}"
    );
    assert!(
        !screen.contains("○ codex"),
        "with no installed hook nothing reaches the ledger, so no agent row registers:\n{screen}"
    );

    // The user follows the hint, installs hooks, and runs codex again. Now the
    // installed `SessionStart` reaches the ledger and the process row resolves
    // into an idle agent row.
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    let screen = room.wait_for(|s| s.contains("○ codex"), SETTLE);
    assert!(
        screen.contains("○ codex"),
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
    let screen = room.wait_for(|s| s.contains("○ codex") && s.contains("GPT-5.5"), SETTLE);
    assert!(
        screen.contains("main ┄"),
        "the agent groups under its worktree:\n{screen}"
    );
    assert!(
        screen.contains("○ codex"),
        "a launched-but-unprompted agent is idle:\n{screen}"
    );
    assert!(
        screen.contains("GPT-5.5"),
        "the capability line shows the model:\n{screen}"
    );
    assert!(
        !screen.contains("all clear"),
        "idle never demands an all-clear attention line:\n{screen}"
    );

    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));

    let screen = room.wait_for(|s| s.contains("fix auth flow"), SETTLE);
    assert!(
        running_row(&screen, "codex"),
        "a prompted agent is running:\n{screen}"
    );
    assert!(
        screen.contains("fix auth flow"),
        "the task descriptor is the prompt:\n{screen}"
    );

    room.agent_hook("codex", &permission_request("sess-1", "DO_NOT_RENDER_ME"));

    let screen = room.wait_for(
        |s| s.contains("? codex") && s.contains("fix auth flow"),
        SETTLE,
    );
    assert!(
        screen.contains("? codex"),
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

/// Phase 3b — a resolver in front. With a fresh enrolled resolver the same
/// waiting row shows the chain working (braille `⠋ <resolver> <budget>`) instead
/// of a static `?`, and still counts in the attention tally. (Implemented; stretch.)
#[test]
fn phase3b_resolver_in_front_shows_chain() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // Enrol a resolver and make it look alive so the bridge engages.
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", jiff::Timestamp::now());

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));
    // Fire the blocking hook in the background; it holds open on the bridge
    // while we observe the mid-flight render.
    let mut child = room.spawn_agent("codex", &permission_request("sess-1", "DO_NOT_RENDER_ME"));

    // Wait on the resolver name — it is stable, while the leading braille cell
    // animates — so the capture lands on the engaged frame instead of timing
    // out past the bridge's budget.
    let screen = room.wait_for(|s| s.contains("opus-policy"), SETTLE);
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        resolver_spinner(&screen),
        "a resolver in front leads the row with the braille spinner:\n{screen}"
    );
    assert!(
        screen.contains("opus-policy"),
        "the row names the active resolver:\n{screen}"
    );
    assert!(
        screen.contains("? 1"),
        "a delegated item is still pending, so it still counts in the tally:\n{screen}"
    );
}

/// Phase 4 — a fleet across worktrees. Agents spread across `main` and
/// `feature-migration`, plus a script paused at a gate. Grouping and the
/// worktree headers are implemented; the script lands in the `external`
/// catch-all (a dim `┄ external ┄┄┄` divider) because it is not tied to a
/// worktree.
#[test]
fn phase4_fleet_groups_and_tallies() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // A catch-all script paused at a gate. The CLI always attaches the cwd
    // worktree, so push straight to the ledger with no worktree to land it in
    // the `external` catch-all (scripts not tied to a worktree render there).
    push_workspace_script_ask_fixture(&env, "promote release?");

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex", "claude"]);
    room.agent_hook("codex", &session_start("m1", "GPT-5.5", "high", "main"));
    room.agent_hook("claude", &session_start("m2", "Opus", "xhigh", "main"));
    room.agent_hook(
        "codex",
        &session_start("f1", "GPT-5.5", "low", "feature-migration"),
    );

    // The seal/lane mark only the *selected* worktree, and the default selection
    // lands on the floating `waiting` script ask in the external catch-all — so
    // both worktree headers render as bare bold labels here. (Phase 1 covers the
    // selected-worktree seal, where the lone agent's worktree is the selection.)
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
        screen.contains("┄ external"),
        "scripts not tied to a worktree live in the external catch-all:\n{screen}"
    );
    assert!(
        screen.contains("promote release?"),
        "the script's ask shows its title:\n{screen}"
    );
    assert!(
        screen.contains("? 1"),
        "exactly one item waits across the room:\n{screen}"
    );
}

/// Within a worktree, the most attention-hungry rises: a `waiting` row sorts
/// above a calm agent in the same group. (Implemented — independent of the
/// idle/running mechanics, since both calm statuses outrank `waiting`.)
#[test]
fn phase4_waiting_rises_within_worktree() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // A waiting ask and a calm agent in the same worktree. The short
    // `feed ask --no-block` process is audit-only under runtime expel rules,
    // so this fixture keeps the script owner live for the rendered scenario.
    push_worktree_script_ask_fixture(&env, "approve deploy?");
    let worktree = env.project_root.display().to_string();

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook(
        "codex",
        &session_start_at("sess-1", "GPT-5.5", "high", worktree, None),
    );

    let screen = room.wait_for(
        |s| s.contains("approve deploy?") && s.contains("○ codex"),
        SETTLE,
    );
    let waiting_at = screen
        .find("approve deploy?")
        .unwrap_or_else(|| panic!("waiting row missing:\n{screen}"));
    let agent_at = screen
        .find("codex")
        .unwrap_or_else(|| panic!("agent row missing:\n{screen}"));
    assert!(
        waiting_at < agent_at,
        "the waiting row must rank above the calm agent in its worktree:\n{screen}"
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
        room.wait_for(|s| s.contains("○ codex"), SETTLE);
    } // walk away — the renderer drops, the ledger stays.

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    let screen = room.wait_for(|s| s.contains("○ codex"), SETTLE);
    assert!(
        screen.contains("main ┄"),
        "reattach reconstructs the worktree group:\n{screen}"
    );
    assert!(
        screen.contains("○ codex"),
        "every agent returns where you left it:\n{screen}"
    );
}

/// Phase 9 — degraded refresh. The renderer keeps running when its public
/// `rimz sidebar snapshot` subprocess fails, labels the frame as degraded, and
/// suppresses the healthy empty-room hint.
#[test]
fn phase9_degraded_loop_shows_banner_not_first_run_hint() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // A present-but-failing `rimz`: the renderer resolves the snapshot binary
    // per tick and keeps a launch path that still exists (so it never heals to
    // the installed `rimz` on PATH), then forks it and sees every snapshot
    // fail — the degraded loop a *vanished* binary now heals out of.
    let broken_rimz = env.project_root.join("broken-rimz");
    std::fs::write(&broken_rimz, "#!/bin/sh\nexit 1\n").expect("write broken rimz stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(&broken_rimz)
            .expect("broken rimz metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&broken_rimz, perms).expect("chmod broken rimz stub");
    }
    let room = RoomHarness::launch_with_rimz_bin(&env, MuxName::Tmux, broken_rimz);

    let screen = room.wait_for(|s| s.contains("Sidebar degraded"), SETTLE);
    assert!(
        screen.contains("Sidebar degraded"),
        "a failed snapshot command should surface a degraded banner:\n{screen}"
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

/// A resolver-in-front row leads with an animated braille spinner, so a live
/// capture may show any frame — confirm the leading cell is one of them.
fn resolver_spinner(screen: &str) -> bool {
    ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
        .iter()
        .any(|frame| screen.contains(*frame))
}

/// Push a script `Question` item straight to the ledger with no worktree so it
/// lands in the `workspace` group. The public `rimz feed ask --no-block`
/// command always attaches the caller's current worktree, so this remains the
/// narrow fixture setup for the workspace-level script state the CLI cannot
/// produce yet.
fn push_workspace_script_ask_fixture(env: &Env, title: &str) {
    let mut item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        title,
        "deploy",
        "script",
    );
    item.runtime_owner = Some(current_process_owner(
        RuntimeOwnerKind::Script,
        item.request_id.to_string(),
    ));
    env.ledger()
        .push_feed_item(&item, "rimz-journey")
        .expect("push script ask");
}

fn push_worktree_script_ask_fixture(env: &Env, title: &str) {
    let mut item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        title,
        "deploy",
        "script",
    );
    item.worktree_path = Some(env.project_root.display().to_string());
    item.runtime_owner = Some(current_process_owner(
        RuntimeOwnerKind::Script,
        item.request_id.to_string(),
    ));
    env.ledger()
        .push_feed_item(&item, "rimz-journey")
        .expect("push worktree script ask");
}
