//! Deep end-to-end smokes: the *real* sidebar pane in a *real* multiplexer.
//!
//! The phase journey (`sidebar_phases.rs`) drives the renderer through a
//! `portable-pty`; these two tests close the loop by birthing a real session
//! with a real `rimz sidebar serve` pane, firing an agent hook, and capturing what
//! the actual mux pane shows. They self-skip without the mux binary (the
//! common CI shape) and under a socket-bind sandbox.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::diag::record::DiagEvent;
use rimz::mux::{MuxBackend, PaneListOptions, PaneReadConsistency, ZellijBackend};
use tempfile::TempDir;

use super::{RoomHarness, SETTLE, rimz_bin, session_start_at};
use crate::common::{
    CommandTimeoutExt, Env, ScrubSessionEnvExt, ZellijNamespace, path_with_front,
    write_hook_firing_agent,
};

const CAPTURE_BUDGET: Duration = Duration::from_secs(30);

/// Trailing diagnostic records carried into a width assertion's message.
const DIAG_EVIDENCE_RECORDS: usize = 40;

/// Shell line that runs the renderer over `env`'s store but with its own short
/// `XDG_RUNTIME_DIR` (the wakeup socket must stay under the AF_UNIX limit).
fn sidebar_serve_line(
    env: &Env,
    rimz: &Path,
    runtime: &Path,
    mux: &str,
    session: &str,
    extra_env: &[(&str, &str)],
) -> String {
    let extra_env = extra_env
        .iter()
        .map(|(key, value)| format!("{key}={value} "))
        .collect::<String>();
    format!(
        "XDG_STATE_HOME={state} XDG_CONFIG_HOME={config} XDG_RUNTIME_DIR={runtime} HOME={home} \
         {extra_env}RIMZ_BIN={rimz} exec {rimz} sidebar serve --mux {mux} --workspace-id {ws} \
         --session-name {session} --tick-seconds 1",
        state = env.state_root().display(),
        config = env.config_root().display(),
        runtime = runtime.display(),
        home = env.project_root.display(),
        rimz = rimz.display(),
        ws = env.workspace_id.as_str(),
    )
}

fn fake_codex_bin(dir: &Path) -> PathBuf {
    let target = which::which("sleep").expect("sleep binary");
    let path = dir.join("codex");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &path).expect("symlink fake codex");
    #[cfg(not(unix))]
    std::fs::copy(&target, &path).expect("copy fake codex");
    path
}

/// tmux: split a real `rimz sidebar serve` pane beside a live command, fire `codex
/// SessionStart`, and capture the sidebar pane until the agent row appears.
#[test]
fn tmux_room_shows_agent_after_hook() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping deep tmux smoke");
        return;
    }
    let Some(rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping deep tmux smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // The agent claims branch `main`, modelling a repo room — make the room
    // root one, or the directory room's name-only root pod (correctly)
    // suppresses the branch label this test waits for.
    let git_init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&env.project_root)
        .status();
    if !git_init.map(|status| status.success()).unwrap_or(false) {
        eprintln!("git unavailable; skipping deep tmux smoke");
        return;
    }

    let server_dir = TempDir::new().expect("tmux socket dir");
    let runtime = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("short runtime dir");
    let socket = managed_socket(runtime.path());
    let _server = TmuxServerGuard::new(socket.clone());
    let fake_codex = fake_codex_bin(server_dir.path());

    // A session with a foreground agent-shaped command, then a sidebar pane
    // beside it. The hook still drives store identity; the live pane list
    // supplies presence.
    tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            "room",
            "-x",
            "120",
            "-y",
            "40",
            "-c",
            &env.project_root.display().to_string(),
            &format!("{} 60", fake_codex.display()),
        ],
    );
    // The fake_codex pane is the only one until we split; capture its id so the
    // hook stamps it exactly as TMUX_PANE would inside that pane, binding the
    // agent row to its live pane.
    let codex_pane = tmux_capture(&socket, &["list-panes", "-t", "room", "-F", "#{pane_id}"]);
    let codex_pid = tmux_capture(
        &socket,
        &["display-message", "-p", "-t", &codex_pane, "#{pane_pid}"],
    );
    let serve = sidebar_serve_line(&env, &rimz, runtime.path(), "tmux", "room", &[]);
    tmux(&socket, &["split-window", "-h", "-t", "room", &serve]);

    // Wire codex the way the user does, then run it through its installed
    // hook against the shared store — the only way a real agent reaches RimZ.
    env.install_agent_hooks("codex");
    let hook_env = [
        ("TMUX_PANE", codex_pane.as_str()),
        ("RIMZ_AGENT_PID", codex_pid.as_str()),
        (rimz::harness::launch::ENV_AGENT_ROLE, "coder"),
        (rimz::harness::launch::ENV_AGENT_PROFILE, "codex-coder"),
    ];
    let out = env.run_installed_hook_in_pane(
        "codex",
        &session_start_at(
            "sess-1",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        )
        .to_string(),
        &hook_env,
    );
    assert!(
        out.status.success(),
        "codex hook failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The split pane (active) is the sidebar; capture it until a *complete*
    // frame lands. Waiting for both the row and its worktree header (not just
    // the first frame that mentions the agent) rides out a partial repaint
    // captured mid-paint under load.
    let screen = capture_until(
        &socket,
        "room",
        |s| s.contains("coder") && s.contains("main"),
        CAPTURE_BUDGET,
    );
    assert!(
        screen.contains("coder"),
        "the live tmux sidebar pane should show the agent row:\n{screen}"
    );
    assert!(
        screen.contains("main"),
        "the agent should appear under its worktree group:\n{screen}"
    );
}

/// tmux: closing a work column returns its width to the remaining work panes,
/// not the fixed-width sidebar.
#[test]
fn tmux_sidebar_keeps_width_when_work_pane_closes() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping deep tmux resize smoke");
        return;
    }
    let Some(rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping deep tmux resize smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let runtime = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("short runtime dir");
    let socket = managed_socket(runtime.path());
    let _server = TmuxServerGuard::new(socket.clone());

    tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            "room",
            "-x",
            "160",
            "-y",
            "40",
            "sleep 300",
        ],
    );
    tmux(
        &socket,
        &["set-option", "-t", "room", "@rimz_sidebar_cols", "40"],
    );
    let first_work = tmux_capture(&socket, &["list-panes", "-t", "room", "-F", "#{pane_id}"]);
    let serve = sidebar_serve_line(&env, &rimz, runtime.path(), "tmux", "room", &[]);
    let sidebar = tmux_capture(
        &socket,
        &[
            "split-window",
            "-h",
            "-b",
            "-d",
            "-l",
            "40",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &first_work,
            &serve,
        ],
    );
    tmux(
        &socket,
        &["split-window", "-h", "-d", "-t", &first_work, "sleep 300"],
    );
    tmux(
        &socket,
        &["split-window", "-h", "-d", "-t", &first_work, "sleep 300"],
    );
    tmux(&socket, &["select-layout", "-t", "room", "even-horizontal"]);

    env.wait_for_diag(
        "room",
        |record| {
            matches!(
                record.event,
                DiagEvent::SidebarWidthSettle {
                    settled_cols: 40,
                    ..
                }
            )
        },
        CAPTURE_BUDGET,
    );
    assert_eq!(tmux_pane_width(&socket, &sidebar), Some(40));

    let adjacent_work = tmux_capture(
        &socket,
        &["list-panes", "-t", "room", "-F", "#{pane_index} #{pane_id}"],
    )
    .lines()
    .find_map(|line| {
        let (index, pane) = line.split_once(' ')?;
        (index == "1").then(|| pane.to_owned())
    })
    .expect("work pane adjacent to sidebar");
    tmux(&socket, &["kill-pane", "-t", &adjacent_work]);

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && tmux_pane_width(&socket, &sidebar) != Some(40) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let settled = tmux_pane_width(&socket, &sidebar);
    let configured = tmux_capture(
        &socket,
        &["show-option", "-qv", "-t", "room", "@rimz_sidebar_cols"],
    );
    let diag = env.diag_tail("room", DIAG_EVIDENCE_RECORDS);
    assert_eq!(
        settled,
        Some(40),
        "sidebar did not return to its fixed width after the adjacent work pane closed; \
         @rimz_sidebar_cols={configured}\n{diag}"
    );
    assert_eq!(
        configured, "40",
        "structural redistribution must not become the session-wide sidebar default\n{diag}"
    );
}

/// tmux: prove the sidebar self-closes when its last sibling dies *without*
/// flashing to the freed full width on the way out.
///
/// Closing the working pane first grows the sidebar to the whole window (a
/// SIGWINCH), and the renderer holds that grow-repaint until the sibling-count
/// verdict lands — a "close" verdict exits without painting the grown frame on
/// the healthy path. The hold is bounded, so a verdict that never arrives may
/// paint after `RESIZE_PAINT_HOLD_CEILING`. This drives the real path end to
/// end: split a sidebar beside a live command, let it latch `seen_sibling`, kill
/// the command, then sample the sidebar pane until it vanishes.
///
/// Best-effort on the flash itself: the flash (if the guard regressed) is a
/// single sub-frame paint, so a poll may miss it. Before the hold ceiling, a
/// sampled wide frame is a real regression; after the ceiling, painting wide is
/// the bounded recovery behavior. The authoritative guards are the `resize_grew`
/// and `PaintHold` unit tests plus the frame-phase `!should_exit`/hold-blocked
/// gate; this closes the loop in a real mux. The path is backend-agnostic (the
/// decision is the same on Zellij), so one backend smoke is representative.
#[test]
fn tmux_sidebar_self_closes_without_full_width_flash() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping deep tmux self-close smoke");
        return;
    }
    let Some(rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping deep tmux self-close smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let server_dir = TempDir::new().expect("tmux socket dir");
    let runtime = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("short runtime dir");
    let socket = managed_socket(runtime.path());
    let _server = TmuxServerGuard::new(socket.clone());
    let fake_codex = fake_codex_bin(server_dir.path());

    tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            "room",
            "-x",
            "120",
            "-y",
            "40",
            "-c",
            &env.project_root.display().to_string(),
            &format!("{} 60", fake_codex.display()),
        ],
    );
    let codex_pane = tmux_capture(&socket, &["list-panes", "-t", "room", "-F", "#{pane_id}"]);
    let codex_pid = tmux_capture(
        &socket,
        &["display-message", "-p", "-t", &codex_pane, "#{pane_pid}"],
    );
    let serve = sidebar_serve_line(
        &env,
        &rimz,
        runtime.path(),
        "tmux",
        "room",
        &[("RIMZ_TEST_PANE_CARRY_MS", "3000")],
    );
    tmux(&socket, &["split-window", "-h", "-t", "room", &serve]);

    // Drive a real agent until its row renders. That row means a snapshot
    // enumerated the live panes, so the self-close latch has seen its sibling
    // (the codex pane). Now an empty tab means teardown.
    env.install_agent_hooks("codex");
    let hook_env = [
        ("TMUX_PANE", codex_pane.as_str()),
        ("RIMZ_AGENT_PID", codex_pid.as_str()),
        (rimz::harness::launch::ENV_AGENT_ROLE, "coder"),
        (rimz::harness::launch::ENV_AGENT_PROFILE, "codex-coder"),
    ];
    let out = env.run_installed_hook_in_pane(
        "codex",
        &session_start_at(
            "sess-1",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        )
        .to_string(),
        &hook_env,
    );
    assert!(
        out.status.success(),
        "codex hook failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let latched = capture_until(&socket, "room", |s| s.contains("coder"), CAPTURE_BUDGET);
    assert!(
        latched.contains("coder"),
        "the sidebar must render its sibling before we test self-close:\n{latched}"
    );

    // The active pane is the sidebar split. Record its id and pre-close width;
    // a held grow keeps painted content within that width, a flash spills toward
    // the full 120 columns.
    let (sidebar_pane, split_width) = tmux_current_pane(&socket, "room");
    let flash_ceiling = split_width + 5;

    tmux(&socket, &["kill-pane", "-t", &codex_pane]);
    let killed_at = Instant::now();
    let flash_guard_deadline = killed_at + rimz::sidebar::timing::RESIZE_PAINT_HOLD_CEILING;

    // Sample fast until the sidebar pane is gone (it self-closed) or the budget
    // elapses. Every frame we see before the hold ceiling must stay within the
    // split width; after the ceiling, wide paint is the designed escape hatch.
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let mut closed = false;
    let mut closed_after = None;
    while Instant::now() < deadline {
        if !tmux_pane_alive(&socket, "room", &sidebar_pane) {
            closed = true;
            closed_after = Some(killed_at.elapsed());
            break;
        }
        let frame = capture_until(&socket, &sidebar_pane, |_| true, Duration::from_millis(0));
        let sampled_at = Instant::now();
        let widest = max_line_width(&frame);
        if sampled_at < flash_guard_deadline {
            assert!(
                widest <= flash_ceiling,
                "sidebar painted {widest} cols wide before self-close (split was \
                 {split_width}); it flashed toward the freed full width:\n{frame}"
            );
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        closed,
        "the sidebar never self-closed after its last sibling died"
    );
    let closed_after = closed_after.expect("closed path records elapsed time");
    assert!(
        closed_after < Duration::from_secs(10),
        "the sidebar self-closed after {closed_after:?}; the test carry TTL override should keep \
         this path well below the 30s production carry window"
    );
}

/// Zellij: same arc through a real Zellij session. Self-skips without `zellij`.
#[test]
fn zellij_room_shows_agent_and_holds_width_keys() {
    if which::which("zellij").is_err() {
        eprintln!("zellij not on PATH; skipping deep zellij smoke");
        return;
    }
    let Some(rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping deep zellij smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let namespace = ZellijNamespace::new();
    let cwd = TempDir::new().expect("cwd dir");
    let fake_codex = fake_codex_bin(cwd.path());
    let name = "rimz-deep-zellij";
    let cleanup = ZellijSessionGuard {
        name: name.to_owned(),
        namespace,
    };
    let runtime = cleanup.namespace.path();
    let runtime_paths =
        rimz::RuntimePaths::under(env.workspace_id.clone(), runtime).expect("zellij runtime paths");

    // Birth a background session whose left pane is a real renderer over the
    // shared store (the self-close layout shape from `backend/zellij.rs`).
    let serve = sidebar_serve_line(&env, &rimz, runtime, "zellij", name, &[]);
    let layout = format!(
        r#"layout {{
    tab name="room" {{
        pane split_direction="vertical" {{
            pane size="30%" name="rimz-sidebar" {{
                command "sh"
                args "-c" {serve}
            }}
            pane focus=true {{
                command {agent}
                args "60"
            }}
        }}
    }}
}}
"#,
        serve = serde_json::to_string(&serve).expect("kdl escape"),
        agent = serde_json::to_string(&fake_codex.display().to_string()).expect("kdl escape"),
    );
    let layout_path = cwd.path().join("layout.kdl");
    std::fs::write(&layout_path, layout).expect("write layout");
    let created = cleanup
        .namespace
        .command()
        .args(["attach", "--create-background", name, "options"])
        .arg("--default-cwd")
        .arg(&env.project_root)
        .arg("--default-layout")
        .arg(&layout_path)
        .bounded_status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    // Reproduce the real startup ordering: the presence feed can publish the
    // sidebar before the rest of the layout. That one-pane extent must remain
    // unknown rather than becoming the room's viewport basis.
    write_zellij_topology(&cleanup.namespace, &runtime_paths, name, true);
    std::thread::sleep(Duration::from_millis(1_200));

    env.install_agent_hooks("codex");
    let hook_env = [
        (rimz::harness::launch::ENV_AGENT_ROLE, "codex"),
        (rimz::harness::launch::ENV_AGENT_PROFILE, "codex"),
    ];
    let out = env.run_installed_hook_in_pane(
        "codex",
        &session_start_at(
            "sess-1",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        )
        .to_string(),
        &hook_env,
    );
    assert!(
        out.status.success(),
        "codex hook failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // This geometry is load-bearing: below the 240-column policy breakpoint,
    // the correct automatic target aliases the 24-column safety floor closely
    // enough that a sidebar-only viewport bug is invisible.
    let client = AttachedZellijScreen::new(&cleanup.namespace, name, 320, 80);
    let screen = client.wait_until(|screen| screen.contains("codex"), CAPTURE_BUDGET);
    assert!(
        screen.contains("codex"),
        "the live zellij sidebar pane should show the agent row:\n{screen}"
    );

    // Keep the raced feed in place long enough for the old controller to
    // converge against it, then publish the completed tab without a target
    // broadcast. The next keypress must resolve against this proven viewport.
    std::thread::sleep(Duration::from_secs(3));
    write_zellij_topology(&cleanup.namespace, &runtime_paths, name, false);

    let backend = ZellijBackend::with_runtime_dir(runtime);
    let pane = wait_for_zellij_sidebar_pane(&backend, &runtime_paths, name);
    std::thread::sleep(Duration::from_secs(3));
    let initial = wait_for_rendered_sidebar_width(&client, |_| true, "initial sidebar width");

    backend.send_keys(&pane, "d").expect("send wider key");
    let wider =
        wait_for_rendered_sidebar_width(&client, |width| width > initial, "wider sidebar frame");
    std::thread::sleep(Duration::from_secs(3));
    let held_wider = wait_for_rendered_sidebar_width(&client, |_| true, "settled wider frame");
    let diag = env.diag_tail(name, DIAG_EVIDENCE_RECORDS);
    assert_eq!(
        held_wider, wider,
        "the wider sidebar frame reverted after the convergence settle window\n{diag}",
    );

    backend.send_keys(&pane, "a").expect("send narrower key");
    let narrower = wait_for_rendered_sidebar_width(
        &client,
        |width| width < held_wider,
        "narrower sidebar frame",
    );
    std::thread::sleep(Duration::from_secs(3));
    let held_narrower =
        wait_for_rendered_sidebar_width(&client, |_| true, "settled narrower frame");
    let diag = env.diag_tail(name, DIAG_EVIDENCE_RECORDS);
    assert_eq!(
        held_narrower, narrower,
        "the narrower sidebar frame reverted after the convergence settle window\n{diag}",
    );
}

#[test]
fn tmux_steer_delivers_text_and_enter_to_real_agent_pane() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping deep tmux steer smoke");
        return;
    }
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let (socket, session, _pane, _server) = real_agent_room(&env, "sess-steer-enter");

    let out = run_steer(&env, &socket, &["@codex", "--", "focus the parser test"]);
    assert!(
        out.status.success(),
        "steer failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let screen = capture_all_until(
        &socket,
        &session,
        |s| s.contains("SUBMITTED:focus the parser test"),
        CAPTURE_BUDGET,
    );
    assert!(
        screen.contains("SUBMITTED:focus the parser test"),
        "steer should submit a discrete Enter after the prompt:\n{screen}"
    );
}

#[test]
fn tmux_steer_without_enter_suppresses_submit_in_real_agent_pane() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping deep tmux steer smoke");
        return;
    }
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let (socket, session, _pane, _server) = real_agent_room(&env, "sess-steer-no-enter");

    let out = run_steer(
        &env,
        &socket,
        &["@codex", "--no-enter", "--", "hold the line"],
    );
    assert!(
        out.status.success(),
        "steer --no-enter failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let screen = capture_all_until(
        &socket,
        &session,
        |s| s.contains("hold the line"),
        CAPTURE_BUDGET,
    );
    assert!(
        screen.contains("hold the line"),
        "steer --no-enter should still type the prompt:\n{screen}"
    );
    assert!(
        !screen.contains("SUBMITTED:hold the line"),
        "steer --no-enter should not send the submitting Enter:\n{screen}"
    );
}

#[test]
fn tmux_supervised_print_launches_hook_firing_agent_binary() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping supervised tmux smoke");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping supervised tmux smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let stub_dir = write_hook_firing_agent(&env, "codex");
    let agent_path = path_with_front(&stub_dir);
    trust_codex_agent_path(&env, &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());

    // `rimz agents -p` births the tmux session and run tab cold, launches the
    // trusted agent binary, and waits for it. The stub fires its hooks against
    // the shared store, then exits 0 with a final `stub done` message that the
    // supervised run surfaces on stdout. Reading the child's stdout directly
    // keeps this on the deterministic launch-and-exit path; the run's sidebar
    // rendering is owned by `tmux_room_shows_agent_after_hook`, which avoids the
    // cold-start-versus-run-lifetime race a concurrent capture here would invite.
    let out = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "summarize the diff",
            "--name",
            "journey-runner",
            "-p",
            "--timeout",
            "30s",
            "--keep",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("wait supervised print");
    assert!(
        out.status.success(),
        "supervised print failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("stub done"),
        "supervised print should emit the hook-firing stub's final message:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tmux_subagent_nests_under_parent_and_parent_stop_cascades() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping subagent tmux smoke");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping subagent tmux smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let stub_dir = write_hook_firing_agent(&env, "codex");
    let agent_path = path_with_front(&stub_dir);
    trust_codex_agent_path(&env, &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let session = workspace_session(&env);

    let parent = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("RIMZ_TEST_AGENT_SESSION", "sess-journey-parent")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "30000")
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "coordinate the review",
            "--name",
            "journey-parent",
            "-p",
            "--bg",
            "--keep",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch parent");
    assert!(
        parent.status.success(),
        "parent launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&parent.stdout),
        String::from_utf8_lossy(&parent.stderr)
    );

    let parent_agent = wait_for_named_agent(&env, "journey-parent", true, CAPTURE_BUDGET);
    let parent_launch_id = parent_agent
        .launch_id
        .clone()
        .expect("RimZ-launched parent has launch id");
    let parent_pane = wait_for_named_run(&env, "journey-parent", CAPTURE_BUDGET)
        .pane_id
        .expect("parent run pane");
    let parent_pane_raw = parent_pane.raw().to_owned();

    // New panes inherit the tmux server environment, not the environment of
    // the client asking tmux to create them. Give each provider a distinct
    // native session so the cascade exercises adopted child identities.
    tmux(
        &socket,
        &[
            "set-environment",
            "-t",
            &session,
            "RIMZ_TEST_AGENT_SESSION",
            "sess-journey-child",
        ],
    );

    let child = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-journey-child")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "30000")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "inspect the implementation",
            "--name",
            "journey-child",
            "--keep",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch subagent");
    assert!(
        child.status.success(),
        "subagent launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&child.stdout).trim(),
        "journey-child"
    );

    // The first launch returns only after its wrapper's durable pane binding
    // is visible. Launch the sibling immediately to prove that signal reaches
    // the next placement decision.
    tmux(
        &socket,
        &[
            "set-environment",
            "-t",
            &session,
            "RIMZ_TEST_AGENT_SESSION",
            "sess-journey-sibling",
        ],
    );
    let sibling = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-journey-sibling")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "30000")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "review the implementation",
            "--name",
            "journey-sibling",
            "--keep",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch sibling subagent");
    assert!(
        sibling.status.success(),
        "sibling launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sibling.stdout),
        String::from_utf8_lossy(&sibling.stderr)
    );

    let child_agent = wait_for_named_agent(&env, "journey-child", false, CAPTURE_BUDGET);
    assert_eq!(
        child_agent.parent_agent_id.as_ref(),
        Some(&parent_agent.agent_id)
    );
    assert_eq!(child_agent.launch_depth, Some(1));
    let sibling_agent = wait_for_named_agent(&env, "journey-sibling", false, CAPTURE_BUDGET);
    assert_eq!(
        sibling_agent.parent_agent_id.as_ref(),
        Some(&parent_agent.agent_id)
    );
    let child_pane = wait_for_named_run(&env, "journey-child", CAPTURE_BUDGET)
        .pane_id
        .expect("child run pane");
    let sibling_pane = wait_for_named_run(&env, "journey-sibling", CAPTURE_BUDGET)
        .pane_id
        .expect("sibling run pane");
    let parent_position = tmux_pane_position(&socket, parent_pane.raw());
    let child_position = tmux_pane_position(&socket, child_pane.raw());
    let sibling_position = tmux_pane_position(&socket, sibling_pane.raw());
    assert!(
        parent_position.0 < child_position.0,
        "first child must split right of parent: parent={parent_position:?}, child={child_position:?}"
    );
    assert_eq!(
        child_position.0, sibling_position.0,
        "siblings must share one right-hand column: child={child_position:?}, sibling={sibling_position:?}"
    );
    assert_ne!(
        child_position.1, sibling_position.1,
        "tmux stacks sibling panes vertically"
    );

    let room = RoomHarness::launch(&env, rimz::ids::MuxName::Tmux);
    let screen = room.wait_for(
        |screen| {
            screen.contains("subagents (2)")
                && screen.contains("journey-child")
                && screen.contains("journey-sibling")
        },
        SETTLE,
    );
    assert!(
        screen.contains("subagents (2)")
            && screen.contains("journey-child")
            && screen.contains("journey-sibling"),
        "children should render only in the parent's subagent section:\n{screen}"
    );
    assert_eq!(
        screen.matches("journey-child").count(),
        1,
        "child should not also render as a top-level card:\n{screen}"
    );
    assert_eq!(
        screen.matches("journey-sibling").count(),
        1,
        "sibling should not also render as a top-level card:\n{screen}"
    );

    let stopped = env
        .rimz()
        .env("TMUX", tmux_env(&socket))
        .args(["--mux", "tmux", "agents", "stop", "@journey-parent"])
        .bounded_output_within(Duration::from_secs(20))
        .expect("stop parent");
    assert!(
        stopped.status.success(),
        "parent stop failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stopped.stdout),
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("subagent of @journey-parent"),
        "cascade should report the stopped child:\n{}",
        String::from_utf8_lossy(&stopped.stdout)
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && (tmux_pane_alive(&socket, &session, parent_pane.raw())
            || tmux_pane_alive(&socket, &session, child_pane.raw())
            || tmux_pane_alive(&socket, &session, sibling_pane.raw()))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !tmux_pane_alive(&socket, &session, parent_pane.raw()),
        "parent pane should be closed"
    );
    assert!(
        !tmux_pane_alive(&socket, &session, child_pane.raw()),
        "child pane should be closed before its parent"
    );
    assert!(
        !tmux_pane_alive(&socket, &session, sibling_pane.raw()),
        "sibling pane should be closed before its parent"
    );
}

#[test]
fn tmux_completed_subagent_status_lingers_until_parent_pane_disappears() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping subagent parent-watch smoke");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping subagent parent-watch smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let stub_dir = write_hook_firing_agent(&env, "codex");
    let agent_path = path_with_front(&stub_dir);
    trust_codex_agent_path(&env, &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let session = workspace_session(&env);

    let parent = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("RIMZ_TEST_AGENT_SESSION", "sess-parent-watch-parent")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "30000")
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "coordinate the review",
            "--name",
            "parent-watch-parent",
            "-p",
            "--bg",
            "--keep",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch parent");
    assert!(
        parent.status.success(),
        "parent launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&parent.stdout),
        String::from_utf8_lossy(&parent.stderr)
    );

    let parent_agent = wait_for_named_agent(&env, "parent-watch-parent", true, CAPTURE_BUDGET);
    let parent_launch_id = parent_agent
        .launch_id
        .clone()
        .expect("RimZ-launched parent has launch id");
    let parent_provider_pid = parent_agent
        .runtime_owner
        .as_ref()
        .expect("parent runtime owner")
        .pid;
    let parent_wrapper_pid = rimz::proc::comm_and_ppid(parent_provider_pid)
        .map(|(_, ppid)| ppid)
        .expect("parent provider process parent");
    let parent_actual_pane =
        tmux_pane_for_pid(&socket, &session, parent_wrapper_pid).expect("parent wrapper pane");
    let parent_run = wait_for_named_run(&env, "parent-watch-parent", CAPTURE_BUDGET);
    assert!(
        !parent_run.status.is_terminal(),
        "parent run must still be live while its child completes"
    );
    let parent_pane = parent_run.pane_id.expect("parent run pane");
    let parent_pane_raw = parent_pane.raw().to_owned();

    for (key, value) in [
        ("RIMZ_TEST_SUBAGENT_PARENT_PROBE_INTERVAL_MS", "500"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "0"),
        ("RIMZ_TEST_AGENT_SESSION", "sess-parent-watch-child"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }

    let child = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-parent-watch-child")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "0")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "inspect the implementation",
            "--name",
            "parent-watch-child",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch subagent");
    assert!(
        child.status.success(),
        "subagent launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

    let child_run = wait_for_named_terminal_run(&env, "parent-watch-child", CAPTURE_BUDGET);
    assert!(
        child_run.status.is_terminal(),
        "hook-firing child should have completed before the lifecycle assertion"
    );
    let child_pane = child_run.pane_id.expect("child run pane");
    let child_pane_raw = child_pane.raw().to_owned();
    assert_ne!(
        parent_actual_pane, child_pane_raw,
        "parent and child must occupy distinct panes"
    );

    let deadline = Instant::now() + CAPTURE_BUDGET;
    while Instant::now() < deadline && tmux_pane_alive(&socket, &session, &child_pane_raw) {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !tmux_pane_alive(&socket, &session, &child_pane_raw),
        "completed subagent pane should close by default"
    );

    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("read completed child audit projection");
    let child_agent = audit
        .agents
        .iter()
        .find(|agent| agent.name.as_deref() == Some("parent-watch-child"))
        .expect("completed child should remain in durable history");
    assert!(
        child_agent.ended_at.is_some(),
        "closing the completed subagent pane should stamp its durable end: {child_agent:#?}"
    );
    assert_ne!(
        child_agent.status,
        rimz::agents::AgentStatus::Running,
        "retaining the completed child must preserve its terminal verdict"
    );
    let live_projection = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .expect("read live child projection");
    assert!(
        live_projection
            .agents
            .iter()
            .any(|agent| agent.name.as_deref() == Some("parent-watch-child")),
        "ended subagent status should remain visible under its live parent; audit={audit:#?}"
    );

    tmux(&socket, &["kill-pane", "-t", &parent_actual_pane]);

    let deadline = Instant::now() + CAPTURE_BUDGET;
    loop {
        let child_visible = env
            .store()
            .runtime_projection(rimz::RuntimeScope::Runtime)
            .expect("read projection after parent exit")
            .agents
            .iter()
            .any(|agent| agent.name.as_deref() == Some("parent-watch-child"));
        if !child_visible {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "completed subagent status should retire after its parent disappears"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn tmux_supervised_print_returns_failed_when_agent_binary_exits_nonzero() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping supervised tmux smoke");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        eprintln!("rimz not built; skipping supervised tmux smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let stub_dir = write_hook_firing_agent(&env, "codex");
    let agent_path = path_with_front(&stub_dir);
    trust_codex_agent_path(&env, &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());

    let out = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("RIMZ_TEST_AGENT_EXIT", "1")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "1000")
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "summarize the diff",
            "--name",
            "failing-runner",
            "-p",
            "--timeout",
            "30s",
            "--keep",
            "--output-format",
            "json",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("wait failed supervised print");
    assert_eq!(
        out.status.code(),
        Some(1),
        "non-zero agent exit should fail the supervised run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("failed supervised run should print JSON record");
    assert_eq!(
        record.get("status").and_then(serde_json::Value::as_str),
        Some("failed"),
        "non-zero agent exit should produce a failed run record\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        record.get("agent_name").and_then(serde_json::Value::as_str),
        Some("failing-runner"),
        "failed run record should be the launched supervised agent, not a launch precondition error"
    );
}

fn wait_for_named_agent(
    env: &Env,
    name: &str,
    require_bound: bool,
    budget: Duration,
) -> rimz::agents::AgentState {
    let deadline = Instant::now() + budget;
    loop {
        let snapshot = env.store().snapshot().expect("read agent snapshot");
        if let Some(agent) = snapshot.agents.iter().find(|agent| {
            agent.name.as_deref() == Some(name)
                && (!require_bound || !agent.agent_id.is_provisional())
        }) {
            return agent.clone();
        }
        if Instant::now() >= deadline {
            let agents = snapshot
                .agents
                .iter()
                .map(|agent| {
                    (
                        agent.name.as_deref(),
                        agent.agent_id.as_str(),
                        agent.launch_id.as_deref(),
                        agent.parent_agent_id.as_deref(),
                        agent.status,
                    )
                })
                .collect::<Vec<_>>();
            panic!("timed out waiting for agent {name}; agents: {agents:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_named_run(env: &Env, name: &str, budget: Duration) -> rimz::harness::run::RunRecord {
    let deadline = Instant::now() + budget;
    loop {
        let runs = rimz::harness::run::list(env.store().paths()).expect("read runs");
        if let Some(run) = runs
            .into_iter()
            .find(|run| run.agent_name.as_deref() == Some(name) && run.pane_id.is_some())
        {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for run {name}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_named_terminal_run(
    env: &Env,
    name: &str,
    budget: Duration,
) -> rimz::harness::run::RunRecord {
    let deadline = Instant::now() + budget;
    loop {
        let run = wait_for_named_run(env, name, budget);
        if run.status.is_terminal() {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for terminal run {name}; last status: {:?}",
            run.status
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A persistent `portable-pty` client whose parser exposes the composited
/// screen while width actions run against the attached Zellij session.
struct AttachedZellijScreen {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    parser: Arc<Mutex<vt100::Parser>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl AttachedZellijScreen {
    fn new(namespace: &ZellijNamespace, session: &str, cols: u16, rows: u16) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("zellij");
        cmd.args(["attach", session]);
        namespace.pin_pty(&mut cmd);
        let child = pair.slave.spawn_command(cmd).expect("attach zellij");
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let sink = Arc::clone(&parser);
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => sink.lock().expect("parser").process(&chunk[..n]),
                }
            }
        });

        Self {
            child,
            master: Some(pair.master),
            parser,
            reader: Some(reader),
        }
    }

    fn contents(&self) -> String {
        self.parser.lock().expect("parser").screen().contents()
    }

    fn wait_until(&self, mut ready: impl FnMut(&str) -> bool, budget: Duration) -> String {
        let deadline = Instant::now() + budget;
        let mut text = String::new();
        while Instant::now() < deadline {
            text = self.contents();
            if ready(&text) {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        text
    }
}

impl Drop for AttachedZellijScreen {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop(self.master.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn write_zellij_topology(
    namespace: &ZellijNamespace,
    runtime: &rimz::RuntimePaths,
    session: &str,
    sidebar_only: bool,
) {
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let mut panes = loop {
        let output = namespace
            .command()
            .args(["--session", session, "action", "list-panes", "--json"])
            .bounded_output()
            .expect("list Zellij panes for topology fixture");
        assert!(output.status.success(), "list-panes failed for {session}");
        let mut values: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).expect("decode Zellij pane list");
        for value in &mut values {
            let Some(object) = value.as_object_mut() else {
                continue;
            };
            if object
                .get("tab_position")
                .is_some_and(|value| value.is_number())
            {
                object.remove("tab_id");
            } else {
                object.remove("tab_position");
            }
        }
        let panes: Vec<rimz::mux::zellij::pane_topology::PaneTopologyPane> = values
            .into_iter()
            .map(|value| serde_json::from_value(value).expect("decode Zellij pane topology"))
            .collect();
        if panes.iter().filter(|pane| !pane.is_plugin).count() >= 2 {
            break panes;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Zellij layout panes",
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    if sidebar_only {
        let sidebar_id = panes
            .iter()
            .filter(|pane| !pane.is_plugin && !pane.is_floating)
            .min_by_key(|pane| (pane.pane_x.unwrap_or(u64::MAX), pane.id))
            .map(|pane| pane.id)
            .expect("startup fixture needs a tiled pane");
        panes.retain(|pane| !pane.is_plugin && pane.id == sidebar_id);
        assert_eq!(panes.len(), 1, "startup fixture needs one sidebar pane");
    }
    let cache = rimz::mux::zellij::pane_topology::PaneTopologyCache {
        session_name: session.to_owned(),
        produced_at_ms: rimz::sidebar::timing::unix_now_ms(),
        writer: None,
        focused_pane: None,
        clients: None,
        panes,
    };
    rimz::sidebar::cache::write_pane_topology_cache(runtime, &cache)
        .expect("publish Zellij pane topology fixture");
}

fn wait_for_zellij_sidebar_pane(
    backend: &ZellijBackend,
    runtime: &rimz::RuntimePaths,
    session: &str,
) -> rimz::ids::PaneId {
    let deadline = Instant::now() + CAPTURE_BUDGET;
    loop {
        let pane = backend
            .list_panes(PaneListOptions {
                session_name: Some(session.to_owned()),
                runtime_paths: Some(runtime.clone()),
                workspace_id: Some(runtime.workspace_id.clone()),
                consistency: PaneReadConsistency::PreferAuthoritative,
                ..PaneListOptions::default()
            })
            .ok()
            .and_then(|listing| {
                listing
                    .panes
                    .into_iter()
                    .find(|pane| pane.title.as_deref() == Some("rimz-sidebar"))
            });
        if let Some(pane) = pane {
            return pane.pane_id;
        }
        assert!(
            Instant::now() < deadline,
            "timed out locating the live Zellij sidebar pane",
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn rendered_sidebar_width(screen: &str) -> Option<usize> {
    const FOOTER: &str = "? for help";
    screen
        .lines()
        .find_map(|line| line.split_once(FOOTER).map(|(prefix, _)| prefix))
        .map(|prefix| prefix.chars().count() + FOOTER.chars().count())
}

fn wait_for_rendered_sidebar_width(
    client: &AttachedZellijScreen,
    mut ready: impl FnMut(usize) -> bool,
    label: &str,
) -> usize {
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let mut last_width = None;
    loop {
        let last_screen = client.contents();
        if let Some(width) = rendered_sidebar_width(&last_screen) {
            last_width = Some(width);
            if ready(width) {
                return width;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label}; last width {last_width:?}\n{last_screen}",
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn real_agent_room(env: &Env, agent_session: &str) -> (PathBuf, String, String, TmuxServerGuard) {
    let server_dir = TempDir::new().expect("tmux socket dir");
    let socket = managed_socket(&env.runtime_root);
    let agent = server_dir.path().join("codex");
    #[cfg(unix)]
    std::os::unix::fs::symlink("/bin/sh", &agent).expect("symlink steer agent shell");
    #[cfg(not(unix))]
    std::fs::copy("/bin/sh", &agent).expect("copy steer agent shell");
    let server = TmuxServerGuard::with_dir(socket.clone(), server_dir);
    let session = workspace_session(env);
    // The default human sender envelope contributes three newline-terminated
    // header lines. Consume those first so only the discrete Enter after the
    // bracketed paste can finish the body read and produce SUBMITTED.
    let script = "printf 'READY\\n'; \
        IFS= read -r type; \
        IFS= read -r from; \
        IFS= read -r content; \
        IFS= read -r line; \
        printf 'SUBMITTED:%s\\n' \"$line\"; \
        sleep 30";
    tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "120",
            "-y",
            "40",
            "-c",
            &env.project_root.display().to_string(),
            &format!("{} -c {}", agent.display(), shell_quote(script)),
        ],
    );
    let codex_pane = tmux_capture(&socket, &["list-panes", "-t", &session, "-F", "#{pane_id}"]);
    let codex_pid = tmux_capture(
        &socket,
        &["display-message", "-p", "-t", &codex_pane, "#{pane_pid}"],
    );
    env.install_agent_hooks("codex");
    let hook_env = [
        ("TMUX_PANE", codex_pane.as_str()),
        ("RIMZ_AGENT_PID", codex_pid.as_str()),
        (rimz::harness::launch::ENV_AGENT_ROLE, "coder"),
        (rimz::harness::launch::ENV_AGENT_PROFILE, "codex-coder"),
    ];
    let out = env.run_installed_hook_in_pane(
        "codex",
        &session_start_at(
            agent_session,
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        )
        .to_string(),
        &hook_env,
    );
    assert!(
        out.status.success(),
        "codex hook failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (socket, session, codex_pane, server)
}

fn run_steer(env: &Env, socket: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = env.rimz();
    cmd.env("TMUX", tmux_env(socket))
        .args(["--mux", "tmux", "message", "--steer"]);
    cmd.args(args).output().expect("spawn steer")
}

fn workspace_session(env: &Env) -> String {
    rimz::WorkspaceResolver::resolve(&env.project_root, None)
        .expect("resolve workspace")
        .session_name
}

fn tmux_env(socket: &Path) -> String {
    format!("{},0,0", socket.display())
}

fn trust_codex_hooks(env: &Env) {
    let config = env.agent_config_path("codex");
    let mut text = std::fs::read_to_string(&config).expect("read codex config");
    // Leave forward-compatible Interrupt advisory to exercise preflight.
    for token in [
        "session_start",
        "user_prompt_submit",
        "subagent_start",
        "subagent_stop",
        "stop",
        "permission_request",
        "pre_tool_use",
        "post_tool_use",
        "pre_compact",
        "post_compact",
    ] {
        text.push_str(&format!(
            "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
            config.display(),
        ));
    }
    std::fs::write(&config, text).expect("write trust state");
}

fn trust_codex_agent_path(env: &Env, path: &OsStr) {
    #[derive(serde::Serialize)]
    struct Config {
        agents: [Agent; 1],
    }
    #[derive(serde::Serialize)]
    struct Agent {
        name: &'static str,
        env: std::collections::BTreeMap<&'static str, String>,
    }

    let text = toml::to_string(&Config {
        agents: [Agent {
            name: "codex",
            env: std::collections::BTreeMap::from([("PATH", path.to_string_lossy().into_owned())]),
        }],
    })
    .expect("serialize trusted codex PATH config");
    env.write_config(&env.project_root, &text);
    let out = env
        .rimz()
        .args(["trust", "grant"])
        .output()
        .expect("spawn trust grant");
    assert!(
        out.status.success(),
        "trust grant failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// --- tmux helpers ---

fn tmux(socket: &Path, args: &[&str]) {
    tmux_capture(socket, args);
}

/// The endpoint `rimz` resolves from `runtime_root`.
///
/// A pane's `rimz` derives the managed socket from its own `XDG_RUNTIME_DIR`
/// rather than following `$TMUX`, so the harness server has to listen exactly
/// where that derivation points.
fn managed_socket(runtime_root: &Path) -> PathBuf {
    let socket = rimz::mux::tmux::managed_server_socket_path_under(runtime_root);
    std::fs::create_dir_all(socket.parent().expect("socket parent"))
        .expect("mkdir managed socket dir");
    socket
}

/// Run a tmux command and return its trimmed stdout (used to read a pane id).
fn tmux_capture(socket: &Path, args: &[&str]) -> String {
    let out = Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(socket)
        .args(args)
        .bounded_output()
        .expect("spawn tmux");
    assert!(
        out.status.success(),
        "tmux {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Poll `capture-pane` on the session's active pane (the sidebar split) until
/// `pred` holds on the captured frame or the budget elapses. Returns the last
/// frame either way so assertions can print it.
fn capture_until(
    socket: &Path,
    session: &str,
    pred: impl Fn(&str) -> bool,
    budget: Duration,
) -> String {
    let deadline = Instant::now() + budget;
    let mut last = String::new();
    loop {
        let out = Command::new("tmux")
            .scrub_session_env()
            .arg("-S")
            .arg(socket)
            .args(["capture-pane", "-p", "-t", session])
            .bounded_output()
            .expect("spawn tmux capture-pane");
        if out.status.success() {
            last = String::from_utf8_lossy(&out.stdout).into_owned();
            if pred(&last) {
                return last;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn capture_all_until(
    socket: &Path,
    session: &str,
    pred: impl Fn(&str) -> bool,
    budget: Duration,
) -> String {
    let deadline = Instant::now() + budget;
    let mut last = String::new();
    loop {
        let panes = Command::new("tmux")
            .scrub_session_env()
            .arg("-S")
            .arg(socket)
            .args(["list-panes", "-t", session, "-F", "#{pane_id}"])
            .bounded_output()
            .expect("spawn tmux list-panes");
        if panes.status.success() {
            let mut frame = String::new();
            for pane in String::from_utf8_lossy(&panes.stdout).lines() {
                frame.push_str(&capture_until(socket, pane, |_| true, Duration::ZERO));
                frame.push('\n');
            }
            last = frame;
            if pred(&last) {
                return last;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The active pane's id and width for `session` — after a `split-window`, that
/// is the freshly created sidebar split.
fn tmux_current_pane(socket: &Path, session: &str) -> (String, usize) {
    let raw = tmux_capture(
        socket,
        &[
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_active} #{pane_id} #{pane_width}",
        ],
    );
    for line in raw.lines() {
        let mut cols = line.split_whitespace();
        if cols.next() == Some("1") {
            let id = cols.next().expect("pane id").to_owned();
            let width = cols
                .next()
                .and_then(|w| w.parse().ok())
                .expect("pane width");
            return (id, width);
        }
    }
    panic!("no active pane in {session}:\n{raw}");
}

fn tmux_pane_width(socket: &Path, pane: &str) -> Option<usize> {
    let out = Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(socket)
        .args(["display-message", "-p", "-t", pane, "#{pane_width}"])
        .bounded_output()
        .expect("spawn tmux display-message");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

fn tmux_pane_position(socket: &Path, pane: &str) -> (usize, usize) {
    let out = Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(socket)
        .args([
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_left} #{pane_top}",
        ])
        .bounded_output()
        .expect("spawn tmux display-message");
    assert!(out.status.success(), "pane geometry unavailable for {pane}");
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut fields = raw.split_whitespace();
    let left = fields
        .next()
        .expect("pane left")
        .parse()
        .expect("numeric pane left");
    let top = fields
        .next()
        .expect("pane top")
        .parse()
        .expect("numeric pane top");
    (left, top)
}

/// Whether `pane` still exists in `session`. A self-closed sidebar pane (and its
/// now-empty session) drops off the list, which is how the close is observed.
fn tmux_pane_alive(socket: &Path, session: &str, pane: &str) -> bool {
    let out = Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(socket)
        .args(["list-panes", "-a", "-F", "#{session_name}:#{pane_id}"])
        .bounded_output()
        .expect("spawn tmux list-panes");
    let expected = format!("{session}:{pane}");
    out.status.success()
        && String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|id| id == expected)
}

fn tmux_pane_for_pid(socket: &Path, session: &str, pid: u32) -> Option<String> {
    let out = Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(socket)
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}:#{pane_id}:#{pane_pid}",
        ])
        .bounded_output()
        .expect("spawn tmux pane pid listing");
    let prefix = format!("{session}:");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .find_map(|line| {
            let (pane, raw_pid) = line.rsplit_once(':')?;
            (raw_pid.parse::<u32>().ok() == Some(pid)).then(|| pane.to_owned())
        })
}

/// The rightmost painted column across a captured frame (trailing blanks
/// trimmed), a proxy for how wide the renderer painted.
fn max_line_width(frame: &str) -> usize {
    frame
        .lines()
        .map(|line| line.trim_end().chars().count())
        .max()
        .unwrap_or(0)
}

struct TmuxServerGuard {
    socket: std::path::PathBuf,
    _dir: Option<TempDir>,
}

impl TmuxServerGuard {
    fn new(socket: PathBuf) -> Self {
        Self { socket, _dir: None }
    }

    fn with_dir(socket: PathBuf, dir: TempDir) -> Self {
        Self {
            socket,
            _dir: Some(dir),
        }
    }
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .scrub_session_env()
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .bounded_output();
    }
}

struct ZellijSessionGuard {
    name: String,
    namespace: ZellijNamespace,
}

impl Drop for ZellijSessionGuard {
    fn drop(&mut self) {
        self.namespace.delete_session(&self.name);
    }
}
