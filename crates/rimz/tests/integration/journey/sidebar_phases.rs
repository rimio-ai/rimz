//! The user journey, phase by phase (`docs/guide/product.md` and
//! `docs/guide/experience.md`).
//!
//! Backend-neutral: each test drives the renderer once over a real ledger and
//! reads the parsed pane. Renderer mechanics live in `docs/internals/sidebar.md`;
//! layout/tabs/focus live in `backend/zellij.rs`; the actual mux-pane content
//! smokes live in `journey/deep.rs`.

use rimz::EventEnvelope;
use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::ids::MuxName;
use serde_json::json;

use super::{
    RoomHarness, SETTLE, SETTLE_SHORT, permission_request, session_end, session_start,
    session_start_at, user_prompt_submit,
};
use crate::common::Env;

/// Phase 0 — today's empty snapshot on a fresh machine. A healthy empty room
/// points at the real next step. With no hooks wired yet, that step is `rimz
/// hooks install` — "run claude or codex" would be a lie until the hooks land
/// (covered by the onboarding test below).
#[test]
fn phase0_empty_snapshot_shows_first_run_hint() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);

    let screen = room.wait_for(|s| s.contains("rimz hooks install"), SETTLE);
    assert!(
        !screen.contains("all clear"),
        "the empty-room nudge should not spend the top line on all-clear copy:\n{screen}"
    );
    assert!(
        screen.contains("rimz hooks install"),
        "an un-wired empty room must point at hook install, not a dead-end \
         'run claude or codex':\n{screen}"
    );
}

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

/// Phase 1 — launched, no prompt. `SessionStart` registers the agent as
/// `○ idle` with no task; no attention summary is rendered.
#[test]
fn phase1_launch_registers_idle_no_prompt() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));

    // Wait for the agent *row* (`○ codex`), never the bare substring "codex" —
    // the first-run hint "run claude or codex" contains it, so a loose
    // predicate would return the empty room before the row paints.
    let screen = room.wait_for(|s| s.contains("○ codex"), SETTLE);
    assert!(
        screen.contains("▌main"),
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
}

/// Phase 2 — prompted and working. `UserPromptSubmit` moves the agent to
/// `▸ running` with the prompt as its task.
#[test]
fn phase2_prompt_moves_to_running_with_task() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));

    let screen = room.wait_for(|s| s.contains("fix auth flow"), SETTLE);
    assert!(
        screen.contains("▸ codex"),
        "a prompted agent is running:\n{screen}"
    );
    assert!(
        screen.contains("fix auth flow"),
        "the task descriptor is the prompt:\n{screen}"
    );
}

/// Phase 3 — a question waits on you. A `PermissionRequest` (no resolver) writes
/// a feed item: the row flips to `◆ waiting`, the attention line counts it, and
/// the sidebar never reproduces the question. (Implemented.)
#[test]
fn phase3_question_waits_and_counts_attention() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));
    room.agent_hook("codex", &permission_request("sess-1", "DO_NOT_RENDER_ME"));

    let screen = room.wait_for(
        |s| s.contains("◆ codex") && s.contains("fix auth flow"),
        SETTLE,
    );
    assert!(
        screen.contains("◆ codex"),
        "a permission prompt makes the agent wait:\n{screen}"
    );
    assert!(
        screen.contains("◆1"),
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
/// waiting row shows the chain working (`⟳ <resolver> <budget>`) instead of
/// `◆`, and still counts in the attention tally. (Implemented; stretch.)
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
    let mut child = room.spawn_agent(
        "codex",
        "PermissionRequest",
        &permission_request("sess-1", "DO_NOT_RENDER_ME"),
    );

    let screen = room.wait_for(|s| s.contains("⟳"), SETTLE);
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        screen.contains("⟳"),
        "a resolver in front replaces ◆ with ⟳ on the row:\n{screen}"
    );
    assert!(
        screen.contains("opus-policy"),
        "the row names the active resolver:\n{screen}"
    );
    assert!(
        screen.contains("◆1"),
        "a delegated item is still pending, so it still counts in the tally:\n{screen}"
    );
}

/// Phase 4 — a fleet across worktrees. Agents spread across `main` and
/// `feature-migration`, plus a `workspace` script paused at a gate. Grouping
/// and the worktree headers are implemented; the script lands in the
/// `workspace` group because it is not tied to a worktree.
#[test]
fn phase4_fleet_groups_and_tallies() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // A workspace-group script paused at a gate. The CLI always attaches the
    // cwd worktree, so push straight to the ledger with no worktree to land it
    // in the `workspace` group (scripts not tied to a worktree render there).
    push_workspace_script_ask_fixture(&env, "promote release?");

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex", "claude"]);
    room.agent_hook("codex", &session_start("m1", "GPT-5.5", "high", "main"));
    room.agent_hook("claude", &session_start("m2", "Opus", "xhigh", "main"));
    room.agent_hook(
        "codex",
        &session_start("f1", "GPT-5.5", "low", "feature-migration"),
    );

    let screen = room.wait_for(|s| s.contains("▌feature-migration"), SETTLE);
    assert!(
        screen.contains("▌main"),
        "the main worktree group renders:\n{screen}"
    );
    assert!(
        screen.contains("▌feature-migration"),
        "the feature-migration worktree group renders:\n{screen}"
    );
    assert!(
        screen.contains("▌workspace"),
        "scripts not tied to a worktree live in the workspace group:\n{screen}"
    );
    assert!(
        screen.contains("promote release?"),
        "the script's ask shows its title:\n{screen}"
    );
    assert!(
        screen.contains("◆1"),
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
    // A waiting ask and a calm agent in the same worktree, both produced
    // through public user surfaces where possible. `feed ask --no-block`
    // attaches the current worktree, so the agent reports that same path.
    env.feed_ask_no_block("approve deploy?", &["yes", "no"]);
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
        screen.contains("▌main"),
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
    let fake_rimz = env.project_root.join("missing-rimz-bin");
    let room = RoomHarness::launch_with_rimz_bin(&env, MuxName::Tmux, fake_rimz);

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

#[test]
#[ignore = "TDD: first-run consent gate copy not implemented yet"]
fn target_first_run_consent_gate_is_additive_skippable_and_reversible() {
    let env = Env::new();
    let bin_dir = tempfile::TempDir::new().expect("stub agent bin");
    let codex = bin_dir.path().join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").expect("write codex stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(&codex)
            .expect("codex metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex, perms).expect("chmod codex stub");
    }
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin_dir.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .expect("join PATH");
    let out = env
        .rimz()
        .arg("--no-attach")
        .env("PATH", path)
        .output()
        .expect("spawn rimz first run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        text.contains("first run on this machine"),
        "first run should show a consent gate before the room:\n{text}"
    );
    assert!(
        text.contains("additive") && text.contains("show full diff"),
        "the consent gate must frame hook writes as an additive diff:\n{text}"
    );
    assert!(
        text.contains("choose per agent") && text.contains("skip"),
        "the gate must offer choose and skip paths:\n{text}"
    );
    assert!(
        text.contains("rimz hooks uninstall"),
        "the gate must name the reversible uninstall path:\n{text}"
    );
    assert!(
        !env.agent_hooks_installed("codex") && !env.agent_hooks_installed("claude"),
        "viewing/skipping the gate must not install hooks"
    );
}

#[test]
#[ignore = "TDD: empty-room shell process row not rendered yet"]
fn target_empty_room_presence_shows_shell_row_and_hook_hint() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);

    let screen = room.wait_for(|s| s.contains("· zsh"), SETTLE);
    assert!(
        screen.contains("· zsh"),
        "a healthy empty room should still show the shell pane row:\n{screen}"
    );
    assert!(
        !screen.contains("Sidebar degraded"),
        "the first shell frame must parse process rows without degrading:\n{screen}"
    );
    assert!(
        !contains_agent_row(&screen),
        "a fresh empty shell must not reuse stale agent rows:\n{screen}"
    );
    assert!(
        screen.contains("rimz hooks install"),
        "with hooks skipped/unwired, the shell row should keep the hook install hint:\n{screen}"
    );
}

#[test]
#[ignore = "TDD: stale agent attention without a live pane still renders"]
fn target_empty_room_ignores_stale_agent_attention_without_live_pane() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    push_stale_agent_attention_fixture(&env, "claude", "dead-claude");

    let room = RoomHarness::launch(&env, MuxName::Tmux);

    let screen = room.wait_for(|s| s.contains("· zsh"), SETTLE);
    assert!(
        screen.contains("· zsh"),
        "the live shell pane should still render as the only row:\n{screen}"
    );
    assert!(
        !contains_agent_row(&screen),
        "a pending agent item without a live agent pane must not render as an agent row:\n{screen}"
    );
    assert!(
        !screen.contains("◆"),
        "stale agent attention must not count against the room:\n{screen}"
    );
}

#[test]
#[ignore = "TDD: live pane must not inherit stale cross-agent attention"]
fn target_live_codex_process_does_not_inherit_stale_claude_attention() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);

    push_idle_agent_fixture(&env, "claude", "stale-claude", "claude-opus-4-7");
    push_stale_agent_attention_fixture_at(
        &env,
        "claude",
        "stale-claude",
        jiff::Timestamp::now() - std::time::Duration::from_secs(17 * 60),
    );

    // The user starts Codex before its hook fires. The live pane is Codex, so
    // the sidebar must not route an old Claude prompt to that pane or reuse
    // the prompt's 17m age.
    room.agent_hook(
        "codex",
        &session_start_at(
            "fresh-codex",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            None,
        ),
    );

    let screen = room.wait_for(|s| s.contains("· codex") || s.contains("◆ claude"), SETTLE);
    assert!(
        screen.contains("· codex"),
        "an unwired live codex pane should remain a codex process row:\n{screen}"
    );
    assert!(
        !screen.contains("◆ claude"),
        "a stale claude ask must not steal the live codex pane:\n{screen}"
    );
    assert!(
        !screen.contains("17m"),
        "a fresh codex process must not inherit the stale claude ask age:\n{screen}"
    );
}

#[test]
#[ignore = "TDD: ambiguous old-rollup reuse not yet prevented"]
fn target_live_codex_process_does_not_reuse_ambiguous_old_codex_rollup() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    let old = jiff::Timestamp::now() - std::time::Duration::from_secs(17 * 60);

    push_idle_agent_fixture_at(&env, "codex", "old-codex-1", "GPT-5.5", old);
    push_idle_agent_fixture_at(&env, "codex", "old-codex-2", "GPT-5.5", old);

    room.agent_hook(
        "codex",
        &session_start_at(
            "fresh-codex",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            None,
        ),
    );

    let screen = room.wait_for(|s| s.contains("· codex") || s.contains("○ codex"), SETTLE);
    assert!(
        screen.contains("· codex"),
        "when multiple old codex sessions could claim one pane, the fresh \
         unwired pane should stay a process row:\n{screen}"
    );
    assert!(
        !screen.contains("○ codex"),
        "the sidebar must not choose an arbitrary old codex rollup:\n{screen}"
    );
    assert!(
        !screen.contains("17m"),
        "a fresh codex process must not inherit an old codex rollup age:\n{screen}"
    );
}

#[test]
#[ignore = "TDD: unwired agent still renders as an idle agent row"]
fn target_unwired_agent_stays_process_row_not_idle_agent() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);

    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    let screen = room.wait_for(|s| s.contains("· codex"), SETTLE_SHORT);
    assert!(
        screen.contains("· codex"),
        "an unwired running codex process should show as a plain process row:\n{screen}"
    );
    assert!(
        !screen.contains("○ codex"),
        "an unwired agent must not look like a hooked idle agent:\n{screen}"
    );
}

#[test]
#[ignore = "TDD: agent-exit pane revert to shell not implemented yet"]
fn target_agent_exit_reverts_to_shell_or_vanishes() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["claude"]);

    room.agent_hook("claude", &session_start("sess-1", "Opus", "xhigh", "main"));
    room.wait_for(|s| s.contains("○ claude"), SETTLE);
    room.agent_hook("claude", &session_end("sess-1"));

    let screen = room.wait_for(|s| !contains_claude_row(s), SETTLE);
    assert!(
        !contains_claude_row(&screen),
        "after the agent exits, its agent row should be gone:\n{screen}"
    );
    assert!(
        !screen.contains("Sidebar degraded"),
        "reverting an agent pane to a process row must not degrade the renderer:\n{screen}"
    );
    assert!(
        screen.contains("· zsh") || !screen.contains("▌main"),
        "the pane should either revert to its shell row or disappear with the pane:\n{screen}"
    );
}

fn contains_claude_row(screen: &str) -> bool {
    ["○ claude", "▸ claude", "◆ claude", "✗ claude"]
        .iter()
        .any(|needle| screen.contains(needle))
}

fn contains_agent_row(screen: &str) -> bool {
    [
        "○ claude",
        "▸ claude",
        "◆ claude",
        "✗ claude",
        "○ codex",
        "▸ codex",
        "◆ codex",
        "✗ codex",
    ]
    .iter()
    .any(|needle| screen.contains(needle))
}

#[test]
#[ignore = "TDD: native footer keys and help legend not implemented yet"]
fn target_native_footer_keys_and_help_legend_render() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex"]);
    room.agent_hook("codex", &session_start("sess-1", "GPT-5.5", "high", "main"));
    room.agent_hook("codex", &user_prompt_submit("sess-1", "fix auth flow"));
    room.agent_hook("codex", &permission_request("sess-1", "DO_NOT_RENDER_ME"));

    let screen = room.wait_for(|s| s.contains("↵ jump"), SETTLE);
    assert!(
        screen.contains("↵ jump") && screen.contains("␣ next ◆") && screen.contains("?"),
        "the native footer should advertise jump, next-attention, and help keys:\n{screen}"
    );

    room.send_keys("?");
    let help = room.wait_for(|s| s.contains("keys & legend"), SETTLE);
    assert!(
        help.contains("◆ waiting") && help.contains("✗ failed") && help.contains("○ idle"),
        "the `?` key should open the in-place legend:\n{help}"
    );
}

#[test]
#[ignore = "TDD: fleet attention-row cap behavior not implemented yet"]
fn target_full_fleet_tallies_script_gate_and_never_caps_attention_rows() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    push_workspace_script_ask_fixture(&env, "promote release?");

    let room = RoomHarness::launch(&env, MuxName::Tmux);
    room.onboard(&["codex", "claude"]);
    for index in 0..8 {
        let session_id = format!("m{index}");
        room.agent_hook(
            "codex",
            &session_start(&session_id, "GPT-5.5", "high", "main"),
        );
    }
    room.agent_hook("codex", &user_prompt_submit("m0", "fix auth flow"));
    room.agent_hook("codex", &permission_request("m0", "DO_NOT_RENDER_ME"));
    room.agent_hook(
        "claude",
        &session_start("failed-1", "Opus", "xhigh", "feature-migration"),
    );
    push_failed_agent_fixture(&env, "failed-1", "feature-migration");

    let screen = room.wait_for(
        |s| s.contains("◆2") && s.contains("✗1") && s.contains("+"),
        SETTLE,
    );
    assert!(
        screen.contains("◆2") && screen.contains("✗1"),
        "the attention line should count waiting and failed rows across all groups:\n{screen}"
    );
    assert!(
        screen.contains("promote release?"),
        "the workspace script gate should stay visible:\n{screen}"
    );
    assert!(
        screen.contains("fix auth flow") && screen.contains("+"),
        "per-worktree caps may hide calm rows but never attention rows:\n{screen}"
    );
}

/// Push a script `Question` item straight to the ledger with no worktree so it
/// lands in the `workspace` group. The public `rimz feed ask --no-block`
/// command always attaches the caller's current worktree, so this remains the
/// narrow fixture setup for the workspace-level script state the CLI cannot
/// produce yet.
fn push_workspace_script_ask_fixture(env: &Env, title: &str) {
    let item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        title,
        "deploy",
        "script",
    );
    env.ledger()
        .push_feed_item(&item, "rimz-journey")
        .expect("push script ask");
}

fn push_stale_agent_attention_fixture(env: &Env, source: &str, session_id: &str) {
    push_stale_agent_attention_fixture_at(env, source, session_id, jiff::Timestamp::now());
}

fn push_stale_agent_attention_fixture_at(
    env: &Env,
    source: &str,
    session_id: &str,
    updated_at: jiff::Timestamp,
) {
    let mut item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        format!("{source} needs attention"),
        source,
        "agent-hook",
    );
    item.worktree_path = Some(env.project_root.display().to_string());
    item.payload = json!({ "session_id": session_id });
    item.created_at = updated_at;
    item.updated_at = updated_at;
    env.ledger()
        .push_feed_item(&item, "rimz-journey")
        .expect("push stale agent attention");
}

fn push_idle_agent_fixture(env: &Env, source: &str, session_id: &str, model: &str) {
    push_idle_agent_fixture_at(env, source, session_id, model, jiff::Timestamp::now());
}

fn push_idle_agent_fixture_at(
    env: &Env,
    source: &str,
    session_id: &str,
    model: &str,
    timestamp: jiff::Timestamp,
) {
    let event = EventEnvelope::new(
        env.workspace_id.clone(),
        "rimz-journey",
        source,
        "agent-hook",
        "agent.lifecycle",
        json!({
            "agent_id": session_id,
            "status": "idle",
            "mode": "interactive",
            "worktree_path": env.project_root.display().to_string(),
            "worktree_branch": "main",
            "task": null,
            "model": model,
            "effort": null,
        }),
    );
    let mut event = event;
    event.timestamp = timestamp;
    env.ledger()
        .append_event(&event)
        .expect("push idle agent fixture");
}

fn push_failed_agent_fixture(env: &Env, session_id: &str, branch: &str) {
    let event = EventEnvelope::new(
        env.workspace_id.clone(),
        "rimz-journey",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        json!({
            "agent_id": session_id,
            "status": "failed",
            "mode": "interactive",
            "worktree_path": format!("/work/query-engine-{branch}"),
            "worktree_branch": branch,
            "task": "db migrate",
            "model": "Opus",
            "effort": "xhigh",
        }),
    );
    env.ledger()
        .append_event(&event)
        .expect("push failed agent fixture");
}
